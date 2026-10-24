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
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, DISPLAYCONFIG_TOPOLOGY_CLONE, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_DATABASE_CURRENT, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    SDC_APPLY, SDC_TOPOLOGY_CLONE, SDC_TOPOLOGY_EXTEND, SetDisplayConfig,
};
use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CDS_NORESET, CDS_SET_PRIMARY, CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW,
    DISP_CHANGE_SUCCESSFUL, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICEW, DM_POSITION,
    ENUM_CURRENT_SETTINGS, EnumDisplayDevicesW, EnumDisplayMonitors, EnumDisplaySettingsW,
    GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
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

pub fn describe(choice: &ScreenChoice, screens: &[Screen]) -> String {
    match choice {
        ScreenChoice::Main if screens.len() < 2 => "Main screen (only one connected)".into(),
        ScreenChoice::Main => "Main screen".into(),
        ScreenChoice::Screen(device) => screens.iter().find(|s| s.device == *device).map_or_else(
            || "A screen that isn't connected".into(),
            |s| s.name.clone(),
        ),
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

/// Device name (`\\.\DISPLAY1`) -> the monitor's own name, from Windows' display configuration.
fn friendly_names() -> HashMap<String, String> {
    let mut names: HashMap<String, String> = HashMap::new();
    let Some((paths, _)) = query(QDC_ONLY_ACTIVE_PATHS) else {
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

fn query(flags: u32) -> Option<(Vec<DISPLAYCONFIG_PATH_INFO>, i32)> {
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
        Some((paths, topology))
    }
}

/// What Windows is set to now.
pub fn current(screens: &[Screen]) -> Layout {
    let duplicated = query(QDC_DATABASE_CURRENT)
        .is_some_and(|(_, topology)| topology == DISPLAYCONFIG_TOPOLOGY_CLONE);
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
            set_primary(&layout.primary, &screens)?;
        }
    }
    Ok(())
}

/// Makes `device` the main display: every screen moves so that it sits at (0, 0).
fn set_primary(device: &str, screens: &[Screen]) -> Result<(), String> {
    let target = screens
        .iter()
        .find(|s| s.device == device)
        .ok_or_else(|| format!("{device} isn't connected"))?;
    let (dx, dy) = (target.x, target.y);
    unsafe {
        let mut index = 0;
        loop {
            let mut adapter = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            if EnumDisplayDevicesW(null(), index, &mut adapter, 0) == 0 {
                break;
            }
            index += 1;
            if adapter.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP == 0 {
                continue;
            }
            let mut mode = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            if EnumDisplaySettingsW(
                adapter.DeviceName.as_ptr(),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            ) == 0
            {
                continue;
            }
            mode.Anonymous1.Anonymous2.dmPosition.x -= dx;
            mode.Anonymous1.Anonymous2.dmPosition.y -= dy;
            mode.dmFields = DM_POSITION;
            let mut flags = CDS_UPDATEREGISTRY | CDS_NORESET;
            if text(&adapter.DeviceName) == device {
                flags |= CDS_SET_PRIMARY;
            }
            let result = ChangeDisplaySettingsExW(
                adapter.DeviceName.as_ptr(),
                &mode,
                null_mut(),
                flags,
                null(),
            );
            if result != DISP_CHANGE_SUCCESSFUL {
                return Err(format!(
                    "Windows refused to move {} (error {result})",
                    text(&adapter.DeviceName)
                ));
            }
        }
        // Applies everything staged above at once.
        ChangeDisplaySettingsExW(null(), null(), null_mut(), 0, null());
    }
    Ok(())
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
    fn this_pc_has_a_main_screen() {
        let screens = screens();
        assert!(!screens.is_empty());
        assert!(screens[0].primary);
        assert!(screens[0].name.starts_with("Screen 1"));
        assert_eq!(current(&screens).primary, screens[0].device);
    }
}
