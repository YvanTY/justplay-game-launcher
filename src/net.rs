//! HTTPS GET over WinHTTP, which ships with Windows (TLS, proxies and redirects included), so the
//! launcher bundles no HTTP or TLS library.

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};

/// Art files are a few MB at most; anything bigger is not what was asked for.
const MAX_BODY: usize = 16 * 1024 * 1024;

#[derive(Debug, PartialEq)]
pub enum Error {
    /// The server answered, but not with 200 (404: that file doesn't exist).
    Status(u32),
    /// No answer: offline, DNS, TLS or timeout. Worth retrying later.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Status(code) => write!(f, "HTTP {code}"),
            Error::Failed(message) => f.write_str(message),
        }
    }
}

/// Closes a WinHTTP handle when it goes out of scope.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn open(handle: *mut c_void, step: &str) -> Result<Handle, Error> {
    if handle.is_null() {
        let code = unsafe { GetLastError() };
        return Err(Error::Failed(format!("{step} failed (error {code})")));
    }
    Ok(Handle(handle))
}

/// One session for the whole run: WinHTTP keeps connections to a server open inside a session,
/// so the many small requests of an art search reuse one TLS connection instead of each
/// paying for a new handshake. WinHTTP handles are safe to share between threads.
fn session() -> Result<*mut c_void, Error> {
    static SESSION: Mutex<usize> = Mutex::new(0);
    let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if *session == 0 {
        *session = unsafe { open_session() };
    }
    if *session == 0 {
        return Err(Error::Failed("WinHttpOpen failed".into()));
    }
    Ok(*session as *mut c_void)
}

unsafe fn open_session() -> usize {
    unsafe {
        let handle = WinHttpOpen(
            wide("GameLauncher/0.2").as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            null(),
            null(),
            0,
        );
        if !handle.is_null() {
            // Resolve, connect, send, receive (ms): a stalled server can't hang a worker.
            WinHttpSetTimeouts(handle, 10_000, 10_000, 15_000, 20_000);
        }
        handle as usize
    }
}

fn ok(result: i32, step: &str) -> Result<(), Error> {
    if result == 0 {
        let code = unsafe { GetLastError() };
        return Err(Error::Failed(format!("{step} failed (error {code})")));
    }
    Ok(())
}

pub fn get(url: &str) -> Result<Vec<u8>, Error> {
    get_with_header(url, None)
}

/// `header` is one extra request header, e.g. `Authorization: Bearer <key>`.
pub fn get_with_header(url: &str, header: Option<&str>) -> Result<Vec<u8>, Error> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::Failed(format!("not an https URL: {url}")))?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    unsafe {
        let connection = open(
            WinHttpConnect(
                session()?,
                wide(host).as_ptr(),
                INTERNET_DEFAULT_HTTPS_PORT,
                0,
            ),
            "WinHttpConnect",
        )?;
        let request = open(
            WinHttpOpenRequest(
                connection.0,
                wide("GET").as_ptr(),
                wide(&path).as_ptr(),
                null(),
                null(),
                null(),
                WINHTTP_FLAG_SECURE,
            ),
            "WinHttpOpenRequest",
        )?;
        ok(
            match header.map(wide) {
                // u32::MAX: the header string ends with its NUL.
                Some(header) => {
                    WinHttpSendRequest(request.0, header.as_ptr(), u32::MAX, null(), 0, 0, 0)
                }
                None => WinHttpSendRequest(request.0, null(), 0, null(), 0, 0, 0),
            },
            "WinHttpSendRequest",
        )?;
        ok(
            WinHttpReceiveResponse(request.0, null_mut()),
            "WinHttpReceiveResponse",
        )?;
        let mut status: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        ok(
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&mut status as *mut u32).cast(),
                &mut size,
                null_mut(),
            ),
            "WinHttpQueryHeaders",
        )?;
        if status != 200 {
            return Err(Error::Status(status));
        }
        let mut body = Vec::new();
        loop {
            let mut available: u32 = 0;
            ok(
                WinHttpQueryDataAvailable(request.0, &mut available),
                "WinHttpQueryDataAvailable",
            )?;
            if available == 0 {
                break;
            }
            if body.len() + available as usize > MAX_BODY {
                return Err(Error::Failed(format!("{url} is larger than expected")));
            }
            let start = body.len();
            body.resize(start + available as usize, 0);
            let mut read: u32 = 0;
            ok(
                WinHttpReadData(
                    request.0,
                    body[start..].as_mut_ptr().cast(),
                    available,
                    &mut read,
                ),
                "WinHttpReadData",
            )?;
            body.truncate(start + read as usize);
        }
        Ok(body)
    }
}

/// A title as search words: "Demon Slayer - Kimetsu no Yaiba" finds nothing on Steam, while
/// "Demon Slayer Kimetsu no Yaiba" does. Punctuation and trademark signs become spaces
/// (apostrophes are dropped: "Dante's" searches as "Dantes"). Matching still uses the title.
pub fn search_words(title: &str) -> String {
    title
        .chars()
        .filter(|c| *c != '\'' && *c != '’')
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Percent-encodes a query value (everything but unreserved characters).
pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_become_plain_search_words() {
        assert_eq!(
            search_words("Demon Slayer - Kimetsu no Yaiba"),
            "Demon Slayer Kimetsu no Yaiba"
        );
        assert_eq!(
            search_words("NARUTO SHIPPUDEN: Ultimate Ninja STORM 4"),
            "NARUTO SHIPPUDEN Ultimate Ninja STORM 4"
        );
        assert_eq!(
            search_words("The Last of Us™ Part I"),
            "The Last of Us Part I"
        );
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode("Dante's Inferno"), "Dante%27s%20Inferno");
        assert_eq!(encode("Ragnarök"), "Ragnar%C3%B6k");
    }

    /// `cargo test request_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn request_timing() {
        let url =
            "https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/1593500/logo.png";
        let started = std::time::Instant::now();
        for _ in 0..10 {
            get(url).unwrap();
        }
        println!("10 requests: {:?}", started.elapsed());
    }

    #[test]
    fn only_https_is_accepted() {
        assert!(matches!(get("http://example.com/"), Err(Error::Failed(_))));
    }
}
