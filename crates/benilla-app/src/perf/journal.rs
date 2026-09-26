//! The FPS journal (`/console fpsJournal 1` in any build, or `WOW_FPS_JOURNAL=<csv path>`): once
//! a second, one row of where the player is, what the frame cost on the wall, the CPU and the
//! GPU, and what is resident ([`JOURNAL_HEADER`]). It ships in the player build so a player on any
//! hardware can record a run; the file is `benilla-config/Diagnostics/fps-journal.csv`, opened
//! with a `#` line naming the adapter, the backend and whether the device can time passes.
//!
//! **Player-facing, so it lives outside the `dev` seam** (2008, on 1495's precedent): the people
//! whose frames we need to read run the player build, which compiles every other instrument out
//! (1173). The CVar's knob is [`FpsJournalSetting`]; the file is
//! `benilla-config/Diagnostics/fps-journal.csv` ([`crate::local_state::fps_journal_path`]), the
//! thing a reporter attaches. `WOW_FPS_JOURNAL` names another path and turns the journal on for
//! the run regardless of the CVar — the harness lever it always was. A fresh file opens with one
//! `#` line naming the adapter, the backend and whether the device can time passes, so a journal
//! from a machine we have never seen says what it was read on.
//!
//! **The GPU columns read bevy's render diagnostics** (`RenderDiagnosticsPlugin`, registered here
//! so it is present in every build): one timestamp pair per render pass, resolved a few frames
//! later into `render/<pass>/elapsed_gpu` measurements. THREE features decide whether any of that
//! happens, and they are not interchangeable. `TIMESTAMP_QUERY` builds the query set
//! (`bevy_render-0.18.1/src/diagnostic/internal.rs:205`). `TIMESTAMP_QUERY_INSIDE_ENCODERS` is
//! what every span actually rides: bevy takes all of them through `encoder.write_timestamp`, and
//! its `write_timestamp` returns `None` at the first line without that feature (ibid. 285, its
//! comment: "unsupported on WebGPU"). `TIMESTAMP_QUERY_INSIDE_PASSES` gates only spans opened
//! *within* a pass, which no column here uses. This file once tested the first AND the third and
//! printed one `gpu_spans` from it - so it read `no` on a browser that has `timestamp-query`, and
//! the one feature that was really missing was never named at all. The preamble now prints all
//! three. An Apple GPU samples counters only at stage boundaries (the `perf` module header); a
//! browser has no encoder timestamps whatever, so bevy's spans cannot exist there and our own
//! whole-frame meter (`perf::gpu`) is blocked on the same feature. Where nothing was read the
//! columns stay EMPTY rather than zero, so "no reading" cannot be mistaken for "free".
//! Our own passes (`static_gx`, the `ffx_glow` chain, `ui_gamma_decode`) open spans of their own,
//! because a city's biggest draw must not land in `gpu_other`. The buckets are [`gpu_bucket`]; a
//! pass this file does not name is still counted, under `gpu_other`, and `gpu_ms` is the sum of
//! every pass — the GPU's busy time inside passes, which is what "GPU-bound" reads against the
//! wall `mean_ms` beside it.
//!
//! The residency columns make it the **leak curve** instrument too (B131): `FPS_PROBE`'s residency
//! meter samples once per run, which can only compare two runs at one point each — it cannot tell
//! "grows with distance streamed" from "grows with time elapsed", and cannot show *where* on a
//! route the cost arrives. A per-second row of `cpu_ms` beside `mats/images/uv/tint` plots the
//! per-frame cost directly against residency along one continuous leg, on the same time axis as
//! the position — so a same-map traverse (no `MapChange`, so no map-scoped eviction) shows its
//! accumulation as a slope instead of a before/after pair.

use std::path::PathBuf;
// `std::time::Instant` panics on wasm32 (no monotonic clock behind it); bevy's re-export is
// `web_time` there and std's everywhere else. Same carry as `net.rs` and `items.rs`.
use bevy::platform::time::Instant;

use std::fmt::Write as _;

use bevy::diagnostic::DiagnosticsStore;
use bevy::prelude::*;
use bevy::render::diagnostic::RenderDiagnosticsPlugin;
use bevy::render::renderer::{RenderAdapterInfo, RenderDevice};
use bevy::time::Real;

use super::clock::{main_thread_cpu_secs, process_cpu_secs};

#[cfg(target_arch = "wasm32")]
#[path = "journal_web.rs"]
mod web;

pub(crate) struct FpsJournalPlugin;

/// The `fpsJournal` CVar: on, the journal appends from the next second; off, it stops and the
/// file keeps what it has.
#[derive(Resource, Default)]
pub(crate) struct FpsJournalSetting(pub(crate) bool);

/// The column order, written once into a fresh file after the `#` adapter line. Columns only
/// ever grow at the end, so an older journal still parses against its own header.
const JOURNAL_HEADER: &str = "t,x,y,z,mean_ms,p95_ms,streamed,entities,cpu_ms,mats,meshes,images,\
                              m2,uv,tint,pmat,emat,skin,cmat,tex,cgeo,evicted,fx,fy,fz,main_ms,\
                              gpu_ms,gpu_opaque,gpu_static,gpu_transp,gpu_glow,gpu_post,gpu_ui,\
                              gpu_other,lua_errs,lua_err_us,msg_hashed,ui_us,col_us,emitters,fx_kits,fx_impacts,net_pkts,net_us,pipes,\
                              rscale,farclip,\
                              skins_new,skin_us,tex_hit,tex_dec,\
                              rcpu_ms,rcpu_opaque,rcpu_static,rcpu_transp,rcpu_glow,rcpu_post,rcpu_ui,rcpu_other,sched_us,s_first,s_pre,s_upd,s_post,s_last,\
                              u_net,u_input,u_stream,p_pre,p_xform,p_cull,p_vis,moved,\
                              t_stream,t_furnish,t_mfurnish,t_spawn,t_collider,rig_wr,rig_sk,rapp,r_extract,r_assets,r_queue,r_sort,r_prepare,r_render,r_clean,drop_chat,drop_other,tex_big,rd_hit,rd_miss,rd_kb,rd_big\n";

/// The FPS journal switch's change callback (2008, 2303): a flag, the client's int-parse +
/// `!= 0`. The journal system reads the knob every frame, so the file opens on the next second
/// and closes the second it is turned off.
pub(crate) fn on_cvar(
    ev: On<crate::cvars::CvarChanged>,
    mut journal: ResMut<FpsJournalSetting>,
    mut ui_cost: ResMut<crate::ui_script::UiCostWanted>,
    mut commands: Commands,
) {
    // `/console archCensus 1` - one archetype dump to the console. Run through
    // `run_system_cached` rather than registered in a schedule, so the instrument costs a player
    // who never asks for it exactly nothing (`crate::perf::arch`).
    if ev.is("skinCacheMb") {
        let mb = benilla_formats::set_skin_cache_mb(ev.num() as usize);
        info!("skin decode cache: {mb} MiB");
    }
    if ev.is("archCensus") && ev.flag() {
        commands.run_system_cached(crate::perf::arch::arch_census);
        // Disarm, because every row here persists. Left at "1" the value would survive the
        // session, and the next boot would either dump a census of a world that does not exist
        // yet or - worse - refuse the next `archCensus 1` as a no-op write, which reads exactly
        // like a broken instrument. `mirror` rather than `set`: it moves the row without firing
        // an observer, so this cannot re-enter itself (`cvars.rs:1456-1462`).
        commands.queue(|world: &mut World| {
            world
                .resource_mut::<crate::cvars::Cvars>()
                .mirror("archCensus", "0");
        });
    }
    if ev.is("fpsJournal") {
        journal.0 = ev.flag();
        // **Arm the UI cost meter with the journal, and never disarm it.** `ui_us` read a flat
        // zero for two runs because the UI pass's `lap()` returns 0 unless something asked for the
        // meter — its own comment says so six lines above the values I was summing. Turning it on
        // here is what makes the column a measurement rather than a shape.
        //
        // One-way on purpose: the hover recorder and the book probe ask for the same flag, and a
        // journal switched off has no business cancelling their request. The meter's cost is a
        // handful of clock reads per frame, which is the wrong thing to be frugal about while
        // somebody is recording a journal to find a stall.
        if journal.0 {
            ui_cost.0 = true;
        }
        // The per-system profiler rides the journal switch: far too expensive to leave on for a
        // player who asked for nothing, and exactly what a player who turned the journal on is
        // asking for.
        benilla_world::sysprof::arm(journal.0);
    }
}

