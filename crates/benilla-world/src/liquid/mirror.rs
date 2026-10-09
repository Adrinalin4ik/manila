//! MONKEY (planar water): real reflections for the High water tier. The scene is rendered a second
//! time, from the world camera's eye reflected in the water plane near the camera, into
//! [`WaterMirrorImage`] at a fraction of the screen's resolution; `enhanced_water.wgsl`
//! (`water_planar`) reads it at the water pixel's own, flipped, screen position. Screen-space
//! reflection can only show what is on screen, so the undersides of docks and hulls, the far side
//! of a post and anything above the frame were missing or smeared; the mirror has them all, with
//! the real sky and its clouds.
//!
//! The mirror camera is an ordinary Bevy `Camera3d` with the world camera's view key (HDR, MSAA,
//! perspective, shadow filter), so it shares every specialised pipeline, and Bevy's oblique near
//! plane ([`PerspectiveProjection::near_clip_plane`]) clips it at the water: nothing below the
//! surface (the lake bed, the drowned half of a post) can stand between the mirrored eye and the
//! scenery. It is cheaper than the world view by construction: no water (the mirror never reflects
//! water), no sun shadow cascades (it is not on the shadow rig's render layer), none of the world
//! camera's post passes, and it draws the static set the world camera's cull already admitted.
//!
//! The plane is the water most of the screen looks at: a fan of rays through the frame is marched
//! against the liquid footprints and each votes, nearer hits weighing more, for the height it first
//! meets ([`plane_votes`], [`choose_plane`]). Water at another height fades back to the
//! screen-space march in the shader.

use bevy::camera::primitives::{Frustum, HalfSpace};
use bevy::camera::visibility::{VisibilitySystems, VisibleEntities};
use bevy::camera::{CameraUpdateSystems, RenderTarget};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::image::ToExtents;
use bevy::light::ShadowFilteringMethod;
use bevy::prelude::*;
use bevy::render::view::{Hdr, Msaa};

use benilla_assets::coords::bevy_to_wow;
use benilla_assets::materials::LiquidMaterial;
use benilla_assets::{WaterMirrorImage, WaterQuality, WaterReflections};

use super::query::WaterChunkInfo;
use super::spatial::WaterIndex;
use crate::view::WorldCamera;

/// The mirror's resolution as a share of the world view's, per axis.
const MIRROR_SCALE: f32 = 0.5;
/// Below this eye height over the plane the mirror is not used: the oblique near plane passes
/// almost through the eye, and its depth range collapses.
const MIN_EYE_HEIGHT: f32 = 0.5;
/// How far a ray is marched for the plane vote, in yards.
const VOTE_REACH: f32 = 260.0;
/// Two votes closer than this, in yards, are one plane.
const PLANE_MERGE: f32 = 0.3;
/// The current plane is kept while it holds this share of the leading plane's votes.
const PLANE_HOLD: f32 = 0.6;
/// The vote's screen fan: columns across the whole width, rows from the bottom edge to a little
/// above the centre (water is below the horizon).
const FAN_COLS: usize = 7;
const FAN_ROWS: usize = 6;

/// Marks the mirror camera.
#[derive(Component)]
pub struct WaterMirrorCamera;

/// The mirror's state between frames.
#[derive(Resource, Default)]
struct MirrorState {
    /// The plane height (Bevy Y) the mirror renders about, `None` with no water in view.
    plane: Option<f32>,
    /// Consecutive frames the mirror camera has been active: the image is trusted from the second.
    live_frames: u32,
    /// What the materials were last told, so they are only touched on a change.
    published: Vec4,
    /// Frames seen by the `WOW_WATER_MIRROR_LOG` line.
    log_frames: u32,
}

/// `WOW_WATER_REFLECT=0|1` overrides the setting for this run, as `WOW_WATER` does the tier. The
/// tier's own override is read here too: `scene_depth.rs` re-applies it only in `Last`, after the
/// options bridge has put the saved tier back for the frame, and the mirror is decided before that.
#[derive(Resource)]
struct ReflectOverride {
    reflections: Option<u8>,
    tier: Option<u8>,
}

/// `WOW_WATER_MIRROR_LOG=1`: one line a second of the mirror's verdict (plane, votes, active).
fn mirror_log() -> bool {
    std::env::var_os("WOW_WATER_MIRROR_LOG").is_some()
}

/// The planar reflection lane. Needs the liquid plugin (the index and the footprints).
pub struct WaterMirrorPlugin;

