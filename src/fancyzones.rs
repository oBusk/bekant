//! Read zone rectangles from the FancyZones layout files.
//!
//! FancyZones gives a new virtual desktop the default layout for the monitor's orientation, and
//! bekant's desktop is always new, so only the default layout is read. Only custom grid layouts
//! without spacing are supported.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, ensure};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

/// A rectangle in 1/10000ths of the monitor's work area, the same units FancyZones uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct DefaultLayouts {
    default_layouts: Vec<DefaultLayout>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct DefaultLayout {
    monitor_configuration: String,
    layout: LayoutRef,
}

#[derive(Deserialize)]
struct LayoutRef {
    uuid: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct CustomLayouts {
    custom_layouts: Vec<CustomLayout>,
}

#[derive(Deserialize)]
struct CustomLayout {
    uuid: String,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    /// Shape depends on `kind`; parsed as `GridInfo` once we know it's a grid.
    info: serde_json::Value,
}

/// Row and column sizes are in 1/10000ths and sum to 10000. `cell_child_map[row][column]` is the
/// 0-based zone index of each cell; a zone spanning several cells appears in each of them.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct GridInfo {
    rows_percentage: Vec<i32>,
    columns_percentage: Vec<i32>,
    cell_child_map: Vec<Vec<usize>>,
    show_spacing: bool,
}

pub struct Layout {
    name: String,
    grid: GridInfo,
}

impl Layout {
    /// Load FancyZones' default layout for horizontal or vertical monitors.
    pub fn load_default(horizontal: bool) -> Result<Self> {
        let dir = fancyzones_dir()?;
        let orientation = if horizontal { "horizontal" } else { "vertical" };

        let defaults: DefaultLayouts = read_json(&dir.join("default-layouts.json"))?;
        let layout = defaults
            .default_layouts
            .into_iter()
            .find(|d| d.monitor_configuration == orientation)
            .ok_or_else(|| anyhow!("no default FancyZones layout for {orientation} monitors"))?
            .layout;
        ensure!(
            layout.kind == "custom",
            "the default FancyZones layout for {orientation} monitors is the built-in \"{}\" \
             template; only custom grid layouts are supported",
            layout.kind
        );

        let customs: CustomLayouts = read_json(&dir.join("custom-layouts.json"))?;
        let custom = customs
            .custom_layouts
            .into_iter()
            .find(|c| c.uuid == layout.uuid)
            .ok_or_else(|| anyhow!("FancyZones custom layout {} not found", layout.uuid))?;
        ensure!(
            custom.kind == "grid",
            "FancyZones layout \"{}\" is a {} layout; only grid layouts are supported",
            custom.name,
            custom.kind
        );
        let grid: GridInfo = serde_json::from_value(custom.info)
            .with_context(|| format!("invalid FancyZones layout \"{}\"", custom.name))?;
        ensure!(
            !grid.show_spacing,
            "FancyZones layout \"{}\" has spacing enabled, which isn't supported",
            custom.name
        );
        ensure!(
            grid.cell_child_map.len() == grid.rows_percentage.len()
                && grid
                    .cell_child_map
                    .iter()
                    .all(|row| row.len() == grid.columns_percentage.len()),
            "invalid FancyZones layout \"{}\": cell map doesn't match rows and columns",
            custom.name
        );

        Ok(Self {
            name: custom.name,
            grid,
        })
    }

    /// The rectangle covering `zones`, numbered from 1 as FancyZones shows them.
    pub fn zone(&self, zones: &[usize]) -> Result<Zone> {
        let columns = edges(&self.grid.columns_percentage);
        let rows = edges(&self.grid.rows_percentage);
        let mut rect: Option<Zone> = None;
        for &zone in zones {
            let mut found = false;
            for (r, row) in self.grid.cell_child_map.iter().enumerate() {
                for (c, &index) in row.iter().enumerate() {
                    if index + 1 != zone {
                        continue;
                    }
                    found = true;
                    let cell = Zone {
                        left: columns[c],
                        top: rows[r],
                        right: columns[c + 1],
                        bottom: rows[r + 1],
                    };
                    rect = Some(match rect {
                        None => cell,
                        Some(z) => Zone {
                            left: z.left.min(cell.left),
                            top: z.top.min(cell.top),
                            right: z.right.max(cell.right),
                            bottom: z.bottom.max(cell.bottom),
                        },
                    });
                }
            }
            ensure!(
                found,
                "FancyZones layout \"{}\" has no zone {zone}",
                self.name
            );
        }
        rect.ok_or_else(|| anyhow!("no zones given"))
    }
}

/// Cumulative edges from sizes: `[a, b, c]` becomes `[0, a, a + b, a + b + c]`.
fn edges(sizes: &[i32]) -> Vec<i32> {
    std::iter::once(0)
        .chain(sizes.iter().scan(0, |sum, size| {
            *sum += size;
            Some(*sum)
        }))
        .collect()
}

/// FancyZones' settings folder, found the same way PowerToys does: the LocalAppData known folder
/// (not the LOCALAPPDATA environment variable) plus `Microsoft\PowerToys`. It doesn't depend on
/// where PowerToys is installed.
fn fancyzones_dir() -> Result<PathBuf> {
    let local = unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None)
            .context("cannot find the LocalAppData folder")?;
        let local = OsString::from_wide(path.as_wide());
        CoTaskMemFree(Some(path.0 as *const _));
        local
    };
    Ok(Path::new(&local).join(r"Microsoft\PowerToys\FancyZones"))
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The "oBusk Standard" layout should give the same rectangles bekant used to hardcode.
    #[test]
    fn grid_zones() {
        let grid: GridInfo = serde_json::from_str(
            r#"{
                "rows": 2,
                "columns": 3,
                "rows-percentage": [5000, 5000],
                "columns-percentage": [3761, 3119, 3120],
                "cell-child-map": [[0, 2, 3], [1, 4, 5]],
                "show-spacing": false,
                "spacing": 16,
                "sensitivity-radius": 50
            }"#,
        )
        .unwrap();
        let layout = Layout {
            name: "test".into(),
            grid,
        };

        let zed = layout.zone(&[3, 4, 5, 6]).unwrap();
        assert_eq!(
            (zed.left, zed.top, zed.right, zed.bottom),
            (3761, 0, 10000, 10000)
        );
        let firefox = layout.zone(&[2]).unwrap();
        assert_eq!(
            (firefox.left, firefox.top, firefox.right, firefox.bottom),
            (0, 5000, 3761, 10000)
        );
        assert!(layout.zone(&[7]).is_err());
    }
}
