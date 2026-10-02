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
                              gpu_other,lua_errs,lua_err_us,msg_hashed,ui_us,col_us,emitters,fx_live,fx_models,fx_kits,fx_impacts,net_pkts,net_us,pipes,\
                              rscale,farclip,\
                              skins_new,skin_us,tex_hit,tex_dec,\
                              rcpu_ms,rcpu_opaque,rcpu_static,rcpu_transp,rcpu_glow,rcpu_post,rcpu_ui,rcpu_other,sched_us,s_first,s_pre,s_upd,s_post,s_last,\
                              u_net,u_input,u_stream,p_pre,p_xform,p_cull,p_vis,moved,\
                              t_stream,t_furnish,t_mfurnish,t_spawn,t_collider,rig_wr,rig_sk,rapp,r_extract,r_assets,r_queue,r_sort,r_prepare,r_render,r_clean,drop_chat,drop_other,tex_big,rd_hit,rd_miss,rd_kb,rd_big,r_between,r_xsched,mesh_vis,mesh_all,r_postcl,px_anim,px_asset,px_prop,px_bounds,px_check,px_uifeed,px_unitfeed,px_uiinput,px_feedunits,gate_n,gate_open,rigs_live,rigs_park,arch,ent_alloc,px_vmtick,skf_n,skf_us,px_input,px_asstrk,px_uiload,px_auras,px_attach,px_drive,aev,wix_us,ui_ex_us,fu_us,fu_open,au_us,flat_us\n";

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
    // **`/console unmanagedGeosets 0`** - the A/B for upstream PR 438's `> 1700` rule (see
    // `benilla_formats::characters::geosets`). It only matters on data that authors geosets up
    // there, which is the owner's and not upstream's, so the question is his to look at. A body
    // keeps the policy it was dressed under, so walk out of range and back after toggling.
    if ev.is("unmanagedGeosets") {
        let on = ev.flag();
        benilla_formats::set_unmanaged_geosets_visible(on);
        info!("geosets above 1700 bypass the selection: {on}");
    }
    // **`/console feedGateTrace 1`** - names which input holds a feed's change gate open.
    // `feed_units` and its 38 siblings in `UnitFeed` run behind gates that should skip a frame
    // where nothing moved, yet the set measured 5.76 ms with the network off. A `Res::is_changed()`
    // is true after ANY mutable access, so one system taking `ResMut` unconditionally holds every
    // gate built on that resource open for ever - and this trace is what names it.
    // **The ablation switches.** The owner's method, and a better one than naming a suspect and
    // measuring it: turn a subsystem OFF and read the frame. Two of these already existed as env
    // vars — `WOW_ANIM_PARK_ALL` is the pose lane's cost FLOOR and `WOW_NO_ANIM_LOD` its CEILING —
    // and were unreachable in the browser, where `std::env::var_os` is always `None`.
    // **One switch, every lane.** The owner asked for "stop all objects" and got creature rigs
    // only: water, torches and portals kept moving, correctly - none of them is a creature rig.
    // Doodads own their own gate and material/UV animation is not a rig at all, so the CVar drives
    // all three from here rather than asking anyone to remember three names.
    if ev.is("animParkAll") {
        let on = ev.flag();
        crate::creature_anim::lod::set_park_all(on);
        benilla_world::doodad_anim::set_park_all(on);
        benilla_world::doodad_anim::set_mat_anim_off(on);
        // A torch flame and a forge fire are PARTICLES, not animation, and water scrolls in the
        // shader (`liquid.wgsl:94`, `w.anim.w * globals.time`) where no CPU-side parking reaches
        // it. The owner asked for one switch that stops everything, so it drives those lanes too
        // rather than asking him to remember three names.
        benilla_world::particles::set_fx_off(on);
        benilla_world::liquid::set_freeze(on);
        info!("all motion stopped (rigs, doodads, material/UV, global sequences, effects, water): {on}");
    }
    if ev.is("animLodOff") {
        let on = ev.flag();
        crate::creature_anim::lod::set_lod_off(on);
        info!("animation LOD disabled (pose-lane ceiling): {on}");
    }
    if ev.is("roomLodOff") {
        let on = ev.flag();
        crate::creature_anim::lod::set_room_lod_off(on);
        info!("portal-PVS leg of the animation LOD disabled: {on}");
    }
    // **`/console matAnimOff 1`** - the third animation lane. Water and torches keep moving with
    // every rig parked, correctly: neither has a rig. Their motion is material and UV animation,
    // which `sample_mat_anim` evaluates for EVERY instance each frame, hidden ones included, as
    // the reference does. Faithful, and never priced.
    // **`/console uiLua 0`** - the player interface off, and with it all 190 `feed_*`/`drain_*`
    // systems that bridge the game into its VM. They are every one gated on `ingame_ui_up`, so
    // one refusal there refuses the lot. The frame counter moves to the page (`journal_web::fps`)
    // because the in-game one is drawn by the interface this switches off.
    if ev.is("uiLua") {
        let on = ev.flag();
        crate::ui_script::set_ui_lua(on);
        info!("player UI (Lua) {}", if on { "ON" } else { "OFF" });
    }
    if ev.is("matAnimOff") {
        let on = ev.flag();
        benilla_world::doodad_anim::set_mat_anim_off(on);
        info!("material/UV animation sampling stopped: {on}");
    }
    if ev.is("fxOff") {
        let on = ev.flag();
        benilla_world::particles::set_fx_off(on);
        info!("particle simulation stopped: {on}");
    }
    // **`/console auraTrace 1`** - the aura subsystem's own trace, which existed behind an env
    // var and so was unreachable in the browser (`std::env::var` is always `None` on wasm32).
    // **`/console skinComposite 0`** - the body composite back on the drawing thread, so the two
    // lanes can be compared inside ONE session. See `entities::skin_composite::INLINE`.
    // **`/console matKeyOrder 1`** - the wide material key back, for the one-session A/B. See
    // `benilla_world::model_render::KEY_ORDER`.
    if ev.is("matKeyOrder") {
        let on = ev.flag();
        benilla_world::model_render::set_mat_key_order(on);
        info!(
            "material key: batch_order {}",
            if on { "in the key for every batch (wide)" } else { "dropped for non-sorting batches" }
        );
    }
    if ev.is("skinComposite") {
        let on = ev.flag();
        crate::entities::skin_composite::set_off_thread(on);
        info!("body composites {}", if on { "off-thread" } else { "INLINE (drawing thread)" });
    }
    // **`/console gpuMs 1`** - the whole-frame GPU meter, which was behind an env var and so had
    // never run in a browser. See `perf::gpu::ON`.
    if ev.is("gpuMs") {
        let on = ev.flag();
        crate::perf::gpu::set_enabled(on);
        info!("gpu meter: {on} (takes effect on the next pipeline build)");
    }
    if ev.is("auraTrace") {
        let on = ev.flag();
        crate::ui_aura::set_trace(on);
        info!("aura trace: {on}");
    }
    if ev.is("feedGateTrace") {
        let on = ev.flag();
        crate::ui_script::gate::set_trace(on);
        info!("feed gate trace: {on}");
    }
    #[cfg(not(target_os = "macos"))]
    if ev.is("flatParts") {
        let yards = ev.num();
        benilla_world::rig_flat::set_tolerance(yards);
        info!("flat parts: {}", if yards < 0.0 { "off (bevy propagates)".to_string() } else { format!("{yards} yd") });
    }
    if ev.is("uploadBudgetMb") {
        let mb = ev.num().max(0.0) as usize;
        // Logged by the mirror when it lands, browser only.
        super::upload_budget::set_mb(mb);
    }
    if ev.is("skinCacheMb") {
        let mb = benilla_formats::set_skin_cache_mb(ev.num() as usize);
        info!("skin decode cache: {mb} MiB");
    }
    if ev.is("dumpSchedule") && ev.flag() {
        // **Armed here, run in `Last`.** Running it straight from this observer printed
        // "Update: 0 systems" while PostUpdate printed 209, and the reason is not the dump: the
        // console is read during `Update`, and bevy TAKES a schedule out of `Schedules` while it
        // runs it, so `get(Update)` answers `None` from inside itself. PostUpdate was merely the
        // one that happened not to be running. Deferring to `Last` puts both schedules back in
        // the world - and `u_net` reading 5.13 ms with the network off is exactly the number that
        // needs Update's list to be readable.
        crate::perf::sched_dump::arm();
        commands.queue(|world: &mut World| {
            world
                .resource_mut::<crate::cvars::Cvars>()
                .mirror("dumpSchedule", "0");
        });
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
    // The handoff to `rmark_open`, which is in the RENDER world and cannot see a main-world
    // resource. See [`MAIN_END`].
    *MAIN_END.lock().expect("MAIN_END is never poisoned: single-threaded, no panic inside") =
        Some(Instant::now());
}

