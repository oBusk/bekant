//! Finding and placing top-level windows. All the raw Win32 window calls live here.

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DWM_CLOAKED_APP, DWM_CLOAKED_INHERITED, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
    DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, MonitorFromWindow,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextW, IsWindowVisible, IsZoomed, SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER,
    SetWindowPos, ShowWindow, WS_EX_TOOLWINDOW,
};

use crate::fancyzones::Zone;

/// Use real pixel coordinates. Otherwise window coordinates get scaled on monitors with display
/// scaling. Call before any other function here.
pub fn set_dpi_aware() -> Result<()> {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        .context("SetProcessDpiAwarenessContext")
}

/// All top-level windows, on every virtual desktop.
pub fn top_level_windows() -> Vec<HWND> {
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
pub fn is_app_window(hwnd: HWND, class: &str) -> bool {
    is_visible_unowned(hwnd) && class_name(hwnd) == class
}

/// Whether `hwnd` is a window the user would think of as an open app window, of any app.
///
/// Stricter than `is_app_window`, since it's used to pick windows to close: it must also have a
/// title, not be a tool window, and not be cloaked by its app (suspended UWP apps leave invisible
/// windows like that around). Windows on other virtual desktops are cloaked by the shell instead,
/// and still count.
pub fn is_main_window(hwnd: HWND) -> bool {
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    let mut cloaked = 0u32;
    let cloak_known = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            size_of::<u32>() as u32,
        )
    }
    .is_ok();
    is_visible_unowned(hwnd)
        && ex_style & WS_EX_TOOLWINDOW.0 == 0
        && cloak_known
        && cloaked & (DWM_CLOAKED_APP | DWM_CLOAKED_INHERITED) == 0
        && !title(hwnd).is_empty()
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..len])
}

pub fn title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..len])
}

fn is_visible_unowned(hwnd: HWND) -> bool {
    let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    let owned = unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok();
    visible && !owned
}

/// Whether the primary monitor is wider than it is tall. FancyZones picks a new desktop's default
/// layout by this.
pub fn primary_monitor_is_horizontal() -> Result<bool> {
    let monitor = unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
    let r = monitor_info(monitor)?.rcMonitor;
    Ok(r.right - r.left >= r.bottom - r.top)
}

/// Resize `hwnd` to fill `zone` on the monitor it's on.
pub fn move_to_zone(hwnd: HWND, zone: &Zone) -> Result<()> {
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }

        let work = monitor_info(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST))?.rcWork;
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

fn monitor_info(monitor: HMONITOR) -> Result<MONITORINFO> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        bail!("GetMonitorInfoW failed");
    }
    Ok(info)
}
