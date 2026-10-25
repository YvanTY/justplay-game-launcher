//! Screens: which are connected, and where Windows shows things. The launcher can be shown on
//! any screen, and a game can be sent to a chosen screen (made the main display while it runs,
//! since games open on the main display) or shown on all screens (Windows' Duplicate mode).
//! The layout the user had is restored afterwards. "Main screen" always means "leave Windows
//! as it is".

use std::collections::HashMap;
use std::ptr::{null, null_mut};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DISPLAYCONFIG_TOPOLOGY_CLONE, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_DATABASE_CURRENT, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig, SDC_ALLOW_CHANGES, SDC_APPLY,
    SDC_SAVE_TO_DATABASE, SDC_TOPOLOGY_CLONE, SDC_TOPOLOGY_EXTEND, SDC_USE_SUPPLIED_DISPLAY_CONFIG,
    SetDisplayConfig,
};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONULL, MONITORINFOEXW,
    MonitorFromWindow,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, GetWindowTextW, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos,
};

/// Where the launcher, or a game, is shown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenChoice {
    /// Wherever Windows shows things now.
    #[default]
    Main,
    /// One screen, by its Windows device name (`\\.\DISPLAY2`).
    Screen(String),
    /// The same picture on every screen.
    Duplicate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    pub device: String,
    /// The monitor's own name ("LG TV"), else "Screen N".
    pub name: String,
    pub primary: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// What Windows is set to: Duplicate or not, and which screen is the main display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub duplicated: bool,
    pub primary: String,
}

/// The layout a choice needs, starting from the user's own. A screen chosen for the launcher
/// only moves the launcher's window, so it needs nothing from Windows.
pub fn wanted(choice: &ScreenChoice, original: &Layout, for_game: bool) -> Layout {
    match choice {
        ScreenChoice::Main => original.clone(),
        ScreenChoice::Screen(device) if for_game => Layout {
            duplicated: false,
            primary: device.clone(),
        },
        ScreenChoice::Screen(_) => Layout {
            duplicated: false,
            primary: original.primary.clone(),
        },
        ScreenChoice::Duplicate => Layout {
            duplicated: true,
            primary: original.primary.clone(),
        },
    }
}

/// The choices Left/Right cycle through: Main, every other screen, and Duplicate (two or more
/// screens only).
pub fn choices(screens: &[Screen]) -> Vec<ScreenChoice> {
    let mut list = vec![ScreenChoice::Main];
    if screens.len() > 1 {
        list.extend(
            screens
                .iter()
                .filter(|s| !s.primary)
                .map(|s| ScreenChoice::Screen(s.device.clone())),
        );
        list.push(ScreenChoice::Duplicate);
    }
    list
}

pub fn step(current: &ScreenChoice, screens: &[Screen], delta: i32) -> ScreenChoice {
    let list = choices(screens);
    let index = list.iter().position(|c| c == current).unwrap_or(0) as i32;
    let next = (index + delta).rem_euclid(list.len() as i32) as usize;
    list[next].clone()
}

/// What a choice means with the screens connected now: a chosen screen that isn't connected,
/// or "All screens" with only one, falls back to the main screen. The choice itself is kept,
/// so plugging the screen back in brings it back.
pub fn effective(choice: &ScreenChoice, screens: &[Screen]) -> ScreenChoice {
    match choice {
        ScreenChoice::Screen(device) if !screens.iter().any(|s| s.device == *device) => {
            ScreenChoice::Main
        }
        ScreenChoice::Duplicate if screens.len() < 2 => ScreenChoice::Main,
        other => other.clone(),
    }
}

pub fn describe(choice: &ScreenChoice, screens: &[Screen]) -> String {
    match choice {
        ScreenChoice::Main if screens.len() < 2 => "Main screen (only one connected)".into(),
        ScreenChoice::Main => "Main screen".into(),
        ScreenChoice::Screen(device) => screens.iter().find(|s| s.device == *device).map_or_else(
            || "Main screen (chosen screen not connected)".into(),
            |s| s.name.clone(),
        ),
        ScreenChoice::Duplicate if screens.len() < 2 => "Main screen (only one connected)".into(),
        ScreenChoice::Duplicate => "All screens (same picture)".into(),
    }
}

