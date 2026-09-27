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
    // **The wall measures from the PLAYER, not the camera**, and the first build measured from the
    // camera and died on it: `camera.single()` returns `Err` for zero matches as well as two, the
    // function returned there, and every line below - including the one meant to explain a slider
    // that does nothing - was unreachable. A diagnostic behind an early return is not a
    // diagnostic. The player's own position is the honest origin anyway: a wall the camera carries
    // slides the crowd in and out as the view swings, which is not what a draw-distance setting
    // means anywhere else in this client.
    player: Option<Res<crate::player::Player>>,
    mut units: Query<(
        Entity,
        &crate::net::NetEntity,
        &crate::net::Guid,
        &GlobalTransform,
        Option<&mut Visibility>,
    )>,
    mut reported: Local<Option<f32>>,
) {
    let eye = player.as_deref().filter(|p| p.active).map(|p| p.pos);
    // Compared squared, so the per-unit test is a subtract and a dot rather than a square root.
    let limit = wall.0 * wall.0;
    let me = self_guid.0;
    let (mut players, mut hidden, mut inserted) = (0u32, 0u32, 0u32);
    if let Some(eye) = eye {
        for (entity, net, guid, at, vis) in &mut units {
            // **A player's pet walks out with its owner.** The setting is a crowd wall, and a
            // crowd's hunters, warlocks and mages bring a body each; leaving those standing while
            // their owners vanish is what the owner saw with the slider at zero - an empty square
            // full of pets. A pet is `EntityKind::Unit`, so the kind test alone never reached it,
            // but its guid says so on its own: `HIGHGUID_PET` (`guid::is_pet`), no owner field
            // and no second query needed.
            //
            // The honest limit: a minion the server spawns as a plain creature - a totem, an
            // unowned guardian - carries `HIGHGUID_UNIT` and is indistinguishable here from a
            // world mob, so it stays. Catching those needs the unit's summoned-by field, which
            // this pass does not read.
            let controlled = net.kind == EntityKind::Player || benilla_protocol::guid::is_pet(guid.0);
            if !controlled || Some(guid.0) == me {
                continue;
            }
            players += 1;
            // **This pass only ever HIDES.** `benilla_world::exterior_cull` writes `Visibility` on
            // every body each frame - it is the window and frustum cull - so showing is its job and
            // its alone. Two systems writing one component with no order between them is what the
            // first build did, and the cull won: 84 bodies were marked hidden every frame and not
            // one of them left the screen. Writing only one direction composes instead of racing,
            // and a wall slid back out restores itself because the cull restates the bodies it can
            // see on the very next frame.
            if at.translation().distance_squared(eye) <= limit {
                continue;
            }
            hidden += 1;
            let want = Visibility::Hidden;
            // **Write only; never insert.** Streamed bodies carry a `Visibility` already - the
            // counter below has read `0 given a Visibility` in every capture since it shipped -
            // so the insert branch was dead code that could still move an archetype mid-frame,
            // and an archetype move on an entity the render world has already collected as
            // visible is the shape of the crash seen in `bevy_pbr`'s material specialization
            // (material.rs:1061, an `unwrap` on a tick table the sweep had just emptied). Dead
            // code is not worth a hazard, however small the odds.
            match vis {
                Some(mut vis) => {
                    if *vis != want {
                        *vis = want;
                    }
                }
                None => inserted += 1,
            }
        }
    }
    // One line per setting change, never per frame, and **outside every early return**: "the
    // slider does nothing" is a question about what the code reached, and the code has to be able
    // to answer it even when it reached nothing.
    if *reported != Some(wall.0) {
        *reported = Some(wall.0);
        info!(
            "playerDistance {:.0} yd - player {}, {players} other players seen, {hidden} past the              wall, {inserted} without a Visibility at all",
            wall.0,
            if eye.is_some() { "found" } else { "ABSENT" }
        );
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
            apply
                // After the cull, so the wall has the last word on the bodies it hides, and
                // before propagation, so that word reaches the children this same frame.
                .after(benilla_world::exterior_cull::ExteriorCullSet)
                .before(bevy::camera::visibility::VisibilitySystems::VisibilityPropagate),
        );
    }
}
