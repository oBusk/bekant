//! bekant: create a named virtual desktop for a project folder, switch to it, open Zed and
//! Firefox there, and place their windows in FancyZones zones.

mod fancyzones;

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, MonitorFromWindow,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetClassNameW, GetWindow, GetWindowRect, IsWindowVisible, IsZoomed,
    SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow,
};

const FIREFOX_EXE: &str = r"C:\Program Files\Mozilla Firefox\firefox.exe";

/// How long to wait for the launched apps to open their windows.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// An app may still move its own window right after showing it (and FancyZones may snap it), so
/// windows are placed a second time after this long.
const SETTLE: Duration = Duration::from_millis(500);

// Process creation flags (winbase.h). Defined here because they aren't in the windows crate
// features we use.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// A rectangle in 1/10000ths of the monitor's work area, the same units FancyZones uses.
#[derive(Clone, Copy)]
struct Zone {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[derive(Clone, Copy)]
struct App {
    name: &'static str,
    /// Window class of the app's top-level windows, used to recognise its new window.
    class: &'static str,
    /// FancyZones zones the window should cover, numbered from 1 as FancyZones shows them.
    zones: &'static [usize],
}

// Zone numbers assume the "oBusk Standard" layout: 3 columns by 2 rows, numbered down each
// column.
const ZED: App = App {
    name: "zed",
    class: "Zed::Window",
    // The right two columns.
    zones: &[3, 4, 5, 6],
};
const FIREFOX: App = App {
    name: "Firefox",
    class: "MozillaWindowClass",
    // Bottom of the left column.
    zones: &[2],
};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let (Some(arg), None) = (args.next(), args.next()) else {
        bail!("usage: bekant <path>");
    };

    let dir = resolve_dir(Path::new(&arg))?;
    let name = dir
        .file_name()
        .ok_or_else(|| anyhow!("{} has no folder name", dir.display()))?
        .to_string_lossy()
        .into_owned();

    // Check before touching desktops, so a missing Firefox doesn't strand us on an empty one.
    if !Path::new(FIREFOX_EXE).is_file() {
        bail!("Firefox not found at {FIREFOX_EXE}");
    }

    // Otherwise window coordinates get scaled on monitors with display scaling.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        .context("SetProcessDpiAwarenessContext")?;

    // Read zones before touching desktops, so a layout problem doesn't leave an empty desktop.
    // Assumes the windows open on a monitor with the primary monitor's orientation.
    let layout = fancyzones::Layout::load_default(primary_monitor_is_horizontal()?)?;
    let mut zed = Command::new("zed");
    zed.arg("-n").arg(&dir);
    let mut firefox = Command::new(FIREFOX_EXE);
    firefox.arg("-new-window");
    let apps = [
        (ZED, layout.zone(ZED.zones)?, zed),
        (FIREFOX, layout.zone(FIREFOX.zones)?, firefox),
    ];

    // Zed and Firefox hand new-window requests to an already running instance, so the process
    // we spawn usually doesn't own the window. Instead, remember which windows exist now and
    // treat a new one with the right class as the app's.
    let existing = top_level_windows();

    // winvd::Error doesn't implement std::error::Error, so format it via Debug.
    let previous =
        winvd::get_current_desktop().map_err(|e| anyhow!("get_current_desktop: {e:?}"))?;
    let desktop = winvd::create_desktop().map_err(|e| anyhow!("create_desktop: {e:?}"))?;
    let setup = || -> Result<()> {
        desktop
            .set_name(&name)
            .map_err(|e| anyhow!("set_name: {e:?}"))?;
        winvd::switch_desktop(desktop).map_err(|e| anyhow!("switch_desktop: {e:?}"))
    };
    if let Err(e) = setup() {
        // Don't leave an empty, unnamed desktop behind on every failed run.
        let _ = winvd::remove_desktop(desktop, previous);
        return Err(e);
    }

    let mut errors = Vec::new();
    let mut waiting = Vec::new();
    for (app, zone, mut command) in apps {
        match spawn_detached(&mut command) {
            Ok(()) => waiting.push((app, zone)),
            Err(e) => errors.push(e.context(format!("failed to launch {}", app.name))),
        }
    }

