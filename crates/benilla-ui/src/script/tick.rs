//! The host runtime loop: event fan-out ([`UiScript::fire_event`]), the per-frame advance
//! ([`UiScript::tick`]) and the `GetTime()` clock ([`UiScript::now`]).

use mlua::Lua;

use crate::widget::FrameHandle;

use super::{editbox, event, tooltip, Model, ScriptValue, UiScript};

/// The most events the tap holds between drains — a frame's worth, generously (a busy raid frame
/// fires a few hundred `UNIT_*`). Past it the tap drops rather than grows: a host that stopped
/// draining must not turn the VM into a leak.
const EVENT_TAP_CAP: usize = 4096;

impl UiScript {
    /// Turn the event tap on or off ([`super::Model::event_tap`]). Turning it off discards
    /// whatever was tapped and not yet drained; turning it on twice is a no-op.
    pub fn set_event_tap(&mut self, on: bool) {
        let mut model = self.model_mut();
        match (on, model.event_tap.is_some()) {
            (true, false) => model.event_tap = Some(Vec::new()),
            (false, true) => model.event_tap = None,
            _ => {}
        }
    }

    /// Drain the events tapped since the last call — every `(name, args)` that went through
    /// [`fire_event_into`], in dispatch order, whether or not any frame was registered for it.
    /// That is [`Self::fire_event`] plus the bindings that fire against the VM directly (1924),
    /// which is the set a host watching the interface wants: an event is an event to the page
    /// whichever side of the seam raised it. Empty when the tap is off.
    pub fn take_tapped_events(&mut self) -> Vec<(String, Vec<ScriptValue>)> {
        self.model_mut()
            .event_tap
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Queue an event to fire at the **start of the next tick**, into the same
    /// [`super::Model::pending_events`] list the engine's own bindings use — so it lands *after*
    /// everything queued earlier in the frame and before that tick's `OnUpdate` pass.
    ///
    /// **Ordering, not laziness, is the whole reason this is public** (decision 1750). A benilla
    /// drain runs a step behind the input pass that fed it, so an event it fires *immediately*
    /// arrives BEFORE events the same input already queued — the reverse of the reference, where
    /// the deferral happens inside the very call that queued them. That inversion is not
    /// hypothetical: the soulbind confirm raised by placing an item into a worn slot was cancelled
    /// one tick later by that same place's own `CURSOR_UPDATE`, whose `StaticPopup_Hide` is
    /// `UIParent.lua:356-360` transcribed correctly. Queueing restores the reference's relative
    /// order — cursor-change first, question second — while a *later* cursor change still retires
    /// the question, which is exactly what that arm is for.
    ///
    /// Use [`Self::fire_event`] for anything whose ordering against in-flight queued events does
    /// not matter, which is nearly everything.
    pub fn queue_event(&mut self, event: &str, args: Vec<ScriptValue>) {
        self.model_mut()
            .pending_events
            .push((event.to_string(), args));
    }

    /// Fire an event to every frame registered for it, shown or not, setting both the
    /// `this`/`event`/`arg1..argN` globals and the `(self, event, ...)` arguments (`0x704d50`,
    /// `0x704f10`); handler errors go to [`UiScript::errors`]. Frames fire in registration order,
    /// as `SignalEvent` (`0x703e50`) walks its tail-appended list (`0x7052d0`), and consumers rely
    /// on it: both ZoneText frames write `PVPInfoTextString` on one event, and the last one wins.
    ///
    /// The walk steps by the next listener saved before each handler runs (`0x703ee8`), so a
    /// handler that unregisters itself does not skip its successor. Deviation: a handler that
    /// unregisters the saved next ends the dispatch, because the reference then reads a freed node.
    pub fn fire_event(&mut self, event: &str, args: Vec<ScriptValue>) {
        fire_event_into(&self.lua, event, args);
    }
}

#[cfg(not(target_os = "macos"))]
#[path = "wenilla_listener.rs"]
mod wenilla_listener;

/// [`UiScript::fire_event`] against the VM directly, for a caller that holds `&Lua` rather than
/// `&UiScript` — which is every Lua binding, and therefore `UpdateSpells` (decision 1924).
///
/// Identical behaviour; `fire_event` is the same call with the script's own VM. The `&mut self` on
/// the method was never load-bearing — the body reaches everything through `lua.app_data_mut()` —
/// and this is the shape [`crate::loader::load_into`] already uses for the same reason (1188 §2).
pub(crate) fn fire_event_into(lua: &Lua, event: &str, args: Vec<ScriptValue>) {
    let model_mut = || lua.app_data_mut::<Model>().expect("model app_data set");
    {
        let mut at = {
            let mut model = model_mut();
            if let Some(tap) = model.event_tap.as_mut() {
                if tap.len() < EVENT_TAP_CAP {
                    tap.push((event.to_string(), args.clone()));
                }
            }
            model
                .event_to_frames
                .get(event)
                .and_then(|l| l.first().copied())
        };
        #[cfg(not(target_os = "macos"))]
        let mut next_index = 0;
        while let Some(h) = at {
            let mut model = model_mut();
            // A saved next that the previous handler unregistered ends the walk.
            let Some(pos) = model
                .event_to_frames
                .get(event)
                .and_then(|l| {
                    #[cfg(target_os = "macos")]
                    {
                        l.iter().position(|&x| x == h)
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        wenilla_listener::position(l, &h, next_index)
                    }
                })
            else {
                break;
            };
            let next = model
                .event_to_frames
                .get(event)
                .and_then(|l| l.get(pos + 1).copied());
            #[cfg(not(target_os = "macos"))]
            {
                next_index = pos + 1;
            }
            let id = model.frame_id(h);
            drop(model);
            if let Err(e) = event::fire_event_handler(lua, id, event, &args) {
                model_mut().record_script_error(e.to_string());
            }
            at = next;
        }
    }
    event::fire_all_event_listeners(lua, event, &args);
}

impl super::UiScript {
    /// The current `GetTime()` value in seconds; the app stamps absolute expiries with it, such as
    /// an aura's `expirationTime`.
    pub fn now(&self) -> f64 {
        self.lua.globals().get("__benilla_now").unwrap_or(0.0)
    }