/// The connected screens, the main display first.
pub fn screens() -> Vec<Screen> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        list: LPARAM,
    ) -> windows_sys::core::BOOL {
        unsafe {
            let list = &mut *(list as *mut Vec<HMONITOR>);
            list.push(monitor);
        }
        1
    }
    let mut monitors: Vec<HMONITOR> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect),
            &mut monitors as *mut Vec<HMONITOR> as LPARAM,
        )
    };
    let names = friendly_names();
    let mut screens: Vec<Screen> = monitors
        .into_iter()
        .filter_map(|monitor| {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            let ok = unsafe { GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()) };
            if ok == 0 {
                return None;
            }
            let device = text(&info.szDevice);
            let rect = info.monitorInfo.rcMonitor;
            Some(Screen {
                name: names.get(&device).cloned().unwrap_or_default(),
                primary: info.monitorInfo.dwFlags & 1 != 0,
                x: rect.left,
                y: rect.top,
                width: rect.right - rect.left,
                height: rect.bottom - rect.top,
                device,
            })
        })
        .collect();
    screens.sort_by_key(|s| (!s.primary, s.x, s.y));
    for (i, screen) in screens.iter_mut().enumerate() {
        let number = format!("Screen {}", i + 1);
        screen.name = if screen.name.is_empty() {
            number
        } else {
            format!("{number} · {}", screen.name)
        };
    }
    screens
}

/// The screen (`\\.\DISPLAY2`) showing most of `window`.
pub fn screen_of(window: HWND) -> Option<String> {
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONULL) };
    if monitor.is_null() {
        return None;
    }
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe { GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()) };
    (ok != 0).then(|| text(&info.szDevice))
}

