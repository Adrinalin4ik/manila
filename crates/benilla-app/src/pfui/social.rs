//! Name-query observation adds persistence and resolves late loot identities without new queries.

use crate::net::NetHandlerApp;
use benilla_protocol::{SessionEvent, SessionEventKind};
use benilla_ui::script::UiScript;
use bevy::prelude::*;

pub(super) fn register(app: &mut App) {
    app.net_handler(SessionEventKind::PlayerName, receive);
}

fn receive(In(event): In<SessionEvent>, script: Option<NonSend<UiScript>>) {
    let (
        SessionEvent::PlayerName {
            guid,
            name,
            race,
            class,
            gender,
        },
        Some(script),
    ) = (event, script)
    else {
        return;
    };
    if !benilla_protocol::guid::is_player(guid) || name.is_empty() {
        return;
    }
    let class = u8::try_from(class)
        .ok()
        .and_then(crate::ui_unit::class_names)
        .map(|(_, token)| token);
    if let Err(error) =
        manila_pfui::player_cache::observe_name(script.lua(), guid, &name, class, race, gender)
    {
        script.report_script_error(&format!("pfUI name cache: {error}"));
    }
    manila_pfui::loot::resolve_player(script.lua(), guid, &name, class);
}
