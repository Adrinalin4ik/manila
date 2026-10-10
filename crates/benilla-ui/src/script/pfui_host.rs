//! Gated connections from the optional pfUI extension to the stock VM's state.
//! A feature-enabled build still uses the stock ABI until the extension installs its VM flag.

use mlua::{Function, Lua, MultiValue, Table, Value};

mod addons;
pub mod attributes;
pub mod auras;
pub mod casts;
mod cursor;
mod descriptors;
pub(crate) mod equipment_actions;
pub mod items;
pub mod maps;
mod merchant;
pub mod player_stats;
pub mod quests;
pub mod social;
pub(crate) mod tooltip_subjects;
pub mod totems;
pub mod units;

/// Timer callbacks are protected individually, but errors still belong in the VM error stream.
pub fn report_error(lua: &Lua, message: String) {
    lua.app_data_mut::<super::Model>()
        .expect("model")
        .record_script_error(message);
}

/// Extension-owned event payloads use the stock protected dispatcher and legacy argN globals.
pub fn fire_event(lua: &Lua, event: &str, args: Vec<super::ScriptValue>) {
    super::tick::fire_event_into(lua, event, args);
}

pub(crate) fn invoke_handler(
    lua: &Lua,
    handler: &Function,
    frame: Table,
    event: Option<&str>,
    extra: &[Value],
) -> mlua::Result<()> {
    if lua.named_registry_value::<Option<bool>>("manila.pfui.installed")? != Some(true) {
        return handler.call(());
    }
    if let Some(event) = event {
        let filters: Table = lua.named_registry_value("manila.pfui.unit_events")?;
        if let Value::Table(events) = filters.raw_get::<Value>(frame.clone())? {
            if let Value::Table(units) = events.raw_get::<Value>(event)? {
                if let Some(Value::String(unit)) = extra.first() {
                    let matches = units
                        .sequence_values::<String>()
                        .collect::<mlua::Result<Vec<_>>>()?
                        .iter()
                        .any(|allowed| allowed.eq_ignore_ascii_case(&unit.to_string_lossy()));
                    if !matches {
                        return Ok(());
                    }
                }
            }
        }
    }
    let mut args = Vec::with_capacity(extra.len() + 2);
    args.push(Value::Table(frame));
    if let Some(event) = event {
        args.push(Value::String(lua.create_string(event)?));
    }
    args.extend_from_slice(extra);
    handler.call(MultiValue::from_vec(args))
}

/// Publish the GUID resolved by the stock unit resolver. Its case folding and target hops are
/// reused, while the extension separately owns additional unit-token kinds.
pub fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    if !globals.get::<Value>("UnitGUID")?.is_nil() {
        return Err(mlua::Error::runtime("UnitGUID already installed"));
    }
    globals.set(
        "UnitGUID",
        lua.create_function(|lua, token: String| {
            let guid = if let Some(guid) = units::custom_guid(lua, &token) {
                guid
            } else {
                lua.app_data_ref::<super::Model>()
                    .expect("model")
                    .unit_guids
                    .guid_of(&token)?
            };
            Ok(guid
                .filter(|guid| *guid != 0)
                .map(|guid| format!("0x{guid:016X}")))
        })?,
    )?;
    let methods: Table = lua.named_registry_value(super::REG_FRAME_METHODS)?;
    methods.set(
        "IsEventRegistered",
        lua.create_function(|lua, (frame, event): (Table, String)| {
            let handle = super::object::frame_handle_of(lua, &frame)?;
            let registered = lua
                .app_data_ref::<super::Model>()
                .expect("model")
                .frame_events
                .get(&handle)
                .is_some_and(|events| events.contains(&event));
            let mut result = vec![Value::Boolean(registered)];
            if registered {
                let filters: Table = lua.named_registry_value("manila.pfui.unit_events")?;
                if let Value::Table(events) = filters.raw_get::<Value>(frame)? {
                    if let Value::Table(units) = events.raw_get::<Value>(event)? {
                        result.extend(
                            units
                                .sequence_values::<Value>()
                                .collect::<mlua::Result<Vec<_>>>()?,
                        );
                    }
                }
            }
            Ok(MultiValue::from_vec(result))
        })?,
    )?;
    globals.set("INTERFACE_VERSION", super::addon_gate::CLIENT_INTERFACE)?;
    items::install(lua)?;
    cursor::install(lua)?;
    merchant::install(lua)?;
    addons::install(lua)?;
    descriptors::install(lua)?;
    social::install(lua)?;
    units::install(lua)?;
    player_stats::install(lua)?;
    totems::install(lua)?;
    super::nameplate::pfui::install(lua)?;
    attributes::install(lua)?;
    auras::install(lua)?;
    super::tooltip_spell::pfui::install(lua)?;
    tooltip_subjects::install(lua)?;
    super::tooltip_item::pfui::install(lua)?;
    quests::install(lua)?;
    super::macros::pfui::install(lua)?;
    super::reputation::pfui::install(lua)?;
    Ok(())
}

