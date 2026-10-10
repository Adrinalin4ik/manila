//! Render the same aura as the modern filtered query through the existing spell builder.

use mlua::{Lua, Table};

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let methods: Table = lua.named_registry_value(crate::script::tooltip::REG_TOOLTIP_METHODS)?;
    methods.set(
        "SetSpellByID",
        lua.create_function(|lua, (this, id): (Table, u32)| {
            super::set_spell_by_id(
                lua,
                &this,
                id,
                None,
                super::SpellRenderOpts::default(),
                None,
            )
        })?,
    )?;
    methods.set(
        "SetUnitAura",
        lua.create_function(
            |lua, (this, unit, index, filter): (Table, String, i64, Option<String>)| {
                let filter = if filter.as_deref().is_some_and(|filter| {
                    filter
                        .split(['|', ' '])
                        .any(|token| token.eq_ignore_ascii_case("HARMFUL"))
                }) {
                    "HARMFUL"
                } else {
                    "HELPFUL"
                };
                let aura = usize::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| {
                        crate::script::pfui_host::auras::list(lua, &unit, Some(filter))
                            .get(index)
                            .cloned()
                    });
                let (id, name) = aura
                    .map(|aura| (aura.state.spell_id, aura.state.name))
                    .unwrap_or((0, None));
                super::set_spell_by_id(
                    lua,
                    &this,
                    id,
                    name,
                    super::SpellRenderOpts {
                        aura: true,
                        ..Default::default()
                    },
                    None,
                )
            },
        )?,
    )
}
