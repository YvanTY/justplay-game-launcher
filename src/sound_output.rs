//! A game's sound on the screen it plays on: the TV's HDMI sound output carries the TV's own
//! name ("TOSHIBA-TV"), so the screen chosen for games finds its output. The game's processes
//! are routed there the way the volume mixer's per-app output does it, so other apps keep
//! their sound where it was. Windows has no public API for this; the mixer's own (used by
//! EarTrumpet and SoundSwitch) is called through its interface table.

use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use windows_sys::core::GUID;

/// A sound output (render endpoint).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// Windows' endpoint id: `{0.0.0.00000000}.{guid}`.
    pub id: String,
    /// "TOSHIBA-TV", "Haut-parleur"...
    pub name: String,
}

const RENDER_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render";
/// PKEY_Device_DeviceDesc: the output's own name.
const NAME_VALUE: &str = "{a45c254e-df1c-4efd-8020-67d146a850e0},2";
const DEVICE_STATE_ACTIVE: u32 = 1;

/// The sound outputs that can play now, from Windows' audio device list.
pub fn outputs() -> Vec<Output> {
    let mut list = Vec::new();
    unsafe {
        let mut render: HKEY = null_mut();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            wide(RENDER_KEY).as_ptr(),
            0,
            KEY_READ | KEY_WOW64_64KEY,
            &mut render,
        ) != 0
        {
            return list;
        }
        for index in 0.. {
            let mut name = [0u16; 64];
            let mut len = name.len() as u32;
            let found = RegEnumKeyExW(
                render,
                index,
                name.as_mut_ptr(),
                &mut len,
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            );
            if found != 0 {
                break;
            }
            let guid = String::from_utf16_lossy(&name[..len as usize]);
            let mut state = 0u32;
            let mut size = 4u32;
            let active = RegGetValueW(
                render,
                wide(&guid).as_ptr(),
                wide("DeviceState").as_ptr(),
                RRF_RT_REG_DWORD,
                null_mut(),
                (&mut state as *mut u32).cast(),
                &mut size,
            ) == 0
                && state == DEVICE_STATE_ACTIVE;
            if !active {
                continue;
            }
            let mut text = [0u16; 256];
            let mut size = (text.len() * 2) as u32;
            let named = RegGetValueW(
                render,
                wide(&format!(r"{guid}\Properties")).as_ptr(),
                wide(NAME_VALUE).as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                text.as_mut_ptr().cast(),
                &mut size,
            ) == 0;
            if named {
                let chars = (size as usize / 2).saturating_sub(1);
                list.push(Output {
                    id: format!("{{0.0.0.00000000}}.{}", guid.to_lowercase()),
                    name: String::from_utf16_lossy(&text[..chars]),
                });
            }
        }
        RegCloseKey(render);
    }
    list
}

/// The output that plays on the monitor named `monitor` (its HDMI or DisplayPort sound).
pub fn for_monitor<'a>(monitor: &str, outputs: &'a [Output]) -> Option<&'a Output> {
    let monitor = monitor.trim().to_lowercase();
    if monitor.is_empty() {
        return None;
    }
    outputs
        .iter()
        .find(|o| o.name.trim().to_lowercase() == monitor)
        .or_else(|| {
            outputs
                .iter()
                .find(|o| o.name.to_lowercase().contains(&monitor))
        })
}

// The Windows SDK's import library for the WinRT core functions (combase.dll).
#[link(name = "runtimeobject")]
unsafe extern "system" {
    fn RoGetActivationFactory(
        class: *mut c_void,
        iid: *const GUID,
        factory: *mut *mut c_void,
    ) -> i32;
    fn WindowsCreateString(text: *const u16, len: u32, string: *mut *mut c_void) -> i32;
    fn WindowsDeleteString(string: *mut c_void) -> i32;
}

/// IAudioPolicyConfigFactory: Windows 10 21H2 and later, then the older one.
const FACTORY_IIDS: [GUID; 2] = [
    GUID::from_u128(0xab3d4648_e242_459f_b02f_541c70306324),
    GUID::from_u128(0x2a59116d_6c4f_45e0_a74f_707e3fef9258),
];
/// Slot of SetPersistedDefaultAudioEndpoint: after IUnknown and IInspectable (6) come 19
/// volume-group and chat methods.
const SET_ENDPOINT_SLOT: usize = 25;

type SetEndpoint = unsafe extern "system" fn(*mut c_void, u32, i32, i32, *mut c_void) -> i32;
type Release = unsafe extern "system" fn(*mut c_void) -> u32;

/// Sends the sound of process `pid` to `output` (an `Output::id`), or back to the default
/// output with None, as the volume mixer's per-app setting does. Windows remembers it for the
/// program file. Runs on its own thread, which owns its COM setup.
pub fn route(pid: u32, output: Option<String>) -> Result<(), String> {
    std::thread::spawn(move || unsafe { route_now(pid, output.as_deref()) })
        .join()
        .map_err(|_| "sound routing crashed".to_string())?
}

unsafe fn route_now(pid: u32, output: Option<&str>) -> Result<(), String> {
    // The mixer's own format for a render endpoint.
    let device = match output {
        Some(id) => Some(HString::new(&format!(
            r"\\?\SWD#MMDEVAPI#{id}#{{e6327cad-dcec-4949-ae8a-991e976a79d2}}"
        ))?),
        None => None,
    };
    let device_ptr = device.as_ref().map_or(null_mut(), |d| d.0);
    unsafe {
        with_policy(|factory, table| {
            let set: SetEndpoint = std::mem::transmute(*table.add(SET_ENDPOINT_SLOT));
            // eRender; the console and multimedia roles, as the mixer sets both.
            let codes = [0, 1].map(|role| set(factory, pid, 0, role, device_ptr));
            if codes.iter().any(|&code| code < 0) {
                return Err(format!(
                    "Windows refused the sound output (console 0x{:08x}, multimedia 0x{:08x})",
                    codes[0], codes[1]
                ));
            }
            Ok(())
        })
    }
}

