//! wenilla carry: **every system's own wall time, by name, in the browser** - the `# sysus` line.
//!
//! The journal's tiles and `px_*` brackets are spans of the schedule, and a span holds whatever
//! systems float into it: `px_unitfeed` read 2.3 ms in journal 2026-10-17T183906 while the one
//! system it is named after (`fu_us`) read microseconds. bevy's own per-system spans need its
//! `trace` feature, which the workspace `Cargo.toml` turned off for costing 10x the frame.
//!
//! This times the systems themselves instead. At `cleanup` every system already in the main
//! world's and the render world's schedules is swapped, in its graph slot, for a [`Timed`] wrapper
//! that delegates every `System` method to it and reads the clock around
//! `run_without_applying_deferred` (the single-threaded executor's call,
//! `bevy_ecs-0.18.1/src/schedule/executor/single_threaded.rs:142`) and the other two run entry
//! points. `type_id` is delegated too: the executor recognises a sync point by it
//! (`is_apply_deferred`), and a wrapper answering its own would turn every `ApplyDeferred` into a
//! plain system that applies nothing. Systems added after `cleanup` are not wrapped.
//!
//! Disarmed it costs one relaxed load per system run. Armed (the journal's switch) it is two
//! `performance.now()` reads per run, whose browser resolution is 5-100 us: a single run is
//! quantised, the per-second sum over thousands of runs is not biased by it.

use std::any::TypeId;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bevy::ecs::change_detection::{CheckChangeTicks, Tick};
use bevy::ecs::query::FilteredAccessSet;
use bevy::ecs::schedule::{InternedSystemSet, Schedules};
use bevy::ecs::system::{RunSystemError, ScheduleSystem, SystemIn, SystemParamValidationError, SystemStateFlags};
use bevy::ecs::world::unsafe_world_cell::UnsafeWorldCell;
use bevy::ecs::world::DeferredWorld;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy::utils::prelude::DebugName;

static ARMED: AtomicBool = AtomicBool::new(false);

/// Every wrapped system: its name and accumulated nanoseconds since the last drain.
static REGISTRY: Mutex<Vec<(String, Arc<AtomicU64>)>> = Mutex::new(Vec::new());

/// Rides the journal switch.
pub(crate) fn arm(on: bool) {
    ARMED.store(on, Ordering::Relaxed);
}

/// The costliest `n` systems since the last call, nanoseconds each, and a reset.
pub(crate) fn take_top(n: usize) -> Vec<(String, u64)> {
    let Ok(reg) = REGISTRY.lock() else {
        return Vec::new();
    };
    let mut rows: Vec<(String, u64)> = reg
        .iter()
        .map(|(name, ns)| (name.clone(), ns.swap(0, Ordering::Relaxed)))
        .filter(|(_, ns)| *ns > 0)
        .collect();
    // Two systems can share a name (one function added twice): fold them.
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.dedup_by(|b, a| {
        if a.0 == b.0 {
            a.1 += b.1;
            true
        } else {
            false
        }
    });
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    rows.truncate(n);
    rows
}

struct Timed {
    inner: ScheduleSystem,
    ns: Arc<AtomicU64>,
}

impl Timed {
    #[inline]
    fn clock<R>(&mut self, f: impl FnOnce(&mut ScheduleSystem) -> R) -> R {
        if !ARMED.load(Ordering::Relaxed) {
            return f(&mut self.inner);
        }
        let t = Instant::now();
        let r = f(&mut self.inner);
        self.ns
            .fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        r
    }
}

impl System for Timed {
    type In = ();
    type Out = ();

    fn name(&self) -> DebugName {
        self.inner.name()
    }
    fn type_id(&self) -> TypeId {
        self.inner.type_id()
    }
    fn flags(&self) -> SystemStateFlags {
        self.inner.flags()
    }
    unsafe fn run_unsafe(
        &mut self,
        input: SystemIn<'_, Self>,
        world: UnsafeWorldCell,
    ) -> Result<(), RunSystemError> {
        // SAFETY: forwarded verbatim; the caller upholds `run_unsafe`'s contract.
        self.clock(|s| unsafe { s.run_unsafe(input, world) })
    }
    fn run(&mut self, input: SystemIn<'_, Self>, world: &mut World) -> Result<(), RunSystemError> {
        self.clock(|s| s.run(input, world))
    }
    fn run_without_applying_deferred(
        &mut self,
        input: SystemIn<'_, Self>,
        world: &mut World,
    ) -> Result<(), RunSystemError> {
        self.clock(|s| s.run_without_applying_deferred(input, world))
    }
    fn apply_deferred(&mut self, world: &mut World) {
        self.inner.apply_deferred(world);
    }
    fn queue_deferred(&mut self, world: DeferredWorld) {
        self.inner.queue_deferred(world);
    }
    unsafe fn validate_param_unsafe(
        &mut self,
        world: UnsafeWorldCell,
    ) -> Result<(), SystemParamValidationError> {
        // SAFETY: forwarded verbatim.
        unsafe { self.inner.validate_param_unsafe(world) }
    }
    fn initialize(&mut self, world: &mut World) -> FilteredAccessSet {
        self.inner.initialize(world)
    }
    fn check_change_tick(&mut self, check: CheckChangeTicks) {
        self.inner.check_change_tick(check);
    }
    fn default_system_sets(&self) -> Vec<InternedSystemSet> {
        self.inner.default_system_sets()
    }
    fn get_last_run(&self) -> Tick {
        self.inner.get_last_run()
    }
    fn set_last_run(&mut self, last_run: Tick) {
        self.inner.set_last_run(last_run);
    }
}

/// Swap every system of every schedule in `world` for its [`Timed`] wrapper; `prefix` names the
/// world in the output ("" for the main world, "R:" for the render world).
fn wrap_world(world: &mut World, prefix: &str) -> usize {
    let Some(mut schedules) = world.get_resource_mut::<Schedules>() else {
        return 0;
    };
    let mut reg = REGISTRY.lock().expect("REGISTRY is never poisoned");
    let mut n = 0;
    for (_label, schedule) in schedules.iter_mut() {
        let graph = schedule.graph_mut();
        let keys: Vec<_> = graph.systems.iter().map(|(k, _, _)| k).collect();
        for key in keys {
            let Some(slot) = graph.systems.get_mut(key) else {
                continue;
            };
            let name = format!("{prefix}{}", slot.system.name());
            let short = name.rsplit("::").next().unwrap_or(&name).to_string();
            let placeholder: ScheduleSystem = Box::new(IntoSystem::into_system(|| {}));
            let inner = std::mem::replace(&mut slot.system, placeholder);
            let ns = Arc::new(AtomicU64::new(0));
            reg.push((format!("{prefix}{short}"), ns.clone()));
            slot.system = Box::new(Timed { inner, ns });
            n += 1;
        }
    }
    n
}

/// Wraps at `cleanup`, after every plugin's `build` and `finish` have added their systems.
pub(crate) struct SysTimePlugin;

impl Plugin for SysTimePlugin {
    fn build(&self, _app: &mut App) {}

    fn cleanup(&self, app: &mut App) {
        let main = wrap_world(app.world_mut(), "");
        let render = app
            .get_sub_app_mut(bevy::render::RenderApp)
            .map_or(0, |r| wrap_world(r.world_mut(), "R:"));
        info!("systime: {main} main-world and {render} render-world systems wrapped");
    }
}
