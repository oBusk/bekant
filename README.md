# bekant

Set up a Windows virtual desktop for a project.

`bekant <path>` creates a new virtual desktop named after the folder, switches to it, and opens
on it:

- Zed with the project (`zed -n <path>`)
- a new Firefox window

It then waits (up to 15 s) for each app's new window, moves it to the new desktop in case it opened
elsewhere, and resizes it to cover FancyZones zones: Zed zones 3-6, Firefox zone 2. Zone
rectangles come from FancyZones' default layout for the primary monitor's orientation (what a new
desktop gets); only custom grid layouts without spacing are supported. Zed and Firefox are
started detached, so closing the terminal doesn't close them.

## Requirements

- Windows 11 24H2, build 26100.2605 or newer (required by
  [`winvd`](https://crates.io/crates/winvd), which drives the undocumented virtual desktop COM API)
- `zed` on `PATH`
- Firefox at `C:\Program Files\Mozilla Firefox\firefox.exe`
- PowerToys FancyZones with a custom grid layout set as the default layout

## Build

```
cargo build --release
```

Build with the Windows toolchain (`x86_64-pc-windows-msvc`), not from WSL.

## Usage

```
bekant <path>
bekant .
```

`Z:\source\oBusk\example` gives a desktop named `example`. Running it twice creates two desktops.