/// Runs `call` with Windows' audio policy factory and its interface table, on this thread.
unsafe fn with_policy<T>(
    call: impl FnOnce(*mut c_void, *const usize) -> Result<T, String>,
) -> Result<T, String> {
    unsafe {
        let com = CoInitializeEx(null(), COINIT_MULTITHREADED as u32);
        let result = (|| {
            let class = HString::new("Windows.Media.Internal.AudioPolicyConfig")?;
            let mut factory: *mut c_void = null_mut();
            let version = FACTORY_IIDS
                .iter()
                .position(|iid| RoGetActivationFactory(class.0, iid, &mut factory) >= 0);
            if version.is_none() || factory.is_null() {
                return Err("Windows' per-app sound output isn't available".to_string());
            }
            if version != Some(0) {
                crate::diag::log("sound output: using the pre-21H2 audio policy interface");
            }
            let table = *(factory as *const *const usize);
            let result = call(factory, table);
            let release: Release = std::mem::transmute(*table.add(2));
            release(factory);
            result
        })();
        if com >= 0 {
            CoUninitialize();
        }
        result
    }
}

/// The output Windows has saved for process `pid` (empty: the default output).
#[cfg(test)]
fn routed(pid: u32) -> Result<String, String> {
    type GetEndpoint =
        unsafe extern "system" fn(*mut c_void, u32, i32, i32, *mut *mut c_void) -> i32;
    #[link(name = "runtimeobject")]
    unsafe extern "system" {
        fn WindowsGetStringRawBuffer(string: *mut c_void, len: *mut u32) -> *const u16;
    }
    std::thread::spawn(move || unsafe {
        with_policy(|factory, table| {
            let get: GetEndpoint = std::mem::transmute(*table.add(SET_ENDPOINT_SLOT + 1));
            let mut value = null_mut();
            let code = get(factory, pid, 0, 0, &mut value);
            if code < 0 {
                return Err(format!("0x{code:08x}"));
            }
            let value = HString(value);
            let mut len = 0;
            let chars = WindowsGetStringRawBuffer(value.0, &mut len);
            Ok(if chars.is_null() {
                String::new()
            } else {
                String::from_utf16_lossy(std::slice::from_raw_parts(chars, len as usize))
            })
        })
    })
    .join()
    .unwrap()
}

/// A WinRT string, deleted when dropped.
struct HString(*mut c_void);

impl HString {
    fn new(text: &str) -> Result<HString, String> {
        let chars: Vec<u16> = text.encode_utf16().collect();
        let mut string = null_mut();
        let code = unsafe { WindowsCreateString(chars.as_ptr(), chars.len() as u32, &mut string) };
        if code < 0 {
            return Err(format!("could not make a WinRT string (0x{code:08x})"));
        }
        Ok(HString(string))
    }
}

impl Drop for HString {
    fn drop(&mut self) {
        unsafe { WindowsDeleteString(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str) -> Output {
        Output {
            id: format!("id-{name}"),
            name: name.into(),
        }
    }

    #[test]
    fn a_screen_finds_its_own_sound_output() {
        let list = [output("Haut-parleur"), output("TOSHIBA-TV")];
        assert_eq!(for_monitor("TOSHIBA-TV", &list), Some(&list[1]));
        assert_eq!(for_monitor("toshiba-tv ", &list), Some(&list[1]));
        assert_eq!(for_monitor("LG TV", &list), None);
        assert_eq!(
            for_monitor("", &list),
            None,
            "unnamed monitors match nothing"
        );
    }

    /// Routes a stand-in program's sound to the output named `SOUND_OUTPUT`, reads it back,
    /// then puts it back on the default output. `STAND_IN_EXE` must be a copy of PowerShell
    /// (Windows remembers the choice per program file, so never the real one) and
    /// `STAND_IN_WINDOW` the script it runs.
    #[test]
    #[ignore]
    fn route_a_stand_in() {
        let var = |name| std::env::var(name).unwrap();
        let mut child = std::process::Command::new(var("STAND_IN_EXE"))
            .args([
                "-NoProfile",
                "-WindowStyle",
                "Hidden",
                "-ExecutionPolicy",
                "Bypass",
            ])
            .args(["-File", &var("STAND_IN_WINDOW")])
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        let outputs = outputs();
        let output = for_monitor(&var("SOUND_OUTPUT"), &outputs).expect("output found");
        let pid = child.id();
        route(pid, Some(output.id.clone())).unwrap();
        let saved = routed(pid);
        route(pid, None).unwrap();
        let cleared = routed(pid);
        let _ = child.kill();
        let _ = child.wait();
        println!(
            "routed to {}: saved {saved:?}, then {cleared:?}",
            output.name
        );
        assert!(saved.unwrap().contains(&output.id));
        assert_eq!(cleared.unwrap(), "");
    }

    #[test]
    fn this_pc_lists_its_sound_outputs() {
        // Every PC the launcher runs on plays sound somewhere; ids are Windows' endpoint ids.
        for output in outputs() {
            assert!(output.id.starts_with("{0.0.0.00000000}.{"), "{output:?}");
            assert!(!output.name.is_empty());
        }
    }
}
