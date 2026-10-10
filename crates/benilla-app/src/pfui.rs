//! Optional pfUI integration and feeds from the client's real state.

use benilla_ui::script::UiScript;
use bevy::prelude::*;

mod bags;
mod casts;
pub(crate) mod clicks;
mod icons;
mod loot;
mod nampower;
mod quests;
mod session_events;
mod social;
pub(crate) mod spell_queue;
mod spells;
mod totems;
mod units;

pub(crate) fn prepare(script: &UiScript) -> Result<(), String> {
    let lua = script.lua();
    if lua
        .named_registry_value::<Option<bool>>("manila.pfui.enabled")
        .map_err(|e| e.to_string())?
        != Some(true)
    {
        return Ok(());
    }
    if lua
        .named_registry_value::<Option<bool>>("manila.pfui.data_installed")
        .map_err(|e| e.to_string())?
        == Some(true)
    {
        return Ok(());
    }
    let locale = script
        .eval::<String>("return GetLocale()")
        .map_err(|e| e.to_string())?;
    let reader = |path: &str| crate::ui_script::pfui_read(path);
    let creatures = manila_pfui::creature::Catalog::read(&reader, &locale)
        .map_err(|e| format!("pfUI creature metadata: {e}"))?;
    let spells = manila_pfui::spell::Catalog::read(&reader, &locale)
        .map_err(|e| format!("pfUI spell metadata: {e}"))?;
    manila_pfui::install(script).map_err(|e| e.to_string())?;
    creatures.install(lua).map_err(|e| e.to_string())?;
    spells.install(lua).map_err(|e| e.to_string())?;
    static MAPS: std::sync::OnceLock<Result<manila_pfui::maps::Catalog, String>> =
        std::sync::OnceLock::new();
    MAPS.get_or_init(|| manila_pfui::maps::Catalog::read(&reader).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(Clone::clone)?
        .install(lua)
        .map_err(|e| e.to_string())?;
    icons::prepare(script)?;
    manila_pfui::install_class_names(
        script,
        creatures
            .classes
            .iter()
            .map(|(token, name)| (token.as_str(), name.as_str())),
    )
    .map_err(|e| e.to_string())?;
    lua.set_named_registry_value("manila.pfui.data_installed", true)
        .map_err(|e| e.to_string())
}

pub(crate) struct PfuiPlugin;

#[derive(Resource)]
struct Enabled;

#[derive(Resource, Default)]
struct SessionFocus(benilla_ui::script::pfui_host::units::FocusMemory);

pub(crate) fn enable_vm(world: &World, script: &UiScript) {
    if world.contains_resource::<Enabled>() {
        if let Some(focus) = world.get_resource::<SessionFocus>() {
            benilla_ui::script::pfui_host::units::retain_focus(script.lua(), focus.0.clone());
        }
        if let Err(error) = script
            .lua()
            .set_named_registry_value("manila.pfui.enabled", true)
        {
            script.report_script_error(&format!("pfUI VM activation: {error}"));
        }
    }
}

/// Extension snapshots are visible before stock unit events dispatch to addons.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct InputSnapshots;

pub(crate) fn feed_friend_guids(
    script: &UiScript,
    friends: &[benilla_ui::script::FriendInfo],
    guids: &[u64],
) {
    if let Err(error) = benilla_ui::script::pfui_host::social::set_friend_guids(
        script.lua(),
        friends
            .iter()
            .zip(guids)
            .map(|(friend, guid)| (friend.name.as_str(), *guid)),
    ) {
        script.report_script_error(&format!("pfUI friend GUIDs: {error}"));
    }
}

pub(crate) fn resolve_item_misses(
    script: &UiScript,
    pending: &mut std::collections::HashSet<u32>,
    items: &crate::items::Items,
) {
    let failed: Vec<_> = pending
        .iter()
        .copied()
        .filter(|id| items.template_answered_unknown(*id))
        .collect();
    for id in failed {
        pending.remove(&id);
        benilla_ui::script::pfui_host::item_result(script.lua(), id, false);
    }
}

impl Plugin for PfuiPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Enabled);
        app.init_resource::<SessionFocus>();
        app.init_resource::<crate::ui_items::pfui::DeferredItemUse>();
        app.configure_sets(
            Update,
            InputSnapshots
                .in_set(crate::ui_script::UiFeed)
                .before(crate::ui_unit::UnitFeed),
        );
        casts::register(app);
        spell_queue::register(app);
        nampower::register(app);
        bags::register(app);
        loot::register(app);
        quests::register(app);
        session_events::register(app);
        social::register(app);
        spells::register(app);
        units::register(app);
        totems::register(app);
        crate::ui_unit::pfui::register(app);
        crate::ui_aura::pfui::register(app);
        crate::target::pfui::register(app);
        app.add_systems(Update, feed_clock.in_set(InputSnapshots));
        app.add_systems(
            Update,
            inventory_events
                .after(crate::ui_unit::UnitFeed)
                .in_set(crate::ui_script::UiFeed)
                .run_if(crate::ui_script::ingame_ui_up),
        );
        app.add_systems(Update, feed_inventory.in_set(InputSnapshots));
    }
}

