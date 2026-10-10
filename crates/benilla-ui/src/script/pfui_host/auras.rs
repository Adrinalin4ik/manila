//! The modern aura projection, with real descriptor slots kept separately from stock DTOs.

use crate::script::{AuraState, Model};
use mlua::{Lua, MultiValue, Table, Value};
use std::collections::HashMap;

#[derive(Clone, PartialEq)]
pub struct Aura {
    /// The actual zero-based UNIT_FIELD_AURA slot; never the display-list position.
    pub slot: u8,
    pub state: AuraState,
    /// Unknown until a corresponding cast was observed.
    pub source_guid: Option<u64>,
    /// DBC mechanics and the descriptor's active aura effects, supplied by the host.
    pub crowd_control: bool,
}

#[derive(Default)]
struct Extras {
    by_guid: HashMap<u64, Vec<Aura>>,
}

/// The stock aura feed owns roster fallback even after a member or pet leaves object range.
pub fn has_group_alias(lua: &Lua, guid: u64) -> bool {
    lua.app_data_ref::<Model>().is_some_and(|model| {
        let units = &model.unit_guids;
        units
            .party
            .iter()
            .chain(&units.party_pets)
            .chain(&units.raid)
            .chain(&units.raid_pets)
            .any(|candidate| *candidate == guid && guid != 0)
    })
}

/// Enrich a streamed aura only after an observed successful cast names its source.
pub fn annotate(lua: &Lua, guid: u64, spell: u32, caster: u64, duration: f64, expires: f64) {
    let player = lua
        .app_data_ref::<Model>()
        .is_some_and(|m| m.unit_guids.player == guid);
    let mut changed = false;
    {
        let Some(mut extras) = lua.app_data_mut::<Extras>() else {
            return;
        };
        if let Some(auras) = extras.by_guid.get_mut(&guid) {
            for aura in auras.iter_mut().filter(|a| a.state.spell_id == spell) {
                let before = aura.clone();
                aura.source_guid = Some(caster);
                // A refresh updates remote timing; a player's duration packet stays authoritative.
                if !player || aura.state.duration == 0.0 {
                    aura.state.duration = duration;
                    aura.state.expiration_time = expires;
                }
                changed |= *aura != before;
            }
        }
    }
    if changed {
        for token in super::units::event_tokens(lua, guid) {
            crate::script::tick::fire_event_into(
                lua,
                "UNIT_AURA",
                vec![crate::script::ScriptValue::Str(token)],
            );
        }
    }
}

/// A raw removal/reuse ends that application's ownership even if no modern feed ran between.
pub fn forget_annotation(lua: &Lua, guid: u64, slot: u8, spell: u32) {
    let player = lua
        .app_data_ref::<Model>()
        .is_some_and(|m| m.unit_guids.player == guid);
    if let Some(mut extras) = lua.app_data_mut::<Extras>() {
        if let Some(aura) = extras.by_guid.get_mut(&guid).and_then(|list| {
            list.iter_mut()
                .find(|aura| aura.slot == slot && aura.state.spell_id == spell)
        }) {
            aura.source_guid = None;
            if !player {
                aura.state.duration = 0.0;
                aura.state.expiration_time = 0.0;
            }
        }
    }
}

