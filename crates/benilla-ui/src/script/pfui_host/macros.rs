//! String-icon macro calls share the stock table, caps, dirty edge and persistence.

use super::{create_macro, edit_macro, flag, opt_string, Model};
use mlua::{Lua, MultiValue, Value};

fn icon(value: Option<&Value>) -> Option<String> {
    let Value::String(texture) = value? else {
        return None;
    };
    let texture = texture.to_string_lossy().replace('/', "\\");
    let basename = texture.rsplit('\\').next().unwrap_or("");
    (!basename.is_empty()).then(|| format!("Interface\\Icons\\{basename}"))
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let namespace = lua.create_table()?;
    namespace.set(
        "CreateMacro",
        lua.create_function(|lua, args: MultiValue| {
            let args: Vec<_> = args.into_iter().collect();
            let Some(name) = opt_string(args.first()) else {
                return Err(mlua::Error::runtime("C_Macro.CreateMacro requires a name"));
            };
            Ok(create_macro(
                &mut lua.app_data_mut::<Model>().expect("model"),
                &name,
                icon(args.get(1)),
                &opt_string(args.get(2)).unwrap_or_default(),
                false,
                flag(args.get(3)),
            ))
        })?,
    )?;
    namespace.set(
        "EditMacro",
        lua.create_function(|lua, args: MultiValue| {
            let args: Vec<_> = args.into_iter().collect();
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let index = match args.first() {
                Some(Value::String(name)) => model.macros.index_by_name(&name.to_string_lossy()),
                Some(value) => value
                    .as_integer()
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(0),
                _ => 0,
            };
            Ok(edit_macro(
                &mut model,
                index,
                opt_string(args.get(1)),
                icon(args.get(2)).map(Some),
                opt_string(args.get(3)),
                None,
            ))
        })?,
    )?;
    lua.globals().set("C_Macro", namespace)
}
