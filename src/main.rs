//! bekant: create a named virtual desktop for a project folder, switch to it, and open Zed and
//! Firefox there.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

const FIREFOX: &str = r"C:\Program Files\Mozilla Firefox\firefox.exe";

// Process creation flags (winbase.h). Defined here to avoid pulling in windows-sys for two
// constants.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

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

    // winvd::Error doesn't implement std::error::Error, so format it via Debug.
    let desktop = winvd::create_desktop().map_err(|e| anyhow!("create_desktop: {e:?}"))?;
    desktop
        .set_name(&name)
        .map_err(|e| anyhow!("set_name: {e:?}"))?;
    winvd::switch_desktop(desktop).map_err(|e| anyhow!("switch_desktop: {e:?}"))?;

    // Windows open on whichever desktop is current when they are created. switch_desktop
    // returns before the switch animation finishes, so in theory a fast-starting window could
    // still land on the previous desktop. Zed and Firefox take long enough to create their
    // windows that we launch immediately for now; if that turns out flaky, add a short
    // configurable delay here.
    spawn_detached(Command::new("zed").arg("-n").arg(&dir)).context("failed to launch zed")?;
    spawn_detached(Command::new(FIREFOX).arg("-new-window")).context("failed to launch Firefox")?;

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