/// **The main schedule's own wall time**, summed over the second and divided by its frames - the
/// `sched_us` column.
///
/// The frame is 85 ms and everything this file measures accounts for ten of them: the render
/// graph's whole CPU side is 4.4 ms (`rcpu_ms`), the UI pass under four, composites about two per
/// cent, Lua and colliders nothing. Meanwhile the browser says the main thread is busy 94% of the
/// wall. Work that large, on that thread, invisible to every column here, has three places left
/// to be: the main schedule (game logic over 32 000 entities), the render app's extract and
/// prepare (the same 32 000 turned into draw data), and the blocking wait on the GPU at submit.
///
/// This pair splits the first from the other two. `sched_us` is the main schedule end to end;
/// `mean_ms` minus it is everything else together. One number decides which half to open.
static SCHED_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SCHED_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Stamped at the top of `First`, read at the bottom of `Last`.
#[derive(Resource)]
struct SchedStart(Instant);

impl Default for SchedStart {
    fn default() -> Self {
        Self(Instant::now())
    }
}

fn sched_open(mut start: ResMut<SchedStart>) {
    start.0 = Instant::now();
}

fn sched_close(start: Res<SchedStart>) {
    use std::sync::atomic::Ordering::Relaxed;
    SCHED_US.fetch_add(start.0.elapsed().as_micros() as u64, Relaxed);
    SCHED_FRAMES.fetch_add(1, Relaxed);
}

/// Microseconds of main schedule per frame this second, and the reset. `None` when no frame ran.
fn take_sched_frames() -> u64 {
    SCHED_FRAMES.swap(0, std::sync::atomic::Ordering::Relaxed)
}

fn take_sched_us(frames: u64) -> Option<u64> {
    let us = SCHED_US.swap(0, std::sync::atomic::Ordering::Relaxed);
    (frames > 0).then(|| us / frames)
}

/// **Which part of the main schedule**, to microseconds - the `s_first`..`s_last` columns, then
/// the seven that cut open the two phases those named.
///
/// **The twelve are DISJOINT and together they tile the frame; none of them contains another.**
/// Read that before reading a number off them, because the five `s_*` names invite the opposite
/// reading and it cost two rounds here. Every mark closes the span since *the previous mark* and
/// advances one shared [`PhaseClock`], so splicing the seven inner marks did not subdivide the
/// five - it SHORTENED them. `s_upd` is not `Update`; it is what is left of `Update` after the
/// Stream stage, once `u_net`, `u_input` and `u_stream` have taken their pieces. `s_post` is
/// likewise only the tail of `PostUpdate` after visibility. The header carried "the sum of the
/// five is the FRAME" from when five was all there was, and it stayed true-looking while being
/// false: on journal 33 those five sum to 13.03 ms of a 40.6 ms frame, and `s_upd` (5.09) came
/// out SMALLER than the three marks supposedly inside it (16.27). A part larger than its whole is
/// the tell; there is no nesting.
///
/// To read a real phase, ADD its tiles:
///   `Update`     = `u_net` + `u_input` + `u_stream` + `s_upd`
///   `PostUpdate` = `p_pre` + `p_xform` + `p_cull` + `p_vis` + `s_post`
/// and all twelve sum to the frame. Journal 33, steady state, 40.6 ms:
/// `Update` 21.36 (53%), `PostUpdate` 10.76 (27%), `s_first` 6.99 (17%), the rest 0.45.
///
/// **`s_first` was not `First`, and now it is.** Its mark closed only when the next frame opened,
/// with the render app running in between, so the column carried extract, prepare, submit and
/// present along with `First` itself. That is now split: a thirteenth mark sits BEFORE `First`
/// (the frame's first label), so `rapp` is the between-schedules span and `s_first` is `First`
/// alone.
///
/// The split was worth its two clock reads because the merged column behaved impossibly: 7.16 ms
/// at 37,330 entities and 8.45 ms at 20,179 - it grew as the scene SHRANK - and a fit across four
/// journals put a fixed 20.79 ms in the frame that no entity count explains
/// (`frame = 20.79 ms + 0.519 us x entities`, residuals inside the 1.4 ms that two journals of the
/// same pin differ by). Something in that floor is why 60 fps is unreachable here whatever is
/// culled, and this column is the first cut at whether half of `s_first` holds it.
///
/// `sched_us` answered the first question and made this the only one left: 50.10 ms of a 65.68 ms
/// frame is the main schedule, 76 per cent, against 1.34 ms for the whole render graph and 3.50
/// for the UI pass. The work is game logic over 37 166 entities.
///
/// The five phase boundaries are exact rather than approximate because they are
/// **their own schedules**, spliced into [`MainScheduleOrder`] between the stock ones: a system
/// inside a schedule has no guaranteed position within it, but a schedule inserted after `Update`
/// runs after every system of `Update` and before every system of `PostUpdate`, by construction.
#[allow(clippy::declare_interior_mutable_const, reason = "an array of zeroed atomics")]
const ZERO: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PHASE_US: [std::sync::atomic::AtomicU64; PHASES] = [ZERO; PHASES];

/// Thirteen tiles: five phase boundaries, three more inside `Update`, four inside `PostUpdate`,
/// and one BEFORE `First` that cuts `s_first` in half (slot 12, the `rapp` column).
/// Disjoint, not nested - see the note above before adding any two of them together.
const PHASES: usize = 13;
/// The slot the `rapp` column reads, emitted apart from the other twelve so the header keeps its
/// append-only rule.
const RAPP: usize = 12;

/// **Inside `rapp`** — the seven `r_*` columns, the same trick one schedule down.
///
/// `rapp` is 18.93 ms of a 67.18 ms frame and the only third of it nothing could name: the render
/// graph's own CPU (`rcpu_ms`, bevy's diagnostic) accounts for 5.23, leaving 13.7 in extract,
/// prepare, queue and present. It matters because the other two thirds are already named and
/// neither can leave the main thread: bevy disables `multi_threaded` on wasm32 in its own cfg
/// (`bevy_tasks/src/lib.rs:21`, `bevy_ecs/schedule/executor/mod.rs:56`), so 94% of this frame is
/// work a worker cannot take and a fork is not on the table.
///
/// Same rule as the twelve above: DISJOINT tiles, each closing the span since the previous mark,
/// one shared clock. The open sits before `ExtractCommands` and accumulates nothing. What the
/// seven do NOT cover is `ExtractSchedule` (it runs in the main world, before this schedule) and
/// present (after it) - `rapp` minus their sum is those two together.
///
/// Slot order, which is also the header's order: `r_extract` (ExtractCommands), `r_assets`
/// (PrepareAssets + PrepareMeshes), `r_queue` (ManageViews + Queue + QueueMeshes + QueueSweep),
/// `r_sort` (PhaseSort), `r_prepare` (Prepare + PrepareResources + PrepareBindGroups),
/// `r_render` (Render), `r_clean` (Cleanup). Written positionally rather than from a name array,
/// because a second list of names is a second place for the header to drift from.
const RPHASES: usize = 7;

static RAPP_US: [std::sync::atomic::AtomicU64; RPHASES] = [ZERO; RPHASES];

/// The render app's own boundary clock — its schedule runs after the main one, so it cannot share
/// [`PhaseClock`], which lives in a different `World`.
#[derive(Resource)]
struct RClock(Instant);

impl Default for RClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

/// Start the render app's chain; accumulates nothing.
fn rmark_open(mut clock: ResMut<RClock>) {
    clock.0 = Instant::now();
}

/// Close render-app tile `N`.
fn rmark<const N: usize>(mut clock: ResMut<RClock>) {
    use std::sync::atomic::Ordering::Relaxed;
    let now = Instant::now();
    RAPP_US[N].fetch_add((now - clock.0).as_micros() as u64, Relaxed);
    clock.0 = now;
}