/// Moves `window` to cover the screen `device`. Borderless and windowed games follow; a game
/// in exclusive full screen may ignore it.
pub fn move_to(window: HWND, device: &str) -> Result<(), String> {
    let screen = screens()
        .into_iter()
        .find(|s| s.device == device)
        .ok_or_else(|| format!("{device} isn't connected"))?;
    let ok = unsafe {
        SetWindowPos(
            window,
            null_mut(),
            screen.x,
            screen.y,
            screen.width,
            screen.height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    if ok == 0 {
        return Err(format!(
            "Windows refused to move the game window to {device}"
        ));
    }
    Ok(())
}

/// A window's title and position, for the log.
pub fn describe_window(window: HWND) -> String {
    let mut title = [0u16; 128];
    let len = unsafe { GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32) };
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { GetWindowRect(window, &mut rect) };
    format!(
        "\"{}\" at ({}, {}) {}x{}",
        String::from_utf16_lossy(&title[..len.max(0) as usize]),
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top
    )
}

/// The monitor's own name for a screen (`\\.\DISPLAY4` -> "TOSHIBA-TV"), if it reports one.
pub fn monitor_name(device: &str) -> Option<String> {
    friendly_names().remove(device)
}

/// Device name (`\\.\DISPLAY1`) -> the monitor's own name, from Windows' display configuration.
fn friendly_names() -> HashMap<String, String> {
    let mut names: HashMap<String, String> = HashMap::new();
    let Some((paths, _, _)) = query(QDC_ONLY_ACTIVE_PATHS) else {
        return names;
    };
    for path in paths {
        unsafe {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = path.sourceInfo.adapterId;
            source.header.id = path.sourceInfo.id;
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target.header.adapterId = path.targetInfo.adapterId;
            target.header.id = path.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0
                || DisplayConfigGetDeviceInfo(&mut target.header) != 0
            {
                continue;
            }
            let name = text(&target.monitorFriendlyDeviceName);
            if name.is_empty() {
                continue;
            }
            // Duplicated screens share one source: name both monitors.
            names
                .entry(text(&source.viewGdiDeviceName))
                .and_modify(|n| *n = format!("{n} + {name}"))
                .or_insert(name);
        }
    }
    names
}

#[allow(clippy::type_complexity)]
fn query(
    flags: u32,
) -> Option<(
    Vec<DISPLAYCONFIG_PATH_INFO>,
    Vec<DISPLAYCONFIG_MODE_INFO>,
    i32,
)> {
    unsafe {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) != 0 {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let mut topology = 0i32;
        let topology_out = if flags == QDC_DATABASE_CURRENT {
            &mut topology as *mut i32
        } else {
            null_mut()
        };
        if QueryDisplayConfig(
            flags,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            topology_out,
        ) != 0
        {
            return None;
        }
        paths.truncate(path_count as usize);
        modes.truncate(mode_count as usize);
        Some((paths, modes, topology))
    }
}

/// What Windows is set to now.
pub fn current(screens: &[Screen]) -> Layout {
    let duplicated = query(QDC_DATABASE_CURRENT)
        .is_some_and(|(_, _, topology)| topology == DISPLAYCONFIG_TOPOLOGY_CLONE);
    let primary = screens
        .iter()
        .find(|s| s.primary)
        .map(|s| s.device.clone())
        .unwrap_or_default();
    Layout {
        duplicated,
        primary,
    }
}

/// Switches Windows to `layout`. Only what differs is changed.
pub fn apply(layout: &Layout) -> Result<(), String> {
    let screens = screens();
    let now = current(&screens);
    if now.duplicated != layout.duplicated {
        let topology = if layout.duplicated {
            SDC_TOPOLOGY_CLONE
        } else {
            SDC_TOPOLOGY_EXTEND
        };
        let result = unsafe { SetDisplayConfig(0, null(), 0, null(), SDC_APPLY | topology) };
        if result != 0 {
            return Err(format!(
                "Windows refused to change the screen layout (error {result})"
            ));
        }
    }
    if !layout.duplicated && !layout.primary.is_empty() {
        let screens = self::screens();
        let is_primary = screens
            .iter()
            .any(|s| s.primary && s.device == layout.primary);
        if !is_primary && screens.iter().any(|s| s.device == layout.primary) {
            set_primary(&layout.primary)?;
        }
    }
    Ok(())
}

/// Makes `device` the main display: every screen moves so that it sits at (0, 0). Uses the
/// display-configuration API (as Windows' Settings does), which also works when the screens hang
/// off different graphics cards; ChangeDisplaySettingsEx refuses that (error -1). One call
/// applies the whole layout, so a refusal leaves nothing half-changed.
fn set_primary(device: &str) -> Result<(), String> {
    let (paths, mut modes, _) = query(QDC_ONLY_ACTIVE_PATHS)
        .ok_or_else(|| "Windows didn't report the screen layout".to_string())?;
    let source_mode = |path: &DISPLAYCONFIG_PATH_INFO| {
        let index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize;
        (index < modes.len() && modes[index].infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)
            .then_some(index)
    };
    let target = paths
        .iter()
        .find(|path| source_name(path).as_deref() == Some(device))
        .and_then(source_mode)
        .ok_or_else(|| format!("{device} isn't connected"))?;
    let origin = unsafe { modes[target].Anonymous.sourceMode.position };
    // Duplicated screens share a source mode: move each one once.
    let mut sources: Vec<usize> = paths.iter().filter_map(source_mode).collect();
    sources.sort_unstable();
    sources.dedup();
    for index in sources {
        let position = unsafe { &mut modes[index].Anonymous.sourceMode.position };
        position.x -= origin.x;
        position.y -= origin.y;
    }
    let result = unsafe {
        SetDisplayConfig(
            paths.len() as u32,
            paths.as_ptr(),
            modes.len() as u32,
            modes.as_ptr(),
            SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_SAVE_TO_DATABASE | SDC_ALLOW_CHANGES,
        )
    };
    if result != 0 {
        return Err(format!(
            "Windows refused to change the main display (error {result})"
        ));
    }
    Ok(())
}

/// The Windows device name (`\\.\DISPLAY2`) of a display path's source.
fn source_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
    let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
    source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
    source.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
    source.header.adapterId = path.sourceInfo.adapterId;
    source.header.id = path.sourceInfo.id;
    (unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } == 0)
        .then(|| text(&source.viewGdiDeviceName))
}