    // A window opens on whichever desktop is current when it's created. That isn't necessarily
    // ours (the switch may not have settled, or the user switched away while the app was
    // starting), so move each new window to our desktop explicitly.
    let mut found = Vec::new();
    let deadline = Instant::now() + WINDOW_TIMEOUT;
    while !waiting.is_empty() && Instant::now() < deadline {
        sleep(POLL_INTERVAL);
        let windows = top_level_windows();
        let mut still_waiting = Vec::new();
        for (app, zone) in waiting {
            let new_window = windows
                .iter()
                .copied()
                .find(|w| !existing.contains(w) && is_app_window(*w, app.class));
            let Some(hwnd) = new_window else {
                still_waiting.push((app, zone));
                continue;
            };
            let placed = winvd::move_window_to_desktop(desktop, &hwnd)
                .map_err(|e| anyhow!("move_window_to_desktop: {e:?}"))
                .and_then(|()| move_to_zone(hwnd, &zone));
            match placed {
                Ok(()) => found.push((app, zone, hwnd)),
                Err(e) => errors.push(e.context(format!("failed to place {}", app.name))),
            }
        }
        waiting = still_waiting;
    }
    for (app, _) in waiting {
        errors.push(anyhow!(
            "no {} window appeared within {}s",
            app.name,
            WINDOW_TIMEOUT.as_secs()
        ));
    }

    if !found.is_empty() {
        sleep(SETTLE);
        for (app, zone, hwnd) in found {
            if let Err(e) = move_to_zone(hwnd, &zone) {
                errors.push(e.context(format!("failed to place {}", app.name)));
            }
        }
    }

    if !errors.is_empty() {
        let messages: Vec<_> = errors.iter().map(|e| format!("{e:#}")).collect();
        bail!("{}", messages.join("\n"));
    }

    println!("{name}");
    Ok(())
}

/// Make `path` absolute (handles `.` and `..`) and check that it's an existing directory.
fn resolve_dir(path: &Path) -> Result<PathBuf> {
    // std::path::absolute rather than canonicalize, which would return a `\\?\` path.
    let dir =
        std::path::absolute(path).with_context(|| format!("cannot resolve {}", path.display()))?;
    match dir.metadata() {
        Ok(m) if m.is_dir() => Ok(dir),
        Ok(_) => bail!("{} is not a directory", dir.display()),
        Err(e) => bail!("{}: {e}", dir.display()),
    }
}

/// Start a process that outlives both bekant and the terminal it was started from.
fn spawn_detached(cmd: &mut Command) -> Result<()> {
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// All top-level windows, on every virtual desktop.
fn top_level_windows() -> Vec<HWND> {
    unsafe extern "system" fn push(hwnd: HWND, list: LPARAM) -> BOOL {
        unsafe { (*(list.0 as *mut Vec<HWND>)).push(hwnd) };
        true.into()
    }
    let mut list = Vec::new();
    // EnumWindows only fails if the callback returns FALSE, which ours never does.
    let _ = unsafe { EnumWindows(Some(push), LPARAM(&mut list as *mut Vec<HWND> as isize)) };
    list
}

/// Whether `hwnd` is a visible, unowned window of class `class`, i.e. a main app window rather
/// than a popup, tooltip or hidden helper window.
fn is_app_window(hwnd: HWND, class: &str) -> bool {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    let owned = unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok();
    visible && !owned && String::from_utf16_lossy(&buf[..len]) == class
}

/// Whether the primary monitor is wider than it is tall. FancyZones picks a new desktop's default
/// layout by this.
fn primary_monitor_is_horizontal() -> Result<bool> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let monitor = unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        bail!("GetMonitorInfoW failed");
    }
    let r = info.rcMonitor;
    Ok(r.right - r.left >= r.bottom - r.top)
}

/// Resize `hwnd` to fill `zone` on the monitor it's on.
fn move_to_zone(hwnd: HWND, zone: &Zone) -> Result<()> {
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }

        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            bail!("GetMonitorInfoW failed");
        }
        let work = info.rcWork;
        let (width, height) = (work.right - work.left, work.bottom - work.top);

        // Windows have invisible resize borders outside their visible frame. Grow the target by
        // those so the visible edges line up with the zone, like FancyZones does.
        let mut outer = RECT::default();
        GetWindowRect(hwnd, &mut outer)?;
        let mut frame = RECT::default();
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame as *mut RECT as *mut _,
            size_of::<RECT>() as u32,
        )?;

        let left = work.left + width * zone.left / 10000 - (frame.left - outer.left);
        let top = work.top + height * zone.top / 10000 - (frame.top - outer.top);
        let right = work.left + width * zone.right / 10000 + (outer.right - frame.right);
        let bottom = work.top + height * zone.bottom / 10000 + (outer.bottom - frame.bottom);
        SetWindowPos(
            hwnd,
            None,
            left,
            top,
            right - left,
            bottom - top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )?;
    }
    Ok(())
}
