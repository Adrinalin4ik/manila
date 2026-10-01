//! Press and Hold Casting: wenilla's own, under the later clients' CVar name
//! `ActionButtonUseKeyHeldSpell`. 1.12 has no such setting; its action-bar keys cast once, on the
//! release (`ActionButtonUp` calls `UseAction`; `ActionButtonDown` only shows the button pushed).
//!
//! With the CVar set, a KEYBOARD latch on an action-bar command repeats its cast while held: each
//! time the slot is ready, the command's own body runs its up-half then its down-half again — so
//! the paging, the self-cast argument and any addon's hook are the Lua's, exactly as on a release —
//! and the `UseAction` that produces is applied. The first cast so lands on the press frame, and
//! the release of a latch that has cast drops its `UseAction`, or a tap would cast twice. A latch
//! that never cast releases as stock, red line included.
//!
//! What a repeat refuses to do (each would be a re-press doing something a cast is not): a slot
//! that is not a spell (a macro would re-run its `/say`, an item re-use itself), the Attack slot
//! and a live auto-repeat (both toggles), a spell whose aura the re-press would cancel
//! (`toggle::active_action_toggle`), and any press while a targeting cursor is up, which the
//! re-press would close. "Ready" is the ladder's own silent gates read in advance — no guarding
//! cast in flight, no channel, no cooldown or GCD (`Cooldowns::not_ready`) — plus the bar's own
//! `IsUsableAction` and `IsActionInRange`, so holding through a cooldown or out of range sends
//! nothing and paints no red line.
//!
//! A refusal the gates cannot see (no target, dead, mounted) still reaches the ladder; [`FLOOR`]
//! caps how often. Mouse latches never repeat: the option is keyboard-only, and a click on a button
//! is its `OnClick`, which no binding sees.

use bevy::platform::time::Instant;
use std::time::Duration;

use bevy::input::keyboard::KeyCode;
use bevy::prelude::*;

use benilla_protocol::messages::ACTION_KIND_SPELL;
use benilla_ui::script::{ActionUse, ScriptCall, UiScript};

use super::{BindKey, BindingsState};
use crate::net::{ObjectStore, SelfPlayer};
use crate::spell::{ActiveChannel, AutoRepeatActive, Cooldowns, PendingCast, SpellTargeting};
use crate::ui_action::{PlayerActions, Spells, SPELL_ATTACK};
use crate::ui_script::UiKeyboardCapture;

/// The CVar's name, the later clients' spelling.
pub(crate) const CVAR: &str = "ActionButtonUseKeyHeldSpell";

/// The least time between two repeats of one latch. Unsourced, our own choice: a successful cast
/// shuts the gate itself (GCD, in-flight), so this only bounds a refusal the gates cannot see — a
/// held key with no target would otherwise reach the ladder every frame.
const FLOOR: Duration = Duration::from_millis(250);

/// `ActionButtonUseKeyHeldSpell`: repeat a held action-bar key's cast.
#[derive(Resource, Default)]
pub(crate) struct PressAndHoldCasting(pub(crate) bool);

/// The CVar's change callback: a flag.
pub(crate) fn on_cvar(ev: On<crate::cvars::CvarChanged>, mut setting: ResMut<PressAndHoldCasting>) {
    if ev.is(CVAR) {
        setting.0 = ev.flag();
    }
}

/// One held action-bar key this module is watching.
pub(super) struct Hold {
    key: KeyCode,
    command: String,
    /// The Lua action id its body last used: learnt from the first repeat, re-learnt on each, so a
    /// page flip is followed.
    action: Option<u32>,
    /// False once its body produced something other than one spell `UseAction`: it releases as
    /// stock and never repeats again.
    eligible: bool,
    /// A repeat's `UseAction` was applied: the release must not cast again.
    fired: bool,
    last: Option<Instant>,
}

/// The action-bar commands, by the 1.12 `Bindings.xml` names: `ACTIONBUTTON1`-`12`,
/// `MULTIACTIONBAR1BUTTON1`..`MULTIACTIONBAR4BUTTON12`, and the self-cast `SELFACTIONBUTTON*`.
fn is_action_command(command: &str) -> bool {
    let base = command.trim_end_matches(|c: char| c.is_ascii_digit());
    if base.len() == command.len() {
        return false;
    }
    matches!(
        base,
        "ACTIONBUTTON"
            | "SELFACTIONBUTTON"
            | "MULTIACTIONBAR1BUTTON"
            | "MULTIACTIONBAR2BUTTON"
            | "MULTIACTIONBAR3BUTTON"
            | "MULTIACTIONBAR4BUTTON"
    )
}

/// The ladder's silent gates, read ahead of a repeat.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct ReadyGate<'w, 's> {
    actions: Res<'w, PlayerActions>,
    spells: Option<Res<'w, Spells>>,
    cooldowns: Res<'w, Cooldowns>,
    pending: Res<'w, PendingCast>,
    channel: Res<'w, ActiveChannel>,
    auto_repeat: Res<'w, AutoRepeatActive>,
    targeting: Res<'w, SpellTargeting>,
    me: Query<'w, 's, &'static ObjectStore, With<SelfPlayer>>,
}

/// What a repeat of one action may do now.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Cast.
    Go,
    /// Not now; ask again next frame.
    Wait,
    /// Never: the slot is not a repeatable spell.
    Never,
}

impl ReadyGate<'_, '_> {
    /// Gates that need no slot: a cursor up, a guarding cast in flight, a channel.
    fn busy(&self, now: Instant) -> bool {
        self.targeting.active()
            || self.pending.in_flight(now)
            || self.channel.current(now).is_some()
    }

