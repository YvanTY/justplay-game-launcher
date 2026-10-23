//! Starting games, noticing when they end, and moving windows to the front.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, RECT, WAIT_FAILED,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, INFINITE, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, SetProcessWorkingSetSize, TerminateProcess, WaitForMultipleObjects,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VK_MENU, keybd_event,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE,
    SW_SHOW, SetForegroundWindow, ShowWindow,
};
use windows_sys::core::BOOL;

use crate::library::Launch;

/// Fallback re-check interval when the game's processes can't be waited on (e.g. run as admin).
const FOLDER_POLL: Duration = Duration::from_secs(2);
/// WaitForMultipleObjects accepts at most this many handles.
const MAX_WAIT_HANDLES: usize = 64;
/// A game that exits this fast with an error code almost certainly failed to start.
const QUICK_EXIT: Duration = Duration::from_secs(5);

pub struct Running {
    pub dir: PathBuf,
    pub root_pid: u32,
}

#[derive(Debug)]
pub enum Ended {
    Normally,
    Failed(String),
}

/// Starts the game plus a watcher thread. `on_end` runs on that thread once nothing started
/// from the game's folder is still alive.
pub fn start(
    launch: &Launch,
    on_end: impl FnOnce(Ended) + Send + 'static,
) -> Result<Running, String> {
    let dir = launch
        .exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut child = Command::new(&launch.exe)
        .args(&launch.args)
        .current_dir(&dir)
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", launch.exe.display()))?;
    let root_pid = child.id();
    let watch_dir = dir.clone();
    std::thread::Builder::new()
        .name("game-watch".into())
        .spawn(move || on_end(watch(&mut child, &watch_dir)))
        .map_err(|e| format!("Could not watch the game: {e}"))?;
    Ok(Running { dir, root_pid })
}

fn watch(child: &mut Child, dir: &Path) -> Ended {
    let started = Instant::now();
    // Waiting on the process handle costs nothing while the game runs.
    let status = child.wait();
    // Launchers often start the real game and exit, so keep waiting while anything from the
    // folder runs (the same approach Playnite uses).
    let mut followed = false;
    if folder_trackable(dir) {
        loop {
            let pids = processes_in(dir);
            if pids.is_empty() {
                break;
            }
            followed = true;
            wait_for_any_exit(&pids);
        }
    }
    match status {
        Ok(status) if !status.success() && !followed && started.elapsed() < QUICK_EXIT => {
            let code = status
                .code()
                .map_or_else(|| "unknown".into(), |c| c.to_string());
            Ended::Failed(format!("closed right away (exit code {code})"))
        }
        Err(e) => Ended::Failed(format!("could not be watched: {e}")),
        _ => Ended::Normally,
    }
}

/// Sleeps until one of `pids` exits. Windows wakes the thread, so this costs no CPU while
/// the game runs; the caller rescans the folder afterwards to catch newly started processes.
fn wait_for_any_exit(pids: &[u32]) {
    let handles: Vec<HANDLE> = pids
        .iter()
        .take(MAX_WAIT_HANDLES)
        .map(|&pid| unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) })
        .filter(|handle| !handle.is_null())
        .collect();
    let waited = !handles.is_empty()
        && unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE) }
            != WAIT_FAILED;
    for handle in handles {
        unsafe { CloseHandle(handle) };
    }
    if !waited {
        std::thread::sleep(FOLDER_POLL);
    }
}

/// Folder tracking on a drive root or the Windows folder would wait on unrelated programs.
fn folder_trackable(dir: &Path) -> bool {
    let windows = std::env::var_os("WINDIR").map(PathBuf::from);
    dir.components().count() >= 3 && windows.is_none_or(|w| !starts_with_ignore_case(dir, &w))
}

