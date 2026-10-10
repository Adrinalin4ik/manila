//! Publish attribute-bound frame hovers through the ordinary world hover path.

use benilla_ui::script::UiScript;
use bevy::prelude::*;

pub(crate) fn register(app: &mut App) {
    app.add_systems(
        Update,
        frame_hover
            .in_set(super::TargetUpdate)
            .after(super::hover::update_hover)
            .before(super::hover::update_hovered_object),
    );
}

fn frame_hover(
    script: Option<NonSend<UiScript>>,
    rig: Res<crate::player::CameraControl>,
    selection: Res<super::Selection>,
    mut hover: ParamSet<(crate::ui_unit::UnitTokens, ResMut<super::Hovered>)>,
) {
    if rig.is_looking() {
        return;
    }
    let Some(script) = script else { return };
    let Ok(Some(token)) = benilla_ui::script::pfui_host::attributes::hovered_unit(script.lua())
    else {
        return;
    };
    let unit = if let Some(guid) =
        benilla_ui::script::pfui_host::units::custom_guid(script.lua(), &token)
    {
        guid.and_then(|guid| hover.p0().held(guid))
    } else {
        hover.p0().resolve(&token, &selection)
    };
    if let Some((entity, guid)) = unit {
        *hover.p1() = super::Hovered {
            target: Some(entity),
            guid: Some(guid),
            distance: 0.0,
            ..Default::default()
        };
    }
}
