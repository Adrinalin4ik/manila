//! Overlay metadata from the owner's DBCs and tile geometry from the owner's BLP headers.

use crate::creature::{number, rows, string};
use mlua::{Lua, Table};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Tile {
    pub file: String,
    pub width: f64,
    pub height: f64,
    pub uv: [f64; 2],
    pub offset: [f64; 2],
}

#[derive(Clone, Debug, Default)]
pub struct Overlay {
    pub name: String,
    pub path: String,
    pub areas: Vec<u32>,
    pub size: [u32; 2],
    pub offset: [i32; 2],
    pub point: [u32; 2],
    pub hit: [i32; 4],
    pub grid: [usize; 2],
    pub upscaled: bool,
    pub tiles: Vec<Tile>,
}

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub zones: BTreeMap<u32, Vec<Overlay>>,
    parents: BTreeMap<u32, u32>,
}

fn dimensions(bytes: &[u8]) -> Option<[u32; 2]> {
    if !matches!(bytes.get(..4)?, b"BLP1" | b"BLP2") {
        return None;
    }
    let w = u32::from_le_bytes(bytes.get(12..16)?.try_into().ok()?);
    let h = u32::from_le_bytes(bytes.get(16..20)?.try_into().ok()?);
    (w > 0 && h > 0 && w <= 8192 && h <= 8192).then_some([w, h])
}

impl Overlay {
    fn resolve(&mut self, headers: &[[u32; 2]]) {
        if headers.is_empty() || self.size.contains(&0) {
            return;
        }
        // Only row/column edges can be partial. This locates an extra sliver column before
        // interpreting the row-major sequence, and leaves unrelated trailing art unused.
        let cell = headers.iter().flatten().copied().max().unwrap_or(256);
        let expected = self.size.map(|side| side.div_ceil(256) as usize);
        let columns = headers
            .iter()
            .take(expected[0] + 1)
            .position(|d| d[0] < cell)
            .map_or(expected[0], |i| i + 1)
            .max(1);
        let rows = headers
            .iter()
            .position(|d| d[1] < cell)
            .map_or(expected[1], |i| i / columns + 1)
            .max(1);
        let whole_cells = self.size.iter().all(|side| side.is_multiple_of(256));
        self.upscaled = headers.len() > columns * rows
            && whole_cells
            && headers.iter().all(|d| *d == [cell, cell]);
        self.grid = if self.upscaled {
            let columns = (headers.len() as f64 * f64::from(self.size[0]) / f64::from(self.size[1]))
                .sqrt()
                .round() as usize;
            let columns = columns.clamp(1, headers.len());
            [columns, headers.len().div_ceil(columns)]
        } else {
            [columns, rows]
        };
        let [columns, rows] = self.grid;
        let scale = f64::from(cell) / 256.0;
        self.tiles = headers
            .iter()
            .take(columns * rows)
            .enumerate()
            .map(|(index, dimensions)| {
                let col = index % columns;
                let row = index / columns;
                let mut size = [256.0; 2];
                let (uv, offset) = if self.upscaled {
                    size = [
                        f64::from(self.size[0]) / columns as f64,
                        f64::from(self.size[1]) / rows as f64,
                    ];
                    ([1.0; 2], [col as f64 * size[0], row as f64 * size[1]])
                } else {
                    for (axis, position, last) in [(0, col, columns - 1), (1, row, rows - 1)] {
                        if position == last {
                            let remainder = i64::from(self.size[axis]) - last as i64 * 256;
                            size[axis] = if remainder > 0 {
                                remainder as f64
                            } else {
                                f64::from(dimensions[axis]) / scale
                            };
                        }
                    }
                    (
                        std::array::from_fn(|axis| {
                            (size[axis] * scale / f64::from(dimensions[axis])).min(1.0)
                        }),
                        [col as f64 * 256.0, row as f64 * 256.0],
                    )
                };
                Tile {
                    file: format!("{}{}", self.path, index + 1),
                    width: size[0],
                    height: size[1],
                    uv,
                    offset: std::array::from_fn(|axis| f64::from(self.offset[axis]) + offset[axis]),
                }
            })
            .collect();
    }

    fn table(&self, lua: &Lua) -> mlua::Result<Table> {
        let table = lua.create_table()?;
        table.set("textureName", self.name.as_str())?;
        table.set("texturePath", self.path.as_str())?;
        table.set("areaID", self.areas.first().copied().unwrap_or(0))?;
        table.set(
            "areaIDs",
            lua.create_sequence_from(self.areas.iter().copied())?,
        )?;
        for (name, value) in [
            ("textureWidth", self.size[0]),
            ("textureHeight", self.size[1]),
            ("mapPointX", self.point[0]),
            ("mapPointY", self.point[1]),
        ] {
            table.set(name, value)?;
        }
        for (name, value) in [
            ("offsetX", self.offset[0]),
            ("offsetY", self.offset[1]),
            ("hitRectTop", self.hit[0]),
            ("hitRectLeft", self.hit[1]),
            ("hitRectBottom", self.hit[2]),
            ("hitRectRight", self.hit[3]),
        ] {
            table.set(name, value)?;
        }
        table.set("tileCols", self.grid[0])?;
        table.set("tileRows", self.grid[1])?;
        table.set("upscaled", self.upscaled)?;
        let tiles = lua.create_table()?;
        let files = lua.create_table()?;
        for (index, tile) in self.tiles.iter().enumerate() {
            let row = lua.create_table()?;
            row.set("file", tile.file.as_str())?;
            for (name, value) in [
                ("width", tile.width),
                ("height", tile.height),
                ("texCoordX", tile.uv[0]),
                ("texCoordY", tile.uv[1]),
                ("offsetX", tile.offset[0]),
                ("offsetY", tile.offset[1]),
            ] {
                row.set(name, value)?;
            }
            tiles.set(index + 1, row)?;
            files.set(index + 1, tile.file.as_str())?;
        }
        table.set("tiles", tiles)?;
        table.set("fileDataIDs", files)?;
        Ok(table)
    }
}

