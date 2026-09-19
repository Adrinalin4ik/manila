//! **`playerDistance` — how far away another player's model is still drawn.** Ours; 1.12 has no
//! such CVar, and no 1.12 machine had to draw eight hundred of them in one street.
//!
//! Why it exists, as a measurement and as a setting:
//!
//! Twenty journals failed to price the crowd, because the crowd is not something a measurement can
//! hold still. The same spot gave 739 players one run and 947 the next, and the machine itself ran
//! up to 1.85x slower between runs — so a comparison ACROSS runs measured the afternoon, not the
//! code. What was missing was a way to change one thing inside ONE run, on one machine, a minute
//! apart. A distance wall is that: slide it to 0, the crowd stops being drawn, and the difference
//! is the cost of drawing it. Nothing else moves.
//!
//! And it is the honest shipping answer to "it should be fast on any machine". Other players are
//! the one part of a city a client cannot budget for: their count, their gear and their composited
//! skins all belong to other people. Everything else here already has a wall — `farclip` for the
//! world, `effectsDistance` for the particles. This is the third.
//!
//! Visibility only. The entities stay, the network keeps updating them, their skins stay
//! composited and the ECS keeps paying for them. What stops is drawing. The player's own model is
//! never hidden: a camera subject that vanishes is disorienting, and one model against several
//! hundred is inside the noise.

use benilla_protocol::EntityKind;
use bevy::prelude::*;

/// The settable range, shared by the CVar apply and the options slider so the two cannot drift.
/// `0` is "draw none" and is a real setting, not a guard value. The top is [`farclip`'s ceiling],
/// i.e. "no wall of our own" — beyond it the world's own far plane decides.
///
/// [`farclip`'s ceiling]: benilla_world::view::FARCLIP_RANGE
pub(crate) const PLAYER_DISTANCE_RANGE: std::ops::RangeInclusive<f32> = 0.0..=777.0;

/// Yards. Default is the top of the range: every player drawn, which is what every build before
/// this one did.
#[derive(Resource, Clone, Copy, PartialEq, Debug)]
pub(crate) struct PlayerDistance(pub(crate) f32);

impl Default for PlayerDistance {
    fn default() -> Self {
        Self(*PLAYER_DISTANCE_RANGE.end())
    }
}

/// Hide the players past the wall, show the ones inside it.
///
/// Runs every frame and costs one squared-distance compare per streamed unit — a few hundred
/// compares against a frame that is drawing a few hundred characters. A `Visibility` is written
/// only when it actually changes, so a settled crowd writes nothing and dirties no change
/// detection downstream.
pub(crate) fn apply(
    wall: Res<PlayerDistance>,
    self_guid: Res<crate::net::SelfGuid>,
    camera: Query<&GlobalTransform, With<benilla_world::view::WorldCamera>>,
    mut units: Query<(
        &crate::net::NetEntity,
        &crate::net::Guid,
        &GlobalTransform,
        &mut Visibility,
    )>,
) {
    let Ok(eye) = camera.single() else {
        return;
    };
    let eye = eye.translation();
    // Compared squared, so the per-unit test is a subtract and a dot rather than a square root.
    let limit = wall.0 * wall.0;
    let me = self_guid.0;
    for (net, guid, at, mut vis) in &mut units {
        if net.kind != EntityKind::Player || Some(guid.0) == me {
            continue;
        }
        let want = if at.translation().distance_squared(eye) <= limit {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
    }
}

/// Registers the wall.
pub(crate) struct PlayerDistancePlugin;

impl Plugin for PlayerDistancePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerDistance>().add_systems(
            PostUpdate,
            // After the frame's transforms are final and before visibility is propagated, so a
            // unit that moved across the wall this frame is drawn correctly on this frame rather
            // than the next.
            apply.before(bevy::camera::visibility::VisibilitySystems::VisibilityPropagate),
        );
    }
}
