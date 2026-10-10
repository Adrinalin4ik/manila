//! Preserve raw aura slots for the optional modern projection without changing stock DTOs.

use benilla_ui::script::{
    pfui_host::auras::{self, Aura},
    AuraState, UiScript,
};
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct VisibleFeed;

pub(crate) fn register(app: &mut App) {
    app.add_systems(
        Update,
        feed_visible
            .after(super::AuraEvents)
            .in_set(VisibleFeed)
            .in_set(crate::ui_script::UiFeed),
    );
}

/// GUID/focus/plate queries include visible units that stock target/group tokens cannot name.
fn feed_visible(
    script: Option<NonSend<UiScript>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    stores: Query<
        (&crate::net::Guid, Ref<crate::net::ObjectStore>),
        Without<crate::net::SelfPlayer>,
    >,
    mut memo: Local<crate::ui_script::VmMemo<HashMap<u64, Vec<super::AuraInput>>>>,
) {
    let Some(script) = script else { return };
    let (memo, fresh) = memo.get_reset(&script);
    let catalog_changed = spells.as_ref().is_some_and(|spells| spells.is_changed());
    let catalog = spells.as_ref().map(|spells| &spells.catalog);
    let mut present = HashSet::new();
    for (guid, store) in &stores {
        if !store.is_unit() {
            continue;
        }
        present.insert(guid.0);
        if !fresh && !catalog_changed && !store.is_changed() && memo.contains_key(&guid.0) {
            continue;
        }
        let inputs: Vec<_> = super::other_unit_inputs(&store, catalog, true).collect();
        if fresh || catalog_changed || memo.get(&guid.0) != Some(&inputs) {
            let list = super::aura_list(catalog, &inputs);
            unit(&script, guid.0, &inputs, &list);
            memo.insert(guid.0, inputs);
        }
    }
    memo.retain(|guid, _| {
        if present.contains(guid) {
            true
        } else {
            remove(&script, *guid);
            false
        }
    });
}

pub(super) fn player(
    script: &UiScript,
    guid: u64,
    slots: &[super::CachedAura],
    list: &[AuraState],
) {
    auras::set_auras(
        script.lua(),
        guid,
        Some(
            slots
                .iter()
                .zip(list)
                .map(|(raw, state)| Aura {
                    slot: raw.slot,
                    state: state.clone(),
                    source_guid: None,
                })
                .collect(),
        ),
    );
}

pub(super) fn unit(script: &UiScript, guid: u64, slots: &[super::AuraInput], list: &[AuraState]) {
    auras::set_auras(
        script.lua(),
        guid,
        Some(
            slots
                .iter()
                .zip(list)
                .map(|(raw, state)| Aura {
                    slot: raw.0,
                    state: state.clone(),
                    source_guid: None,
                })
                .collect(),
        ),
    );
}

pub(super) fn remove(script: &UiScript, guid: u64) {
    auras::set_auras(script.lua(), guid, None);
}