/// Per-frame microseconds for each render-app tile this second, and the reset.
fn take_rapp_us(frames: u64) -> [u64; RPHASES] {
    use std::sync::atomic::Ordering::Relaxed;
    let mut out = [0u64; RPHASES];
    for (slot, cell) in out.iter_mut().zip(&RAPP_US) {
        let us = cell.swap(0, Relaxed);
        *slot = if frames > 0 { us / frames } else { 0 };
    }
    out
}

/// The phase boundary schedules, in order. Each holds one system.
#[derive(bevy::ecs::schedule::ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
struct PhaseMark(u8);

/// The previous boundary's instant, so each mark closes one phase and opens the next.
#[derive(Resource)]
struct PhaseClock(Instant);

impl Default for PhaseClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

/// Close phase `n`: the time since the last mark belongs to it.
fn phase_mark<const N: usize>(mut clock: ResMut<PhaseClock>) {
    use std::sync::atomic::Ordering::Relaxed;
    let now = Instant::now();
    PHASE_US[N].fetch_add((now - clock.0).as_micros() as u64, Relaxed);
    clock.0 = now;
}

/// Per-frame microseconds for each phase this second, and the reset.
fn take_phase_us(frames: u64) -> [u64; PHASES] {
    use std::sync::atomic::Ordering::Relaxed;
    let mut out = [0u64; PHASES];
    for (slot, cell) in out.iter_mut().zip(&PHASE_US) {
        let us = cell.swap(0, Relaxed);
        *slot = if frames > 0 { us / frames } else { 0 };
    }
    out
}

/// **How many entities had their `Transform` written this frame** - the `moved` column, against
/// `entities` beside it.
///
/// The premise this tests is bevy's, not ours: it tracks unchanged subtrees and skips them during
/// propagation, then stops tracking at all once more than 30% of entities moved in a frame
/// (`bevy_transform-0.18.1/src/systems.rs:44-59`). A city where almost nothing moves should be far
/// under that line and should be getting the skip for free.
///
/// Worth a column rather than an argument because the span that was supposed to answer it did not:
/// `p_xform` sat at 12.0-13.5 ms while `entities` went 19 706 -> 34 173, a 73% rise for a 5% cost.
/// Work that does not scale with the thing it is supposed to walk is not that work. This counts
/// the input directly instead of inferring it from a timing.
static MOVED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MOVED_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Runs at the very top of `Last`, where every write of the frame has landed and bevy's own
/// change ticks have not yet rolled over.
fn count_moved(moved: Query<(), Changed<Transform>>) {
    use std::sync::atomic::Ordering::Relaxed;
    MOVED.fetch_add(moved.iter().count() as u64, Relaxed);
    MOVED_FRAMES.fetch_add(1, Relaxed);
}

fn take_moved() -> u64 {
    use std::sync::atomic::Ordering::Relaxed;
    let frames = MOVED_FRAMES.swap(0, Relaxed);
    let moved = MOVED.swap(0, Relaxed);
    if frames > 0 {
        moved / frames
    } else {
        0
    }
}

/// **The streamer's own five timers**, per frame - the `t_*` columns.
///
/// `u_stream` measured 6.94 ms of a 42.91 ms frame standing perfectly still, which is the one
/// number in the whole breakdown that looks wrong rather than expensive: terrain residency with
/// nobody moving should be finding nothing to do. The engine has always timed its own chain
/// (`StreamActivity`'s five `_ms` fields), but the only reader was `WOW_STREAM_TRACE`, which is
/// an env var and a file path - neither of which exists in a browser. So the numbers were being
/// taken every frame and dropped on the floor on the one target that needed them.
///
/// Fed from `trace_stream`, which already consumes the resource per frame by contract.
static STREAM_US: [std::sync::atomic::AtomicU64; 5] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Called once a frame with the chain's own split, in milliseconds as the streamer keeps them.
pub(crate) fn note_stream_ms(parts: [f32; 5]) {
    use std::sync::atomic::Ordering::Relaxed;
    for (cell, ms) in STREAM_US.iter().zip(parts) {
        cell.fetch_add((ms * 1000.0) as u64, Relaxed);
    }
}

fn take_stream_us(frames: u64) -> [u64; 5] {
    use std::sync::atomic::Ordering::Relaxed;
    let mut out = [0u64; 5];
    for (slot, cell) in out.iter_mut().zip(&STREAM_US) {
        let us = cell.swap(0, Relaxed);
        *slot = if frames > 0 { us / frames } else { 0 };
    }
    out
}

impl Plugin for FpsJournalPlugin {
    fn build(&self, app: &mut App) {
        // bevy's per-pass render diagnostics — the source of the GPU columns, and (under the
        // `tracy` feature) the hook Tracy's GPU zones ride. Present in every build: its per-frame
        // cost is one query resolve and one buffer map on the render thread, and a player's
        // journal is exactly the build that has to carry it (2008).
        {
            // Spliced rather than added inside the stock schedules: a system has no guaranteed
            // position within a schedule, but a schedule inserted after `Update` runs after every
            // system of `Update`, by construction. That is the whole reason these boundaries can
            // be trusted to the microsecond.
            let mut order = app.world_mut().resource_mut::<bevy::app::MainScheduleOrder>();
            // **Before `First`, which is the frame's first label** - so this mark closes the
            // span since the mark after `Last`, i.e. everything that happens between two main
            // schedules: the render sub-app (extract, prepare, queue, render, present), the
            // event-loop hop, and any wait at present. `s_first` used to carry all of that plus
            // `First` itself, which is why its own doc said "the name is simply narrower than the
            // thing" - it read 7.16 ms at 37k entities and 8.45 ms at 20k, growing as the scene
            // SHRANK, and nothing could say which half did that. Now `rapp` is that span and
            // `s_first` is `First` alone.
            //
            // The span is wall time, so law 0717 applies to it: while synced it measures the
            // display's present grant, not our cost. It is honest at the frame rates this client
            // runs at (31-41 ms against a 16.7 ms refresh, so present is not what is waiting),
            // and it would stop being honest the day the frame fits in the interval.
            order.insert_before(bevy::app::First, PhaseMark(RAPP as u8));
            order.insert_after(bevy::app::First, PhaseMark(0));
            order.insert_after(bevy::app::PreUpdate, PhaseMark(1));
            order.insert_after(bevy::app::Update, PhaseMark(2));
            order.insert_after(bevy::app::PostUpdate, PhaseMark(3));
            order.insert_after(bevy::app::Last, PhaseMark(4));
        }
        // **The render app's own tiles.** Pinned between `RenderSystems` sets exactly as the
        // main-schedule marks are pinned between schedules: `.after(X).before(Y)` brackets each
        // one on both sides, so a system that opts into neither set can still float into a tile,
        // and the tile is an upper bound on its set rather than a measurement of it. That is the
        // same caveat the `u_*` tiles carry, and it is worth saying out loud because the `u_*`
        // ones were read as stage measurements for two rounds and were not.
        if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
            use bevy::render::{Render, RenderSystems as RS};
            render_app.init_resource::<RClock>().add_systems(
                Render,
                (
                    rmark_open.before(RS::ExtractCommands),
                    rmark::<0>.after(RS::ExtractCommands).before(RS::PrepareAssets),
                    rmark::<1>.after(RS::PrepareMeshes).before(RS::ManageViews),
                    rmark::<2>.after(RS::QueueSweep).before(RS::PhaseSort),
                    rmark::<3>.after(RS::PhaseSort).before(RS::Prepare),
                    rmark::<4>.after(RS::PrepareBindGroups).before(RS::Render),
                    rmark::<5>.after(RS::Render).before(RS::Cleanup),
                    rmark::<6>.after(RS::Cleanup),
                )
                    .chain(),
            );
        }
        app.add_plugins(RenderDiagnosticsPlugin)
            .init_resource::<SchedStart>()
            // First of `First` and last of `Last`: the main schedule end to end, with nothing of
            // the render app in it. Two `Instant` reads a frame whether the journal is on or
            // off - the same price the frame-time window already pays.
            .add_systems(bevy::app::First, sched_open)
            .add_systems(bevy::app::Last, (count_moved, sched_close).chain())
            .init_resource::<PhaseClock>()
            .add_systems(PhaseMark(RAPP as u8), phase_mark::<RAPP>)
            .add_systems(PhaseMark(0), phase_mark::<0>)
            .add_systems(PhaseMark(1), phase_mark::<1>)
            .add_systems(PhaseMark(2), phase_mark::<2>)
            .add_systems(PhaseMark(3), phase_mark::<3>)
            .add_systems(PhaseMark(4), phase_mark::<4>)
            // **Inside** the two phases that hold three quarters of the frame. Unlike the five
            // above these are ordinary systems, so they are pinned to sets that already exist -
            // the world's own stage order in `Update`, bevy's transform and visibility sets in
            // `PostUpdate` - and each mark closes the span since the one before it.
            .add_systems(
                bevy::app::Update,
                (
                    phase_mark::<5>.after(benilla_world::schedule::WorldStage::Net),
                    phase_mark::<6>.after(benilla_world::schedule::WorldStage::Input),
                    phase_mark::<7>.after(benilla_world::schedule::WorldStage::Stream),
                )
                    .chain(),
            )
            .add_systems(
                bevy::app::PostUpdate,
                (
                    phase_mark::<8>.before(bevy::transform::TransformSystems::Propagate),
                    phase_mark::<9>
                        .after(bevy::transform::TransformSystems::Propagate)
                        .before(benilla_world::exterior_cull::ExteriorCullSet),
                    phase_mark::<10>
                        .after(benilla_world::exterior_cull::ExteriorCullSet)
                        .before(bevy::camera::visibility::VisibilitySystems::VisibilityPropagate),
                    phase_mark::<11>
                        .after(bevy::camera::visibility::VisibilitySystems::CheckVisibility),
                )
                    .chain(),
            )
            .init_resource::<FpsJournalSetting>()
            .add_observer(on_cvar)
            .insert_resource(FpsJournal {
                #[cfg(not(target_arch = "wasm32"))]
                env_path: std::env::var("WOW_FPS_JOURNAL")
                    .ok()
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from),
                #[cfg(target_arch = "wasm32")]
                env_path: crate::webenv::var("WOW_FPS_JOURNAL")
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from),
                path: None,
                window: Vec::new(),
                last_flush: 0.0,
                cpu_at_flush: None,
                main_at_flush: None,
                gpu: GpuAccum::default(),
                rcpu: GpuAccum::default(),
            })
            .add_systems(Update, journal_fps);
    }
}

