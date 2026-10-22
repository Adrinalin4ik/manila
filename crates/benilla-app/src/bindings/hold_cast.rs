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
use benilla_ui::script::{ActionUse, CallHost, CallTail, ScriptCall, UiScript};

use super::{BindingsState, Held};
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
            || self.pending.current().is_some()
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
        if self.cooldowns.not_ready(b.action, 0, 0, def, now) {
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

/// The host a hold lends around a binding body: each call is put to `pass` first, and only what
/// it lets through reaches the world's host ([`crate::script_calls::ScriptCallHost`]). Since
/// upstream's calls apply inside their verb (#2143), there is no queue left to pick a `UseAction`
/// out of afterwards; the hold judges it in the call instead, with the input events its verb was
/// made under still raised, so the protected-call gate reads the key press's as before.
struct HoldHost<'h, F: FnMut(&mut UiScript, &ScriptCall) -> bool> {
    inner: Option<&'h mut (dyn CallHost + 'h)>,
    pass: F,
}

impl<F: FnMut(&mut UiScript, &ScriptCall) -> bool> CallHost for HoldHost<'_, F> {
    fn apply(&mut self, script: &mut UiScript, call: ScriptCall) -> Option<CallTail> {
        if !(self.pass)(script, &call) {
            return None;
        }
        self.inner.as_mut()?.apply(script, call)
    }
}

/// The host a repeat lends around its binding body: the body's one spell `UseAction` reaches the
/// world's host if its slot is ready; any other call marks the hold ineligible and applies nothing.
/// The verdict is read inside the call, so a page flip in the body is followed.
struct RepeatHost<'a, 'w, 's> {
    gated: &'a mut ParamSet<
        'w,
        's,
        (
            ReadyGate<'static, 'static>,
            Option<crate::script_calls::ScriptCallHost<'static, 'static>>,
        ),
    >,
    hold: &'a mut Hold,
    now: Instant,
    seen_use: bool,
    cast: bool,
}

impl CallHost for RepeatHost<'_, '_, '_> {
    fn apply(&mut self, script: &mut UiScript, call: ScriptCall) -> Option<CallTail> {
        let ScriptCall::UseAction(u) = &call else {
            self.hold.eligible = false;
            return None;
        };
        if std::mem::replace(&mut self.seen_use, true) {
            self.hold.eligible = false;
            return None;
        }
        self.hold.action = Some(u.action);
        match self.gated.p0().verdict(script, u.action, self.now) {
            Verdict::Go => {}
            Verdict::Wait => return None,
            Verdict::Never => {
                self.hold.eligible = false;
                return None;
            }
        }
        self.cast = true;
        self.gated.p1().as_mut()?.apply(script, call)
    }
}

/// A latch's release after a repeat has cast through it: its up-half runs as stock, less the
/// first `UseAction` of `action`, the cast a tap would otherwise make twice.
pub(super) fn release_dropping_use<R>(
    script: &mut UiScript,
    host: Option<&mut crate::script_calls::ScriptCallHost>,
    action: u32,
    f: impl FnOnce(&mut UiScript) -> R,
) -> R {
    let mut dropped = false;
    let mut hold = HoldHost {
        inner: host.map(|host| host as &mut dyn CallHost),
        pass: |_: &mut UiScript, call: &ScriptCall| {
            let ours = !dropped
                && matches!(call, ScriptCall::UseAction(ActionUse { action: a, .. }) if *a == action);
            dropped |= ours;
            !ours
        },
    };
    script.hosted(&mut hold, f)
}

