//! `bekant watch`: when a virtual desktop is closed, close the windows that were on it.
//!
//! Without `--close` it's a dry run that only prints which windows it would close.
//!
//! Windows moves a closed desktop's windows to a fallback desktop before telling us, so by the
//! time we hear about it they're mixed in with the fallback's own windows. We therefore keep a
//! record of which desktop each window is on and look the closed desktop up in that.

use std::collections::HashMap;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use windows::Win32::Foundation::HWND;
use windows::core::GUID;
use winvd::DesktopEvent;

use crate::window;

/// How often to record which desktop each window is on. A window that moves or opens less than
/// this long before its desktop is closed may be missed, which leaves it open.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Watch until stopped. With `close` false, only print what would be closed.
pub fn run(close: bool) -> Result<()> {
    let (tx, rx) = channel::<DesktopEvent>();
    // Dropping this stops the listener.
    let _listener =
        winvd::listen_desktop_events(tx).map_err(|e| anyhow!("listen_desktop_events: {e:?}"))?;

    if close {
        println!("watching for closed desktops; their windows will be closed. Ctrl+C to stop.");
    } else {
        println!("watching for closed desktops (dry run: nothing is closed). Ctrl+C to stop.");
    }
    let mut seen = Snapshot::take()?;
    let mut taken = Instant::now();
    loop {
        match rx.recv_timeout(REFRESH_INTERVAL) {
            Ok(DesktopEvent::DesktopDestroyed {
                destroyed,
                fallback,
            }) => {
                // Event desktops carry their GUID, so this works although the desktop is gone.
                let ids = destroyed.get_id().and_then(|d| Ok((d, fallback.get_id()?)));
                match ids {
                    Ok((destroyed, fallback)) => handle(&seen, destroyed, fallback, close),
                    Err(e) => eprintln!("desktop closed, but can't identify it: {e:?}"),
                }
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("desktop event listener stopped"),
        }
        // Other events (focus changes etc.) can arrive many times a second; don't refresh on
        // every one of them.
        if taken.elapsed() >= REFRESH_INTERVAL {
            // Fails if explorer.exe has restarted, which also kills the listener. Exit rather than
            // carry on deaf; restarting bekant fixes it.
            seen = Snapshot::take()?;
            taken = Instant::now();
        }
    }
}

/// Which desktop each main window was on at one point in time.
struct Snapshot {
    windows: Vec<SeenWindow>,
    desktop_names: HashMap<GUID, String>,
}

struct SeenWindow {
    hwnd: HWND,
    /// Kept to notice if the handle has been reused by a different window since.
    class: String,
    desktop: GUID,
}

impl Snapshot {
    fn take() -> Result<Self> {
        let desktops = winvd::get_desktops().map_err(|e| anyhow!("get_desktops: {e:?}"))?;
        let desktop_names = desktops
            .iter()
            .filter_map(|d| Some((d.get_id().ok()?, d.get_name().ok()?)))
            .collect();
        let windows = window::top_level_windows()
            .into_iter()
            .filter(|&hwnd| window::is_main_window(hwnd))
            .filter_map(|hwnd| {
                let desktop = winvd::get_desktop_by_window(hwnd).ok()?.get_id().ok()?;
                Some(SeenWindow {
                    hwnd,
                    class: window::class_name(hwnd),
                    desktop,
                })
            })
            .collect();
        Ok(Self {
            windows,
            desktop_names,
        })
    }
}

/// Close (or with `close` false, just list) the windows last seen on the `destroyed` desktop.
fn handle(seen: &Snapshot, destroyed: GUID, fallback: GUID, close: bool) {
    let name = match seen.desktop_names.get(&destroyed) {
        Some(name) if !name.is_empty() => name.as_str(),
        Some(_) => "(unnamed)",
        None => "(unknown)",
    };
    println!("desktop \"{name}\" closed");
    let mut any = false;
    for w in seen.windows.iter().filter(|w| w.desktop == destroyed) {
        any = true;
        let label = format!("{} [{}]", window::title(w.hwnd), window::class_name(w.hwnd));
        match skip_reason(w, fallback) {
            Some(reason) => println!("  {reason}: {label}"),
            None if !close => println!("  would close: {label}"),
            None => match window::close(w.hwnd) {
                // The app may still ask about unsaved work, so this isn't "closed".
                Ok(()) => println!("  asked to close: {label}"),
                Err(e) => println!("  couldn't ask to close: {label}: {e:#}"),
            },
        }
    }
    if !any {
        println!("  no windows were recorded on it");
    }
}

/// Why a window last seen on the closed desktop should be left open, if it should. Any doubt
/// means leave it open.
fn skip_reason(w: &SeenWindow, fallback: GUID) -> Option<&'static str> {
    if !window::is_main_window(w.hwnd) || window::class_name(w.hwnd) != w.class {
        return Some("skip (gone, or no longer the same window)");
    }
    // Some apps handle being closed from under their own modal dialog badly.
    if !window::is_enabled(w.hwnd) {
        return Some("skip (has a dialog open)");
    }
    let pinned = winvd::is_pinned_window(w.hwnd).unwrap_or(true)
        || winvd::is_pinned_app(w.hwnd).unwrap_or(true);
    if pinned {
        return Some("skip (pinned to all desktops)");
    }
    // If it isn't on the fallback, it was moved somewhere else on purpose after our snapshot.
    match winvd::get_desktop_by_window(w.hwnd).and_then(|d| d.get_id()) {
        Ok(id) if id == fallback => None,
        _ => Some("skip (not on the fallback desktop)"),
    }
}