    /// Start this VM's `GetTime()` clock at `secs`, so a rebuilt VM (a relog, a `ReloadUI`) keeps
    /// the process's clock. The reference's `GetTime` (`0x515ea0`, through `0x42c010` and
    /// `0x42b790`) is `GetTickCount` scaled by 0.001, an OS clock that never restarts, which stock
    /// `Cooldown.lua` relies on (`start > 0`). Set once, at construction; after that only
    /// [`Self::tick`] moves it.
    pub fn set_now(&mut self, secs: f64) {
        if let Err(e) = self.lua.globals().set("__benilla_now", secs) {
            self.push_error(e);
        }
    }

    /// Advance a frame: the `GetTime()` clock, the edit boxes and queued events, then
    /// `OnUpdate(self, elapsed)` on every visible frame that has one (`0x704f10`), then the
    /// engine's own fades, model panes and hover.
    pub fn tick(&mut self, elapsed: f32) {
        let clock = {
            let g = self.lua.globals();
            let now: f64 = g.get("__benilla_now").unwrap_or(0.0);
            g.set("__benilla_now", now + f64::from(elapsed))
        };
        if let Err(e) = clock {
            self.push_error(e);
        }
        // The focused edit box's caret blink (`0x77a790`, on the client's frame tick).
        editbox::tick_blink(&self.lua, elapsed);
        // Then `0x77a790`'s drain of the `OnTextChanged`s an edit only marked (`0x77a7a1`), before
        // the OnUpdate sweep, as in the reference.
        editbox::drain_text_changed(&self.lua);
        // Then the caret flush (`0x77d3e0` → `0x77da80`): `OnCursorChanged` when the caret moved,
        // which `ScrollingEdit_OnUpdate` scrolls by.
        editbox::drain_cursor_changed(&self.lua);
        // Events queued since the last tick fire before this tick's OnUpdate.
        let pending = std::mem::take(&mut self.model_mut().pending_events);
        for (event, args) in pending {
            self.fire_event(&event, args);
        }
        let ids: Vec<u32> = {
            let mut model = self.model_mut();
            // `SetScript` keeps the OnUpdate list; a destroyed frame's handle compacts out here.
            let frames: Vec<FrameHandle> = model
                .on_update_frames
                .iter()
                .copied()
                .filter(|&h| model.arena.frame(h).is_some_and(|f| f.effective_visible))
                .collect();
            if model
                .on_update_frames
                .iter()
                .any(|&h| model.arena.frame(h).is_none())
            {
                let arena = &model.arena;
                let live: Vec<FrameHandle> = model
                    .on_update_frames
                    .iter()
                    .copied()
                    .filter(|&h| arena.frame(h).is_some())
                    .collect();
                model.on_update_frames = live;
            }
            let mut ids: Vec<u32> = frames.into_iter().map(|h| model.frame_id(h)).collect();
            // Creation order, by frame id: getters settle on demand, so a handler sees what an
            // earlier one did this sweep, and the order must be stable. The reference walks each
            // strata level's list of shown frames (`0x765650`).
            ids.sort_unstable();
            ids
        };
        for id in ids {
            if let Err(e) = event::fire_update_handler(&self.lua, id, elapsed) {
                self.push_error(e);
            }
        }
        self.tick_model_panes(elapsed);
        // The engine's line fades, apart from any OnUpdate script: ScrollingMessageFrame's
        // (`0x788460`) and MessageFrame's (`0x786200`), which also keeps only the lines that fit
        // its height, so its rows are read from the resolved rect before the mutable walk.
        let mut model = self.model_mut();
        let ticked: Vec<FrameHandle> = model.arena.ticked_kinds().to_vec();
        let message_frames: Vec<(FrameHandle, usize)> = ticked
            .iter()
            .copied()
            .filter(|&h| {
                model
                    .arena
                    .frame(h)
                    .is_some_and(|f| matches!(f.kind_state, crate::widget::KindState::Message(_)))
            })
            .map(|h| (h, Self::message_viewport_rows(&model, h)))
            .collect();
        for (h, viewport_rows) in message_frames {
            if let Some(crate::widget::KindState::Message(mf)) =
                model.arena.frame_mut(h).map(|f| &mut f.kind_state)
            {
                mf.tick(elapsed);
                mf.trim_to_viewport(viewport_rows);
            }
        }
        for &h in &ticked {
            let Some(frame) = model.arena.frame_mut(h) else {
                continue;
            };
            if let crate::widget::KindState::ScrollingMessage(smf) = &mut frame.kind_state {
                smf.tick(elapsed);
            }
        }
        drop(model);
        // Tooltip fades: `FadeOut`'s ramp and the hide at its end.
        tooltip::tick_fades(&self.lua);
        // A frame hid or showed under a still cursor: re-run the hover walk at the saved position,
        // as the reference's pump tail does (`0x765650` → `0x7660d0`). The hidden frame's OnLeave
        // already fired, so only the new winner's `OnEnter` fires.
        let repick = {
            let mut model = self.model_mut();
            let due = model.hover_repick;
            model.hover_repick = false;
            due.then_some(model.cursor_pos)
        };
        if let Some((x, y)) = repick {
            self.mouse_move(x, y);
        }
        // `WOW_UI_HANDLERS=<secs>`: last, so its report covers everything this tick fired.
        self.report_handler_profile(elapsed);
    }
}

impl UiScript {
    /// The model panes' per-frame pass, for visible panes, in the reference's order: the pane's
    /// `OnUpdate` (`0x76d7f0`) advances its clock by `trunc(elapsed · 1000)` ms, the paint
    /// (`0x76d1a0`) fires `OnUpdateModel`, and the animate fires `OnAnimFinished` (`0x76cdc0`) once
    /// per arm when the armed sequence completes naturally, a loop's first pass included
    /// (`0x719370`). A hidden pane's clock stands still. Completion is read after `OnUpdateModel`,
    /// whose handler may re-arm.
    fn tick_model_panes(&mut self, elapsed: f32) {
        let dt_ms = (elapsed * 1000.0).trunc().max(0.0) as u64;
        let update_ids: Vec<u32> = {
            let mut model = self.model_mut();
            let ticked: Vec<FrameHandle> = model.arena.ticked_kinds().to_vec();
            for h in ticked {
                let Some(f) = model.arena.frame_mut(h) else {
                    continue;
                };
                if !f.effective_visible {
                    continue;
                }
                if let crate::widget::KindState::Model(m) = &mut f.kind_state {
                    m.clock_ms += dt_ms;
                }
            }
            // Kept by `SetScript`; a destroyed frame's handle compacts out on its first miss.
            if model
                .on_update_model_frames
                .iter()
                .any(|&h| model.arena.frame(h).is_none())
            {
                let arena = &model.arena;
                let live: Vec<FrameHandle> = model
                    .on_update_model_frames
                    .iter()
                    .copied()
                    .filter(|&h| arena.frame(h).is_some())
                    .collect();
                model.on_update_model_frames = live;
            }
            // Only visible Model panes with a file set, resident or streaming: the paint that fires
            // the handler (`0x76d1bc`) is gated on `[widget+0x318] ≠ 0` (`0x76d24c`). On any other
            // kind the script is inert.
            let frames: Vec<FrameHandle> = model
                .on_update_model_frames
                .iter()
                .copied()
                .filter(|&h| {
                    model.arena.frame(h).is_some_and(|f| {
                        f.effective_visible
                            && matches!(&f.kind_state, crate::widget::KindState::Model(m) if m.path.is_some())
                    })
                })
                .collect();
            let mut ids: Vec<u32> = frames.into_iter().map(|h| model.frame_id(h)).collect();
            ids.sort_unstable(); // creation order, as in the OnUpdate sweep
            ids
        };
        for id in update_ids {
            if let Err(e) = event::fire_widget_handler(&self.lua, id, "OnUpdateModel", Vec::new()) {
                self.push_error(e);
            }
        }
        let finished_ids: Vec<u32> = {
            let mut model = self.model_mut();
            let ticked: Vec<FrameHandle> = model.arena.ticked_kinds().to_vec();
            let mut due: Vec<FrameHandle> = Vec::new();
            for h in ticked {
                let Some(f) = model.arena.frame(h) else {
                    continue;
                };
                if !f.effective_visible {
                    continue;
                }
                let crate::widget::KindState::Model(m) = &f.kind_state else {
                    continue;
                };
                let Some(path) = m.path.as_deref() else {
                    continue;
                };
                let Some(facts) = model.model_facts.get(&crate::widget::model_key(path)) else {
                    continue;
                };
                if m.completion_due(facts) {
                    due.push(h);
                }
            }
            let mut ids = Vec::with_capacity(due.len());
            for h in due {
                if let Some(crate::widget::KindState::Model(m)) =
                    model.arena.frame_mut(h).map(|f| &mut f.kind_state)
                {
                    if let Some(a) = &mut m.armed {
                        a.finished = true;
                    }
                }
                ids.push(model.frame_id(h));
            }
            ids.sort_unstable();
            ids
        };
        for id in finished_ids {
            if let Err(e) = event::fire_widget_handler(&self.lua, id, "OnAnimFinished", Vec::new())
            {
                self.push_error(e);
            }
        }
    }
}
