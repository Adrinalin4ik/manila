//! The per-frame wire→ECS bridge systems: [`apply_net_updates`] drains the inbound
//! [`SessionEvent`] channel through the handler table, and [`tag_self_player`] marks our own
//! streamed entity. Nothing is applied here: every packet's handler lives with its subsystem.

use benilla_protocol::SessionEvent;
use bevy::prelude::*;

use super::{Guid, NetEvents, SelfGuid, SelfPlayer};

#[cfg(test)]
mod seam_tests;

// ── The per-frame bridge systems ─────────────────────────────────────────────────────────────────

/// Runs this frame's events through the handler table ([`super::handlers`]) in wire order, each
/// handler's commands applied before the next, before anything else in
/// [`benilla_world::schedule::WorldStage::Net`] runs.
pub(crate) fn apply_net_updates(world: &mut World) {
    let events: Vec<SessionEvent> = world.resource::<NetEvents>().0.try_iter().collect();
    // The journal's `net_pkts`/`net_us` pair (ours). Measured around the dispatch itself, and the
    // empty early-out is what keeps the column a measurement rather than a shape: without it every
    // idle frame files a 0-packet sample and the per-packet average reads as whatever the idle
    // rate is. `bevy::platform::time::Instant`, not std's, which compiles on wasm32 and panics.
    if events.is_empty() {
        return;
    }
    let count = events.len() as u32;
    let t0 = bevy::platform::time::Instant::now();
    super::handlers::dispatch(world, events);
    crate::perf::journal::note_net(count, t0.elapsed().as_micros() as u64);
}

/// Tags our own streamed entity with [`SelfPlayer`] by matching [`Guid`] against [`SelfGuid`].
/// A pass of its own, not at spawn, so either arrival order of our guid and our create block works;
/// a cross-map worldport re-streams the avatar and it is tagged again.
pub(super) fn tag_self_player(
    mut commands: Commands,
    self_guid: Res<SelfGuid>,
    untagged: Query<(Entity, &Guid), Without<SelfPlayer>>,
) {
    let Some(me) = self_guid.0 else {
        return;
    };
    for (entity, guid) in &untagged {
        if guid.0 == me {
            // Identity only: `MovementState` belongs to the body we steer (`player::embody`).
            commands.entity(entity).insert(SelfPlayer);
        }
    }
}