pub(super) fn item_received(lua: &Lua, item_id: u32) {
    item_result(lua, item_id, true);
}

/// The host calls this for a server's negative item-query answer as well as successful loads.
pub fn item_result(lua: &Lua, item_id: u32, success: bool) {
    if lua
        .named_registry_value::<Option<bool>>("manila.pfui.installed")
        .ok()
        .flatten()
        == Some(true)
    {
        super::tick::fire_event_into(
            lua,
            "GET_ITEM_INFO_RECEIVED",
            vec![
                super::ScriptValue::Number(f64::from(item_id)),
                super::ScriptValue::Bool(success),
            ],
        );
    }
}

/// A load scope shared by TOC files, XML inclusions and synchronous LoadAddOn calls. Restoring
/// the enclosing scope keeps a dependent addon's private table from leaking into its caller.
pub struct AddonScope<'a> {
    lua: &'a Lua,
    previous: Value,
    active: bool,
}

impl Drop for AddonScope<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self
                .lua
                .set_named_registry_value("manila.pfui.addon_context", self.previous.clone());
        }
    }
}

pub fn addon_scope<'a>(lua: &'a Lua, name: &str) -> mlua::Result<AddonScope<'a>> {
    let active = lua.named_registry_value::<Option<bool>>("manila.pfui.installed")? == Some(true);
    let previous: Value = lua.named_registry_value("manila.pfui.addon_context")?;
    if active {
        let privates: Table = lua.named_registry_value("manila.pfui.addon_privates")?;
        let private = match privates.raw_get::<Value>(name)? {
            Value::Table(table) => table,
            _ => {
                let table = lua.create_table()?;
                privates.raw_set(name, table.clone())?;
                table
            }
        };
        let context = lua.create_table()?;
        context.set(1, name)?;
        context.set(2, private)?;
        lua.set_named_registry_value("manila.pfui.addon_context", context)?;
        // Stock globals a preceding load assigned are adapted before this addon reads them.
        if let Some(bridge) =
            lua.named_registry_value::<Option<Table>>("manila.pfui.globals_bridge")?
        {
            if let Some(adapt) = bridge.get::<Option<Function>>("beforeAddon")? {
                adapt.call::<()>(())?;
            }
        }
    }
    Ok(AddonScope {
        lua,
        previous,
        active,
    })
}

pub(crate) fn run_chunk(lua: &Lua, source: &[u8], name: &str) -> mlua::Result<()> {
    let chunk = lua
        .load(source)
        .set_name(name)
        .set_mode(mlua::ChunkMode::Text);
    if let Value::Table(context) = lua.named_registry_value("manila.pfui.addon_context")? {
        return chunk.call((context.get::<String>(1)?, context.get::<Table>(2)?));
    }
    chunk.exec()
}
