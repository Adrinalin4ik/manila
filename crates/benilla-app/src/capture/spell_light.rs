//! MONKEY (gfx): `WOW_CAPTURE_SPELL_LIGHT=x,y,z` adds a held spell-light fixture at WoW coords.
//! Exercises the real envelope, room claims and shadow budget without a server or spell assets.
//! The trace compares the legacy first-within-one-yard shadow lookup with the nearest match.

use crate::entities::{
    spell_fx::{SpellLight, SpellLightMode},
    HeldLight,
};
use benilla_assets::coords::wow_to_bevy;
use benilla_world::{
    lighting::{ResolvedPointLights, SpellFxLight, SyntheticFireLight},
    static_gx::{LightOwner, TorchShadowViews},
    terrain_stream::point_light,
};
use bevy::prelude::*;

pub(super) fn seed(mut commands: Commands, mut seeded: Local<bool>) {
    if *seeded {
        return;
    }
    *seeded = true;
    let Ok(value) = std::env::var("WOW_CAPTURE_SPELL_LIGHT") else {
        return;
    };
    let coords: Vec<f32> = value
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    let [x, y, z] = coords.as_slice() else {
        panic!("WOW_CAPTURE_SPELL_LIGHT needs x,y,z in WoW coordinates");
    };
    let at = wow_to_bevy([*x, *y, *z]);
    let parent = commands
        .spawn((Transform::from_translation(at), Visibility::default()))
        .id();
    let mut light = point_light([1.0, 0.3, 0.05], 1.0);
    let base = light.intensity;
    light.intensity = 0.0;
    let entity = commands
        .spawn((
            light,
            Transform::IDENTITY,
            Visibility::default(),
            SyntheticFireLight,
            SpellFxLight,
            HeldLight,
            SpellLight::new(base, 0.0, SpellLightMode::Kit),
            LightOwner::Instance(parent),
        ))
        .id();
    commands.entity(parent).add_child(entity);
    info!("capture-spell-light: spawned {entity} wow [{x},{y},{z}] bevy {at:?}");
}

pub(super) fn trace(
    points: Res<ResolvedPointLights>,
    shadows: Res<TorchShadowViews>,
    mut previous: Local<Vec<(usize, usize, usize)>>,
) {
    if std::env::var_os("WOW_CAPTURE_SPELL_LIGHT").is_none() {
        return;
    }
    let mut mismatches = Vec::new();
    for (point, light) in points.as_slice().iter().enumerate() {
        if light.lane < 0.5 {
            continue;
        }
        let candidates: Vec<_> = shadows
            .positions
            .iter()
            .enumerate()
            .take(shadows.count as usize)
            .filter(|(i, pos)| pos.w > 0.0 && shadows.caster_meshes[*i].is_some())
            .map(|(i, pos)| (i, pos.truncate().distance_squared(light.position)))
            .filter(|(_, d)| *d < 1.0)
            .collect();
        let Some(&(first, _)) = candidates.first() else {
            continue;
        };
        let &(nearest, distance) = candidates
            .iter()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        if first != nearest {
            mismatches.push((point, first, nearest));
            if !previous.contains(&(point, first, nearest)) {
                info!("capture-spell-light: point {point} at {:?}: first slot {first}, nearest slot {nearest}, nearest distance {distance:.6} yd squared (CPU publication; GPU readiness not asserted)", light.position);
            }
        }
    }
    if *previous != mismatches {
        info!(
            "capture-spell-light: {} ambiguous first-match rows",
            mismatches.len()
        );
        *previous = mismatches;
    }
}