impl Plugin for WaterMirrorPlugin {
    fn build(&self, app: &mut App) {
        let value = std::env::var("WOW_WATER_REFLECT")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|v| *v <= 1);
        if let Some(value) = value {
            app.insert_resource(WaterReflections(value));
        }
        app.init_resource::<WaterReflections>()
            .init_resource::<WaterMirrorImage>()
            .init_resource::<MirrorState>()
            .insert_resource(ReflectOverride {
                reflections: value,
                tier: std::env::var("WOW_WATER")
                    .ok()
                    .and_then(|v| v.parse::<u8>().ok())
                    .filter(|v| *v <= 2),
            })
            .add_systems(Update, spawn_mirror_camera)
            .add_systems(
                PostUpdate,
                (
                    // Before Bevy turns the projection into this frame's matrices.
                    drive_mirror.before(CameraUpdateSystems),
                    clip_mirror_frustum
                        .after(VisibilitySystems::UpdateFrusta)
                        .before(VisibilitySystems::CheckVisibility),
                    drop_water_from_mirror
                        .after(VisibilitySystems::CheckVisibility)
                        .before(VisibilitySystems::MarkNewlyHiddenEntitiesInvisible),
                ),
            );
    }
}

/// The mirror camera, spawned once beside the world camera, inactive until water is in view.
fn spawn_mirror_camera(
    mut commands: Commands,
    world: Query<(), With<WorldCamera>>,
    mirror: Query<(), With<WaterMirrorCamera>>,
    image: Res<WaterMirrorImage>,
) {
    if world.is_empty() || !mirror.is_empty() {
        return;
    }
    commands.spawn((
        Name::new("water mirror"),
        Camera3d::default(),
        Camera {
            // Before the world camera, whose water reads the image.
            order: -1,
            is_active: false,
            clear_color: ClearColorConfig::Custom(Color::BLACK),
            ..default()
        },
        RenderTarget::Image(image.0.clone().into()),
        // The world camera's view shape, so every pipeline is shared (`player/setup.rs`).
        Hdr,
        Tonemapping::None,
        Msaa::Off,
        Projection::from(PerspectiveProjection::default()),
        bevy::light::cluster::ClusterConfig::None,
        crate::static_gx::StaticGxMirror,
        WaterMirrorCamera,
    ));
}

