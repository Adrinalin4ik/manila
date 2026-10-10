//! Read the engine's viewed map; the player's current zone is a separate selection.

pub fn viewed_area(lua: &mlua::Lua) -> Option<u32> {
    let model = lua.app_data_ref::<crate::script::Model>()?;
    let map = &model.worldmap;
    if let Some(id) = map.direct_area {
        return map
            .direct_areas
            .iter()
            .find(|(row, _)| *row == id)
            .map(|(_, zone)| zone.area_id);
    }
    let (continent, zone) = map.selection;
    map.continents
        .get(continent.checked_sub(1)? as usize)?
        .zones
        .get(zone.checked_sub(1)? as usize)
        .map(|zone| zone.area_id)
}
