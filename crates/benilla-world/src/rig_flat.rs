//! wenilla carry: palette-skinned parts leave bevy's transform propagation.
//!
//! **Why.** A skinned part draws from its rig's palette, not from its own matrix: under
//! `WOW_RIG_SKIN` the vertex stage takes the frame from the palette rows and `rig_origin` alone
//! (`wow_model.wgsl`, the `frame_from_local`/`frame_origin` arm), normals included, and the mesh
//! matrix only anchors the point-light pick and orders the transparent sort. Such a part carries
//! [`RigPart`] - set only when the skinned twin is the drawn mesh (`attach/dress.rs`,
//! `equipment/spawn.rs`, `spell_fx`) - and, being culled at its unit's root, `NoFrustumCulling`.
//! That covers the body's merged groups and every rider-lane item (helm, pauldrons, weapons,
//! shield: `rig_rider.rs`).
//!
//! Yet bevy propagates each of them every frame its parent's `GlobalTransform` changes. For an
//! item that is every frame its wearer animates, because the item root hangs off a bone anchor
//! that `compose_rig_models` re-seats each pose; for a body part every frame its unit moves. Each
//! write then raises `Changed<GlobalTransform>`, which the GPU-building extract re-extracts and
//! the water classifier re-tests. Journal 93's crowd: ~5,300 changed `GlobalTransform`s a frame,
//! `p_xform` ~3.7 ms, most of them parts that draw nothing differently for it.
//!
//! **How.** bevy_transform 0.18.1's parallel propagator (the one compiled with `std`) reads
//! children through a node query that requires `Transform`, `GlobalTransform`,
//! `TransformTreeChanged` and `ChildOf` together (`systems.rs`, `NodeQuery`), so a child without
//! `TransformTreeChanged` is skipped with its subtree, and `mark_dirty_trees` stops its upward
//! walk there. `ChildOf` stays: visibility inheritance (`visibility_propagate_system`) reads only
//! `ChildOf`/`Children`, so the root cull still hides the part, and despawn still cascades.
//! [`seat_flat_parts`] then writes the part's `GlobalTransform` itself - but only when its parent
//! has moved more than [`tolerance`] yards or turned past [`TURN_COS`] since the last write, and
//! for every flat part of that parent at once, so a body's own blended parts keep identical
//! matrices and their sort order cannot flicker between them.
//!
//! **Cost of the tolerance.** Up to `tolerance` yards of error in a skinned part's light anchor
//! and sort key; never in where it draws. `0` writes on any change (exact, saving propagation
//! only); negative switches the lane off and hands the parts back to bevy.
//!
//! Re-inserting `Mesh3d` re-adds `TransformTreeChanged` (required through `Transform`), so the
//! strip runs every frame over the parts that have it, which is nothing once settled.

use std::sync::atomic::{AtomicU32, Ordering};

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers, VisibilitySystems};
use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;
use bevy::transform::components::TransformTreeChanged;
use bevy::transform::TransformSystems;

use crate::billboard::BillboardPlace;
use crate::rig_palette::RigPart;

/// The default tolerance in yards; keep in step with the `flatParts` cvar row.
pub const DEFAULT_TOLERANCE: f32 = 0.25;

/// A parent turn past ~10 degrees re-seats its parts even without travel.
const TURN_COS: f32 = 0.996;

/// Microseconds both systems spent, for the journal's `flat_us` (read and cleared by
/// [`take_micros`]): the lane's own cost, which the `p_*` tiles cannot isolate once its two
/// systems sit at their boundaries.
static MICROS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Read and clear [`MICROS`].
pub fn take_micros() -> u64 {
    MICROS.swap(0, Ordering::Relaxed)
}

/// Adds the elapsed time to [`MICROS`] on drop, early returns included.
struct Span(bevy::platform::time::Instant);