/// The plane, the mirrored pose and projection, the target size, and the materials' mirror lane.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn drive_mirror(
    world: Query<
        (
            &Camera,
            &Transform,
            &GlobalTransform,
            Has<ChildOf>,
            &Projection,
            &Msaa,
            Option<&ShadowFilteringMethod>,
        ),
        (With<WorldCamera>, Without<WaterMirrorCamera>),
    >,
    mut mirror: Query<
        (
            Entity,
            &mut Camera,
            &mut Transform,
            &mut Projection,
            &mut Msaa,
            Option<&ShadowFilteringMethod>,
        ),
        With<WaterMirrorCamera>,
    >,
    mut commands: Commands,
    quality: Res<WaterQuality>,
    mut reflections: ResMut<WaterReflections>,
    override_value: Res<ReflectOverride>,
    underwater: Res<super::Underwater>,
    index: Res<WaterIndex>,
    chunks: Query<&WaterChunkInfo>,
    image: Res<WaterMirrorImage>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<LiquidMaterial>>,
    mut state: ResMut<MirrorState>,
) {
    if let Some(value) = override_value.reflections {
        if reflections.0 != value {
            reflections.0 = value;
        }
    }
    let Ok((entity, mut cam, mut tf, mut proj, mut msaa, filter)) = mirror.single_mut() else {
        return;
    };
    let main = world.iter().next();
    let tier = override_value.tier.unwrap_or(quality.0);
    let mut active = false;
    let mut vote_count = 0;
    if let Some((main_cam, main_tf, main_gt, parented, main_proj, main_msaa, main_filter)) = main {
        // A root camera's `Transform` is this frame's pose; Bevy's propagation has not run yet.
        let eye = if parented {
            *main_gt
        } else {
            GlobalTransform::from(*main_tf)
        };
        let wanted = tier >= 2 && reflections.0 == 1 && main_cam.is_active && !underwater.0.any();
        state.plane = if wanted {
            let water_z = |x: f32, y: f32| {
                index
                    .over(x, y)
                    .iter()
                    .filter_map(|e| chunks.get(*e).ok())
                    .filter_map(|c| c.water_z_at(x, y))
                    .reduce(f32::max)
            };
            let votes = match main_proj {
                Projection::Perspective(p) => plane_votes(
                    eye.translation(),
                    fan_dirs(&eye, p.fov, p.aspect_ratio),
                    water_z,
                ),
                _ => Vec::new(),
            };
            vote_count = votes.len();
            choose_plane(&votes, state.plane)
        } else {
            None
        };
        if let (Some(plane), Projection::Perspective(p)) = (state.plane, main_proj) {
            if eye.translation().y - plane >= MIN_EYE_HEIGHT {
                active = true;
                let mirrored = mirror_pose(&eye, plane);
                *tf = mirrored;
                let near_clip_plane = view_plane(&GlobalTransform::from(mirrored), plane);
                // Rewritten every frame: the clip plane moves in view space with the eye. Bevy
                // re-derives the aspect from the mirror's own target, which is the world view's
                // scaled (to a rounding pixel).
                *proj = Projection::Perspective(PerspectiveProjection {
                    fov: p.fov,
                    near: p.near,
                    far: p.far,
                    aspect_ratio: p.aspect_ratio,
                    near_clip_plane,
                });
                if *msaa != *main_msaa {
                    *msaa = *main_msaa;
                }
                if filter != main_filter {
                    match main_filter {
                        Some(f) => {
                            commands.entity(entity).insert(*f);
                        }
                        None => {
                            commands.entity(entity).remove::<ShadowFilteringMethod>();
                        }
                    }
                }
                if let Some(size) = main_cam.physical_target_size() {
                    let want = (size.as_vec2() * MIRROR_SCALE)
                        .round()
                        .as_uvec2()
                        .max(UVec2::ONE);
                    let stale = images.get(&image.0).is_some_and(|i| i.size() != want);
                    if stale {
                        if let Some(img) = images.get_mut(&image.0) {
                            img.resize(want.to_extents());
                        }
                        // A resize replaces the GPU view behind this stable handle: rebuild the
                        // water bind groups (as `scene_depth.rs` does for its own images).
                        let ids: Vec<_> = materials.ids().collect();
                        for id in ids {
                            let _ = materials.get_mut(id);
                        }
                        state.live_frames = 0;
                    }
                }
            }
        }
    }
    if cam.is_active != active {
        cam.is_active = active;
    }
    state.live_frames = if active {
        state.live_frames.saturating_add(1)
    } else {
        0
    };
    if mirror_log() {
        state.log_frames += 1;
        if state.log_frames % 60 == 1 {
            info!(
                "WATER_MIRROR tier={tier} reflections={} plane={:?} votes={vote_count} active={active} live={} cams={}",
                reflections.0,
                state.plane,
                state.live_frames,
                world.iter().count()
            );
        }
    }
    let publish = match state.plane {
        Some(plane) if state.live_frames >= 2 => Vec4::new(1.0, plane, 0.0, 0.0),
        _ => Vec4::ZERO,
    };
    if publish != state.published {
        state.published = publish;
        let ids: Vec<_> = materials
            .iter()
            .filter(|(_, m)| m.extension.water.lane.z < 0.5 && m.extension.water.mirror != publish)
            .map(|(id, _)| id)
            .collect();
        for id in ids {
            if let Some(m) = materials.get_mut(id) {
                m.extension.water.mirror = publish;
            }
        }
    }
}

/// Bevy's frustum near plane is not the oblique one: cull the mirror's view at the water itself, so
/// nothing wholly below the surface is drawn only to be clipped.
fn clip_mirror_frustum(
    state: Res<MirrorState>,
    mut mirror: Query<(&Camera, &mut Frustum), With<WaterMirrorCamera>>,
) {
    let (Some(plane), Ok((cam, mut frustum))) = (state.plane, mirror.single_mut()) else {
        return;
    };
    if cam.is_active {
        frustum.half_spaces[Frustum::NEAR_PLANE_IDX] =
            HalfSpace::new(Vec4::new(0.0, 1.0, 0.0, -plane));
    }
}

/// The mirror never reflects water: drop every liquid surface from its visible list.
fn drop_water_from_mirror(
    mut mirror: Query<(&Camera, &mut VisibleEntities), With<WaterMirrorCamera>>,
    liquids: Query<(), With<MeshMaterial3d<LiquidMaterial>>>,
) {
    let Ok((cam, mut visible)) = mirror.single_mut() else {
        return;
    };
    if !cam.is_active {
        return;
    }
    visible
        .get_mut(std::any::TypeId::of::<Mesh3d>())
        .retain(|e| !liquids.contains(*e));
}