/// **When the main schedules finished**, so the render world can measure `ExtractSchedule`.
///
/// `ExtractSchedule` runs between `Last` and the render app's own `Render` schedule, in neither
/// clock's reach: `PhaseClock` stops at `Last`, `RClock` starts at `Render`. It was inside
/// `r_between` together with present and the browser's idle, and journal 47 made splitting them
/// the whole question - 68% of a calm 50.1 ms frame is in that span while every game system this
/// project has is 7.4 ms of it.
///
/// A `Mutex` and not an atomic because `Instant` is not one; on wasm32 this is single-threaded
/// and uncontended, so it is a branch. `None` before the first frame closes.
static MAIN_END: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// `ExtractSchedule` (plus any main-world tail after `Last`), per frame this second.
static XSCHED_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

/// **The span the seven tiles could not name** - `r_between`.
///
/// Journal 45 measured `rapp` at 44.82 s of which the seven named tiles were 9.82: **78%
/// unnamed**, and on the 65 spiking seconds 29.70 s of 43.9. The `RPHASES` header already said
/// what the residual is - ExtractSchedule plus present - but not in what proportion, and the two
/// have opposite cures: extract is ours to make cheaper, present is the browser and is not.
///
/// The same capture had that residual tracking bytes read (`rd_kb`, r = +0.69 over all rows AND
/// +0.69 inside the spikes alone) while tracking entity count NEGATIVELY (r = -0.10), which
/// argues against extract, whose cost scales with entities. Argues, not settles: `entities`
/// barely moved there, so the negative has little to stand on, and a correlation that agrees
/// with the hypothesis is the kind this project has been burned by twice. Hence a number.
///
/// Held apart from `RAPP_US` on purpose: that array is written into the MIDDLE of the row, so
/// growing it would shift every column after `rapp` and break the header's append-only rule.
static RBETWEEN_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// **`RenderSystems::PostCleanup`** - the last thing the render app does, and until now the only
/// part of the frame behind no mark at all.
///
/// It holds `despawn_temporary_render_entities`, and the shape of the unknown points straight at
/// it. Journal 52, calm rows: the unnamed span grows 12.7 -> 32.5 ms as the scene grows 2k -> 34k
/// ENTITIES, while visible meshes fall the other way (3,028 -> 536), a quarter of the pixels
/// changes it by 0.2 ms (`renderScale 0.5`: 32.1 -> 31.9), and our own schedules sit still at
/// ~13 ms. So it follows entity count and nothing else - and `present` is not the suspect either,
/// because `render_system` does `queue.submit` AND `present_frames` inside `RenderSystems::Render`
/// (`bevy_render-0.18.1/src/lib.rs:495`), which `r_render` already measures at ~2 ms.
///
/// A hypothesis with a number attached, not a fix: if this reads ~20 ms the search is over, and
/// if it reads nothing the residual is the browser's own gap and the next question is a different
/// one entirely.
static POSTCLEAN_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// **Named sets inside the big tiles**, bracketed on both sides rather than chained.
///
/// Journal 54 is the cleanest measurement this project has taken: camera at the floor, every
/// setting at zero, the network off, **36 visible meshes** - and the frame is still 26.5 ms, of
/// which the MAIN schedules are 19.80 ms (75%), the render schedule 4.08 and the browser 1.30. So
/// the ceiling is per-entity CPU over 34,640 entities, 0.57 us each per frame, and it does not
/// care what is on screen. That is why the camera, `renderScale`, the draw count and the network
/// all failed to move it.
///
/// What those 19.8 ms do NOT say is WHICH systems. The five tiles that hold them are upper bounds
/// on a slice, not measurements of a set - `u_net` reading 3.98 ms with zero packets is the proof
/// - because a mark pinned only on one side floats, and the schedule dump showed `phase_mark<8>`
/// and `<9>` with **116 systems between them** while their only constraint was `Propagate`.
///
/// So these five take a set each and bracket it: `open` before, `close` after, its own slot. No
/// `.chain()` between them on purpose - chaining marks across sets whose real order differs from
/// the assumed one is a scheduler CYCLE, which is a panic in the owner's build, and the ordering
/// is exactly what is not known yet. Each still carries the usual caveat: a system in no set can
/// float inside a bracket, so a slot is an upper bound on its set - but a far tighter one than a
/// tile holding 116 systems.
/// **The player-UI bridge**, slots 5..=7 — the three sets `Update` already has.
///
/// The Update dump settled where the frame goes: **733 systems**, with `phase_mark<5>` at index
/// 278 and `<6>` at 461, so `u_net` is not the network at all — it is the FIRST 278 SYSTEMS, and
/// `u_input` the next 182. Reading their names, indices 159-247 are almost solidly `feed_*` and
/// 335-434 almost solidly `drain_*`: the crate defines **101 `feed_*` and 89 `drain_*`** systems,
/// every one of them pushing game state into the Lua VM or pulling a verb back out, every frame.
///
/// That is the shape of everything measured today — a cost that does not care about the camera,
/// the pixels, the draw count or the network, because none of those are what it iterates.
///
/// `UiFeed` (53 members), `UnitFeed` (31) and `UiInput` (2) already exist as sets, so this costs
/// no change to 190 registrations to find out.
/// **Nine brackets sit mid-row and two more do not.** Slots 0-8 are written by the loop in the
/// middle of the row, where the `px_*` columns are; growing that loop would shift `gate_n` and
/// everything after it, which this file has already had happen once and which splits every row a
/// reader checks against the header. So `MIDROW` stays 9 for ever and anything added lands at the
/// END, where columns only ever grow.
/// **What the asset pump actually moved** - the `aev` column, asset events seen in one second.
///
/// `px_asstrk` is 99.6% of `s_pre` but it is an upper bound on a bracket holding TWO systems:
/// `handle_internal_asset_events`, which pumps pending loads and drops, and `AssetTrackingSystems`.
/// They cannot be separated by a mark - the pump is `ambiguous_with_all` and in no named set - so
/// this measures the pump's OUTPUT instead. Thousands of events in the spiking seconds is the pump;
/// a flat count while `s_pre` swings seventeen thousandfold is not.
static ASSET_EVENTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// **Two systems of the `s_upd` residual, timed from INSIDE them.**
///
/// That residual reached 323,236 us in the owner's crowd second while the unit feed was 36 ms and
/// the auras 6, so the cost is in the rest of `Update` - which the schedule dump says is 151
/// systems. These two are what a crowd ARRIVING makes expensive: building each new entity's visual
/// tree, and driving every rig's animation.
///
/// Timed inside rather than bracketed outside because bracketing one by name needs its module, its
/// function, its `SystemParam` and every marker type in it to be `pub(crate)`: four privacy edits
/// deep the chain had not ended, which is the point at which to stop widening and measure from
/// within. `maintain_water_index` and the entry load's steps are already timed this way.
static ATTACH_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DRIVE_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A drop guard that reports a system's own wall time; one line at the top of the system is the
/// whole instrument, and it fires on every exit including an early return.
pub(crate) struct SysTimer(bevy::platform::time::Instant, fn(u64));

