//! Small extension queries over action and equipment snapshots.

use crate::script::{action, Model};
use mlua::{Lua, MultiValue, Value};

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.set(
        "GetActionInfo",
        lua.create_function(|lua, slot: i64| {
            let model = lua.app_data_ref::<Model>().expect("model");
            let action = u32::try_from(slot)
                .ok()
                .filter(|slot| (1..=120).contains(slot))
                .and_then(|slot| model.actions.get(&slot));
            let Some(action) = action.filter(|action| action.action != 0) else {
                return Ok(MultiValue::new());
            };
            let (kind, id, subtype) = match action.kind & 0xf0 {
                action::ACTION_KIND_SPELL => ("spell", Some(action.action), Some("spell")),
                action::ACTION_KIND_ITEM => ("item", Some(action.action), None),
                action::ACTION_KIND_MACRO => {
                    // The host stores macro payloads as 1-based slots, not client hash IDs.
                    if model.macros.get(action.action as usize).is_some() {
                        ("macro", Some(action.action), None)
                    } else {
                        ("item", None, None)
                    }
                }
                _ => return Ok(MultiValue::new()),
            };
            let mut values = vec![Value::String(lua.create_string(kind)?)];
            values.push(id.map_or(Value::Nil, |id| Value::Integer(i64::from(id))));
            if let Some(subtype) = subtype {
                values.push(Value::String(lua.create_string(subtype)?));
            }
            Ok(MultiValue::from_vec(values))
        })?,
    )?;
    globals.set(
        "GetInventoryItemID",
        lua.create_function(|lua, (token, slot): (String, i64)| {
            let model = lua.app_data_ref::<Model>().expect("model");
            let player = model.unit_guids.guid_of("player")?;
            let resolved = model.unit_guids.guid_of(&token).ok().flatten();
            let token = if player.is_some() && resolved == player {
                "player"
            } else {
                &token
            };
            Ok(usize::try_from(slot)
                .ok()
                .and_then(|slot| model.inv_slot(token, slot))
                .map(|slot| slot.item_id)
                .filter(|id| *id != 0))
        })?,
    )?;
    globals.set(
        "GetInventoryItemDurability",
        lua.create_function(|lua, slot: i64| {
            let model = lua.app_data_ref::<Model>().expect("model");
            let durability = (1..=19)
                .contains(&slot)
                .then(|| model.inventory_slots[slot as usize].as_ref())
                .flatten()
                .and_then(|slot| slot.durability)
                .filter(|(_, max)| *max > 0);
            Ok(match durability {
                Some((current, max)) => MultiValue::from_vec(vec![
                    Value::Integer(i64::from(current)),
                    Value::Integer(i64::from(max)),
                ]),
                None => MultiValue::new(),
            })
        })?,
    )?;
    Ok(())
}