/// The eye reflected in the plane `y = plane`, as a PROPER rotation: the mirror image of the basis
/// `(right, up, back)` is left-handed, so up is negated, turning the view half a turn about its axis.
/// Seen from this pose a point X lands where the world camera sees X's reflection, upside down
/// (`enhanced_water.wgsl`, `water_planar`).
pub(super) fn mirror_pose(eye: &GlobalTransform, plane: f32) -> Transform {
    let m = |v: Vec3| Vec3::new(v.x, -v.y, v.z);
    let p = eye.translation();
    let basis = Mat3::from_cols(m(*eye.right()), -m(*eye.up()), m(*eye.back()));
    Transform {
        translation: Vec3::new(p.x, 2.0 * plane - p.y, p.z),
        rotation: Quat::from_mat3(&basis).normalize(),
        scale: Vec3::ONE,
    }
}

/// The water plane `y = plane` in a camera's view space, positive above the water: the
/// [`PerspectiveProjection::near_clip_plane`] that keeps only what is above it.
pub(super) fn view_plane(camera: &GlobalTransform, plane: f32) -> Vec4 {
    // Planes transform by the inverse transpose: view = world_from_view^T · world-plane.
    camera.to_matrix().transpose() * Vec4::new(0.0, 1.0, 0.0, -plane)
}

/// World directions through a fan of points on the screen.
fn fan_dirs(eye: &GlobalTransform, fov: f32, aspect: f32) -> Vec<Vec3> {
    let tan_y = (fov * 0.5).tan();
    let tan_x = tan_y * aspect;
    let mut out = Vec::with_capacity(FAN_COLS * FAN_ROWS);
    for r in 0..FAN_ROWS {
        // NDC y from -0.95 (bottom edge) to 0.25.
        let y = -0.95 + 1.2 * r as f32 / (FAN_ROWS - 1) as f32;
        for c in 0..FAN_COLS {
            let x = -0.9 + 1.8 * c as f32 / (FAN_COLS - 1) as f32;
            let local = Vec3::new(x * tan_x, y * tan_y, -1.0).normalize();
            out.push(eye.rotation() * local);
        }
    }
    out
}

/// March each ray from `eye` until it first passes under a water surface (`water_z`, WoW XY to the
/// surface's WoW Z, i.e. Bevy Y); each hit is a `(height, weight)` vote, nearer hits weighing more.
pub(super) fn plane_votes(
    eye: Vec3,
    dirs: Vec<Vec3>,
    water_z: impl Fn(f32, f32) -> Option<f32>,
) -> Vec<(f32, f32)> {
    let mut votes = Vec::new();
    for d in dirs {
        // Only rays heading down can meet water below the eye.
        if d.y > -0.01 {
            continue;
        }
        let mut t = 1.0;
        let mut step = 1.0;
        while t < VOTE_REACH {
            let p = eye + d * t;
            let wow = bevy_to_wow(p);
            if let Some(z) = water_z(wow[0], wow[1]) {
                if p.y <= z && z < eye.y {
                    votes.push((z, 1.0 / (1.0 + t / 40.0)));
                    break;
                }
            }
            t += step;
            step *= 1.12;
        }
    }
    votes
}

