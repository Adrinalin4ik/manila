//! Extra unit tokens read the same held-unit projection as stock target chains.

use benilla_ui::script::{pfui_host::units, UiScript};
use bevy::prelude::*;

pub(crate) fn register(app: &mut App) {
    app.add_systems(Update, feed.in_set(crate::pfui::InputSnapshots));
}

fn feed(
    script: Option<NonSend<UiScript>>,
    tables: super::SnapshotTables,
    stores: Query<(&crate::net::Guid, Ref<crate::net::ObjectStore>)>,
    self_q: Query<&crate::net::ObjectStore, With<crate::net::SelfPlayer>>,
    names: Res<crate::names::NameCache>,
    commands: Res<crate::net::NetCommands>,
    factions: Option<Res<crate::target::Factions>>,
    reputations: Res<crate::net::Reputations>,
    group: Res<crate::ui_party::GroupState>,
    mut removed: RemovedComponents<crate::net::ObjectStore>,
    mut memo: Local<crate::ui_script::VmMemo<bool>>,
) {
    let Some(script) = script else { return };
    let (memo, fresh) = memo.get_reset(&script);
    let despawned = removed.read().count() != 0;
    if *memo
        && !fresh
        && !despawned
        && !names.is_changed()
        && !group.is_changed()
        && !reputations.is_changed()
        && !factions
            .as_ref()
            .is_some_and(|factions| factions.is_changed())
        && !stores.iter().any(|(_, store)| store.is_changed())
    {
        return;
    }
    let owner = |guid| {
        stores
            .iter()
            .find(|(id, _)| id.0 == guid)
            .map(|(_, store)| (*store).clone())
    };
    let held = super::held::HeldUnits {
        owner_store: &owner,
        names: &names,
        commands: &commands,
        factions: factions.as_deref(),
        reputations: &reputations,
        group: &group,
        self_store: self_q.single().ok(),
        classes: tables.classes(),
        types: tables.types(&names),
    };
    units::set_traits(
        script.lua(),
        stores
            .iter()
            .filter(|(_, store)| store.is_unit())
            .map(|(guid, store)| {
                (
                    guid.0,
                    units::Traits {
                        creature_type: held.types.of(&store),
                        created_by_spell: store.0.unit_created_by_spell().unwrap_or(0),
                    },
                )
            }),
    );
    units::set_unit_states(
        script.lua(),
        stores
            .iter()
            .filter(|(_, store)| store.is_unit())
            .map(|(guid, store)| {
                (
                    guid.0,
                    held.state(&store, guid.0),
                    store.0.unit_target().unwrap_or(0),
                )
            }),
    );
    *memo = true;
}
