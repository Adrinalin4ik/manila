//! **Every system in the schedule, timed, in one capture** — the end of the guess-and-rebuild loop.
//!
//! Finding where a 74 ms frame goes took five rounds of the same shape: pick a suspect, add a
//! column for it, rebuild, have the owner play a hundred and forty seconds, read one number, be
//! wrong, pick the next suspect. Five builds to narrow "the frame is slow" to "PostUpdate", and
//! the span that was supposed to close it turned out to measure a region of the schedule rather
//! than the set it was named after. The loop was the defect, not the answers.
//!
//! bevy already emits what is needed and has from the start: every system run is wrapped in
//! `info_span!("system", name = …)` (`bevy_ecs-0.18.1/src/system/function_system.rs:52,658`),
//! behind its `trace` feature. So the whole question — *which* system, out of all of them, by
//! name, to the microsecond — needs no new instrument per suspect. It needs one listener.
//!
//! **Armed, not always on.** The spans are `info` level and the client's subscriber wants `info`,
//! so leaving this enabled would time every system of every frame for a player who asked for
//! nothing. [`arm`] flips it and rebuilds tracing's interest cache, which is what makes the
//! callsites cheap again when it is off: a disabled span costs an atomic read and a branch.
//!
//! Read with [`take_top`], which drains. The FPS journal calls it once a second and writes the
//! costliest systems beside the row they belong to.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use bevy::log::tracing::field::{Field, Visit};
use bevy::log::tracing::span::{Attributes, Id};
use bevy::log::tracing::Subscriber;
use bevy::log::tracing_subscriber::layer::Context;
use bevy::log::tracing_subscriber::registry::LookupSpan;
use bevy::log::tracing_subscriber::Layer;
use bevy::platform::time::Instant;

static ARMED: AtomicBool = AtomicBool::new(false);
/// Name → microseconds accumulated since the last drain.
static TOTALS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

/// Turn the profiler on or off. Rebuilds tracing's interest cache so the change reaches callsites
/// that were already asked about — without it, spans decided at startup stay decided.
pub fn arm(on: bool) {
    ARMED.store(on, Ordering::Relaxed);
    // One line, so "the profiler produced nothing" can be told from "the profiler never ran" -
    // which is exactly the ambiguity that cost the first build of this.
    bevy::log::tracing::info!("sysprof {}", if on { "armed" } else { "off" });
}

/// The costliest `n` systems since the last call, microseconds each, and a reset. Empty when the
/// profiler is off or nothing ran.
pub fn take_top(n: usize) -> Vec<(String, u64)> {
    let Ok(mut guard) = TOTALS.lock() else {
        return Vec::new();
    };
    let Some(map) = guard.take() else {
        return Vec::new();
    };
    let mut rows: Vec<(String, u64)> = map.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    rows.truncate(n);
    rows
}

/// Pulls `name` out of the span's fields. bevy records it as a `Display` value, not a `&str`, so
/// `record_str` never fires and `record_debug` is the one that does.
#[derive(Default)]
struct NameOf(Option<String>);

impl Visit for NameOf {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "name" {
            self.0 = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "name" && self.0.is_none() {
            self.0 = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
}

/// Per-span state: the name, resolved once at creation, and the instant of the current entry.
struct Timing {
    name: String,
    entered: Option<Instant>,
}

pub struct SystemProfiler;

impl<S> Layer<S> for SystemProfiler
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    // **No `enabled` override, deliberately.** The first version had one, returning false for
    // everything but the system spans - and in `tracing_subscriber` a layer's `enabled` is
    // ANDed across the stack, so a `false` here does not mean "this layer is not interested",
    // it means "nobody sees this event". That build shipped a profiler that recorded nothing and
    // may have silenced the client's own log with it. Interest is not a per-layer opinion; the
    // filtering belongs in the callbacks, where it only decides this layer's own work.
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if attrs.metadata().name() != "system" || !ARMED.load(Ordering::Relaxed) {
            return;
        }
        let mut name = NameOf::default();
        attrs.record(&mut name);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Timing {
                name: name.0.unwrap_or_else(|| "?".to_string()),
                entered: None,
            });
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if let Some(t) = span.extensions_mut().get_mut::<Timing>() {
                t.entered = Some(Instant::now());
            }
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut ext = span.extensions_mut();
        let Some(t) = ext.get_mut::<Timing>() else {
            return;
        };
        let Some(started) = t.entered.take() else {
            return;
        };
        let us = started.elapsed().as_micros() as u64;
        let name = t.name.clone();
        drop(ext);
        if let Ok(mut guard) = TOTALS.lock() {
            *guard.get_or_insert_with(HashMap::new).entry(name).or_insert(0) += us;
        }
    }
}
