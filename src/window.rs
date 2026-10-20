//! Finding and placing top-level windows. All the raw Win32 window calls live here.

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, MonitorFromWindow,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetClassNameW, GetWindow, GetWindowRect, IsWindowVisible, IsZoomed,
    SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow,
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
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    let owned = unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok();
    visible && !owned && String::from_utf16_lossy(&buf[..len]) == class
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