/// Feed before UNIT_AURA dispatch. The host supplies raw slots, including out-of-range roster slots.
pub fn set_auras(lua: &Lua, guid: u64, mut auras: Option<Vec<Aura>>) {
    let player = lua
        .app_data_ref::<Model>()
        .is_some_and(|m| m.unit_guids.player == guid);
    if let (Some(incoming), Some(extras)) = (&mut auras, lua.app_data_ref::<Extras>()) {
        for aura in incoming {
            if let Some(old) = extras.by_guid.get(&guid).and_then(|list| {
                list.iter()
                    .find(|old| old.slot == aura.slot && old.state.spell_id == aura.state.spell_id)
            }) {
                aura.source_guid = aura.source_guid.or(old.source_guid);
                if !player && aura.state.duration == 0.0 {
                    aura.state.duration = old.state.duration;
                    aura.state.expiration_time = old.state.expiration_time;
                }
                // Joining the two clocks can introduce sub-millisecond rounding drift.
                if (aura.state.duration - old.state.duration).abs() < 0.001 {
                    aura.state.duration = old.state.duration;
                }
                if (aura.state.expiration_time - old.state.expiration_time).abs() < 0.001 {
                    aura.state.expiration_time = old.state.expiration_time;
                }
            }
        }
    }
    // Legacy UnitBuff/UnitDebuff on GUID/focus aliases read this same live list.
    if let Some(mut model) = lua.app_data_mut::<Model>() {
        if guid != model.unit_guids.player {
            match &auras {
                Some(auras) => {
                    model
                        .unit_auras
                        .insert(guid, auras.iter().map(|a| a.state.clone()).collect());
                }
                None => {
                    model.unit_auras.remove(&guid);
                }
            }
        }
    }
    let (changed, timing_changed) = if let Some(mut extras) = lua.app_data_mut::<Extras>() {
        let changed = extras.by_guid.get(&guid) != auras.as_ref();
        let timing_changed = player
            && auras
                .as_ref()
                .zip(extras.by_guid.get(&guid))
                .is_some_and(|(incoming, previous)| {
                    incoming.iter().any(|aura| {
                        previous.iter().any(|old| {
                            old.slot == aura.slot
                                && old.state.spell_id == aura.state.spell_id
                                && (old.state.duration != aura.state.duration
                                    || old.state.expiration_time != aura.state.expiration_time)
                        })
                    })
                });
        if let Some(auras) = auras {
            extras.by_guid.insert(guid, auras);
        } else {
            extras.by_guid.remove(&guid);
        }
        (changed, timing_changed)
    } else {
        (false, false)
    };
    if changed {
        // Stock PLAYER_AURAS_CHANGED deliberately ignores countdown-only refreshes.
        let tokens = if timing_changed {
            super::units::event_tokens(lua, guid)
        } else {
            super::units::custom_tokens(lua, guid)
        };
        for token in tokens {
            crate::script::tick::fire_event_into(
                lua,
                "UNIT_AURA",
                vec![crate::script::ScriptValue::Str(token)],
            );
        }
    }
}

pub(crate) fn unit_guid(lua: &Lua, token: &str) -> Option<u64> {
    if let Some(guid) = super::units::custom_guid(lua, token) {
        return guid;
    }
    if let Some(hex) = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
    {
        return u64::from_str_radix(hex, 16).ok().filter(|guid| *guid != 0);
    }
    lua.app_data_ref::<Model>()?
        .unit_guids
        .guid_of(token)
        .ok()
        .flatten()
}

pub(crate) fn list(lua: &Lua, token: &str, filter: Option<&str>) -> Vec<Aura> {
    let Some(guid) = unit_guid(lua, token) else {
        return Vec::new();
    };
    let Some(extras) = lua.app_data_ref::<Extras>() else {
        return Vec::new();
    };
    let Some(auras) = extras.by_guid.get(&guid) else {
        return Vec::new();
    };
    let model = lua.app_data_ref::<Model>().expect("model");
    let tokens: Vec<_> = filter
        .unwrap_or("HELPFUL")
        .split(['|', ' '])
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    let has = |token: &str| tokens.iter().any(|s| s == token);
    let harmful = has("HARMFUL") || !has("HELPFUL");
    let helpful = has("HELPFUL") || !has("HARMFUL");
    let mut result: Vec<_> = auras
        .iter()
        .filter(|aura| {
            let state = &aura.state;
            if (state.helpful && (!helpful || has("!HELPFUL")))
                || (!state.helpful && (!harmful || has("!HARMFUL")))
            {
                return false;
            }
            let ours = aura.source_guid.is_some_and(|guid| {
                guid != 0 && (guid == model.unit_guids.player || guid == model.unit_guids.pet)
            });
            if (has("PLAYER") && !ours) || (has("!PLAYER") && ours) {
                return false;
            }
            let dispellable = state
                .debuff_type
                .as_deref()
                .is_some_and(|ty| matches!(ty, "Magic" | "Curse" | "Disease" | "Poison"));
            !((has("DISPELLABLE") && !dispellable)
                || (has("!DISPELLABLE") && dispellable)
                || (has("CROWD_CONTROL") && !aura.crowd_control)
                || (has("!CROWD_CONTROL") && aura.crowd_control))
        })
        .cloned()
        .collect();
    // Descriptor order, with ordinary harmful slots before overflow slots.
    result.sort_by_key(|aura| {
        (
            !aura.state.helpful,
            !aura.state.helpful && aura.slot < 32,
            aura.slot,
        )
    });
    result
}

