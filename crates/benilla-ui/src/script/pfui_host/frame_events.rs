//! Frame-valued extension events reuse the engine's protected handler calling convention.

use super::Model;
use mlua::{Lua, Table, Value};

pub(crate) fn fire(lua: &Lua, event: &str, frame: Table) {
    let mut index = 0;
    loop {
        let next = {
            let mut model = lua.app_data_mut::<Model>().expect("model");
            model
                .event_to_frames
                .get(event)
                .and_then(|list| list.get(index))
                .copied()
                .map(|handle| (handle, model.frame_id(handle)))
        };
        let Some((handle, id)) = next else { break };
        dispatch(lua, id, event, &frame);
        let model = lua.app_data_ref::<Model>().expect("model");
        if model
            .event_to_frames
            .get(event)
            .and_then(|list| list.get(index))
            == Some(&handle)
        {
            index += 1;
        }
    }
    let all = lua
        .app_data_ref::<Model>()
        .expect("model")
        .all_event_frames
        .clone();
    for handle in all {
        let id = {
            let mut model = lua.app_data_mut::<Model>().expect("model");
            if !model.all_event_frames.contains(&handle)
                || model
                    .event_to_frames
                    .get(event)
                    .is_some_and(|list| list.contains(&handle))
            {
                continue;
            }
            model.frame_id(handle)
        };
        dispatch(lua, id, event, &frame);
    }
}

fn dispatch(lua: &Lua, id: u32, event: &str, frame: &Table) {
    if let Err(error) = super::fire(
        lua,
        id,
        "OnEvent",
        Some(event),
        vec![Value::Table(frame.clone())],
    ) {
        lua.app_data_mut::<Model>()
            .expect("model")
            .record_script_error(error.to_string());
    }
}