impl Catalog {
    pub fn read(
        read: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let maps = rows(read, "WorldMapArea", 8)?;
        let overlays = rows(read, "WorldMapOverlay", 17)?;
        let areas = rows(read, "AreaTable", 25)?;
        let mut catalog = Self::default();
        for row in areas.records() {
            catalog.parents.insert(number(row, 0)?, number(row, 2)?);
        }
        let mut mapping = BTreeMap::new();
        for row in maps.records() {
            let area = number(row, 2)?;
            if area == 0 {
                continue;
            }
            mapping.insert(number(row, 0)?, (area, string(&maps, row, 3)?));
            catalog.zones.entry(area).or_default();
        }
        for row in overlays.records() {
            let Some((area, folder)) = mapping.get(&number(row, 1)?) else {
                continue;
            };
            let name = string(&overlays, row, 8)?;
            let mut overlay = Overlay {
                path: format!("Interface\\WorldMap\\{folder}\\{name}"),
                name,
                areas: (2..6)
                    .map(|col| number(row, col))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .filter(|id| *id != 0)
                    .collect(),
                size: [number(row, 9)?, number(row, 10)?],
                offset: [number(row, 11)? as i32, number(row, 12)? as i32],
                point: [number(row, 6)?, number(row, 7)?],
                hit: [
                    number(row, 13)? as i32,
                    number(row, 14)? as i32,
                    number(row, 15)? as i32,
                    number(row, 16)? as i32,
                ],
                ..Default::default()
            };
            if !overlay.name.is_empty() && !overlay.size.contains(&0) {
                let headers: Vec<_> = (1..=256)
                    .map_while(|index| {
                        read(&format!("{}{index}.blp", overlay.path))
                            .and_then(|bytes| dimensions(&bytes))
                    })
                    .collect();
                overlay.resolve(&headers);
            }
            catalog.zones.entry(*area).or_default().push(overlay);
        }
        Ok(catalog)
    }

    pub fn install(&self, lua: &Lua) -> mlua::Result<()> {
        lua.set_app_data(self.clone());
        let api = lua.create_table()?;
        api.set(
            "GetMapOverlays",
            lua.create_function(|lua, area: Option<u32>| {
                let Some(mut area) =
                    area.or_else(|| benilla_ui::script::pfui_host::maps::viewed_area(lua))
                else {
                    return Ok(None);
                };
                let catalog = lua.app_data_ref::<Catalog>().expect("map catalog");
                for _ in 0..catalog.parents.len() {
                    if catalog.zones.contains_key(&area) {
                        break;
                    }
                    let Some(parent) = catalog
                        .parents
                        .get(&area)
                        .copied()
                        .filter(|parent| *parent != 0 && *parent != area)
                    else {
                        return Ok(None);
                    };
                    area = parent;
                }
                let Some(overlays) = catalog.zones.get(&area) else {
                    return Ok(None);
                };
                let list = lua.create_table()?;
                for (index, overlay) in overlays.iter().enumerate() {
                    list.set(index + 1, overlay.table(lua)?)?;
                }
                Ok(Some(list))
            })?,
        )?;
        lua.globals().set("C_Map", api)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tile_headers_resolve_padding_slivers_foreign_tails_and_both_hd_layouts() {
        let mut overlay = Overlay {
            path: "Map\\Area".into(),
            size: [500, 300],
            offset: [10, 20],
            ..Default::default()
        };
        overlay.resolve(&[[256, 256], [256, 256], [256, 64], [256, 64], [128, 128]]);
        assert_eq!(overlay.grid, [2, 2]);
        assert_eq!(overlay.tiles.len(), 4);
        assert_eq!(overlay.tiles[3].width, 244.0);
        assert_eq!(overlay.tiles[3].height, 44.0);
        assert_eq!(overlay.tiles[3].uv, [244.0 / 256.0, 44.0 / 64.0]);
        assert_eq!(overlay.tiles[3].offset, [266.0, 276.0]);
        overlay.size = [512, 300];
        overlay.resolve(&[
            [256, 256],
            [256, 256],
            [8, 256],
            [256, 64],
            [256, 64],
            [8, 64],
        ]);
        assert_eq!(overlay.grid, [3, 2]);
        assert_eq!(overlay.tiles[2].width, 8.0);
        assert_eq!(overlay.tiles[5].uv, [1.0, 44.0 / 64.0]);
        overlay.size = [256, 256];
        overlay.resolve(&[[256, 256]; 4]);
        assert!(overlay.upscaled);
        assert_eq!(overlay.grid, [2, 2]);
        assert_eq!(overlay.tiles[3].width, 128.0);
        overlay.size = [500, 300];
        overlay.resolve(&[[512, 512], [512, 512], [512, 128], [512, 128]]);
        assert!(!overlay.upscaled);
        assert_eq!(overlay.tiles[3].uv, [244.0 / 256.0, 44.0 / 64.0]);
    }
}