fn slot(lua: &Lua, token: &str, slot: i64) -> Option<Aura> {
    let guid = unit_guid(lua, token)?;
    lua.app_data_ref::<Extras>()?
        .by_guid
        .get(&guid)?
        .iter()
        .find(|aura| i64::from(aura.slot) == slot)
        .cloned()
}

fn source(lua: &Lua, aura: &Aura) -> Option<String> {
    super::units::token_from_guid(lua, aura.source_guid?)
}

fn cast_by_player(aura: &Aura) -> bool {
    aura.source_guid.is_some_and(|guid| {
        // 1.12 player GUIDs have a zero high word; permanent pets use HIGHGUID_PET.
        guid != 0 && (guid >> 48 == 0 || guid >> 48 == 0xf140)
    })
}

fn data(lua: &Lua, aura: Option<Aura>) -> mlua::Result<Option<Table>> {
    let Some(aura) = aura else { return Ok(None) };
    let state = &aura.state;
    let table = lua.create_table()?;
    table.set("name", state.name.as_deref())?;
    table.set("icon", state.icon.as_deref())?;
    table.set("applications", state.count)?;
    table.set("spellId", state.spell_id)?;
    table.set("dispelName", state.debuff_type.as_deref().unwrap_or(""))?;
    table.set("duration", state.duration)?;
    table.set("expirationTime", state.expiration_time)?;
    table.set("sourceUnit", source(lua, &aura))?;
    table.set(
        "sourceGUID",
        aura.source_guid.map(|guid| format!("0x{guid:016X}")),
    )?;
    table.set("isHelpful", state.helpful)?;
    table.set("isHarmful", !state.helpful)?;
    table.set("isFromPlayerOrPlayerPet", cast_by_player(&aura))?;
    for name in [
        "isStealable",
        "isBossAura",
        "isNameplateOnly",
        "nameplateShowAll",
        "nameplateShowPersonal",
        "canApplyAura",
        "shouldConsolidate",
        "isRaid",
    ] {
        table.set(name, false)?;
    }
    table.set("charges", 0)?;
    table.set("maxCharges", 0)?;
    table.set("timeMod", 1)?;
    Ok(Some(table))
}

