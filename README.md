# bekant

Set up a Windows virtual desktop for a project.

`bekant <path>` creates a new virtual desktop named after the folder, switches to it, and opens
on it:

- Zed with the project (`zed -n <path>`)
- a new Firefox window

Then it exits. Zed and Firefox are started detached, so closing the terminal doesn't close them.

## Requirements

- Windows 11 24H2, build 26100.2605 or newer (required by
  [`winvd`](https://crates.io/crates/winvd), which drives the undocumented virtual desktop COM API)
- `zed` on `PATH`
- Firefox at `C:\Program Files\Mozilla Firefox\firefox.exe`

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
