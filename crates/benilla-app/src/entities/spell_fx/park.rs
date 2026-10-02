//! wenilla carry: an effect model on a parked host stops writing its joints.
//!
//! An effect instance (a spell-kit visual, a weapon-enchant glow) still poses through bevy's
//! one-entity-per-bone lane: [`super::arm_effect_rig`] gives every bone an `AnimatedBy` target,
//! and `animate_targets` commits every joint's `Transform` every frame its player plays. Those
//! writes raise `Changed<Transform>`, so propagation re-walks the instance's subtree and the
//! palette pass (`rig_palette.rs`, `compute_rig_palettes`) rebuilds its rows. The rig LOD gate
//! (`creature_anim/lod.rs`) never sees it - an effect root has no `RigPose` - so an enchanted
//! weapon or a standing aura behind the camera costs exactly what one on screen does. In a
//! battleground that is every geared player.
//!
//! The lever is the one `animate_targets` already honours: a clip whose `ActiveAnimation` weight
//! is literally 0.0 is skipped for every target (`bevy_animation-0.18.1/src/lib.rs`,
//! `animate_targets`, "If the weight is zero ... stop here"), so no evaluator is current and
//! `commit_all` writes nothing. The joints hold their last pose and raise no change. The player's
//! clock still advances (`advance_animations` does not read the weight), so the lifecycle
//! (`lifecycle.rs`, completions), the event scan and the emitters' sequence clock run on, and a
//! wake resumes in phase rather than snapping.
//!
//! An instance parks when the nearest rig above its host model is parked, which is the LOD gate's
//! own verdict (padded frustum sphere, portal room, crowd wall); a free-standing instance (a
//! missile, a ground effect: no [`ParentModel`]) is left alone. Effect players carry weight 1.0 and
//! nothing else writes one (`play()`'s default; no `set_weight` in `spell_fx/`, `item_glow.rs`,
//! `missile.rs` or `dest_fx.rs`), so a wake restores 1.0. A clip the lifecycle starts while parked
//! arrives at 1.0 and is zeroed on the next pass.

use bevy::ecs::entity::EntityHashSet;
use bevy::prelude::*;

use benilla_world::model_fade::{ParentModel, MAX_MODEL_CHAIN};
use benilla_world::rig_anim::{AnimParked, RigPose};

use super::EffectModel;

/// How far up the transform tree a host's rig may sit: item root -> anchor -> joints root ->
/// body, or one seat further for a rider.
const MAX_HOST_DEPTH: usize = 2 * MAX_MODEL_CHAIN;

/// Zero or restore each hosted effect's clip weights from its host rig's park state.
pub(crate) fn park_effect_rigs(
    mut effects: Query<(Entity, &ParentModel, &mut AnimationPlayer), With<EffectModel>>,
    parents: Query<&ChildOf>,
    rigs: Query<Has<AnimParked>, With<RigPose>>,
    mut frozen: Local<EntityHashSet>,
) {
    for (root, host, mut player) in &mut effects {
        let parked = host_parked(host.0, &parents, &rigs);
        if parked {
            // Through `Deref` first: a weight already 0 costs no `Mut` write.
            if player.playing_animations().any(|(_, a)| a.weight() != 0.0) {
                for (_, a) in player.playing_animations_mut() {
                    a.set_weight(0.0);
                }
            }
            frozen.insert(root);
        } else if frozen.remove(&root) {
            for (_, a) in player.playing_animations_mut() {
                a.set_weight(1.0);
            }
        }
    }
    // Despawned instances: nothing left to restore.
    frozen.retain(|e| effects.contains(*e));
}

/// Whether the nearest `RigPose` holder at or above `host` is parked; `false` when none is found.
fn host_parked(
    host: Entity,
    parents: &Query<&ChildOf>,
    rigs: &Query<Has<AnimParked>, With<RigPose>>,
) -> bool {
    let mut e = host;
    for _ in 0..MAX_HOST_DEPTH {
        if let Ok(parked) = rigs.get(e) {
            return parked;
        }
        match parents.get(e) {
            Ok(c) => e = c.parent(),
            Err(_) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::animation::graph::AnimationNodeIndex;

    #[test]
    fn a_parked_host_zeroes_its_effect_and_a_wake_restores_it() {
        let mut app = App::new();
        app.add_systems(Update, park_effect_rigs);
        let pose = RigPose::new(Entity::PLACEHOLDER, &benilla_assets::ModelSkeleton::default());
        let body = app.world_mut().spawn((pose, AnimParked)).id();
        let item = app.world_mut().spawn(ChildOf(body)).id();
        let mut player = AnimationPlayer::default();
        player.play(AnimationNodeIndex::new(1));
        let fx = app
            .world_mut()
            .spawn((EffectModel, ParentModel(item), player))
            .id();
        let weight = |app: &App| {
            app.world()
                .get::<AnimationPlayer>(fx)
                .unwrap()
                .playing_animations()
                .map(|(_, a)| a.weight())
                .next()
                .unwrap()
        };
        app.update();
        assert_eq!(weight(&app), 0.0);
        app.world_mut().entity_mut(body).remove::<AnimParked>();
        app.update();
        assert_eq!(weight(&app), 1.0);
    }
}
