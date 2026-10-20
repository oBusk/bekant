//! bekant: create a named virtual desktop for a project folder, switch to it, open Zed and
//! Firefox there, and place their windows in FancyZones zones.

mod fancyzones;
mod watch;
mod window;

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

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
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let arg = match args.as_slice() {
        [cmd] if cmd == "watch" => return watch::run(false),
        [cmd, flag] if cmd == "watch" && flag == "--close" => return watch::run(true),
        [path] => path,
        _ => bail!("usage: bekant <path> | bekant watch [--close]"),
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

    window::set_dpi_aware()?;

    // Read zones before touching desktops, so a layout problem doesn't leave an empty desktop.
    // Assumes the windows open on a monitor with the primary monitor's orientation.
    let layout = fancyzones::Layout::load_default(window::primary_monitor_is_horizontal()?)?;
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
    let existing = window::top_level_windows();

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
        let windows = window::top_level_windows();
        let mut still_waiting = Vec::new();
        for (app, zone) in waiting {
            let new_window = windows
                .iter()
                .copied()
                .find(|w| !existing.contains(w) && window::is_app_window(*w, app.class));
            let Some(hwnd) = new_window else {
                still_waiting.push((app, zone));
                continue;
            };
            let placed = winvd::move_window_to_desktop(desktop, &hwnd)
                .map_err(|e| anyhow!("move_window_to_desktop: {e:?}"))
                .and_then(|()| window::move_to_zone(hwnd, &zone));
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
            if let Err(e) = window::move_to_zone(hwnd, &zone) {
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