#[derive(Resource)]
struct FpsJournal {
    /// `WOW_FPS_JOURNAL`: a fixed path, on for the whole run whatever the CVar says.
    env_path: Option<PathBuf>,
    /// Where rows go while the journal is on; `None` when off, or on a hermetic run with no
    /// state folder.
    path: Option<PathBuf>,
    window: Vec<f32>,
    last_flush: f32,
    /// Process CPU seconds at the previous flush, for the row's per-frame `cpu_ms`.
    cpu_at_flush: Option<f64>,
    /// Main-thread CPU seconds at the previous flush, for the row's `main_ms`: `cpu_at_flush`'s
    /// measurement narrowed to the serialized part.
    main_at_flush: Option<f64>,
    /// This second's GPU spans, folded per frame from the diagnostics store.
    gpu: GpuAccum,
    /// The render graph's CPU cost, same buckets - see [`cpu_bucket`]. bevy records these with
    /// no query set and no timestamp feature, so unlike `gpu` they are never empty in a browser.
    rcpu: GpuAccum,
}

/// **What a script error and a chat line cost, per second** — the three columns after `gpu_other`.
///
/// The frame writes; the journal's one-second flush reads and clears. Atomics rather than a
/// resource because the producer is the UI pass (which already holds the `!Send` VM) and the
/// consumer is this system, and a shared resource between them would be plumbing for a meter.
///
/// **Counts, and one duration that is not a guess.** 0735's rule is counts-never-milliseconds, and
/// `lua_err_us` is the exception it earns: the drain's own wall time, taken only on frames that
/// had something to drain, because the question is not how many errors there are but what one
/// costs on a single-threaded browser — a Lua handler call, a chat print, and a console write
/// across the wasm boundary.
static LUA_ERRS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static LUA_ERR_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MSG_HASHED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UI_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// **The combat columns.** Spell-visual kits and impacts played this second — the two doors every
/// cast's art comes through (`creature_anim::spell_visual`'s `play_kit` and `play_impact`).
///
/// They exist because the drop is reported in COMBAT and nowhere else: fighting a shaman, then a
/// paladin, then "somewhere in fighting". That rules out one class's kit and points at the thing
/// they share — somebody near you casting something. A rate is the first question to ask of it: if
/// the slow seconds are the ones with many plays, the cost is per-effect and the fix is in the
/// effect path; if the plays are flat while the frame doubles, it is not the spawning at all and
/// the next column takes over. Neither answer is available without counting.
static FX_KITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static FX_IMPACTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// **The network columns**, and the owner's own hypothesis: a browser has one thread, so every
/// packet the session received is decoded and applied inside the frame that received it. Combat is
/// when that stream is heaviest — movement for everyone in sight, spell casts, aura and health
/// updates, combat log — which fits a drop that appears in fights and nowhere else just as well as
/// the effect path does. Two candidates, one column pair each, one run to separate them.
static NET_PKTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static NET_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Frames folded into [`UI_US`] this second — the divisor, so the column is per-frame and
/// comparable with `mean_ms` directly rather than with a rate.
static UI_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Called by the UI pass after draining this frame's script errors and warnings. `micros` covers
/// the handler dispatch as well as the log writes: they are one act from the frame's point of view.
pub(crate) fn note_script_errors(count: u32, micros: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    LUA_ERRS.fetch_add(count, Relaxed);
    LUA_ERR_US.fetch_add(micros, Relaxed);
}

/// Called every frame with the UI pass's own measured split — tick + resolve + measure, the three
/// phases that run before the quad half. Summed over the second and divided by the frames in it,
/// so `ui_us` reads as "microseconds of UI per frame".
///
/// **This is the column the last journal could not supply.** That run had `cpu_ms`, `main_ms` and
/// every `gpu_*` empty for all 256 rows — the browser hands us no CPU time and no GPU spans — so
/// it could say when the frame was slow and nothing whatever about where. Our own phases are the
/// only timings available on that target, which makes measuring them the difference between
/// another hypothesis and an answer.
/// **What the two debug gates refused this second** - the `drop_chat`/`drop_other` columns.
///
/// An instrument that only shows the frame cannot tell "the switch did nothing" from "the switch
/// never fired", and this session has already lost two rounds to exactly that confusion. These say
/// which: a `netOthers 0` capture whose `drop_other` is 0 means the gate is not seeing the traffic,
/// not that the traffic is free.
pub(crate) fn note_net_dropped(chat: u32, other: u32) {
    use std::sync::atomic::Ordering::Relaxed;
    NET_DROP_CHAT.fetch_add(chat, Relaxed);
    NET_DROP_OTHER.fetch_add(other, Relaxed);
}

static NET_DROP_CHAT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static NET_DROP_OTHER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub(crate) fn note_net(packets: u32, micros: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    NET_PKTS.fetch_add(packets, Relaxed);
    NET_US.fetch_add(micros, Relaxed);
}

fn take_net_costs() -> (u32, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (NET_PKTS.swap(0, Relaxed), NET_US.swap(0, Relaxed))
}

/// Character body atlases COMPOSITED this second, and what they cost - the `skins_new` and
/// `skin_us` columns.
///
/// The `skin` residency column beside them counts atlases the cache HOLDS. That is a stock, and
/// a stock cannot say whether the frame in front of you paid for one: in a crowd it climbs to
/// several hundred and then sits still while the frame stays slow. These two are the flow.
///
/// Worth their own pair because the work is large and entirely on the main thread: a composite
/// reads half a dozen BLPs off the shared chain, decodes them, layers the body texel by texel on
/// the CPU and uploads the result. `char_skin.rs` calls that "fine behind the cache (once per
/// look)", and once per look is once per distinct APPEARANCE - measured at 72 and 95 atlases in a
/// quiet street and **621** in a crowd, where the frame was 45 ms against that street's 23.
static SKINS_NEW: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static SKIN_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Called once per cache MISS on the way out, including a miss that fails to compose: the reads
/// and the decode were paid either way, and the frame does not care that the result was dropped.
pub(crate) fn note_skin_composite(micros: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    SKINS_NEW.fetch_add(1, Relaxed);
    SKIN_US.fetch_add(micros, Relaxed);
}