impl SysTimer {
    pub(crate) fn new(sink: fn(u64)) -> Self {
        Self(bevy::platform::time::Instant::now(), sink)
    }
}

impl Drop for SysTimer {
    fn drop(&mut self) {
        (self.1)(self.0.elapsed().as_micros() as u64);
    }
}

/// One run of `attach_entity_visuals`.
pub(crate) fn note_attach(us: u64) {
    ATTACH_US.fetch_add(us, std::sync::atomic::Ordering::Relaxed);
}

/// One run of `drive_animations`.
pub(crate) fn note_drive(us: u64) {
    DRIVE_US.fetch_add(us, std::sync::atomic::Ordering::Relaxed);
}

/// Count this frame's asset events for the three types that stream: the pump produced them.
fn count_asset_events(
    mut images: bevy::ecs::message::MessageReader<bevy::asset::AssetEvent<bevy::image::Image>>,
    mut meshes: bevy::ecs::message::MessageReader<bevy::asset::AssetEvent<bevy::prelude::Mesh>>,
    mut mats: bevy::ecs::message::MessageReader<
        bevy::asset::AssetEvent<benilla_assets::materials::WowModelMaterial>,
    >,
) {
    let n = (images.read().count() + meshes.read().count() + mats.read().count()) as u64;
    if n > 0 {
        ASSET_EVENTS.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }
}

const MIDROW: usize = 9;
const NSETS: usize = 13;
static SET_US: [std::sync::atomic::AtomicU64; NSETS] = [ZERO; NSETS];

/// Where each bracket's `open` leaves its timestamp. A resource rather than a static because it
/// is written every frame from the main world and read by its own pair only.
#[derive(Resource, Default)]
struct SetClocks([Option<Instant>; NSETS]);

fn set_open<const N: usize>(mut c: ResMut<SetClocks>) {
    c.0[N] = Some(Instant::now());
}

