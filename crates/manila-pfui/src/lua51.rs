use mlua::{Lua, MultiValue, Table, Value, Variadic};

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    // The vendored parser checks this per VM; untouched clients retain their grammar.
    lua.set_named_registry_value("manila.pfui.lua51", true)?;
    // Reopen only string and math; never open IO, OS, debug or package libraries.
    // These standard Lua entry points take no arguments and return their library table.
    for (name, open) in [
        (
            "string",
            mlua::ffi::luaopen_string as mlua::ffi::lua_CFunction,
        ),
        ("math", mlua::ffi::luaopen_math as mlua::ffi::lua_CFunction),
    ] {
        let original: Table = lua.globals().get(name)?;
        let entries: Vec<_> = original
            .pairs::<Value, Value>()
            .collect::<mlua::Result<_>>()?;
        unsafe {
            lua.create_c_function(open)?.call::<Table>(())?;
        }
        // Stock string.format accepts WoW's positional %N$ format. Reopening the library must
        // add missing functions without replacing it or any existing addon-visible binding.
        for (key, value) in entries {
            original.raw_set(key, value)?;
        }
    }
    lua.globals().set(
        "select",
        lua.create_function(|_, values: Variadic<Value>| {
            let Some(index) = values.first() else {
                return Err(mlua::Error::runtime("select: index expected"));
            };
            let count = values.len() - 1;
            if matches!(index, Value::String(s) if s.as_bytes().as_ref() == b"#") {
                return Ok(MultiValue::from_vec(vec![Value::Integer(count as i64)]));
            }
            let index = match index {
                Value::Integer(i) => *i,
                Value::Number(n) if n.is_finite() => *n as i64,
                Value::String(s) => {
                    let n = s
                        .to_str()?
                        .parse::<f64>()
                        .map_err(|_| mlua::Error::runtime("select: invalid index"))?;
                    if !n.is_finite() {
                        return Err(mlua::Error::runtime("select: invalid index"));
                    }
                    n as i64
                }
                _ => return Err(mlua::Error::runtime("select: invalid index")),
            };
            let first = if index < 0 {
                count as i64 + index + 1
            } else {
                index
            };
            if first < 1 {
                return Err(mlua::Error::runtime("select: index out of range"));
            }
            Ok(MultiValue::from_vec(
                values.into_iter().skip(first as usize).collect(),
            ))
        })?,
    )?;
    Ok(())
}