fn inventory_events(script: Option<NonSend<UiScript>>) {
    if let Some(script) = script {
        benilla_ui::script::pfui_host::units::announce_restored_focus(script.lua());
        benilla_ui::script::pfui_host::items::dispatch_inventory_events(script.lua());
    }
}

fn feed_clock(script: Option<NonSendMut<UiScript>>, clock: Res<crate::net::ServerTime>) {
    let Some(script) = script else {
        return;
    };
    if let Err(error) =
        manila_pfui::clock::update(script.lua(), clock.0.and_then(|c| c.pfui_local_epoch()))
    {
        script.report_script_error(&format!("pfUI clock: {error}"));
    }
}

/// The full flavor manifest is selected explicitly, as the ClassicAPI DLL's redirect does; the
/// extension publishes the version that manifest's load gate requires.
pub(crate) fn manifest(name: &str) -> Option<&'static str> {
    name.eq_ignore_ascii_case("pfUI")
        .then_some(if cfg!(feature = "pfui-turtle") {
            "pfUI_Turtle.toc"
        } else {
            "pfUI_ClassicAPI.toc"
        })
}

fn feed_inventory(
    script: Option<NonSend<UiScript>>,
    self_q: Query<&crate::net::ObjectStore, With<crate::net::SelfPlayer>>,
    objects: crate::net::Objects,
    items: Res<crate::items::Items>,
    props: Option<Res<crate::items::RandomProperties>>,
) {
    let Some(script) = script else {
        return;
    };
    let mut instances = Vec::new();
    benilla_ui::script::pfui_host::items::observe_equipment_guids(
        script.lua(),
        std::array::from_fn(|slot| {
            self_q
                .single()
                .ok()
                .and_then(|player| player.0.player_inv_slot(slot as u8))
                .unwrap_or(0)
        }),
    );
    if let Ok(player) = self_q.single() {
        for bag in [
            benilla_ui::script::EQUIPMENT_BAG,
            -2,
            -1,
            0,
            1,
            2,
            3,
            4,
            5,
            6,
            7,
            8,
            9,
            10,
        ] {
            let slots = match bag {
                benilla_ui::script::EQUIPMENT_BAG => 19,
                -2 | 0 => 16,
                -1 => 24,
                _ => {
                    let bag_guid = if bag <= 4 {
                        player.0.player_inv_slot(19 + bag as u8 - 1)
                    } else {
                        player.0.player_bank_bag_slot(bag as u8 - 5)
                    };
                    bag_guid
                        .and_then(|g| objects.object(g))
                        .and_then(|o| o.container_num_slots())
                        .unwrap_or(0)
                        .min(255)
                }
            };
            for slot0 in 0..slots as u8 {
                let Some(guid) = crate::ui_items::slot_guid(&player.0, bag, slot0, &objects) else {
                    continue;
                };
                let Some(item) = objects.object(guid) else {
                    continue;
                };
                let item_id = item.object_entry().unwrap_or(0);
                let count = item.item_stack_count().unwrap_or(1);
                let charges = item.item_spell_charges(0).unwrap_or(0);
                let name = items.template_cached(item_id).map(|t| {
                    crate::items::item_display_name(
                        &t.name,
                        item.item_random_properties_id() as i32,
                        props.as_deref(),
                    )
                });
                instances.push((
                    (bag, u32::from(slot0) + 1),
                    benilla_ui::script::pfui_host::items::Instance {
                        guid,
                        item_id,
                        count,
                        name,
                        uses: count.saturating_mul(if charges < 0 {
                            charges.unsigned_abs()
                        } else {
                            1
                        }),
                    },
                ));
            }
        }
    }
    benilla_ui::script::pfui_host::items::set_instances(script.lua(), instances);
}