fn take_skin_costs() -> (u32, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (SKINS_NEW.swap(0, Relaxed), SKIN_US.swap(0, Relaxed))
}

pub(crate) fn note_fx_kit() {
    FX_KITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn note_fx_impact() {
    FX_IMPACTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn take_fx_counts() -> (u32, u32) {
    use std::sync::atomic::Ordering::Relaxed;
    (FX_KITS.swap(0, Relaxed), FX_IMPACTS.swap(0, Relaxed))
}

pub(crate) fn note_ui_micros(micros: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    UI_US.fetch_add(micros, Relaxed);
    // **The divisor, and forgetting it is why the column shipped empty.** `take_ui_micros_per_frame`
    // returns `None` on zero frames — the deliberate "unmeasured, not free" cell — so a counter that
    // never counted read exactly like a target with no timings, which is the failure this column was
    // added to end. One journal run was spent on that.
    UI_FRAMES.fetch_add(1, Relaxed);
}

/// Called every frame with the message-sweep counter's delta (`UiScript::msg_lines_hashed`).
pub(crate) fn note_msg_lines_hashed(delta: u64) {
    MSG_HASHED.fetch_add(delta, std::sync::atomic::Ordering::Relaxed);
}

/// Read and clear — one call per journal row, so each row is that second alone.
fn take_script_costs() -> (u32, u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        LUA_ERRS.swap(0, Relaxed),
        LUA_ERR_US.swap(0, Relaxed),
        MSG_HASHED.swap(0, Relaxed),
    )
}

/// The UI pass's microseconds **per frame** over the second, or `None` when no frame reported —
/// an empty cell then, because a zero there would claim the UI was free rather than unmeasured.
fn take_ui_micros_per_frame() -> Option<u64> {
    use std::sync::atomic::Ordering::Relaxed;
    let total = UI_US.swap(0, Relaxed);
    let frames = UI_FRAMES.swap(0, Relaxed);
    (frames > 0).then(|| total / frames)
}

/// The GPU columns after `gpu_ms`, in header order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GpuBucket {
    /// Bevy's `main_opaque_pass_3d`: terrain, opaque model parts, the sky shells.
    Opaque = 0,
    /// Our retained static pass (`static_gx`): the WMOs and the doodads.
    Static,
    /// Bevy's transparent and transmissive 3D passes: water, glow cards, particles.
    Transparent,
    /// The `ffx_glow` chain: the quarter-res downsample, the two Gauss taps and a bake's combine.
    /// The world's combine draws inside `main_transparent_pass_2d`'s span, so it lands in
    /// [`Self::Ui`].
    Glow,
    /// The full-screen tail on every camera: tonemapping, upscaling, the MSAA writeback.
    Post,
    /// The 2D camera's passes, bevy UI, and our `ui_gamma_decode`.
    Ui,
    /// Every span this file does not name, counted, never dropped.
    Other,
}

const GPU_BUCKETS: usize = 7;

/// Which column a diagnostics path lands in; `None` for anything but a top-level GPU span (a
/// nested span's parent already carries it).
fn gpu_bucket(path: &str) -> Option<GpuBucket> {
    bucket_of(path, "/elapsed_gpu")
}

/// The same classification for the **CPU** side of a render-graph span.
///
/// bevy records `elapsed_cpu` for every span it opens, through `bevy_platform::time::Instant`
/// and nothing else - no query set, no timestamp feature, no readback
/// (`bevy_render-0.18.1/src/diagnostic/internal.rs:370,460`). It has therefore been recording the
/// render graph's per-pass CPU cost in this browser the whole time, while this file filtered every
/// one of those measurements out on the way past, because `gpu_bucket` demanded the `_gpu` suffix.
///
/// That is the hole twenty journals could not see into: the frame is 40 ms, everything measured
/// accounts for about five of them, and the rest was never asked about.
fn cpu_bucket(path: &str) -> Option<GpuBucket> {
    bucket_of(path, "/elapsed_cpu")
}

fn bucket_of(path: &str, suffix: &str) -> Option<GpuBucket> {
    let pass = path.strip_prefix("render/")?.strip_suffix(suffix)?;
    if pass.contains('/') {
        return None;
    }
    Some(match pass {
        "main_opaque_pass_3d" => GpuBucket::Opaque,
        "static_gx" => GpuBucket::Static,
        "main_transparent_pass_3d" | "main_transmissive_pass_3d" => GpuBucket::Transparent,
        p if p.starts_with("ffx_glow") => GpuBucket::Glow,
        "tonemapping" | "upscaling" | "msaa_writeback" | "postprocessing" => GpuBucket::Post,
        "main_opaque_pass_2d" | "main_transparent_pass_2d" | "ui" | "ui_gamma_decode" => {
            GpuBucket::Ui
        }
        _ => GpuBucket::Other,
    })
}

/// One second's GPU spans, summed per bucket and divided by the frames whose readback landed:
/// bevy hands the store at most one frame per sync and drops the rest when readbacks bunch up.
#[derive(Default)]
struct GpuAccum {
    sum: [f64; GPU_BUCKETS],
    frames: u32,
    /// The newest measurement time consumed; all measurements of one sync share one `Instant`,
    /// which is what makes a frame countable.
    seen: Option<Instant>,
    /// `WOW_GPU_PASSES=1`: the same sums per pass, printed beside each row as a `GPU_PASSES`
    /// line; empty unless armed.
    passes: std::collections::BTreeMap<String, f64>,
}

/// `WOW_GPU_PASSES=1`, read once.
fn passes_armed() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WOW_GPU_PASSES").is_some())
}

impl GpuAccum {
    /// Fold in every GPU measurement newer than the last fold.
    fn fold(&mut self, store: &DiagnosticsStore) {
        self.fold_with(store, gpu_bucket, "/elapsed_gpu");
    }

    /// The CPU twin - see [`cpu_bucket`]. Same accumulator, same arithmetic, the other suffix.
    fn fold_cpu(&mut self, store: &DiagnosticsStore) {
        self.fold_with(store, cpu_bucket, "/elapsed_cpu");
    }

    fn fold_with(
        &mut self,
        store: &DiagnosticsStore,
        bucket: fn(&str) -> Option<GpuBucket>,
        suffix: &str,
    ) {
        let mut newest = self.seen;
        let mut frame_times: Vec<Instant> = Vec::new();
        for diagnostic in store.iter() {
            let path = diagnostic.path().as_str();
            let Some(bucket) = bucket(path) else {
                continue;
            };
            let pass = passes_armed()
                .then(|| path.strip_prefix("render/")?.strip_suffix(suffix))
                .flatten();
            for m in diagnostic
                .measurements()
                .filter(|m| self.seen.is_none_or(|s| m.time > s))
            {
                self.sum[bucket as usize] += m.value;
                if let Some(pass) = pass {
                    *self.passes.entry(pass.to_string()).or_default() += m.value;
                }
                if !frame_times.contains(&m.time) {
                    frame_times.push(m.time);
                }
                if newest.is_none_or(|n| m.time > n) {
                    newest = Some(m.time);
                }
            }
        }
        self.frames += frame_times.len() as u32;
        self.seen = newest;
    }

    /// The row's GPU cells, `,gpu_ms,<one per bucket>`, and the reset; all empty when no frame
    /// was read this second.
    fn columns(&mut self) -> String {
        let mut s = String::new();
        if self.frames == 0 {
            s.push_str(&",".repeat(GPU_BUCKETS + 1));
        } else {
            let n = f64::from(self.frames);
            let total: f64 = self.sum.iter().sum();
            s.push_str(&format!(",{:.2}", total / n));
            for bucket in self.sum {
                s.push_str(&format!(",{:.2}", bucket / n));
            }
            if passes_armed() {
                // Costliest first, ms per read frame.
                let mut rows: Vec<(&String, &f64)> = self.passes.iter().collect();
                rows.sort_by(|a, b| b.1.total_cmp(a.1));
                let line: Vec<String> = rows
                    .iter()
                    .map(|(k, v)| format!("{k}={:.3}", *v / n))
                    .collect();
                eprintln!("GPU_PASSES frames={} {}", self.frames, line.join(" "));
            }
        }
        self.sum = [0.0; GPU_BUCKETS];
        self.frames = 0;
        self.passes.clear();
        s
    }
}