/// After the dispatch pass: forget released latches, take up new ones, and repeat the ready.
pub(super) fn tick(
    mut script: Option<NonSendMut<UiScript>>,
    setting: Res<PressAndHoldCasting>,
    mut state: ResMut<BindingsState>,
    capture: Res<UiKeyboardCapture>,
    // The gate reads what the host writes (the cooldowns, the in-flight slot), so they take turns.
    mut gated: ParamSet<(
        ReadyGate<'static, 'static>,
        Option<crate::script_calls::ScriptCallHost<'static, 'static>>,
    )>,
    mut holds: Local<crate::ui_script::VmMemo<Vec<Hold>>>,
) {
    let state = &mut *state;
    let Some(script) = script.as_mut() else {
        holds.get_for(None).clear();
        return;
    };
    let holds = holds.get(script);
    let latched = &state.latched;
    let held = |key: KeyCode, command: &str| {
        latched
            .iter()
            .any(|(k, c)| *k == Held::Key(key) && c == command)
    };
    // A released latch's cast was dropped by its release (`bindings::release`); what is left
    // names latches no longer held.
    holds.retain(|h| held(h.key, &h.command));
    state.hold_fired.retain(|(k, c, _)| held(*k, c));
    if !setting.0 {
        holds.clear();
        return;
    }
    for (k, command) in &state.latched {
        let Held::Key(key) = *k else { continue };
        if is_action_command(command)
            && !holds.iter().any(|h| h.key == key && h.command == *command)
        {
            holds.push(Hold {
                key,
                command: command.clone(),
                action: None,
                eligible: true,
                last: None,
            });
        }
    }
    if capture.typing || holds.is_empty() {
        return;
    }
    let now = Instant::now();
    if gated.p0().busy(now) {
        return;
    }
    for h in holds.iter_mut().filter(|h| h.eligible) {
        if h.last.is_some_and(|t| now.duration_since(t) < FLOOR) {
            continue;
        }
        // A known slot is asked before its body runs, so a held key on cooldown costs no Lua run.
        if let Some(action) = h.action {
            match gated.p0().verdict(script, action, now) {
                Verdict::Go => {}
                Verdict::Wait => continue,
                Verdict::Never => {
                    h.eligible = false;
                    continue;
                }
            }
        }
        // The body's up-half then its down-half, as a release and a re-press. Its one spell
        // `UseAction` applies if the slot it names is ready; anything else it makes (a macro's
        // lines, a second use) marks it ineligible and applies nothing more.
        let command = h.command.clone();
        let mut repeat = RepeatHost {
            gated: &mut gated,
            hold: &mut *h,
            now,
            seen_use: false,
            cast: false,
        };
        script.hosted(&mut repeat, |script| {
            for down in [false, true] {
                if let Err(e) = script.execute_binding(&command, down) {
                    warn!("bindings({command}): hold repeat: {e}");
                }
            }
        });
        let cast = repeat.cast;
        if cast {
            h.last = Some(now);
            if let Some(action) = h.action {
                match state
                    .hold_fired
                    .iter_mut()
                    .find(|(k, c, _)| *k == h.key && *c == h.command)
                {
                    Some(entry) => entry.2 = action,
                    None => state.hold_fired.push((h.key, h.command.clone(), action)),
                }
            }
            // One cast per pass: the next latch would only meet this one's in-flight guard.
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reload_relearns_a_held_commands_body_in_the_new_vm() {
        use bevy::ecs::system::{IntoSystem, System};
        let mut app = crate::game_plugins::schedule_tests::headless_client();
        app.world_mut().resource_mut::<PressAndHoldCasting>().0 = true;
        app.world_mut().resource_mut::<BindingsState>().latched =
            vec![(Held::Key(KeyCode::Digit1), "ACTIONBUTTON1".into())];
        let vm = |body: &str| {
            let script = UiScript::new().unwrap();
            script.run("calls=0").unwrap();
            script.register_bindings(&benilla_ui::bindings_xml::parse(&format!(
                "<Bindings><Binding name=\"ACTIONBUTTON1\" runOnUp=\"true\">{body}</Binding></Bindings>"
            )).unwrap());
            script
        };
        // A macro-like body is ineligible for repetition in this VM.
        app.world_mut()
            .insert_non_send_resource(vm("calls=calls+1;ClearTarget();ClearTarget()"));
        let mut system = IntoSystem::into_system(tick);
        system.initialize(app.world_mut());
        system.run((), app.world_mut()).unwrap();
        app.world()
            .non_send_resource::<UiScript>()
            .run("assert(calls==2)")
            .unwrap();
        // A UI reload can replace the body while the physical key remains latched.
        app.world_mut()
            .insert_non_send_resource(vm("calls=calls+1"));
        system.run((), app.world_mut()).unwrap();
        app.world()
            .non_send_resource::<UiScript>()
            .run("assert(calls==2)")
            .unwrap();
    }

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
