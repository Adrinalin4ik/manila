//! An addon's own `PlayerModel` bound to a unit, such as pfUI's 3D portraits. The stock client
//! draws the unit's model through its portrait camera; here the pane shows the still the client
//! already bakes through that camera for the round unit portraits, so only the units with such a
//! bake draw: the player, target, target's target, pet, and the party with its pets.
//!
//! While a pane shows a unit, that unit's bake loops Stand and keeps rendering, as the reference
//! animates the model in the pane; a round portrait of the same unit then moves with it.

use bevy::prelude::*;

use crate::portrait::{BoothBridge, BoothPanes, PortraitSource};
use crate::ui_pass::UiQuad;

/// The key under which a drawn unit pane asks for its slot's bake to stay alive.
fn live_key(token: &str) -> String {
    format!("live:{token}")
}

/// Whether a unit pane drew `token`'s slot last frame.
pub(crate) fn animated(panes: &BoothPanes, token: &str) -> bool {
    panes.0.contains_key(&live_key(token))
}

/// The bake is square and the portrait projection holds the diagonal field of view, so a pane
/// sees what a square of its own diagonal sees: the still is scaled to that diagonal, centred,
/// and cut to the pane.
fn placed(rect: Rect) -> Rect {
    let side = rect.width().hypot(rect.height()) / std::f32::consts::SQRT_2;
    Rect::from_center_size(rect.center(), Vec2::splat(side))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit(
    unit: Option<&str>,
    rect: Rect,
    clip: Option<Rect>,
    z_key: u64,
    alpha: f32,
    booths: &mut BoothBridge,
    assets: &mut Option<ResMut<benilla_assets::WorldAssets>>,
    images: &mut Assets<Image>,
    out: &mut Vec<UiQuad>,
) {
    let Some(unit) = unit else { return };
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let token = unit.to_ascii_lowercase();
    // Marks the slot drawn: the target's target and a party pet bake only while something shows
    // them. The round slots bake square whatever aspect is published.
    booths.panes.0.insert(token.clone(), 1.0);
    booths.panes.0.insert(live_key(&token), 1.0);
    let (texture, premultiplied) = match booths.images.0.get(&token) {
        Some(PortraitSource::Live(handle)) => (Some(handle.clone()), true),
        Some(PortraitSource::File(path)) => (
            assets.as_mut().and_then(|a| a.sprite_texture(path, images)),
            false,
        ),
        None => (None, false),
    };
    let Some(texture) = texture else { return };
    out.push(UiQuad {
        rect: placed(rect),
        z_key,
        texture: Some(texture),
        color: [1.0, 1.0, 1.0, alpha],
        premultiplied,
        clip: Some(clip.map_or(rect, |c| c.intersect(rect))),
        ..default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_square_pane_shows_the_whole_still_and_a_bar_a_band_of_it() {
        let square = placed(Rect::new(10.0, 10.0, 60.0, 60.0));
        assert!((square.width() - 50.0).abs() < 1e-4);
        assert_eq!(square.center(), Vec2::new(35.0, 35.0));
        // A 200x20 bar: the still spans the bar's diagonal over root two, taller than the bar.
        let bar = placed(Rect::new(0.0, 0.0, 200.0, 20.0));
        assert!((bar.width() - 200.0f32.hypot(20.0) / std::f32::consts::SQRT_2).abs() < 1e-3);
        assert!(bar.height() > 20.0 && bar.width() < 200.0);
        assert_eq!(bar.center(), Vec2::new(100.0, 10.0));
    }
}