/// The `#` line a fresh file opens with: what the rows were read on, and the two timestamp
/// features reported one by one, because an `AND` over them is not a fact about either.
/// Everything a reader needs to know before comparing two journals.
fn preamble(adapter: Option<&RenderAdapterInfo>, device: Option<&RenderDevice>) -> String {
    let (gpu, backend, driver) = adapter.map_or_else(
        || ("?".to_string(), "?".to_string(), "?".to_string()),
        |a| {
            let driver = format!("{} {}", a.driver, a.driver_info).trim().to_string();
            (
                a.name.clone(),
                format!("{:?}", a.backend),
                // Metal reports no driver string.
                if driver.is_empty() {
                    "?".to_string()
                } else {
                    driver
                },
            )
        },
    );
    // Reported SEPARATELY, because the two features are not one fact and an `AND` over them lied
    // here for a whole round of measurement. Bevy builds its timestamp query set on
    // `TIMESTAMP_QUERY` alone (`bevy_render-0.18.1/src/diagnostic/internal.rs:205`);
    // `TIMESTAMP_QUERY_INSIDE_PASSES` only decides whether a span may be taken *within* a pass,
    // and the `gpu_*` columns are pass-boundary spans. So a browser run that has `timestamp-query`
    // and not the in-pass extension - which is exactly what Chrome's WebGPU adapter offers - can
    // fill every GPU column while the old single `gpu_spans no` claimed the platform could not.
    // Three, not two, because the feature that actually gates these columns is the THIRD one.
    // bevy writes every timestamp through `encoder.write_timestamp`, and its own `write_timestamp`
    // returns `None` outright unless TIMESTAMP_QUERY_INSIDE_ENCODERS is present
    // (`bevy_render-0.18.1/src/diagnostic/internal.rs:285`, whose comment reads "unsupported on
    // WebGPU"). So a browser can hold `timestamp-query`, build the query set, and still record not
    // one span - which is exactly what this journal did. Printing all three is what separates
    // "the device cannot" from "the device can and we record nothing".
    let (ts, enc, inside) = device.map_or(("?", "?", "?"), |d| {
        let f = d.features();
        let yn = |b| if b { "yes" } else { "no" };
        (
            yn(f.contains(wgpu::Features::TIMESTAMP_QUERY)),
            yn(f.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)),
            yn(f.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES)),
        )
    });
    let head = "# benilla fps journal";
    let gate = format!("gpu_ts {ts} | gpu_inside_encoders {enc} | gpu_inside_passes {inside}");
    format!("{head} | gpu {gpu} | backend {backend} | driver {driver} | {gate}\n")
}

/// The journal's residency columns, grouped for Bevy's system-param arity limit. The
/// `Assets<T>` counts are totals; the [`ArtCensus`] half breaks the same population down by the
/// cache that holds it, and `evicted` is the running total dropped by distance.
///
/// [`ArtCensus`]: benilla_world::art_scope::ArtCensus
#[derive(bevy::ecs::system::SystemParam)]
struct JournalResidency<'w> {
    mats: Res<'w, Assets<benilla_assets::materials::WowModelMaterial>>,
    meshes: Res<'w, Assets<Mesh>>,
    images: Res<'w, Assets<bevy::image::Image>>,
    m2: Res<'w, Assets<benilla_assets::M2Model>>,
    uv_reg: Res<'w, benilla_world::doodad_anim::UvAnimMaterials>,
    tint_reg: Res<'w, benilla_world::doodad_anim::TintAnimMaterials>,
    art: Res<'w, benilla_world::art_scope::ArtCensus>,
    /// The view focus, where art is asked for; it leaves the avatar's `x,y,z` in a detached
    /// free-fly.
    scope: Res<'w, benilla_world::art_scope::ArtScopeState>,
}

/// The diagnostics store and the preamble's adapter and device; all absent without a renderer.
#[derive(bevy::ecs::system::SystemParam)]
struct JournalGpu<'w> {
    store: Option<Res<'w, DiagnosticsStore>>,
    adapter: Option<Res<'w, RenderAdapterInfo>>,
    device: Option<Res<'w, RenderDevice>>,
    /// The two settings an A/B run turns - see the row tail for why they are written at all.
    rscale: Option<Res<'w, crate::world_backdrop::RenderScale>>,
    view: Option<Res<'w, benilla_world::view::ViewDistance>>,
}