fn tuple(lua: &Lua, aura: Option<Aura>) -> mlua::Result<MultiValue> {
    let Some(aura) = aura else {
        return Ok(MultiValue::from_vec(vec![Value::Nil]));
    };
    let string = |text: Option<&str>| -> mlua::Result<Value> {
        match text {
            Some(text) => Ok(Value::String(lua.create_string(text)?)),
            None => Ok(Value::Nil),
        }
    };
    let state = &aura.state;
    Ok(MultiValue::from_vec(vec![
        string(state.name.as_deref())?,
        string(state.icon.as_deref())?,
        Value::Integer(i64::from(state.count)),
        string(state.debuff_type.as_deref())?,
        Value::Number(state.duration),
        Value::Number(state.expiration_time),
        string(source(lua, &aura).as_deref())?,
        Value::Boolean(false),
        Value::Boolean(false),
        Value::Integer(i64::from(state.spell_id)),
        Value::Boolean(false),
        Value::Boolean(false),
        Value::Boolean(cast_by_player(&aura)),
        Value::Boolean(false),
        Value::Number(1.0),
    ]))
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.set_app_data(Extras::default());
    let api = lua.create_table()?;
    api.set(
        "GetAuraDataByIndex",
        lua.create_function(
            |lua, (unit, index, filter): (String, i64, Option<String>)| {
                let aura = index
                    .checked_sub(1)
                    .and_then(|index| usize::try_from(index).ok())
                    .and_then(|index| list(lua, &unit, filter.as_deref()).get(index).cloned());
                data(lua, aura)
            },
        )?,
    )?;
    api.set(
        "UnitAura",
        lua.create_function(
            |lua, (unit, index, filter): (String, i64, Option<String>)| {
                let aura = index
                    .checked_sub(1)
                    .and_then(|index| usize::try_from(index).ok())
                    .and_then(|index| list(lua, &unit, filter.as_deref()).get(index).cloned());
                tuple(lua, aura)
            },
        )?,
    )?;
    api.set(
        "GetAuraDataBySlot",
        lua.create_function(|lua, (unit, id): (String, Option<i64>)| {
            data(lua, id.and_then(|id| slot(lua, &unit, id)))
        })?,
    )?;
    api.set(
        "UnitAuraBySlot",
        lua.create_function(|lua, (unit, id): (String, Option<i64>)| {
            tuple(lua, id.and_then(|id| slot(lua, &unit, id)))
        })?,
    )?;
    api.set(
        "GetAuraDataBySpellName",
        lua.create_function(
            |lua, (unit, name, filter): (String, String, Option<String>)| {
                // The spell-name lookup searches both signs unless the caller supplies a filter.
                let aura = list(
                    lua,
                    &unit,
                    Some(filter.as_deref().unwrap_or("HELPFUL|HARMFUL")),
                )
                .into_iter()
                .find(|aura| aura.state.name.as_deref() == Some(&name));
                data(lua, aura)
            },
        )?,
    )?;
    api.set(
        "GetAuraSlots",
        lua.create_function(
            |lua,
             (unit, filter, max, continuation, output): (
                String,
                Option<String>,
                Option<u32>,
                Option<usize>,
                Option<Table>,
            )| {
                let auras = list(lua, &unit, filter.as_deref());
                let start = continuation.unwrap_or(0).min(auras.len());
                let count = max
                    .filter(|max| *max > 0)
                    .map_or(auras.len(), |max| max as usize);
                let end = start.saturating_add(count).min(auras.len());
                let next = if end < auras.len() {
                    Value::Integer(end as i64)
                } else {
                    Value::Nil
                };
                let batch = &auras[start..end];
                if let Some(output) = output {
                    let previous = output
                        .raw_get::<Option<usize>>("n")?
                        .unwrap_or(output.raw_len())
                        .min(48);
                    for (index, aura) in batch.iter().enumerate() {
                        output.raw_set(index + 1, aura.slot)?;
                    }
                    for index in batch.len() + 1..=previous {
                        output.raw_set(index, Value::Nil)?;
                    }
                    output.raw_set("n", batch.len())?;
                    return Ok(MultiValue::from_vec(vec![
                        next,
                        Value::Integer(batch.len() as i64),
                    ]));
                }
                let mut values = vec![next];
                values.extend(
                    batch
                        .iter()
                        .map(|aura| Value::Integer(i64::from(aura.slot))),
                );
                Ok(MultiValue::from_vec(values))
            },
        )?,
    )?;
    api.set(
        "GetAuraDispelTypeColor",
        lua.create_function(|lua, name: Option<String>| {
            let (r, g, b) = match name.as_deref() {
                Some("Magic") => (0.2, 0.6, 1.0),
                Some("Curse") => (0.6, 0.0, 1.0),
                Some("Disease") => (0.6, 0.4, 0.0),
                Some("Poison") => (0.0, 0.6, 0.0),
                Some("Enrage") => (1.0, 0.55, 0.0),
                _ => (0.8, 0.0, 0.0),
            };
            lua.globals()
                .get::<mlua::Function>("CreateColor")?
                .call::<Table>((r, g, b, 1.0))
        })?,
    )?;
    lua.globals().set("C_UnitAuras", api)
}

pub fn install_spell_cancel(lua: &Lua) -> mlua::Result<()> {
    let api: Table = lua.globals().get("C_Spell")?;
    api.set(
        "CancelSpellByID",
        lua.create_function(|lua, spell: u32| {
            cancel_player_spell(lua, spell);
            Ok(())
        })?,
    )
}

/// Extension helpers use the same ownership and cancelability gate as the stock buff button.
pub fn cancel_player_spell(lua: &Lua, spell: u32) {
    let mut model = lua.app_data_mut::<Model>().expect("model");
    if model
        .player_auras
        .iter()
        .any(|aura| aura.spell_id == spell && crate::script::aura::cancel_authorized(aura))
    {
        model.cancel_aura_requests.push(spell);
    }
}
