//! Read the stock cursor payload, without introducing a second cursor state.

use crate::script::cursor::{CursorPayload, EQUIPMENT_BAG};
use mlua::{Lua, MultiValue, Value};

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().set(
        "GetCursorInfo",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<crate::script::Model>().expect("model");
            let Some(payload) = &model.cursor else {
                return Ok(MultiValue::new());
            };
            let mut result = Vec::new();
            let (kind, id) = match payload {
                CursorPayload::Item(item) => ("item", item.item_id),
                CursorPayload::Spell(spell) => ("spell", spell.book_slot),
                CursorPayload::Macro(macro_) => ("macro", macro_.index),
                CursorPayload::Merchant(item) => ("merchant", item.row + 1),
                CursorPayload::Money(money) => ("money", money.copper),
                _ => return Ok(MultiValue::new()),
            };
            result.push(Value::String(lua.create_string(kind)?));
            result.push(Value::Integer(i64::from(id)));
            match payload {
                CursorPayload::Item(item) if item.bag != EQUIPMENT_BAG => {
                    if let Some(link) = &item.link {
                        result.push(Value::String(lua.create_string(link)?));
                    }
                }
                CursorPayload::Spell(spell) => {
                    result.push(Value::String(lua.create_string(&spell.book_type)?));
                    result.push(Value::Integer(i64::from(spell.spell_id)));
                }
                _ => {}
            }
            Ok(MultiValue::from_vec(result))
        })?,
    )
}