/// Ends the game: the process we started plus everything else running from its folder.
pub fn close(running: &Running) {
    let mut pids = processes_in(&running.dir);
    pids.push(running.root_pid);
    for pid in pids {
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !handle.is_null() {
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }
}

/// The game's main window: the largest visible top-level window owned by one of its processes.
pub fn game_window(running: &Running) -> Option<HWND> {
    struct Search {
        pids: Vec<u32>,
        best: Option<(HWND, i64)>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, param: LPARAM) -> BOOL {
        let search = unsafe { &mut *(param as *mut Search) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if search.pids.contains(&pid) && unsafe { IsWindowVisible(hwnd) } != 0 {
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            unsafe { GetWindowRect(hwnd, &mut rect) };
            let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
            if search.best.is_none_or(|(_, best)| area > best) {
                search.best = Some((hwnd, area));
            }
        }
        1
    }
    let mut pids = processes_in(&running.dir);
    pids.push(running.root_pid);
    let mut search = Search { pids, best: None };
    unsafe { EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
    search.best.map(|(hwnd, _)| hwnd)
}

pub fn bring_to_front(hwnd: HWND) {
    unsafe {
        // Windows only lets a background process take focus right after it received input;
        // a synthetic Alt tap counts, which is the standard workaround launchers use.
        keybd_event(VK_MENU as u8, 0, KEYEVENTF_EXTENDEDKEY, 0);
        keybd_event(VK_MENU as u8, 0, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP, 0);
        // SW_RESTORE would also un-maximize, so only use it for minimized windows.
        ShowWindow(
            hwnd,
            if IsIconic(hwnd) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        SetForegroundWindow(hwnd);
    }
}

/// Hands the launcher's idle memory back to Windows while a game runs; it pages back in on return.
pub fn trim_memory() {
    unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };
}

/// Process ids whose executable lives inside `dir` (excluding the launcher itself).
fn processes_in(dir: &Path) -> Vec<u32> {
    let own = std::process::id();
    let mut found = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return found;
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snapshot, &mut entry);
        while ok != 0 {
            let pid = entry.th32ProcessID;
            if pid != 0
                && pid != own
                && process_path(pid).is_some_and(|path| starts_with_ignore_case(&path, dir))
            {
                found.push(pid);
            }
            ok = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
    }
    found
}

fn process_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let ok =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        (ok != 0).then(|| PathBuf::from(OsString::from_wide(&buffer[..len as usize])))
    }
}

/// Windows paths are case-insensitive; Path::starts_with compares case-sensitively.
fn starts_with_ignore_case(path: &Path, prefix: &Path) -> bool {
    let path = path.to_string_lossy().to_lowercase();
    let prefix = prefix.to_string_lossy().to_lowercase();
    let prefix = prefix.trim_end_matches('\\');
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_match_ignores_case_and_respects_folder_boundaries() {
        let dir = Path::new(r"E:\Yvan\PS3 Emulator");
        assert!(starts_with_ignore_case(
            Path::new(r"e:\yvan\ps3 emulator\rpcs3.exe"),
            dir
        ));
        assert!(!starts_with_ignore_case(
            Path::new(r"E:\Yvan\PS3 Emulator Old\rpcs3.exe"),
            dir
        ));
        assert!(starts_with_ignore_case(
            Path::new(r"C:\Games\x.exe"),
            Path::new(r"C:\Games\")
        ));
    }

    #[test]
    fn drive_roots_and_windows_folder_are_not_folder_tracked() {
        assert!(!folder_trackable(Path::new(r"C:\")));
        assert!(folder_trackable(Path::new(r"E:\Games\God of War")));
        if let Some(windir) = std::env::var_os("WINDIR") {
            assert!(!folder_trackable(&PathBuf::from(windir).join("System32")));
        }
    }

    #[test]
    fn finds_own_test_binary_folder_processes_but_not_itself() {
        // The test process itself is excluded even though it runs from target\debug\deps.
        let exe = std::env::current_exe().unwrap();
        let pids = processes_in(exe.parent().unwrap());
        assert!(!pids.contains(&std::process::id()));
    }

    #[test]
    fn launches_and_reports_quick_failure() {
        // `cmd /C exit 3` exits immediately with an error, like a misconfigured game.
        let windir = std::env::var_os("WINDIR").unwrap();
        let launch = Launch {
            exe: PathBuf::from(windir).join("System32").join("cmd.exe"),
            args: vec!["/C".into(), "exit 3".into()],
        };
        let (tx, rx) = std::sync::mpsc::channel();
        start(&launch, move |ended| tx.send(ended).unwrap()).unwrap();
        let ended = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            matches!(ended, Ended::Failed(ref m) if m.contains("exit code 3")),
            "{ended:?}"
        );
    }

    #[test]
    fn missing_exe_is_an_error_not_a_panic() {
        let launch = Launch {
            exe: PathBuf::from(r"Z:\nope\game.exe"),
            args: vec![],
        };
        assert!(start(&launch, |_| {}).is_err());
    }
}
