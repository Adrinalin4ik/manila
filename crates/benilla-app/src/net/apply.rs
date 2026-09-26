//! The per-frame wire→ECS bridge systems: [`apply_net_updates`] drains the inbound
//! [`SessionEvent`] channel through the handler table, and [`tag_self_player`] marks our own
//! streamed entity. Nothing is applied here: every packet's handler lives with its subsystem.

use benilla_protocol::SessionEvent;
use bevy::prelude::*;

use super::{Guid, NetEvents, SelfGuid, SelfPlayer};

// ── The debug gates' classifiers ─────────────────────────────────────────────────────────────────

/// Is this event a CHAT line, for `/console netChat 0`? The player-visible chat surface: the
/// message itself, the channel notices a join prints, and the server's own broadcasts. `TextEmote`
/// is here rather than with the unit events because "X waves at Y" is a chat line that happens to
/// name a unit - it is checked first, so it goes with chat when both gates are down.
fn is_chat(ev: &SessionEvent) -> bool {
    use SessionEvent as E;
    matches!(
        ev,
        E::Chat(_)
            | E::TextEmote { .. }
            | E::ChannelNotify { .. }
            | E::ChannelList { .. }
            | E::ChatPlayerNotFound { .. }
            | E::ChatWrongFaction
            | E::ChatRestricted
            | E::ServerMessage { .. }
    )
}

/// The guid an event is ABOUT, for `/console netOthers 0`. `None` means it is not about one unit
/// and is never dropped.
///
/// **Per-frame churn only.** `ObjectCreate` and `ObjectDestroyed` are deliberately absent: they
/// are a one-off cost per unit, and dropping them would empty the world rather than quiet it -
/// a different experiment, and one the player-distance slider already ran. What is here is what
/// arrives again and again for the same unit: its movement, its descriptor fields (which is where
/// a gear change lands), its casts, its melee swings and its emotes.
fn subject_guid(ev: &SessionEvent) -> Option<u64> {
    use SessionEvent as E;
    Some(match ev {
        E::ObjectMove { guid, .. }
        | E::UnitMove { guid, .. }
        | E::MonsterMove { guid, .. }
        | E::ObjectValues { guid, .. }
        | E::MoveTimeSkipped { guid, .. }
        | E::Emote { guid, .. }
        | E::SpeedChanged { guid, .. }
        | E::ForceSpeedChange { guid, .. }
        | E::SplineMoveMode { guid, .. }
        | E::MoveMode { guid, .. } => *guid,
        E::SpellStart { caster, .. } | E::SpellGo { caster, .. } => *caster,
        E::AttackStart { attacker, .. } | E::AttackStop { attacker, .. } => *attacker,
        _ => return None,
    })
}

#[cfg(test)]
mod seam_tests;

// ── The per-frame bridge systems ─────────────────────────────────────────────────────────────────

/// Runs this frame's events through the handler table ([`super::handlers`]) in wire order, each
/// handler's commands applied before the next, before anything else in
/// [`benilla_world::schedule::WorldStage::Net`] runs.
pub(crate) fn apply_net_updates(world: &mut World) {
    let mut events: Vec<SessionEvent> = world.resource::<NetEvents>().0.try_iter().collect();
    // **The two debug gates** (`/console netChat 0`, `/console netOthers 0`) - see [`NetDebug`].
    // Filtered HERE, before the handler table, because that is the one place a packet can be
    // refused without any subsystem knowing it exists: a gate inside a handler would still pay
    // the dispatch, and a gate on the draw side (which is what the player-distance slider was)
    // stops pixels, not work.
    {
        let gate = *world.resource::<super::NetDebug>();
        // The master switch: the channel is still DRAINED - leaving it to back up would measure
        // a growing queue rather than a quiet wire - and nothing is applied.
        if !gate.packets {
            crate::perf::journal::note_net_dropped(0, events.len() as u32);
            return;
        }
        if !gate.chat || !gate.others {
            let me = world.resource::<SelfGuid>().0;
            let before = events.len();
            let mut chat_dropped = 0u32;
            events.retain(|ev| {
                if !gate.chat && is_chat(ev) {
                    chat_dropped += 1;
                    return false;
                }
                if !gate.others {
                    // Ours is never dropped: the experiment is about the CROWD, and a client that
                    // stops applying its own movement is not a slower client, it is a broken one.
                    if let Some(g) = subject_guid(ev) {
                        if Some(g) != me {
                            return false;
                        }
                    }
                }
                true
            });
            crate::perf::journal::note_net_dropped(chat_dropped, (before - events.len()) as u32 - chat_dropped);
        }
    }
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
