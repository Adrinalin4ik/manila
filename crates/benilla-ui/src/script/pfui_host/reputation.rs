//! Structured reputation reads use the same visible-row resolver as the stock pane.

use super::{rows, FactionEntry, Model, Row};
use mlua::{Lua, Table};

fn data(lua: &Lua, model: &Model, entry: &FactionEntry) -> mlua::Result<Table> {
    let data = lua.create_table()?;
    data.set("factionID", entry.faction_id)?;
    data.set("name", entry.name.as_str())?;
    data.set("description", entry.description.as_str())?;
    data.set("reaction", entry.standing_id)?;
    data.set("currentReactionThreshold", entry.bar_min)?;
    data.set("nextReactionThreshold", entry.bar_max)?;
    data.set("currentStanding", entry.standing)?;
    data.set("atWarWith", entry.at_war)?;
    data.set("canToggleAtWar", entry.can_toggle_at_war)?;
    data.set("isHeader", entry.is_header)?;
    data.set(
        "isCollapsed",
        entry.is_header
            && model
                .reputation_collapsed
                .contains(&i64::from(entry.faction_id)),
    )?;
    data.set(
        "isWatched",
        model.reputation.watched == Some(entry.rep_list_id),
    )?;
    data.set("canSetInactive", !entry.is_header)?;
    for name in [
        "isHeaderWithRep",
        "isChild",
        "hasBonusRepGain",
        "isAccountWide",
    ] {
        data.set(name, false)?;
    }
    Ok(data)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let namespace = lua.create_table()?;
    namespace.set(
        "GetFactionDataByIndex",
        lua.create_function(|lua, index: i64| {
            let model = lua.app_data_ref::<Model>().expect("model");
            let row = usize::try_from(index)
                .ok()
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| rows(&model).get(i).copied());
            let entry = match row {
                Some(Row::Entry(i)) => model.reputation.entries.get(i),
                Some(Row::Header(i)) => model
                    .reputation_groups
                    .get(i)
                    .and_then(|group| u32::try_from(group.key).ok())
                    .filter(|id| *id != 0)
                    .and_then(|id| {
                        model
                            .reputation
                            .entries
                            .iter()
                            .find(|entry| entry.faction_id == id)
                    }),
                None => None,
            };
            entry.map(|entry| data(lua, &model, entry)).transpose()
        })?,
    )?;
    namespace.set(
        "GetWatchedFactionData",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model");
            model
                .reputation
                .watched
                .and_then(|watched| {
                    model
                        .reputation
                        .entries
                        .iter()
                        .find(|entry| entry.rep_list_id == watched)
                })
                .map(|entry| data(lua, &model, entry))
                .transpose()
        })?,
    )?;
    lua.globals().set("C_Reputation", namespace)
}