fn text(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(device: &str, primary: bool) -> Screen {
        Screen {
            device: device.into(),
            name: device.into(),
            primary,
            x: if primary { 0 } else { 1920 },
            y: 0,
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn one_screen_offers_only_main() {
        let one = [screen("D1", true)];
        assert_eq!(choices(&one), [ScreenChoice::Main]);
        assert_eq!(step(&ScreenChoice::Main, &one, 1), ScreenChoice::Main);
        assert_eq!(
            describe(&ScreenChoice::Main, &one),
            "Main screen (only one connected)"
        );
    }

    #[test]
    fn two_screens_cycle_main_other_and_duplicate() {
        let two = [screen("D1", true), screen("D2", false)];
        let list = choices(&two);
        assert_eq!(
            list,
            [
                ScreenChoice::Main,
                ScreenChoice::Screen("D2".into()),
                ScreenChoice::Duplicate
            ]
        );
        assert_eq!(step(&ScreenChoice::Main, &two, -1), ScreenChoice::Duplicate);
        assert_eq!(step(&ScreenChoice::Duplicate, &two, 1), ScreenChoice::Main);
        // A screen that was unplugged starts the cycle over.
        assert_eq!(step(&ScreenChoice::Screen("D9".into()), &two, 1), list[1]);
    }

    #[test]
    fn games_move_the_main_display_the_launcher_only_its_window() {
        let original = Layout {
            duplicated: false,
            primary: "D1".into(),
        };
        let tv = ScreenChoice::Screen("D2".into());
        assert_eq!(wanted(&tv, &original, true).primary, "D2");
        assert_eq!(wanted(&tv, &original, false), original);
        assert_eq!(wanted(&ScreenChoice::Main, &original, true), original);
        assert!(wanted(&ScreenChoice::Duplicate, &original, true).duplicated);
        // "Main" leaves a user's own Duplicate setting alone.
        let cloned = Layout {
            duplicated: true,
            primary: "D1".into(),
        };
        assert_eq!(wanted(&ScreenChoice::Main, &cloned, true), cloned);
    }

    #[test]
    fn a_screen_that_is_not_connected_means_the_main_screen() {
        let one = [screen(r"\\.\DISPLAY1", true)];
        let two = [
            screen(r"\\.\DISPLAY1", true),
            screen(r"\\.\DISPLAY4", false),
        ];
        let tv = ScreenChoice::Screen(r"\\.\DISPLAY4".into());
        assert_eq!(effective(&tv, &two), tv);
        assert_eq!(
            effective(&tv, &one),
            ScreenChoice::Main,
            "the TV is unplugged"
        );
        assert_eq!(
            effective(&ScreenChoice::Duplicate, &one),
            ScreenChoice::Main
        );
        assert_eq!(
            effective(&ScreenChoice::Duplicate, &two),
            ScreenChoice::Duplicate
        );
        assert_eq!(
            describe(&tv, &one),
            "Main screen (chosen screen not connected)"
        );
        // Left/Right from there offers only what is connected.
        assert_eq!(step(&tv, &one, 1), ScreenChoice::Main);
    }

    #[test]
    fn this_pc_has_a_main_screen() {
        let screens = screens();
        assert!(!screens.is_empty());
        assert!(screens[0].primary);
        assert!(screens[0].name.starts_with("Screen 1"));
        assert_eq!(current(&screens).primary, screens[0].device);
    }

    /// Changes the real screens for 3 s: `cargo test swap_main_screen -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn swap_main_screen() {
        let screens = screens();
        assert!(screens.len() > 1, "needs two screens");
        let original = current(&screens);
        let other = screens.iter().find(|s| !s.primary).unwrap().device.clone();
        let result = apply(&Layout {
            duplicated: false,
            primary: other.clone(),
        });
        let after = current(&self::screens());
        std::thread::sleep(std::time::Duration::from_secs(3));
        let restored = apply(&original);
        println!("swap: {result:?}, main was {after:?}; restore: {restored:?}");
        assert_eq!(result, Ok(()));
        assert_eq!(after.primary, other);
        assert_eq!(restored, Ok(()));
        assert_eq!(current(&self::screens()), original);
    }
}