    fn verdict(&self, script: &UiScript, action: u32, now: Instant) -> Verdict {
        let Some(b) = u8::try_from(action.saturating_sub(1))
            .ok()
            .and_then(|slot| self.actions.buttons.get(&slot))
        else {
            return Verdict::Wait; // an empty slot: a page flip may fill it
        };
        if b.kind != ACTION_KIND_SPELL || b.action == SPELL_ATTACK {
            return Verdict::Never;
        }
        if self.busy(now) || self.auto_repeat.0 == Some(b.action) {
            return Verdict::Wait;
        }
        let def = self.spells.as_ref().and_then(|s| s.catalog.get(b.action));
        if let (Some(d), Some(store)) = (def, self.me.iter().next()) {
            if crate::ui_action::toggle::active_action_toggle(b.action, d, store) {
                return Verdict::Wait;
            }
        }
        if self.cooldowns.not_ready(b.action, 0, def, now) {
            return Verdict::Wait;
        }
        // The bar's own reads (`IsActionInRange`'s 0 is out; nil is rangeless or no target).
        let bar = script.eval::<(bool, bool)>(&format!(
            "local r = IsActionInRange({action}) return IsUsableAction({action}) and true or false, r ~= 0"
        ));
        match bar {
            Ok((true, true)) => Verdict::Go,
            _ => Verdict::Wait,
        }
    }
}

/// After the dispatch pass and before the script calls are applied: forget released latches
/// (dropping a cast release's `UseAction`), take up new ones, and repeat the ready.
pub(super) fn tick(
    mut script: Option<NonSendMut<UiScript>>,
    setting: Res<PressAndHoldCasting>,
    state: Res<BindingsState>,
    capture: Res<UiKeyboardCapture>,
    gate: ReadyGate,
    mut holds: Local<Vec<Hold>>,
) {
    let Some(script) = script.as_mut() else {
        holds.clear();
        return;
    };
    let held = |h: &Hold| {
        state
            .latched
            .iter()
            .any(|(k, c)| *k == BindKey::Key(h.key) && *c == h.command)
    };
    // Released this pass: its up-half has queued the stock cast, which a latch that cast drops.
    for h in holds.iter().filter(|h| !held(h)) {
        if let (true, Some(action)) = (h.fired, h.action) {
            drop_one_use(script, action);
        }
    }
    holds.retain(|h| held(h));
    if !setting.0 {
        holds.clear();
        return;
    }
    for (k, command) in &state.latched {
        let BindKey::Key(key) = *k else { continue };
        if is_action_command(command)
            && !holds.iter().any(|h| h.key == key && h.command == *command)
        {
            holds.push(Hold {
                key,
                command: command.clone(),
                action: None,
                eligible: true,
                fired: false,
                last: None,
            });
        }
    }
    if capture.typing || holds.is_empty() {
        return;
    }
    let now = Instant::now();
    if gate.busy(now) {
        return;
    }
    for h in holds.iter_mut().filter(|h| h.eligible) {
        if h.last.is_some_and(|t| now.duration_since(t) < FLOOR) {
            continue;
        }
        // A known slot is asked before its body runs, so a held key on cooldown costs no Lua run.
        if let Some(action) = h.action {
            match gate.verdict(script, action, now) {
                Verdict::Go => {}
                Verdict::Wait => continue,
                Verdict::Never => {
                    h.eligible = false;
                    continue;
                }
            }
        }
        let earlier = script.take_script_calls();
        for down in [false, true] {
            if let Err(e) = script.execute_binding(&h.command, down) {
                warn!("bindings({}): hold repeat: {e}", h.command);
            }
        }
        let made = script.take_script_calls();
        let used = match made.as_slice() {
            [] => None,
            [ScriptCall::UseAction(u)] => Some(*u),
            _ => {
                h.eligible = false;
                None
            }
        };
        let keep = used.filter(|u| {
            h.action = Some(u.action);
            match gate.verdict(script, u.action, now) {
                Verdict::Go => true,
                Verdict::Wait => false,
                Verdict::Never => {
                    h.eligible = false;
                    false
                }
            }
        });
        // Queue order: what was waiting, then this repeat's cast.
        if let Some(u) = keep {
            script.requeue_script_calls(vec![ScriptCall::UseAction(u)]);
            h.fired = true;
            h.last = Some(now);
        }
        script.requeue_script_calls(earlier);
        if keep.is_some() {
            // One cast per pass: the next latch would only meet this one's in-flight guard.
            break;
        }
    }
}

/// Remove the first queued `UseAction` of `action`: a released latch's stock cast.
fn drop_one_use(script: &mut UiScript, action: u32) {
    let mut calls = script.take_script_calls();
    if let Some(i) = calls.iter().position(
        |c| matches!(c, ScriptCall::UseAction(ActionUse { action: a, .. }) if *a == action),
    ) {
        calls.remove(i);
    }
    script.requeue_script_calls(calls);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_action_bar_commands_and_nothing_else() {
        for c in [
            "ACTIONBUTTON1",
            "ACTIONBUTTON12",
            "SELFACTIONBUTTON3",
            "MULTIACTIONBAR4BUTTON12",
        ] {
            assert!(is_action_command(c), "{c}");
        }
        for c in [
            "ACTIONBUTTON",
            "BONUSACTIONBUTTON1",
            "SHAPESHIFTBUTTON1",
            "MOVEFORWARD",
        ] {
            assert!(!is_action_command(c), "{c}");
        }
    }
}