fn set_close<const N: usize>(mut c: ResMut<SetClocks>) {
    if let Some(t) = c.0[N].take() {
        SET_US[N].fetch_add(t.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Close [`POSTCLEAN_US`]; the clock then runs on into [`RBETWEEN_US`] as before.
fn rmark_postclean(mut clock: ResMut<RClock>) {
    use std::sync::atomic::Ordering::Relaxed;
    let now = Instant::now();
    POSTCLEAN_US.fetch_add((now - clock.0).as_micros() as u64, Relaxed);
    clock.0 = now;
}

/// Start the render app's chain, closing [`RBETWEEN_US`] - everything since the previous frame's
/// `Cleanup`, which is the main schedules, ExtractSchedule, present and the browser's idle
/// together. Subtracting the five `s_*` tiles leaves exactly the pair in question, and that is
/// deliberately the SAME quantity `rapp` minus the seven named tiles gives, reached from the
/// other end. Two routes to one number: if they disagree the instrument is wrong, not the world.
///
/// It used to discard this span, which is how 78% of `rapp` went unnamed.
fn rmark_open(mut clock: ResMut<RClock>) {
    use std::sync::atomic::Ordering::Relaxed;
    let now = Instant::now();
    RBETWEEN_US.fetch_add((now - clock.0).as_micros() as u64, Relaxed);
    // ...and the part of it that is `ExtractSchedule`: `Last` finished, this is the start of the
    // render schedule, and only extract runs in between. `present + idle` is then
    // `r_between - (the five s_* tiles) - r_xsched`, every term named.
    if let Some(end) = *MAIN_END
        .lock()
        .expect("MAIN_END is never poisoned: single-threaded, no panic inside")
    {
        XSCHED_US.fetch_add((now - end).as_micros() as u64, Relaxed);
    }
    clock.0 = now;
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

/// Microseconds inside `ui_script::tick_script`, the Lua VM's own tick — timed from inside the
/// system because a scheduler bracket around it measured 32.77 ms of a 34.27 ms frame.
static VMTICK_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Called by the tick's own drop guard.
pub(crate) fn note_vm_tick(us: u64) {
    VMTICK_US.fetch_add(us, std::sync::atomic::Ordering::Relaxed);
}

/// Meshes that SURVIVE the cull, beside the ones that merely exist.
///
/// The archetype census counts what is spawned; `meshes` counts what `Assets<Mesh>` holds. Neither
/// says how many are handed to the renderer, and that is the number a draw-call argument needs.
/// Journal 50 stands at ~13,500 mesh entities against journal 48's ~8,000, with present+idle
/// 34.2 ms against 13.1 - which READS as draw-bound and is not evidence of it: if the cull already
/// throws most of them away, the two counts diverge and the argument is about the wrong quantity.
///
/// `ViewVisibility` is what `check_visibility` writes and what the extract then reads, so it is
/// the last main-world hop before a mesh becomes a draw call - the honest place to count. One
/// query pass over the mesh entities per frame, in `Last`, beside the two counters already there.
static MESH_VIS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MESH_ALL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// **Rigs posed against rigs parked** — the `rigs_live` and `rigs_park` columns.
///
/// The owner's reading of his own screen: a camera pointed at the floor should not cost 30 fps,
/// so something works outside what the camera can see. The counts say he is right - journal 60
/// stands at **4,210 bone-anchor writes a frame with 35 visible meshes** - and the mechanism is
/// in `doodad_anim`'s parking rule, which asks
///
///     vis.get(e).is_ok_and(|v| *v != Visibility::Hidden)
///
/// `Visibility` is "allowed to be seen"; `ViewVisibility` is "survived this frame's frustum".
/// A doodad behind the camera keeps `Visibility::Inherited`, reads as drawn, and its rig is posed
/// every frame. `mesh_all` 2,093 against `mesh_vis` 35 is exactly that gap.
///
/// Counted before it is fixed, because a fix whose size nobody measured is how eight hypotheses
/// died this week.
static RIGS_LIVE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RIGS_PARK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// **Archetypes and allocated entity ids** — the `arch` and `ent_alloc` columns.
///
/// Journal 63 measured a real drift: standing still, **+3.08 ms per 100 s**, taking 30.1 fps to
/// 22.2 over ten minutes, while `entities` (37,295 -> 37,269), `rigs_live` (781 -> 768), `mats`
/// (5,746 -> 5,767) and `mesh_vis` (35) all held. Everything slowed in proportion - `px_anim`
/// 3.19 -> 4.49 keeps its ~10% share of the frame - so it is not one system getting slower, it is
/// the whole runtime.
///
/// That is not the shape of a leak of OBJECTS, whose count would grow. It is the shape of
/// iteration getting more expensive over the same objects, and in an ECS the usual cause is
/// archetype fragmentation: components inserted and removed every frame - `AnimParked`,
/// `ParkedMesh`, `HiddenFrames` - move entities between archetypes, and every query walks the
/// archetype list. The census has read 254, then 323, then 395 in one session.
///
/// `ent_alloc` is beside it because id churn has the same effect on the entity meta table and is
/// invisible to the live `entities` count.
fn count_world_shape(world: &World) {
    use std::sync::atomic::Ordering::Relaxed;
    ARCH.store(world.archetypes().len() as u64, Relaxed);
    ENT_ALLOC.store(u64::from(world.entities().len()), Relaxed);
}

static ARCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ENT_ALLOC: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn count_rigs(
    rigs: Query<Has<benilla_world::rig_anim::AnimParked>, With<benilla_world::rig_anim::RigPose>>,
) {
    use std::sync::atomic::Ordering::Relaxed;
    let (mut live, mut park) = (0u64, 0u64);
    for parked in &rigs {
        if parked {
            park += 1;
        } else {
            live += 1;
        }
    }
    RIGS_LIVE.fetch_add(live, Relaxed);
    RIGS_PARK.fetch_add(park, Relaxed);
}

fn count_visible_meshes(
    meshes: Query<&bevy::camera::visibility::ViewVisibility, With<Mesh3d>>,
) {
    use std::sync::atomic::Ordering::Relaxed;
    let (mut vis, mut all) = (0u64, 0u64);
    for v in &meshes {
        all += 1;
        if v.get() {
            vis += 1;
        }
    }
    MESH_VIS.fetch_add(vis, Relaxed);
    MESH_ALL.fetch_add(all, Relaxed);
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
/// Fed from `trace_stream` in a `dev` build and from [`note_stream_activity`] otherwise - and
/// THAT is why these columns were empty. `trace_stream` lives in `PerfPlugin`, which is
/// `#[cfg(feature = "dev")]` whole, while this journal is registered in every build. The browser
/// build carries no `dev`, so the one system that published these numbers did not exist on the
/// one target that ships the journal reading them.
///
/// Nanoseconds; see [`note_stream_ms`] for why not microseconds.
static STREAM_NS: [std::sync::atomic::AtomicU64; 5] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Called once a frame with the chain's own split, in milliseconds as the streamer keeps them.
///
/// **Accumulated in NANOSECONDS**, because the obvious `(ms * 1000.0) as u64` truncated each
/// frame's contribution to whole microseconds BEFORE adding it: a stage costing 0.4 us a frame
/// added 0 for ever, however long the run. That is the second half of the same defect
/// [`take_stream_us`] carries the first half of, and fixing only the division would have left
/// these columns reading 0 for exactly the same reason.
pub(crate) fn note_stream_ms(parts: [f32; 5]) {
    use std::sync::atomic::Ordering::Relaxed;
    for (cell, ms) in STREAM_NS.iter().zip(parts) {
        cell.fetch_add((f64::from(ms) * 1_000_000.0) as u64, Relaxed);
    }
}

/// **The second's TOTAL microseconds, not a per-frame average** - and that is a correction.
///
/// These five divided by the frame count, like the phase tiles above them, and read **0 in every
/// row of every journal they have ever appeared in**. Not because nothing was measured: integer
/// division floors anything under 1 us per frame to nothing, and the streamer's own chain is
/// exactly that cheap. A 40 us second over 50 frames is 0.
///
/// Journal 83 is where that cost a reading. `u_stream` is 1.55 ms a frame and the second strongest
/// correlate of the frame in a crowd (r = +0.868); these five exist to say what of it is the
/// streamer, and they answered "nothing" in a spelling indistinguishable from "not measured". This
/// file already says the difference matters, three hundred lines up, about `ui_us`: "unmeasured and
/// free are different claims".
///
/// `col_us` beside them is a second's total (`take_collider_build_micros`) and reads 166,850 in the
/// same rows these read 0. Matching it makes the two comparable and keeps a cheap lane legible: a
/// 40 us second now prints 40.
/// **The non-`dev` half of the ownership, so these columns have a publisher in the build that
/// actually reads them.** Exactly one system takes [`StreamActivity`] per frame in either build:
/// `trace_stream` under `dev`, this otherwise. Two would each see half the frame's numbers.
///
/// Nothing else consumed the resource in a non-`dev` build - `any_event` is read only inside
/// `trace_stream` - so before this it simply accumulated, unread, for the life of the session.
#[cfg(not(feature = "dev"))]
fn note_stream_activity(mut activity: ResMut<benilla_world::terrain_stream::StreamActivity>) {
    let a = std::mem::take(&mut *activity);
    note_stream_ms([
        a.stream_ms,
        a.furnish_ms,
        a.mfurnish_ms,
        a.spawn_ms,
        a.collider_ms,
    ]);
}

fn take_stream_us() -> [u64; 5] {
    use std::sync::atomic::Ordering::Relaxed;
    let mut out = [0u64; 5];
    for (slot, cell) in out.iter_mut().zip(&STREAM_NS) {
        *slot = cell.swap(0, Relaxed) / 1_000;
    }
    out
}

impl Plugin for FpsJournalPlugin {
    fn build(&self, app: &mut App) {
        super::upload_budget::plugin(app);
        // The page-side way back from `/console uiLua 0`; see `journal_web::install_ui_lua_hook`.
        #[cfg(target_arch = "wasm32")]
        web::install_ui_lua_hook();
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
                    rmark::<6>.after(RS::Cleanup).before(RS::PostCleanup),
                    rmark_postclean.after(RS::PostCleanup),
                )
                    .chain(),
            );
        }
        // The GPU meter's own plugin, in a non-`dev` build only: `PerfPlugin` installs it under
        // `dev`, and two installs would mean two sentinel pass pairs around one camera.
        #[cfg(not(feature = "dev"))]
        crate::perf::gpu::plugin(app);
        app.add_plugins(RenderDiagnosticsPlugin)
            .init_resource::<SchedStart>()
            // First of `First` and last of `Last`: the main schedule end to end, with nothing of
            // the render app in it. Two `Instant` reads a frame whether the journal is on or
            // off - the same price the frame-time window already pays.
            .add_systems(bevy::app::First, sched_open)
            .add_systems(
                bevy::app::Last,
                (
                    count_moved,
                    count_visible_meshes,
                    count_rigs,
                    count_world_shape,
                    sched_close,
                )
                    .chain(),
            )
            // The streamer's five timers, published before the row that prints them. Only in a
            // non-`dev` build: `trace_stream` owns the take under `dev`, and two owners would
            // each see half the frame. See `note_stream_activity`.
            .add_systems(
                bevy::app::Last,
                #[cfg(not(feature = "dev"))]
                note_stream_activity.before(sched_close),
                #[cfg(feature = "dev")]
                || {},
            )
            // The armed one-shot dump; see `dumpSchedule` in `on_cvar` for why it cannot run
            // from the observer itself. Costs one atomic read per frame when disarmed.
            .add_systems(bevy::app::Last, crate::perf::sched_dump::dump_if_armed)
            .init_resource::<SetClocks>()
            // `PreUpdate`'s two bracketable residents; see the write site for why the third, and
            // likeliest, is not one of them.
            .add_systems(
                bevy::app::PreUpdate,
                (
                    set_open::<9>.before(bevy::input::InputSystems),
                    set_close::<9>.after(bevy::input::InputSystems),
                    // **`.after(run_pending_entry_load)`, and that is the fix for this bracket.**
                    // With only `.before(AssetTrackingSystems)` the scheduler put `set_open::<10>`
                    // at POSITION 0 of `PreUpdate` - the dump says so - so it measured the whole
                    // schedule and read 99.6% of `s_pre`, which was then reported as "the asset
                    // lane is the freeze". It was "the bracket is everything".
                    set_open::<10>
                        .after(crate::ui_script::lifecycle::run_pending_entry_load)
                        .before(bevy::asset::AssetTrackingSystems),
                    set_close::<10>.after(bevy::asset::AssetTrackingSystems),
                    // The UI load, ours, and the one named thing in `PreUpdate` big enough to be
                    // seconds: it runs the client's own XML and Lua through the VM.
                    set_open::<11>.before(crate::ui_script::lifecycle::run_pending_entry_load),
                    set_close::<11>.after(crate::ui_script::lifecycle::run_pending_entry_load),
                    // After the bracket, so it sees what the pump produced this frame.
                    count_asset_events.after(bevy::asset::AssetTrackingSystems),
                ),
            )
            // The player-UI bridge, in `Update`. Separate from the PostUpdate group and never
            // chained to it: they are different schedules, and an ordering edge across the two
            // is meaningless to the scheduler and misleading to a reader.
            .add_systems(
                bevy::app::Update,
                (
                    set_open::<5>.before(crate::ui_script::UiFeed),
                    set_close::<5>.after(crate::ui_script::UiFeed),
                    set_open::<6>.before(crate::ui_unit::UnitFeed),
                    set_close::<6>.after(crate::ui_unit::UnitFeed),
                    set_open::<7>.before(crate::ui_script::UiInput),
                    set_close::<7>.after(crate::ui_script::UiInput),
                    // **One system, not a set.** `px_unitfeed` read 4.79 ms against
                    // `px_uifeed`'s 4.87 - nearly equal, for a 39-member sub-phase inside a
                    // 53-member parent - which is the arithmetic saying the bracket is swallowing
                    // its neighbours again, exactly as the nested PostUpdate brackets inflated
                    // `CheckVisibility` sevenfold. 4.79 ms over 39 systems that SKIP would be
                    // 123 us apiece, which is not believable.
                    //
                    // A bracket around a single named system has almost nothing to float into it,
                    // so this is the honest per-system price of entering a gated feed and leaving
                    // it: bevy builds the params before the body can decide, and `feed_units`
                    // additionally calls `FieldEdges::collect` above its own gate. Multiply it by
                    // the 190 `feed_*`/`drain_*` systems for the ceiling a set-level run
                    // condition could buy.
                    // Through the SET, not the function: `feed_auras` and its `SystemParam` are
                    // private, and opening three types to bracket one system is a worse trade than
                    // bracketing the set it already declares.
                    set_open::<12>.before(crate::ui_aura::AuraEvents),
                    set_close::<12>.after(crate::ui_aura::AuraEvents),
                    set_open::<8>.before(crate::ui_unit::feed_units),
                    set_close::<8>.after(crate::ui_unit::feed_units),
                    // **The VM's own tick, alone.** `/console uiLua 0` took 10.9 ms off the MAIN
                    // schedules (52.35 -> 41.44) while `ui_us`, the interface's DRAWING, is only
                    // 1.70 of it - so about nine milliseconds belong to the bridge and this tick
                    // together, and the switch cannot tell them apart because it stops both.
                    // A bracket on one named system can: `px_feedunits` reads 0.00 for a single
                    // feed, so if this reads most of the nine, the 190 systems are not the story
                    // and the Lua frame tree is.

                ),
            )
            .add_systems(
                bevy::app::PostUpdate,
                (
                    set_open::<0>.before(bevy::app::AnimationSystems),
                    set_close::<0>.after(bevy::app::AnimationSystems),
                    set_open::<1>.before(bevy::asset::AssetEventSystems),
                    set_close::<1>.after(bevy::asset::AssetEventSystems),
                    set_open::<2>.before(bevy::transform::TransformSystems::Propagate),
                    set_close::<2>.after(bevy::transform::TransformSystems::Propagate),
                    set_open::<3>.before(bevy::camera::visibility::VisibilitySystems::CalculateBounds),
                    set_close::<3>.after(bevy::camera::visibility::VisibilitySystems::CalculateBounds),
                    set_open::<4>.before(bevy::camera::visibility::VisibilitySystems::CheckVisibility),
                    set_close::<4>.after(bevy::camera::visibility::VisibilitySystems::CheckVisibility),
                )
                    // **Chained now, and only now.** The first cut left these unordered against
                    // each other deliberately: chaining across sets whose real order is unknown is
                    // a scheduler cycle, which is a panic in the owner's build. Journal 55 then
                    // showed the five summing to 106% of the MAIN tiles - overlapping, so only
                    // their RANKING was usable - and `dumpSchedule` had meanwhile printed
                    // PostUpdate's true order: animation at 31-43, asset events at 65-100,
                    // transform propagation at 134-136, then the two visibility passes. Chaining
                    // in the order the schedule itself reports adds no constraint the scheduler
                    // does not already meet, so it cannot cycle, and it makes the brackets
                    // disjoint instead of nested.
                    .chain(),
            )
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

/// **The FORCED lane's own pair**, split out because one meter over two lanes cannot be read.
///
/// Since the composite moved off the thread there are two kinds of main-thread skin cost, and they
/// answer opposite questions: `skin_us` is landing a finished atlas (a decode and an upload, which
/// is what is LEFT), while this is a composite the client had to do here and now - the previews,
/// a re-dress of a standing body, a rig heal - which the reference forces too (`0x44ad50`).
///
/// They were one meter for an afternoon and it voided a measurement: two runs of the SAME setting
/// read 4,045 us and 108,610, because the second happened to force more composites. The spread was
/// not noise and not the lane; it was the question being ambiguous.
static SKINS_FORCED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static SKIN_FORCED_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One composite the caller could not wait for; see [`SKINS_FORCED`].
pub(crate) fn note_skin_forced(micros: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    SKINS_FORCED.fetch_add(1, Relaxed);
    SKIN_FORCED_US.fetch_add(micros, Relaxed);
}

fn take_skin_forced() -> (u32, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (SKINS_FORCED.swap(0, Relaxed), SKIN_FORCED_US.swap(0, Relaxed))
}

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

/// **`ui_ex_us`: the UI's other half.** `ui_us` is tick + resolve + measure; the extract walk,
/// its conversion to quads and the diff against last frame were never in it, so the 10.9 ms that
/// `uiLua 0` took off the main schedule against `ui_us` 1.70 had nowhere to be named. Per UI frame,
/// empty when no frame reported, as `ui_us`.
static UI_EX_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UI_EX_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// **`fu_us`/`fu_open`/`au_us`: `feed_units` and `feed_auras` timed from inside.** The
/// `px_feedunits` bracket read ~4 ms a frame at 1,200 rigs in journal 99 after c0bb7ca6 narrowed
/// the gate to four units - so either the gate still opens every frame, or the bracket floats over
/// neighbours, which a `before`/`after` pair cannot rule out. A guard over the body answers the
/// first; the open count says which. Per frame, like the tiles; `fu_open` is frames per second.
pub(crate) static FU_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static FU_OPEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static AU_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Adds its own lifetime to a counter on drop, so every early return is timed too.
pub(crate) struct SpanGuard(
    &'static std::sync::atomic::AtomicU64,
    bevy::platform::time::Instant,
);

impl SpanGuard {
    pub(crate) fn new(counter: &'static std::sync::atomic::AtomicU64) -> Self {
        Self(counter, bevy::platform::time::Instant::now())
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        self.0.fetch_add(
            self.1.elapsed().as_micros() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

pub(crate) fn note_ui_extract_micros(micros: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    UI_EX_US.fetch_add(micros, Relaxed);
    UI_EX_FRAMES.fetch_add(1, Relaxed);
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
    // `fx_live` needs the emitter itself, not a marker: `emitters` says how many effects are
    // running and this says how big they are. The two can move opposite ways, and a budget - the
    // only lever that bounds the MIDDLE of a fight, where every distance wall is looking at
    // something next to the camera - would cap this one and not that one.
    emitters: Query<&benilla_world::particles::ParticleEmitter>,
    // **Live effect MODELS**, against `fx_kits`' "effects started this second". A spell visual is
    // an M2 with a rig of its own - `spell_fx` calls `spawn_joints`, one entity per bone - so
    // twenty people casting at once is not twenty entities, and this is the only column that
    // could say so. The marker rides the root `attach_effect_visuals` builds, so this counts
    // spell visuals, projectiles and ground effects and not their parts.
    fx_models: Query<(), With<crate::entities::spell_fx::EffectModel>>,
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
    // **The frame counter outside the interface.** The in-game readout is drawn by the Lua UI, so
    // it goes dark exactly when `/console uiLua 0` switches that off - which is the one
    // measurement the switch exists to take. This one lives in the page, survives it, and costs a
    // single `textContent` write a second.
    #[cfg(target_arch = "wasm32")]
    {
        use std::sync::atomic::Ordering::Relaxed;
        // **The two numbers that decide the next fix, read off the screen instead of a journal.**
        // `/console uiLua 0` is worth ~11 ms, of which the interface's DRAWING is 1.7 - so nine
        // belong to the VM tick and the 190-system bridge together, and the switch stops both at
        // once. Showing the tick alone says which of the two to fix, and the owner has downloaded
        // enough journals.
        //
        // Peeked, not consumed: the row still takes these through its own `swap`.
        // `v.len()` is this window's frame count - the same divisor the row uses, and the only
        // one in scope here.
        let n = v.len().max(1) as f32;
        let vm = VMTICK_US.load(Relaxed) as f32 / 1000.0 / n;
        let ui = UI_US.load(Relaxed) as f32 / 1000.0 / n;
        web::fps(&format!(
            "{:.1} fps   {:.1} ms
vm {:.1}   ui {:.1}",
            1000.0 / mean.max(0.001),
            mean,
            vm,
            ui
        ));
    }
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
    // One walk for both: the count of running effects and the particles they are carrying. The
    // ceiling `MAX_PARTICLES` enforces is per EMITTER, so these two are independent - three
    // hundred small emitters and thirty large ones are the same `emitters` and very different
    // `fx_live`, and only the second is what a shared budget would have to bound.
    let (fx_emitters, fx_live) = emitters
        .iter()
        .fold((0u32, 0u64), |(n, live), e| (n + 1, live + e.live() as u64));
    line.push_str(&format!(
        ",{},{fx_emitters},{fx_live},{}",
        benilla_world::terrain_stream::take_collider_build_micros(),
        fx_models.iter().count()
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
    // The streamer's own split of `u_stream` - see `STREAM_NS`. Totals for the second, like
    // `col_us`, not per-frame averages: see `take_stream_us` for the rows that cost.
    for us in take_stream_us() {
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
    // The last column, and it must STAY last: `RBETWEEN_US` is held apart from `RAPP_US` exactly
    // so that naming this span appends instead of shifting everything after `rapp`. Per frame,
    // like every other tile here.
    {
        use std::sync::atomic::Ordering::Relaxed;
        let _ = write!(line, ",{}", RBETWEEN_US.swap(0, Relaxed) / frames.max(1));
        let _ = write!(line, ",{}", XSCHED_US.swap(0, Relaxed) / frames.max(1));
        // Per frame, like every tile: these are populations, not events.
        let f = frames.max(1);
        let _ = write!(
            line,
            ",{},{}",
            MESH_VIS.swap(0, Relaxed) / f,
            MESH_ALL.swap(0, Relaxed) / f
        );
        let _ = write!(line, ",{}", POSTCLEAN_US.swap(0, Relaxed) / f);
        for cell in &SET_US[..MIDROW] {
            let _ = write!(line, ",{}", cell.swap(0, Relaxed) / f);
        }
        // Gated feeds reached, and those the gate let through — per frame, like the tiles.
        let _ = write!(
            line,
            ",{},{}",
            crate::ui_script::gate::GATES.swap(0, Relaxed) / f,
            crate::ui_script::gate::GATES_OPEN.swap(0, Relaxed) / f
        );
        let _ = write!(
            line,
            ",{},{}",
            RIGS_LIVE.swap(0, Relaxed) / f,
            RIGS_PARK.swap(0, Relaxed) / f
        );
        // Populations, not rates: these are the world's shape at the moment the row was written.
        let _ = write!(
            line,
            ",{},{}",
            ARCH.load(Relaxed),
            ENT_ALLOC.load(Relaxed)
        );
        // **Last, and it has to stay last.** Written before the `SET_US` loop, this
        // line shifted every `px_*` column one place and `px_vmtick` printed
        // `ent_alloc` - 32,768 over a thousand, the same 32.77 "ms" in two captures
        // whose frames differed, and a power of two rather than a duration.
        let _ = write!(line, ",{}", VMTICK_US.swap(0, Relaxed) / f);
    }
    // The forced-composite pair, appended after it for the same reason that one is last: columns
    // only ever grow at the end here, so every journal already recorded keeps parsing. See
    // `SKINS_FORCED` for why it is not folded into `skin_us`.
    {
        let (n, us) = take_skin_forced();
        let _ = write!(line, ",{n},{us}");
    }
    // **The two `PreUpdate` brackets**, at the end of the row (see `MIDROW`).
    //
    // `s_pre` reached 2,852,155 us in one second of the owner's journal 86 and 2,565,728 in a run
    // here, against ~213 calm, and nothing could say what inside it. This workspace puts four
    // systems in `PreUpdate` and one of them, the water index, has been measured and cleared
    // (`wix_us`, 1,755 us over a whole run). So the cost is bevy's own, and bevy puts three things
    // there: `InputSystems`, `AssetTrackingSystems`, and `handle_internal_asset_events` - an
    // EXCLUSIVE system taking `&mut World` that pumps every pending asset load and drop, which is
    // exactly the work a player riding into a crowded place creates.
    //
    // That one cannot be bracketed: it is `ambiguous_with_all` and in no named set. The other two
    // can, so `s_pre - px_input - px_asstrk` is what it costs, by subtraction - the same shape the
    // `r_*` tiles use for `ExtractSchedule` and present.
    //
    // **Per FRAME, divided like the tiles they are compared against.** Written as second-totals
    // first, which made `px_asstrk` read 517% of `s_pre` and the residual negative - the one
    // arithmetic that cannot be true, and the reason the residual is computed at all.
    {
        use std::sync::atomic::Ordering::Relaxed;
        let per = frames.max(1);
        let _ = write!(
            line,
            ",{},{}",
            SET_US[9].swap(0, Relaxed) / per,
            SET_US[10].swap(0, Relaxed) / per
        );
        let _ = write!(line, ",{}", SET_US[11].swap(0, Relaxed) / per);
        // Two members of `UnitFeed`, bracketed by name. The set's own bracket reached 160,590 us
        // in one second of the owner's journal 88 against ~3,000 calm, while `feed_units` - the
        // only member with a bracket until now - was 4,532 of it. So the cost is in the other
        // thirty, and these two are the ones a crowd can plausibly make expensive: every held
        // unit's aura list, and the hover scan.
        let _ = write!(line, ",{}", SET_US[12].swap(0, Relaxed) / per);
        // **Per FRAME, like every tile they are read against.** Written as second-totals first,
        // which made `px_attach` read 135% of `s_upd` with a negative residual - the identical
        // mistake `px_asstrk` made two days ago, in the same file, after I had written the comment
        // explaining it. The residual is computed on purpose; it is what catches this.
        let _ = write!(
            line,
            ",{},{}",
            ATTACH_US.swap(0, Relaxed) / per,
            DRIVE_US.swap(0, Relaxed) / per
        );
    }
    // What the pump moved this second; see `ASSET_EVENTS`. A TOTAL, not a per-frame average: the
    // question is how much work arrived, not how it was spread.
    let _ = write!(
        line,
        ",{}",
        ASSET_EVENTS.swap(0, std::sync::atomic::Ordering::Relaxed)
    );
    // The water index's own rebuild cost - see `benilla_world::liquid::WATER_INDEX_US`.
    let _ = write!(
        line,
        ",{}",
        benilla_world::liquid::take_water_index_micros()
    );
    // `ui_ex_us`, per UI frame; empty when unmeasured (see `UI_EX_US`).
    {
        use std::sync::atomic::Ordering::Relaxed;
        let total = UI_EX_US.swap(0, Relaxed);
        match UI_EX_FRAMES.swap(0, Relaxed) {
            0 => line.push(','),
            n => {
                let _ = write!(line, ",{}", total / n);
            }
        }
    }
    // `fu_us`, `fu_open`, `au_us` (see `FU_US`).
    {
        use std::sync::atomic::Ordering::Relaxed;
        let per = frames.max(1);
        let _ = write!(
            line,
            ",{},{},{}",
            FU_US.swap(0, Relaxed) / per,
            FU_OPEN.swap(0, Relaxed),
            AU_US.swap(0, Relaxed) / per
        );
        // `flat_us`: `benilla_world::rig_flat`'s two systems, per frame.
        #[cfg(not(target_os = "macos"))]
        let _ = write!(line, ",{}", benilla_world::rig_flat::take_micros() / per);
        #[cfg(target_os = "macos")]
        line.push(',');
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
    // **The guard that shifted column earned.** A row whose width disagrees with the
    // header is not slightly wrong: every column past the break reads its neighbour, and
    // it reads as data. That is how `px_vmtick` reported 32.77 ms twice before it was
    // doubted.
    debug_assert_eq!(
        line.matches(',').count(),
        JOURNAL_HEADER.trim_end().matches(',').count(),
        "journal row width does not match the header"
    );
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
