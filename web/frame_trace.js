// frame_trace.js — the frame cadence as the BROWSER sees it.
//
// Why this exists. The client's own journal can say how long its schedules took and how long the
// render app took, and it tiles the frame with those. What it cannot see is the span between
// handing a frame to the browser and being called again: `rapp` minus the seven render tiles is
// "ExtractSchedule plus present", and on the owner's late freezes that remainder is 480-520 ms
// while the named tiles are 1.5% of it. Present is the browser's, and nothing inside wasm can
// tell "the GPU is busy" from "we were not scheduled".
//
// The GPU half is closed off on this target: the journal's `gpu_*` columns come from bevy's render
// diagnostics, which need `TIMESTAMP_QUERY_INSIDE_ENCODERS`, and Chrome's WebGPU here reports
// `gpu_ts yes | gpu_inside_encoders no`. So this measures the other half, from the only place that
// can: the page.
//
// Two things, both standard and both cheap:
//
//   - a self-chaining `requestAnimationFrame`, whose intervals ARE the frame cadence. It runs in
//     the same event loop as the client's own, so a gap here is a gap there.
//   - `PerformanceObserver('longtask')`, where the browser reports every task over 50 ms and names
//     the container it came from. That is the browser saying what blocked it, rather than us
//     inferring it from a hole in our own timeline.
//
// Nothing here is load-bearing. Every entry point is feature-detected and every failure is
// swallowed: a browser without `PerformanceObserver` or without the `longtask` entry type still
// plays exactly as it does today, it just reports less.

const MAX_LONGTASKS = 64;
/// Gaps at or above this are worth a console line on their own: three frames at 60 Hz is already a
/// visible hitch, and the owner's reports are hundreds of milliseconds.
const LOUD_GAP_MS = 200;

const state = {
  frames: 0,
  worst: 0,
  worstAt: 0,
  gaps: [],
  longtasks: [],
  started: 0,
  last: 0,
};

function onFrame(now) {
  if (state.last) {
    const gap = now - state.last;
    state.frames += 1;
    state.gaps.push(gap);
    if (gap > state.worst) {
      state.worst = gap;
      state.worstAt = now;
    }
    if (gap >= LOUD_GAP_MS) {
      // Named with what the browser blamed, if it blamed anything in the same window: a gap with a
      // long task against it is work; a gap with none is the tab not being scheduled at all, and
      // those two want opposite fixes.
      const blame = state.longtasks.filter((t) => t.end > now - gap - 5 && t.start < now);
      const why = blame.length
        ? blame.map((t) => `${t.name}:${t.dur.toFixed(0)}ms`).join(' ')
        : 'no long task reported — the tab was simply not called';
      console.warn(`frame gap ${gap.toFixed(0)} ms — ${why}`);
    }
  }
  state.last = now;
  requestAnimationFrame(onFrame);
}

function startLongTasks() {
  if (typeof PerformanceObserver !== 'function') return;
  // Not every browser ships the entry type, and asking for one it does not know throws.
  const types = PerformanceObserver.supportedEntryTypes;
  if (Array.isArray(types) && !types.includes('longtask')) return;
  try {
    const obs = new PerformanceObserver((list) => {
      for (const e of list.getEntries()) {
        state.longtasks.push({
          name: e.name,
          start: e.startTime,
          end: e.startTime + e.duration,
          dur: e.duration,
        });
      }
      // Bounded: this runs for the life of the tab and nobody reads most of it.
      if (state.longtasks.length > MAX_LONGTASKS) {
        state.longtasks.splice(0, state.longtasks.length - MAX_LONGTASKS);
      }
    });
    obs.observe({ entryTypes: ['longtask'] });
  } catch (_) {
    // A browser that refuses the observer reports no blame; the gaps still read.
  }
}

/// The run so far, and a reset. Returned as data rather than logged so the harness can pull it out
/// of the page the same way it pulls the journal.
function summary() {
  const gaps = state.gaps.slice().sort((a, b) => a - b);
  const pick = (q) => (gaps.length ? gaps[Math.min(gaps.length - 1, Math.floor(q * gaps.length))] : 0);
  const out = {
    frames: state.frames,
    seconds: state.last ? (state.last - state.started) / 1000 : 0,
    median_ms: pick(0.5),
    p90_ms: pick(0.9),
    p99_ms: pick(0.99),
    worst_ms: state.worst,
    over_100ms: gaps.filter((g) => g >= 100).length,
    over_200ms: gaps.filter((g) => g >= LOUD_GAP_MS).length,
    longtasks: state.longtasks.length,
    longtask_ms_total: state.longtasks.reduce((a, t) => a + t.dur, 0),
    longtask_worst_ms: state.longtasks.reduce((a, t) => Math.max(a, t.dur), 0),
  };
  state.gaps = [];
  state.longtasks = [];
  state.frames = 0;
  state.worst = 0;
  return out;
}

try {
  state.started = performance.now();
  startLongTasks();
  requestAnimationFrame(onFrame);
  globalThis.__wenilla_frame_trace = summary;
  console.info('frame trace: on');
} catch (e) {
  console.warn('frame trace: not started', e);
}
