//! The FPS JOURNAL — `/console fpsJournal 1` in any build, or `WOW_FPS_JOURNAL=<csv path>` on a
//! harness run: once a second, append one row of where the player is, what the frame cost on
//! the wall, on the CPU and on the GPU, and **what is resident** (see [`JOURNAL_HEADER`] for the
//! column order, written into every fresh file) — the "where does it dip" instrument for a
//! director-driven run, and since decision 2008 the instrument a PLAYER on hardware we do not
//! own can run for us. They play normally; the journal turns "it drops in the Dwarven District"
//! into coordinates a headless probe can tele straight back to, and its GPU columns turn a Steam
//! Deck's "GPU 94 % busy" into which pass is eating it. Negligible cost: one line of IO a second,
//! samples reused from the frame meters.
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

/// The `fpsJournal` CVar's knob (2008): on, the journal appends to the player's
/// `Diagnostics/fps-journal.csv` from the next second on; off, it stops mid-run and the file
/// keeps what it has. `/console fpsJournal 1` is the whole recipe a reporter needs.
#[derive(Resource, Default)]
pub(crate) struct FpsJournalSetting(pub(crate) bool);

/// The journal's column order, written as the first line of a fresh file (after the `#` adapter
/// line). Appended-to files keep whatever header they were created with — the columns only ever
/// grow at the end, so an older journal still parses against its own header.
const JOURNAL_HEADER: &str = "t,x,y,z,mean_ms,p95_ms,streamed,entities,cpu_ms,mats,meshes,images,\
                              m2,uv,tint,pmat,emat,skin,cmat,tex,cgeo,evicted,fx,fy,fz,main_ms,\
                              gpu_ms,gpu_opaque,gpu_static,gpu_transp,gpu_glow,gpu_post,gpu_ui,\
                              gpu_other,lua_errs,lua_err_us,msg_hashed,ui_us,col_us,emitters,fx_kits,fx_impacts,net_pkts,net_us,pipes,\
                              rscale,farclip,\
                              skins_new,skin_us,tex_hit,tex_dec,\
                              rcpu_ms,rcpu_opaque,rcpu_static,rcpu_transp,rcpu_glow,rcpu_post,rcpu_ui,rcpu_other,sched_us\n";