/// The plane with the most weight, unless the current one still holds [`PLANE_HOLD`] of it.
pub(super) fn choose_plane(votes: &[(f32, f32)], current: Option<f32>) -> Option<f32> {
    // Merge the votes into planes: (weighted height sum, weight).
    let mut planes: Vec<(f32, f32)> = Vec::new();
    for &(h, w) in votes {
        match planes
            .iter_mut()
            .find(|(sum, weight)| (sum / weight - h).abs() < PLANE_MERGE)
        {
            Some(p) => {
                p.0 += h * w;
                p.1 += w;
            }
            None => planes.push((h * w, w)),
        }
    }
    let (best_sum, best_w) = planes.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1))?;
    if let Some(c) = current {
        let held = planes
            .iter()
            .find(|(sum, weight)| (sum / weight - c).abs() < PLANE_MERGE)
            .map_or(0.0, |p| p.1);
        if held >= PLANE_HOLD * best_w {
            return Some(c);
        }
    }
    Some(best_sum / best_w)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eye() -> GlobalTransform {
        GlobalTransform::from(
            Transform::from_xyz(3.0, 12.0, 7.0).looking_at(Vec3::new(20.0, 2.0, -30.0), Vec3::Y),
        )
    }

    /// The mirrored camera sees a point where the world camera sees its reflection, flipped in y:
    /// the contract `water_planar` reads the image by.
    #[test]
    fn the_mirror_sees_the_reflection_upside_down() {
        let eye = eye();
        let plane = 2.0;
        let mirror = GlobalTransform::from(mirror_pose(&eye, plane));
        let proj = Mat4::perspective_infinite_reverse_rh(0.8, 1.7, 0.1);
        for x in [
            Vec3::new(10.0, 6.0, -20.0),
            Vec3::new(-4.0, 2.0, -9.0),
            Vec3::new(30.0, 25.0, -60.0),
        ] {
            let reflected = Vec3::new(x.x, 2.0 * plane - x.y, x.z);
            let a = proj * eye.to_matrix().inverse() * reflected.extend(1.0);
            let b = proj * mirror.to_matrix().inverse() * x.extend(1.0);
            let (a, b) = (a.truncate().truncate() / a.w, b.truncate().truncate() / b.w);
            assert!(
                (a.x - b.x).abs() < 1e-4 && (a.y + b.y).abs() < 1e-4,
                "{a} vs {b}"
            );
        }
        // A proper rotation: Bevy's culling and winding stay as they are.
        assert!((mirror.to_matrix().determinant() - 1.0).abs() < 1e-4);
    }

    /// The oblique near plane keeps what is above the water and clips what is below it, and depth
    /// still falls with distance along a ray (reverse-Z) above the plane.
    #[test]
    fn the_near_plane_is_the_water() {
        let eye = eye();
        let plane = 2.0;
        let mirror = GlobalTransform::from(mirror_pose(&eye, plane));
        let projection = PerspectiveProjection {
            fov: 0.8,
            aspect_ratio: 1.7,
            near: 0.1,
            far: 1000.0,
            near_clip_plane: view_plane(&mirror, plane),
        };
        use bevy::camera::CameraProjection;
        let clip_from_world = projection.get_clip_from_view() * mirror.to_matrix().inverse();
        let depth = |p: Vec3| {
            let c = clip_from_world * p.extend(1.0);
            (c.z, c.w)
        };
        // The mirror eye looks up through the plane at the scenery.
        let target = Vec3::new(20.0, 6.0, -30.0);
        let (z, w) = depth(target);
        assert!(z >= 0.0 && z <= w, "above water is kept: {z} {w}");
        let under = Vec3::new(8.0, 1.0, -10.0);
        let (z, w) = depth(under);
        assert!(z > w, "below water is clipped: {z} {w}");
        let m = mirror.translation();
        let dir = (target - m).normalize();
        let along = |t: f32| {
            let (z, w) = depth(m + dir * t);
            z / w
        };
        assert!(along(20.0) > along(40.0) && along(40.0) > along(400.0) && along(400.0) >= 0.0);
    }

    /// A lake under the camera wins the vote; a pond above the eye and an off-screen pool do not.
    #[test]
    fn the_plane_is_the_water_the_view_looks_at() {
        let eye = eye();
        let dirs = fan_dirs(&eye, 0.8, 1.7);
        let lake = |_x: f32, _y: f32| Some(2.0);
        let votes = plane_votes(eye.translation(), dirs.clone(), lake);
        assert!(!votes.is_empty());
        assert_eq!(choose_plane(&votes, None), Some(2.0));
        // Water above the eye is never a mirror.
        let high = |_x: f32, _y: f32| Some(40.0);
        assert!(plane_votes(eye.translation(), dirs.clone(), high).is_empty());
    }

    /// The held plane survives a close second, and yields to a clear winner.
    #[test]
    fn the_plane_holds_against_a_close_second() {
        let votes = [(2.0, 1.0), (2.1, 1.0), (5.0, 2.5)];
        // 2.0-ish holds 2.0 of the leader's 2.5: kept.
        assert_eq!(choose_plane(&votes, Some(2.0)), Some(2.0));
        let votes = [(2.0, 1.0), (5.0, 4.0)];
        assert_eq!(choose_plane(&votes, Some(2.0)), Some(5.0));
        assert_eq!(choose_plane(&[], Some(2.0)), None);
    }
}
