//! Frame spell/macro actions stay ordered with ordinary selection and cast calls.

use crate::spell::{CastCommit, ScriptCast, TargetedBind, TargetingWants};
use benilla_ui::script::{
    pfui_host::attributes::{self, ClickAction},
    ScriptCall, SelectionRequest, UiScript,
};

pub(crate) fn apply(script: &mut UiScript, action: ClickAction, cast: &mut ScriptCast) {
    match action {
        ClickAction::RestoreSelectionCast { call, selection } => {
            if selection.is_some_and(|guid| cast.targeting.pfui_context(guid).is_none()) {
                return;
            }
            let select = |guid: Option<u64>| {
                guid.map_or(ScriptCall::ClearTarget, |guid| {
                    ScriptCall::Select(SelectionRequest::PfuiGuid(guid))
                })
            };
            script.requeue_script_calls(vec![
                select(selection),
                *call,
                select(cast.targeting.selection.guid),
            ]);
        }
        ClickAction::Cast { .. }
        | ClickAction::Item { .. }
        | ClickAction::TargetItem { .. }
        | ClickAction::TargetSpell { .. }
        | ClickAction::CancelQueue { .. }
        | ClickAction::QueueScript { .. } => {
            unreachable!("the spell queue normalizes cast policies and selection tokens")
        }
        ClickAction::RunScript { body } => {
            if let Err(error) =
                attributes::run_script(script.lua(), &body, cast.targeting.selection.guid)
            {
                script.report_script_error(&format!("Nampower queued script: {error}"));
            }
        }
        ClickAction::PetSpell { spell_id, target } => {
            if cast.targeting.pfui_context(target).is_none() {
                return;
            }
            let restore = match cast.targeting.selection.guid {
                Some(guid) => ScriptCall::Select(SelectionRequest::PfuiGuid(guid)),
                None => ScriptCall::ClearTarget,
            };
            script.requeue_script_calls(vec![
                ScriptCall::Select(SelectionRequest::PfuiGuid(target)),
                ScriptCall::CastPetSpell {
                    spell_id,
                    on_self: false,
                },
                restore,
            ]);
        }
        ClickAction::Spell {
            spell_id,
            target: None,
        } => crate::ui_spellbook::cast_spell(cast, spell_id, false),
        ClickAction::Spell {
            spell_id,
            target: Some(guid),
        } => cast_at_unit(cast, spell_id, guid),
        ClickAction::GroundSpell { spell_id, point } => {
            crate::ui_spellbook::cast_spell(cast, spell_id, false);
            commit_ground(cast, spell_id, point);
        }
        ClickAction::Macro {
            input,
            named,
            target,
        } => {
            let mut calls = Vec::new();
            if let Some(guid) = target {
                // A disappeared unit never runs the macro against an unrelated selection.
                if cast.targeting.pfui_context(guid).is_none() {
                    return;
                }
                calls.push(ScriptCall::Select(SelectionRequest::PfuiGuid(guid)));
            }
            calls.push(ScriptCall::PfuiClick(ClickAction::RunMacro {
                input,
                named,
            }));
            if target.is_some() {
                calls.push(match cast.targeting.selection.guid {
                    Some(guid) => ScriptCall::Select(SelectionRequest::PfuiGuid(guid)),
                    None => ScriptCall::ClearTarget,
                });
            }
            script.requeue_script_calls(calls);
        }
        ClickAction::RunMacro { input, named } => {
            if let Err(error) =
                attributes::run_macro(script.lua(), &input, named, cast.targeting.selection.guid)
            {
                script.report_script_error(&format!("pfUI click macro: {error}"));
            }
        }
    }
}

fn cast_at_unit(cast: &mut ScriptCast, spell_id: u32, guid: u64) {
    let ScriptCast { targeting, ladder } = cast;
    let Some(ctx) = targeting.pfui_context(guid) else {
        return;
    };
    // The same aura/form toggle predicates used by the stock spellbook's cast entry.
    if let (Some(spells), Some(store)) = (ladder.spells.as_ref(), ctx.rel.self_store) {
        if let Some(def) = spells.catalog.get(spell_id) {
            let form = store.0.unit_shapeshift_form();
            let toggle = crate::ui_action::toggle::active_action_toggle(spell_id, def, store);
            let form_toggle = crate::ui_action::toggle::form_recast_disposition(
                def,
                form,
                spells.forms.get(&u32::from(form)),
            );
            if toggle || form_toggle == Some(true) {
                let _ = ladder
                    .commands
                    .0
                    .send(crate::net::ClientCommand::CancelAura { spell_id });
                return;
            }
            if form_toggle == Some(false) {
                return;
            }
        }
    }
    // No attack auto-pick: it could move selection and override the clicked GUID.
    ladder.send(spell_id, &ctx, CastCommit::Spell);
    if ladder.ground.spell() == Some(spell_id) && ladder.ground.wants(TargetingWants::Location) {
        if let Some(position) = ctx.range.target_pos {
            let at = benilla_assets::coords::bevy_to_wow(position);
            commit_ground_ladder(ladder, spell_id, at);
        }
    }
}

fn commit_ground(cast: &mut ScriptCast, spell_id: u32, point: [f32; 3]) {
    commit_ground_ladder(&mut cast.ladder, spell_id, point);
}

fn commit_ground_ladder(ladder: &mut crate::spell::CastLadder, spell_id: u32, point: [f32; 3]) {
    if ladder.ground.spell() != Some(spell_id) || !ladder.ground.wants(TargetingWants::Location) {
        return;
    }
    // BindLocation takes SOURCE before DEST, including patched rows carrying both bits.
    let source = ladder
        .spells
        .as_ref()
        .and_then(|spells| spells.catalog.get(spell_id))
        .is_some_and(|def| crate::spell::cast_target::cast_target_mask(def) & 0x20 != 0);
    let bound = if source {
        TargetedBind::Source(point)
    } else {
        TargetedBind::Dest(point)
    };
    ladder.commit_targeted(spell_id, CastCommit::Spell, bound);
}
