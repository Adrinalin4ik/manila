//! Pace the addon batch over real descriptor confirmations, using the existing item sender.

use crate::net::NetHandlerApp;
use benilla_protocol::{SessionEvent, SessionEventKind};
use benilla_ui::script::{pfui_host::items, UiScript};
use bevy::prelude::*;
use manila_pfui::bags::{Executor, Slot};

pub(super) fn register(app: &mut App) {
    app.net_handler(SessionEventKind::InventoryFailure, failure);
    // A macro can submit bag operations while the ordered script calls are being applied.
    app.add_systems(
        Update,
        drain
            .after(crate::script_calls::apply_script_calls)
            .before(benilla_world::schedule::WorldStage::Stream),
    );
}

fn failure(In(event): In<SessionEvent>, script: Option<NonSend<UiScript>>) {
    if let (SessionEvent::InventoryFailure { item_guid, .. }, Some(script)) = (event, script) {
        items::bag_failure(script.lua(), item_guid);
    }
}

fn drain(
    script: Option<NonSendMut<UiScript>>,
    commands: Res<crate::net::NetCommands>,
    self_q: Query<&crate::net::ObjectStore, With<crate::net::SelfPlayer>>,
    objects: crate::net::Objects,
    templates: Res<crate::items::Items>,
    mut pending: ResMut<crate::pending_item_ops::PendingItemOps>,
    mut transitions: ResMut<crate::pending_item_ops::LockTransitions>,
    mut gate: crate::ui_bind_confirm::BindGate,
    mut memo: Local<crate::ui_script::VmMemo<Executor>>,
    time: Res<Time>,
) {
    let Some(mut script) = script else { return };
    let worker = memo.get(&script);
    let incoming = items::take_bag_moves(script.lua());
    let failures = items::take_bag_failures(script.lua());
    let owned = worker.pending_guids();
    let Ok(store) = self_q.single() else {
        worker.clear();
        items::set_bag_busy(script.lua(), false);
        return;
    };
    let mut error = worker.enqueue(incoming).err();
    for guid in failures {
        if worker.failed(guid) {
            error = Some("pfUI bag operation was rejected by the server".into());
        }
    }
    if error.is_none() {
        let read = |bag, slot| {
            if !items::bag_position_available(script.lua(), bag, slot) {
                return None;
            }
            let guid = crate::ui_items::slot_guid(
                &store.0,
                bag,
                u8::try_from(slot.checked_sub(1)?).ok()?,
                &objects,
            );
            let Some(guid) = guid else {
                return Some(Slot {
                    locked: pending.contains(bag, slot),
                    ..Default::default()
                });
            };
            let fields = objects.object(guid)?;
            let id = fields.object_entry()?;
            let template = templates.template(id, guid, &commands)?;
            Some(Slot {
                guid,
                id,
                count: fields.item_stack_count()?,
                max_stack: template.stackable.max(1),
                locked: pending.contains(bag, slot),
            })
        };
        match worker.next(time.elapsed_secs_f64(), read) {
            Ok(Some(movement)) => {
                if !crate::ui_items::pfui::send_container_move(
                    &mut script,
                    &mut gate,
                    &objects,
                    &templates,
                    &commands,
                    Some(store),
                    &mut pending,
                    movement,
                    false,
                ) {
                    worker.clear();
                    error = Some("pfUI bag operation was deferred or could not be sent".into());
                }
            }
            Ok(None) => {}
            Err(message) => error = Some(message),
        }
    }
    items::set_bag_busy(script.lua(), worker.busy());
    if let Some(message) = error {
        for guid in owned.into_iter().flatten().filter(|guid| *guid != 0) {
            transitions.0.extend(pending.clear_by_guid(guid));
        }
        script.report_script_error(&message);
    }
}