impl Span {
    fn start() -> Self {
        Self(bevy::platform::time::Instant::now())
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        MICROS.fetch_add(self.0.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
}

static TOLERANCE_BITS: AtomicU32 = AtomicU32::new(0x3e80_0000); // 0.25_f32

/// The `flatParts` observer's write: yards of drift allowed, `0` exact, negative off.
pub fn set_tolerance(yards: f32) {
    TOLERANCE_BITS.store(yards.to_bits(), Ordering::Relaxed);
}

fn tolerance() -> f32 {
    f32::from_bits(TOLERANCE_BITS.load(Ordering::Relaxed))
}

/// A part this lane seats; it has no `TransformTreeChanged`, so bevy does not.
#[derive(Component)]
pub struct FlatPart;

/// Palette-skinned leaves outside the booths' layers whose bound nothing reads: a part with an
/// `Aabb` and frustum culling needs its true matrix for the cull, so it stays with bevy.
type Flattenable = (
    With<RigPart>,
    With<TransformTreeChanged>,
    Without<Children>,
    Without<RenderLayers>,
    Or<(With<NoFrustumCulling>, Without<Aabb>)>,
);

/// Strip new parts, or hand every part back when the lane is switched off.
fn flatten_parts(
    mut commands: Commands,
    fresh: Query<Entity, Flattenable>,
    mut flat: Query<(Entity, &mut Transform), With<FlatPart>>,
    mut on: Local<bool>,
) {
    let _span = Span::start();
    if tolerance() < 0.0 {
        if *on {
            for (e, mut t) in &mut flat {
                commands
                    .entity(e)
                    .insert(TransformTreeChanged)
                    .remove::<FlatPart>();
                // Dirties the chain for `mark_dirty_trees`, so the next propagate re-seats it.
                t.set_changed();
            }
            *on = false;
        }
        return;
    }
    *on = true;
    for e in &fresh {
        commands
            .entity(e)
            .remove::<TransformTreeChanged>()
            .insert(FlatPart);
    }
}

/// The last written parent frame, per parent: translation and rotation.
#[derive(Default)]
struct Seated(EntityHashMap<(Vec3, Quat)>);

/// Seat each flat part from its parent, all of one parent's parts together, after every writer
/// of a parent's frame (propagate, then the rig finalize in [`BillboardPlace`]) and before the
/// visibility pass.
fn seat_flat_parts(
    mut parts: Query<(&ChildOf, &Transform, &mut GlobalTransform, Ref<FlatPart>)>,
    parents: Query<&GlobalTransform, Without<FlatPart>>,
    mut seated: Local<Seated>,
    mut decided: Local<EntityHashMap<bool>>,
    mut frame: Local<u32>,
) {
    let _span = Span::start();
    let tol = tolerance();
    if tol < 0.0 {
        seated.0.clear();
        return;
    }
    let tol2 = tol * tol;
    decided.clear();
    for (child_of, local, mut global, flag) in &mut parts {
        let parent = child_of.parent();
        let Ok(pg) = parents.get(parent) else {
            continue;
        };
        let write = *decided.entry(parent).or_insert_with(|| {
            let (_, rot, pos) = pg.to_scale_rotation_translation();
            let moved = match seated.0.get(&parent) {
                Some((last_pos, last_rot)) => {
                    last_pos.distance_squared(pos) > tol2
                        || (tol2 == 0.0 && *last_pos != pos)
                        || last_rot.dot(rot).abs() < TURN_COS
                        || (tol2 == 0.0 && *last_rot != rot)
                }
                None => true,
            };
            if moved {
                seated.0.insert(parent, (pos, rot));
            }
            moved
        });
        if write || flag.is_added() {
            let next = pg.mul_transform(*local);
            if *global != next {
                *global = next;
            }
        }
    }
    // Parents that despawned; rare enough to sweep every ten seconds or so.
    *frame = frame.wrapping_add(1);
    if *frame % 600 == 0 {
        seated.0.retain(|e, _| parents.contains(*e));
    }
}

pub fn plugin(app: &mut App) {
    app.add_systems(
        PostUpdate,
        flatten_parts.before(TransformSystems::Propagate),
    )
    .add_systems(
        PostUpdate,
        seat_flat_parts
            .after(BillboardPlace)
            .before(VisibilitySystems::CheckVisibility),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stripped part is left alone by bevy and seated here, within the tolerance.
    #[test]
    fn a_flat_part_follows_its_parent_past_the_tolerance_only() {
        let mut app = App::new();
        app.add_plugins(bevy::transform::TransformPlugin);
        app.configure_sets(
            PostUpdate,
            BillboardPlace.after(TransformSystems::Propagate),
        );
        app.add_systems(
            PostUpdate,
            seat_flat_parts.after(BillboardPlace),
        );
        app.add_systems(PostUpdate, flatten_parts.before(TransformSystems::Propagate));
        let unit = app.world_mut().spawn(Transform::default()).id();
        let part = app
            .world_mut()
            .spawn((
                Transform::default(),
                ChildOf(unit),
                RigPart(unit),
                NoFrustumCulling,
            ))
            .id();
        app.update();
        app.update();
        assert!(app.world().get::<TransformTreeChanged>(part).is_none());
        let at = |app: &App| app.world().get::<GlobalTransform>(part).unwrap().translation();
        // Under the tolerance: bevy does not move it and neither does the seat.
        app.world_mut().get_mut::<Transform>(unit).unwrap().translation.x = 0.1;
        app.update();
        assert_eq!(at(&app).x, 0.0);
        // Past it: seated at the parent's frame.
        app.world_mut().get_mut::<Transform>(unit).unwrap().translation.x = 1.0;
        app.update();
        assert_eq!(at(&app).x, 1.0);
    }
}
