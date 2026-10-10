//! Reuse the stock algorithms for arbitrary DBC spells, independent of action-bar placement.

use benilla_formats::{RangeTargets, SpellDisplay, SpellRange};
use benilla_ui::script::UiScript;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use manila_pfui::spell::runtime::{self, Cost};
use std::{collections::HashMap, sync::Arc};

struct RangeSpell {
    definition: SpellDisplay,
    row: Option<SpellRange>,
}

#[derive(Default)]
struct Memo {
    costs: Option<(u32, u32, u32)>,
    cooldown_epoch: Option<u64>,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct SpellSnapshots;

pub(super) fn register(app: &mut App) {
    app.configure_sets(
        Update,
        SpellSnapshots
            .in_set(crate::ui_script::UiFeed)
            .after(crate::ui_unit::UnitFeed),
    );
    app.add_systems(Update, feed.in_set(SpellSnapshots));
}

#[allow(clippy::type_complexity)] // Bevy's per-frame spell inputs
fn feed(
    script: Option<NonSend<UiScript>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    actions: Res<crate::ui_action::PlayerActions>,
    cooldowns: Res<crate::spell::Cooldowns>,
    mods: Res<crate::spell::SpellModifiers>,
    clock: Res<crate::ui_script::UiClock>,
    self_q: Query<(&crate::net::ObjectStore, &Transform), With<crate::net::SelfPlayer>>,
    selection: Res<crate::target::Selection>,
    objects: crate::net::Objects,
    positions: Query<(
        &crate::net::Guid,
        Entity,
        &Transform,
        &crate::net::ObjectStore,
    )>,
    range_units: crate::spell::RangeUnits,
    factions: Option<Res<crate::target::Factions>>,
    reputations: Res<crate::net::Reputations>,
    supplies: (Res<crate::items::Items>, Res<crate::net::NetCommands>),
    mut memo: Local<crate::ui_script::VmMemo<Memo>>,
    mut ranges: Local<crate::ui_script::VmMemo<Option<Arc<HashMap<u32, RangeSpell>>>>>,
) {
    let (Some(script), Some(spells)) = (script, spells) else {
        return;
    };
    let memo = memo.get(&script);
    let ranges = ranges.get(&script);
    let now = Instant::now();
    let me = self_q.single().ok();
    let basis = me.map(|(store, _)| {
        (
            store.0.unit_level().unwrap_or(0),
            store.0.unit_base_mana().unwrap_or(0),
            store.0.unit_base_health().unwrap_or(0),
        )
    });
    if mods.is_changed() || spells.is_changed() || basis != memo.costs {
        let costs = me
            .map(|(store, _)| {
                spells
                    .catalog
                    .iter()
                    .map(|(id, d)| {
                        (
                            id,
                            Cost {
                                kind: d.power_type as i32,
                                amount: crate::spell::usable::power_cost(d, store, &mods),
                                percent: d.mana_cost_pct,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        runtime::set_costs(script.lua(), costs);
        memo.costs = basis;
    }
    if memo.cooldown_epoch != Some(cooldowns.feed_epoch()) || cooldowns.sweep_pending(now) {
        runtime::set_cooldowns(
            script.lua(),
            spells
                .catalog
                .iter()
                .map(|(id, d)| {
                    (
                        id,
                        cooldowns
                            .info(id, 0, Some(d), now)
                            .ui_triple(clock.anchor, clock.ui_now)
                            .unwrap_or((0, 0, true)),
                    )
                })
                .collect(),
        );
        memo.cooldown_epoch = Some(cooldowns.feed_epoch());
    }
    let carried = me
        .map(|(store, _)| crate::ui_items::carried_counts(&store.0, &objects))
        .unwrap_or_default();
    let (items, commands) = &supplies;
    let usable = me
        .map(|(store, _)| {
            let context = crate::spell::usable::UsableCtx {
                store,
                target_store: selection
                    .guid
                    .and_then(|guid| objects.entity(guid))
                    .and_then(|entity| positions.get(entity).ok())
                    .map(|(_, _, _, store)| store),
                factions: factions.as_deref(),
                reputations: &reputations,
                cooldowns: &cooldowns,
                spell_mods: &mods,
                carried: &carried,
            };
            actions
                .spells
                .iter()
                .filter_map(|id| {
                    let d = spells.catalog.get(*id)?;
                    Some((
                        *id,
                        crate::spell::usable::spell_usable(
                            *id, d, &spells, &context, &objects, items, commands,
                        ),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    runtime::set_usable(script.lua(), usable);
    if ranges.is_none() || spells.is_changed() {
        // These are exactly the fields min_max_range and the range SpellMod read. Keeping
        // this immutable projection avoids cloning the complete catalog every frame.
        *ranges = Some(Arc::new(
            spells
                .catalog
                .iter()
                .map(|(id, d)| {
                    (
                        id,
                        RangeSpell {
                            row: spells.ranges.get(d.range_index).copied(),
                            definition: SpellDisplay {
                                attributes: d.attributes,
                                attributes_ex3: d.attributes_ex3,
                                spell_family: d.spell_family,
                                spell_family_flags: d.spell_family_flags,
                                ..Default::default()
                            },
                        },
                    )
                })
                .collect(),
        ));
    }
    let query = me.map(|(_, transform)| {
        let caster = range_units.caster();
        let origin = transform.translation;
        let units: HashMap<_, _> = positions
            .iter()
            .filter_map(|(guid, entity, transform, _)| {
                Some((guid.0, (transform.translation, range_units.unit(entity)?)))
            })
            .collect();
        let ranges = ranges.as_ref().expect("range projection").clone();
        let mods = (*mods).clone();
        Box::new(move |id, guid| {
            let spell = ranges.get(&id)?;
            let (position, target) = units.get(&guid)?;
            let targets = RangeTargets {
                target: Some(*target),
                attack_target: None,
            };
            let (min, max) =
                mods.min_max_range(&spell.definition, spell.row.as_ref(), caster, targets)?;
            let distance = origin.distance_squared(*position);
            Some(distance >= min * min && distance <= max * max)
        }) as runtime::RangeQuery
    });
    runtime::set_range_query(script.lua(), query);
}