/// The FPS journal switch's change callback (2008, 2303): a flag, the client's int-parse +
/// `!= 0`. The journal system reads the knob every frame, so the file opens on the next second
/// and closes the second it is turned off.
pub(crate) fn on_cvar(
    ev: On<crate::cvars::CvarChanged>,
    mut journal: ResMut<FpsJournalSetting>,
    mut ui_cost: ResMut<crate::ui_script::UiCostWanted>,
) {
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
fn take_sched_us() -> Option<u64> {
    use std::sync::atomic::Ordering::Relaxed;
    let frames = SCHED_FRAMES.swap(0, Relaxed);
    let us = SCHED_US.swap(0, Relaxed);
    (frames > 0).then(|| us / frames)
}

impl Plugin for FpsJournalPlugin {
    fn build(&self, app: &mut App) {
        // bevy's per-pass render diagnostics — the source of the GPU columns, and (under the
        // `tracy` feature) the hook Tracy's GPU zones ride. Present in every build: its per-frame
        // cost is one query resolve and one buffer map on the render thread, and a player's
        // journal is exactly the build that has to carry it (2008).
        app.add_plugins(RenderDiagnosticsPlugin)
            .init_resource::<SchedStart>()
            // First of `First` and last of `Last`: the main schedule end to end, with nothing of
            // the render app in it. Two `Instant` reads a frame whether the journal is on or
            // off - the same price the frame-time window already pays.
            .add_systems(bevy::app::First, sched_open)
            .add_systems(bevy::app::Last, sched_close)
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
    /// Where rows go while the journal is on; `None` = off, or nowhere to write (a hermetic
    /// run has no state folder, and the CVar has no other place to point).
    path: Option<PathBuf>,
    window: Vec<f32>,
    last_flush: f32,
    /// Process CPU seconds at the previous flush — the row's `cpu_ms` is this second's CPU cost
    /// per frame, the load-robust half of the measurement.
    cpu_at_flush: Option<f64>,
    /// Main-thread CPU seconds at the previous flush, for the row's `main_ms`. Exactly parallel to
    /// `cpu_at_flush`, so the two columns are the same measurement at two scopes: `cpu_ms` is every
    /// thread's work, `main_ms` the serialized part of it. A leg where they diverge is a leg whose
    /// cost moved off (or onto) the critical path — which the all-threads column alone cannot say.
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
    /// bevy's `main_opaque_pass_3d` — terrain, the model lane's opaque parts, the sky shells.
    Opaque = 0,
    /// Our retained static pass (`static_gx`): the WMOs and the doodads, the city's own draw.
    Static,
    /// bevy's transparent (and transmissive) 3D passes — water, glow cards, particles.
    Transparent,
    /// The `ffx_glow` chain: the quarter-res downsample, the two Gauss taps — and a bake's
    /// combine. The world's combine is the first draw of the UI camera's main pass since 2234,
    /// inside `main_transparent_pass_2d`'s own span (it has none of its own — 2258), so it lands
    /// in [`Self::Ui`] with that pass.
    Glow,
    /// The full-screen tail on every camera: tonemapping, upscaling, the MSAA writeback.
    Post,
    /// The 2D camera's passes, bevy UI, and our `ui_gamma_decode`.
    Ui,
    /// Every span this file does not name — counted, never dropped.
    Other,
}

const GPU_BUCKETS: usize = 7;

/// Which column a diagnostics path lands in. `None` = not a top-level GPU span: a CPU span, a
/// non-render diagnostic, or a span nested under another (its parent already carries it).
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

/// One second's GPU spans: summed per bucket, divided at the flush by the number of frames
/// whose readback landed — not by the wall window's frame count, because bevy's diagnostics
/// mutex hands the store at most one frame per sync and drops the rest when readbacks bunch up,
/// so the honest divisor is the frames actually read.
#[derive(Default)]
struct GpuAccum {
    sum: [f64; GPU_BUCKETS],
    frames: u32,
    /// The newest measurement time consumed, so each fold reads only what arrived since. All
    /// measurements of one sync share one `Instant`, which is what makes "a frame" countable.
    seen: Option<Instant>,
    /// `WOW_GPU_PASSES=1` — the same sums per PASS, printed beside each row as a `GPU_PASSES`
    /// line: the journal's buckets fold both 2D passes, the UI pass and the gamma decode into
    /// one `gpu_ui`, and a pass-level question (an empty pass encoded every frame; one filter
    /// pass of a chain) needs the raw split. Empty and unread unless armed.
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

    /// The row's GPU cells — `,gpu_ms,<one per bucket>` — and the reset. All empty when no
    /// frame was read this second: the platform has no in-pass timestamps, and an empty cell
    /// says so where a zero would lie.
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
                // Costliest first, ms per read frame — the raw split the buckets fold.
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
                // Metal reports no driver string at all; a `?` reads better than a blank.
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

/// The journal's residency columns, grouped because `journal_fps` is near Bevy's system-param
/// arity limit.
///
/// The `Assets<T>` counts are the totals — what the process holds. The [`ArtCensus`] half is the
/// same population **broken down by the cache that holds it** (decision 0793), which is what turns
/// "materials are growing" into a named holder in one row instead of a run-length probe. `evicted`
/// is the running total dropped by distance: on a same-map traverse it was structurally zero before
/// 0793, because nothing but a `MapChange` evicted anything (0729).
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
    /// The **view focus** — where art is actually being asked for. Distinct from the row's `x,y,z`,
    /// which is the avatar: through a detached free-fly the body stands still while the camera covers
    /// kilometres, so on that leg the position columns describe nothing that is happening. The
    /// director's first run was exactly that leg, and reading it needed this column.
    scope: Res<'w, benilla_world::art_scope::ArtScopeState>,
}

/// The GPU side: the diagnostics store the render spans sync into, and the two facts the
/// preamble names. All optional — a headless app without a renderer has none of them, and the
/// journal then writes its CPU columns and leaves the GPU cells empty.
#[derive(bevy::ecs::system::SystemParam)]
struct JournalGpu<'w> {
    store: Option<Res<'w, DiagnosticsStore>>,
    adapter: Option<Res<'w, RenderAdapterInfo>>,
    device: Option<Res<'w, RenderDevice>>,
    /// The two settings an A/B run turns - see the row tail for why they are written at all.
    rscale: Option<Res<'w, crate::world_backdrop::RenderScale>>,
    view: Option<Res<'w, benilla_world::view::ViewDistance>>,
}

/// `NonSendMarker` pins this to the main thread, which the `main_ms` column requires:
/// [`main_thread_cpu_secs`] reports *the calling thread*, so on a worker it would silently log
/// whichever pool thread ran the flush.
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
    // The switch, read every frame: the env lever or the CVar. Turning on opens (or creates) the
    // file and restarts every per-second baseline; turning off drops the half-second in hand.
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
                return; // hermetic: no state folder, and nothing else to write into
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
    // CPU per frame over this second — the number the reporter's "CPU %" compares against, and
    // the one that does not move with whatever else is compiling on this machine.
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
    // The per-cache breakdown, in `ArtSlot::ALL` order — which IS the header's column order.
    for slot in benilla_world::art_scope::ArtSlot::ALL {
        line.push_str(&format!(",{}", residency.art.live(slot)));
    }
    line.push_str(&format!(",{}", residency.art.dropped_total()));
    match residency.scope.focus() {
        Some(f) => line.push_str(&format!(",{:.1},{:.1},{:.1}", f[0], f[1], f[2])),
        None => line.push_str(",,,"),
    }
    // Appended at the end, per this file's own rule: the columns only ever grow there, so an
    // existing journal keeps parsing against the header it was created with.
    let main_now = main_thread_cpu_secs();
    match (journal.main_at_flush, main_now) {
        (Some(t0), Some(t1)) => {
            line.push_str(&format!(",{:.2}", (t1 - t0) * 1000.0 / v.len() as f64))
        }
        _ => line.push(','),
    }
    journal.main_at_flush = main_now;
    // The GPU cells (2008), last of all.
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
    match take_sched_us() {
        Some(us) => line.push_str(&format!(",{us}")),
        None => line.push(','),
    }
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
        // Unnamed passes are counted, not dropped.
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