/// Pinned to the main thread: [`main_thread_cpu_secs`] reads the calling thread's clock.
fn journal_fps(
    _pin_to_main_thread: bevy::ecs::system::NonSendMarker,
    mut journal: ResMut<FpsJournal>,
    setting: Res<FpsJournalSetting>,
    time: Res<Time<Real>>,
    player: Option<Res<crate::player::Player>>,
    streamed: Query<(), With<crate::net::NetEntity>>,
    // Live particle emitters — the `emitters` column. A marker-only query, so this is a count
    // of matched archetype rows and not a walk over their data. It is here rather than beside
    // the fx counters because it answers the other half of the question: those say how many
    // effects STARTED this second, this says how many are still running.
    emitters: Query<(), With<benilla_world::particles::ParticleEmitter>>,
    entities: Query<()>,
    residency: JournalResidency,
    gpu: JournalGpu,
) {
    let now = time.elapsed_secs();
    // Read every frame: turning on opens the file and restarts every baseline; turning off
    // drops the partial second.
    let wanted = journal.env_path.is_some() || setting.0;
    match (wanted, journal.path.is_some()) {
        (false, false) => return,
        (false, true) => {
            info!("fps journal: off");
            journal.path = None;
            journal.window.clear();
            journal.gpu = GpuAccum::default();
            journal.rcpu = GpuAccum::default();
            return;
        }
        (true, false) => {
            let Some(path) = journal
                .env_path
                .clone()
                .or_else(crate::local_state::fps_journal_path)
            else {
                return; // hermetic: no state folder
            };
            // The header goes in exactly once, at creation: the rows are appended for the life
            // of the run (and across runs, deliberately — a journal accumulates legs).
            #[cfg(not(target_arch = "wasm32"))]
            if !path.exists() {
                let head = format!(
                    "{}{JOURNAL_HEADER}",
                    preamble(gpu.adapter.as_deref(), gpu.device.as_deref())
                );
                if let Err(e) = crate::local_state::write_atomic(&path, &head) {
                    warn!("fps journal: cannot create {}: {e}", path.display());
                    return;
                }
            }
            #[cfg(target_arch = "wasm32")]
            web::begin(&format!(
                "{}{JOURNAL_HEADER}",
                preamble(gpu.adapter.as_deref(), gpu.device.as_deref())
            ));
            #[cfg(not(target_arch = "wasm32"))]
            info!("fps journal: writing {}", path.display());
            #[cfg(target_arch = "wasm32")]
            info!("fps journal: recording the latest hour; use the download FPS journal button");
            journal.last_flush = now;
            journal.cpu_at_flush = process_cpu_secs();
            journal.main_at_flush = main_thread_cpu_secs();
            // Only spans that land from here on count: the store may hold a history.
            journal.gpu = GpuAccum {
                seen: Some(Instant::now()),
                ..GpuAccum::default()
            };
            journal.rcpu = GpuAccum {
                seen: Some(Instant::now()),
                ..GpuAccum::default()
            };
            journal.path = Some(path);
        }
        (true, true) => {}
    }
    journal.window.push(time.delta_secs() * 1000.0);
    if let Some(store) = gpu.store.as_deref() {
        journal.gpu.fold(store);
        journal.rcpu.fold_cpu(store);
    }
    if now - journal.last_flush < 1.0 {
        return;
    }
    journal.last_flush = now;
    let mut v = std::mem::take(&mut journal.window);
    if v.is_empty() {
        return;
    }
    v.sort_by(f32::total_cmp);
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let p95 = v[((v.len() - 1) as f32 * 0.95).round() as usize];
    // Raw WoW coords, so the line pastes straight into a `.go xyz` probe.
    let pos = player
        .filter(|p| p.active)
        .map(|p| benilla_assets::coords::bevy_to_wow(p.pos))
        .unwrap_or([0.0; 3]);
    let cpu_now = process_cpu_secs();
    let cpu_ms = match (journal.cpu_at_flush, cpu_now) {
        (Some(t0), Some(t1)) => format!("{:.2}", (t1 - t0) * 1000.0 / v.len() as f64),
        _ => String::new(),
    };
    journal.cpu_at_flush = cpu_now;
    let mut line = format!(
        "{now:.1},{:.1},{:.1},{:.1},{mean:.2},{p95:.2},{},{},{cpu_ms},{},{},{},{},{},{}",
        pos[0],
        pos[1],
        pos[2],
        streamed.iter().len(),
        entities.iter().len(),
        residency.mats.len(),
        residency.meshes.len(),
        residency.images.len(),
        residency.m2.len(),
        residency.uv_reg.0.len(),
        residency.tint_reg.0.len(),
    );
    // `ArtSlot::ALL` order is the header's column order.
    for slot in benilla_world::art_scope::ArtSlot::ALL {
        line.push_str(&format!(",{}", residency.art.live(slot)));
    }
    line.push_str(&format!(",{}", residency.art.dropped_total()));
    match residency.scope.focus() {
        Some(f) => line.push_str(&format!(",{:.1},{:.1},{:.1}", f[0], f[1], f[2])),
        None => line.push_str(",,,"),
    }
    // New columns only ever append at the end of the row.
    let main_now = main_thread_cpu_secs();
    match (journal.main_at_flush, main_now) {
        (Some(t0), Some(t1)) => {
            line.push_str(&format!(",{:.2}", (t1 - t0) * 1000.0 / v.len() as f64))
        }
        _ => line.push(','),
    }
    journal.main_at_flush = main_now;
    let gpu_cells = journal.gpu.columns();
    line.push_str(&gpu_cells);
    // The script-cost trio, after the GPU cells and last of all — per this file's own rule
    // that columns only ever grow at the end, so a journal started before this keeps parsing.
    let (errs, err_us, hashed) = take_script_costs();
    line.push_str(&format!(",{errs},{err_us},{hashed}"));
    // The frame-split and combat trio. `ui_us` is an empty cell rather than a zero when no frame
    // reported: unmeasured and free are different claims, and the last journal's all-empty timing
    // columns are exactly what a zero there would have hidden.
    match take_ui_micros_per_frame() {
        Some(us) => line.push_str(&format!(",{us}")),
        None => line.push(','),
    }
    line.push_str(&format!(
        ",{},{}",
        benilla_world::terrain_stream::take_collider_build_micros(),
        emitters.iter().count()
    ));
    let (kits, impacts) = take_fx_counts();
    let (pkts, net_us) = take_net_costs();
    // `pipes` is a SNAPSHOT of the render-pipeline cache, so the row-to-row delta is how many
    // variants were compiled in that second — the owner's "are some objects slow to create?" as a
    // number, and the last candidate standing for the hitches.
    line.push_str(&format!(
        ",{kits},{impacts},{pkts},{net_us},{}",
        crate::pipe_warm::pipeline_total()
    ));
    // **The two settings an A/B run turns, in the row that run produced.** Neither leaves any
    // other trace in this file: `renderScale` moves no count here at all, and `farclip` culls
    // what is drawn without unstreaming a tile or despawning anything, so `streamed` and
    // `entities` sit still while it changes. A four-arm run was recorded with both of them
    // turning and the arms could not be told apart afterwards - not from each other, and not from
    // a crowd wandering out of view. A knob that steers the frame belongs in the row beside it.
    match (gpu.rscale.as_deref(), gpu.view.as_deref()) {
        (Some(r), Some(v)) => line.push_str(&format!(",{:.2},{:.0}", r.0, v.farclip)),
        (Some(r), None) => line.push_str(&format!(",{:.2},", r.0)),
        (None, Some(v)) => line.push_str(&format!(",,{:.0}", v.farclip)),
        (None, None) => line.push_str(",,"),
    }
    // The composite FLOW, next to `skin`'s stock - see `note_skin_composite`.
    // **Does the composite's decode cache fire?** Served-from-cache against decoded-here, per
    // second. Without this pair the cache is a mechanism nobody has watched work: the first
    // journal after it shipped showed the per-composite cost unchanged, and there was no way to
    // tell a cache that never hits from a decode that was never the cost.
    let (tex_hit, tex_dec) = benilla_formats::take_decode_counts();
    let (skins_new, skin_us) = take_skin_costs();
    line.push_str(&format!(",{skins_new},{skin_us},{tex_hit},{tex_dec}"));
    // The render graph's own CPU split. Never empty here, unlike the gpu_* twins.
    let rcpu_cells = journal.rcpu.columns();
    line.push_str(&rcpu_cells);
    // The main schedule against the frame - see `SCHED_US`.
    let frames = take_sched_frames();
    match take_sched_us(frames) {
        Some(us) => line.push_str(&format!(",{us}")),
        None => line.push(','),
    }
    // ...and where inside it. See `PHASE_US`. The thirteenth tile (`rapp`) is held back and
    // written at the end of the row: the columns only ever grow there (see `JOURNAL_HEADER`), and
    // emitting it in slot order would have pushed every column after it one place along, so every
    // journal already recorded would parse one column out of step from `moved` onward.
    let phases = take_phase_us(frames);
    for us in &phases[..RAPP] {
        line.push_str(&format!(",{us}"));
    }
    line.push_str(&format!(",{}", take_moved()));
    // The streamer's own split of `u_stream` - see `STREAM_US`.
    for us in take_stream_us(frames) {
        line.push_str(&format!(",{us}"));
    }
    let (rig_wr, rig_sk) = benilla_world::rig_anim::take_anchor_writes();
    let per = frames.max(1);
    let _ = write!(line, ",{},{}", rig_wr / per, rig_sk / per);
    // **Everything between two main schedules** - the render sub-app, the event-loop hop and
    // present. Held back to here so the header keeps growing only at its end; the mark's own
    // comment has what the span is and when it would stop being honest.
    let _ = write!(line, ",{}", phases[RAPP]);
    // ...and `rapp` cut into the render schedule's own sets. `rapp` minus their sum is
    // `ExtractSchedule` (main world, ahead of this schedule) plus present.
    for us in take_rapp_us(frames) {
        let _ = write!(line, ",{us}");
    }
    // What `netChat 0` / `netOthers 0` refused this second - see `note_net_dropped`. Per second,
    // not per frame: these are counts of packets, and a packet is a thing that happened, not a
    // rate the frame divides.
    {
        use std::sync::atomic::Ordering::Relaxed;
        let _ = write!(
            line,
            ",{},{}",
            NET_DROP_CHAT.swap(0, Relaxed),
            NET_DROP_OTHER.swap(0, Relaxed)
        );
    }
    // Decoded textures refused by the cache for being over a quarter of its budget - never
    // admitted, so re-decoded on every composite however often they are asked for. A non-zero
    // `tex_big` beside a low `tex_hit` says the ceiling is the defect, not the size.
    let _ = write!(line, ",{}", benilla_formats::take_oversize_count());
    // The CHAIN read cache, which is a different cache from the decode one above and had no
    // counters at all: reads served from memory, reads that went to the host, the kilobytes those
    // moved, and entries refused for being over the per-entry ceiling. A network tab cannot answer
    // this - its names are truncated, two loaders are interleaved in it (this one over XHR, bevy's
    // asset path over `fetch`), and a first load is supposed to be a download.
    {
        let (hit, miss, kb, big) = benilla_formats::take_read_counts();
        let _ = write!(line, ",{hit},{miss},{kb},{big}");
    }
    // **Every `#` line goes AFTER the last column, not before it.** Both blocks below used to sit
    // above the trailing columns, which was invisible while the systems one stayed empty and split
    // every row of journal 37 in half the moment the mats one started printing: the row ended at
    // `t_collider`, the `#` line followed, and `,rig_wr,rig_sk,rapp` landed on a third line. A
    // reader that checks field count against the header - mine does - then parsed 1 row of 383.
    // The comment inside the systems block already described this exact failure from the last time
    // it happened, three lines below where I inserted the new one.
    // **Which lane minted the materials**, cumulative - a `#` line for the same reason as the
    // one below, and because a lane list is not a fixed column set. `mats` beside it counts what
    // `Assets<WowModelMaterial>` holds; this says who asked for them. At one pin 19 yards apart
    // that total read 883 (flat for 90 s) in journal 33 and 6,002 (still climbing) in journal 36,
    // with the frame 41 -> 70 ms, and nothing in the code could say which lane widened.
    {
        let counts = benilla_assets::materials::material_counts();
        if counts.iter().any(|&n| n > 0) {
            let mut names = String::from("\n# mats");
            for (name, n) in benilla_assets::materials::MAT_LANE_NAMES.iter().zip(counts) {
                let _ = write!(names, " {name}={n}");
            }
            names.push('\n');
            line.push_str(&names);
        }
    }
    // **And which AXIS of the material key widened** — the lane counter above says the materials
    // are minted by `model_material`; this says what makes each one distinct. If one axis's count
    // sits near `keys`, that axis IS the key space.
    {
        let axes = benilla_world::model_render::key_axis_counts();
        if !axes.is_empty() {
            let mut names = String::from("\n# axes");
            for (name, n) in axes {
                let _ = write!(names, " {name}={n}");
            }
            names.push('\n');
            line.push_str(&names);
        }
    }
    // **The costliest systems of that second, by name.** A `#` line, so every existing reader of
    // this file skips it and the columns stay a table. This is what ends the
    // guess-a-suspect-then-rebuild loop: one capture names them all.
    let top = benilla_world::sysprof::take_top(12);
    if !top.is_empty() {
        // **Its own line.** The first build appended this to the end of the data row, so every
        // reader saw a CSV whose last column ended in `6552# systems …` - and my own grep for
        // `^# systems` found nothing and reported the profiler dead for two rounds. It was
        // working the whole time.
        let mut names = String::from("\n# systems");
        for (name, us) in top {
            // The type path is most of every name and none of the information.
            let short = name.rsplit("::").next().unwrap_or(&name).to_string();
            let _ = write!(names, " {short}={us}");
        }
        names.push('\n');
        line.push_str(&names);
    }
    // **Does the pose guard bite?** Writes that landed against writes it suppressed, per frame,
    // so the pair is comparable to `moved` beside it and independent of how populated the pin
    // happened to be. `rig_sk` near zero means the poses genuinely differ every frame and the
    // guard buys nothing - a real possibility for a global sequence, which samples a continuous
    // curve at an advancing `t`. See `benilla_world::rig_anim::take_anchor_writes`.
    line.push('\n');
    #[cfg(target_arch = "wasm32")]
    web::append(&line);
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::io::Write;
        let Some(path) = journal.path.as_ref() else {
            return;
        };
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::diagnostic::{Diagnostic, DiagnosticMeasurement, DiagnosticPath};
    use std::time::Duration;

    #[test]
    fn buckets_name_every_pass_we_draw_and_skip_what_is_not_a_gpu_span() {
        use GpuBucket::*;
        let b = |p: &str| gpu_bucket(p);
        assert_eq!(b("render/main_opaque_pass_3d/elapsed_gpu"), Some(Opaque));
        assert_eq!(b("render/static_gx/elapsed_gpu"), Some(Static));
        assert_eq!(
            b("render/main_transparent_pass_3d/elapsed_gpu"),
            Some(Transparent)
        );
        assert_eq!(b("render/ffx_glow_gauss_h/elapsed_gpu"), Some(Glow));
        assert_eq!(b("render/ffx_glow_combine/elapsed_gpu"), Some(Glow));
        assert_eq!(b("render/tonemapping/elapsed_gpu"), Some(Post));
        assert_eq!(b("render/upscaling/elapsed_gpu"), Some(Post));
        assert_eq!(b("render/ui/elapsed_gpu"), Some(Ui));
        assert_eq!(b("render/ui_gamma_decode/elapsed_gpu"), Some(Ui));
        assert_eq!(b("render/main_transparent_pass_2d/elapsed_gpu"), Some(Ui));
        assert_eq!(
            b("render/early_mesh_preprocessing/elapsed_gpu"),
            Some(Other)
        );
        // CPU spans, non-render diagnostics and nested spans are not GPU cells.
        assert_eq!(b("render/main_opaque_pass_3d/elapsed_cpu"), None);
        assert_eq!(b("fps"), None);
        assert_eq!(b("render/outer/inner/elapsed_gpu"), None);
    }

    fn push(store: &mut DiagnosticsStore, path: &str, time: Instant, value: f64) {
        let p = DiagnosticPath::new(path.to_string());
        if store.get(&p).is_none() {
            store.add(Diagnostic::new(p.clone()));
        }
        store
            .get_mut(&p)
            .unwrap()
            .add_measurement(DiagnosticMeasurement { time, value });
    }

    #[test]
    fn a_flush_averages_per_frame_read_and_a_fold_takes_only_what_arrived() {
        let mut store = DiagnosticsStore::default();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(10);
        let t2 = t0 + Duration::from_millis(20);
        // Frame 1: opaque 2, static 3, and the full-screen tail on two cameras (0.5 + 0.3).
        push(
            &mut store,
            "render/main_opaque_pass_3d/elapsed_gpu",
            t0,
            2.0,
        );
        push(&mut store, "render/static_gx/elapsed_gpu", t0, 3.0);
        push(&mut store, "render/tonemapping/elapsed_gpu", t0, 0.5);
        push(&mut store, "render/tonemapping/elapsed_gpu", t0, 0.3);
        // A CPU span beside them, never counted.
        push(
            &mut store,
            "render/main_opaque_pass_3d/elapsed_cpu",
            t0,
            99.0,
        );
        let mut acc = GpuAccum::default();
        acc.fold(&store);
        assert_eq!(acc.frames, 1);
        // Frame 2 lands; the fold reads only it (frame 1's values would double otherwise).
        push(
            &mut store,
            "render/main_opaque_pass_3d/elapsed_gpu",
            t1,
            4.0,
        );
        acc.fold(&store);
        acc.fold(&store); // nothing new: a no-op, not a double count
        assert_eq!(acc.frames, 2);
        let cols = acc.columns();
        // gpu_ms = (2 + 3 + 0.8 + 4) / 2; opaque (2 + 4) / 2; static 3 / 2; post 0.8 / 2.
        assert_eq!(cols, ",4.90,3.00,1.50,0.00,0.00,0.40,0.00,0.00");
        // The reset: a second with nothing read writes empty cells, one per column.
        assert_eq!(acc.columns(), ",,,,,,,,");
        // And a later frame is still read after the reset.
        push(
            &mut store,
            "render/main_opaque_pass_3d/elapsed_gpu",
            t2,
            1.0,
        );
        acc.fold(&store);
        assert_eq!(acc.columns(), ",1.00,1.00,0.00,0.00,0.00,0.00,0.00,0.00");
    }

    #[test]
    fn the_header_ends_with_the_gpu_cells_in_bucket_order() {
        let cols: Vec<&str> = JOURNAL_HEADER.trim_end().split(',').collect();
        let gpu: Vec<&str> = cols[cols.len() - (GPU_BUCKETS + 1)..].to_vec();
        assert_eq!(
            gpu,
            [
                "gpu_ms",
                "gpu_opaque",
                "gpu_static",
                "gpu_transp",
                "gpu_glow",
                "gpu_post",
                "gpu_ui",
                "gpu_other"
            ]
        );
        // An empty second writes exactly one cell per GPU column.
        assert_eq!(
            GpuAccum::default().columns().matches(',').count(),
            gpu.len()
        );
    }

    #[test]
    fn the_preamble_names_the_adapter_or_says_it_cannot() {
        assert_eq!(
            preamble(None, None),
            "# benilla fps journal | gpu ? | backend ? | driver ? | gpu_ts ? | gpu_inside_encoders ? | gpu_inside_passes ?\n"
        );
    }
}
