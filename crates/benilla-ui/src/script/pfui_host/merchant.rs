//! Merchant queries and paced junk sales reuse the vendor snapshot and sell queue.

use crate::script::Model;
use mlua::Lua;

fn junk(lua: &Lua) -> Vec<(i64, u32, u64)> {
    let model = lua.app_data_ref::<Model>().expect("model");
    if model.merchant.is_none() {
        return vec![];
    }
    let extras = lua.app_data_ref::<super::items::Extras>().expect("items");
    let mut result: Vec<_> = extras
        .instances
        .iter()
        .filter_map(|(&(bag, slot), item)| {
            if !(0..=4).contains(&bag) {
                return None;
            }
            let state = model.containers.get(&bag)?.slots.get(&slot)?;
            let template = model.item_templates.get(&item.item_id)?;
            (item.guid != 0 && !state.locked && template.quality == 0)
                .then_some((bag, slot, item.guid))
        })
        .collect();
    result.sort_unstable_by_key(|&(bag, slot, _)| (bag, slot));
    result
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let api = lua.create_table()?;
    api.set(
        "GetBuybackItemID",
        lua.create_function(|lua, index: i64| {
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(usize::try_from(index)
                .ok()
                .and_then(|index| index.checked_sub(1))
                .filter(|index| *index < 12)
                .and_then(|index| model.merchant.as_ref()?.buyback.get(index))
                .map(|item| item.item_id)
                .filter(|id| *id != 0))
        })?,
    )?;
    api.set(
        "GetNumJunkItems",
        lua.create_function(|lua, ()| Ok(junk(lua).len()))?,
    )?;
    let bridge = lua.create_table()?;
    bridge.set(
        "snapshot",
        lua.create_function(|lua, ()| {
            let result = lua.create_table()?;
            for (_, _, guid) in junk(lua) {
                result.push(format!("0x{guid:016X}"))?;
            }
            Ok(result)
        })?,
    )?;
    bridge.set(
        "context",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model
                .merchant
                .as_ref()
                .map(|_| format!("0x{:016X}", model.unit_guids.npc)))
        })?,
    )?;
    bridge.set(
        "sell",
        lua.create_function(|lua, guid: String| {
            let id = guid
                .strip_prefix("0x")
                .and_then(|hex| u64::from_str_radix(hex, 16).ok());
            let item = junk(lua).into_iter().find(|(_, _, guid)| Some(*guid) == id);
            if let Some((bag, slot, _)) = item {
                lua.app_data_mut::<Model>()
                    .expect("model")
                    .merchant_cursor_sells
                    .push((bag, slot));
            }
            Ok(item.is_some())
        })?,
    )?;
    lua.globals().set("C_MerchantFrame", api)?;
    lua.set_named_registry_value("manila.pfui.merchant_bridge", bridge)
}
