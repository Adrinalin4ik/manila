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
// ─────────────────────────────────────────────────────────────────────────────────────────────
// **THIS FILE PRINTS NOTHING BY DEFAULT, AND THAT IS THE WHOLE POINT OF ITS SECOND VERSION.**
//
// The first version called `console.warn` on every gap over 200 ms. In the owner's session that
// turned the instrument into the defect, and the log it produced says so three ways:
//
//   - `frame_trace.js:42 [Violation] 'requestAnimationFrame' handler took 1040ms` — line 42 was
//     `onFrame`, whose only expensive statement was the warn;
//   - every record carried `overrideMethod @ installHook.js:1`, React DevTools' console override,
//     which makes each console call far dearer than the bare one;
//   - the captured stacks GREW, 12 frames, then 30, then 50, then 100 — DevTools' async stack
//     chain for a `requestAnimationFrame` that re-arms itself for ever, re-collected and
//     serialized on every single warn. And the reported gaps grew with them: 583, 933, 1233, 1483.
//
// A warning about a stall that causes the next stall is worse than no instrument: it reports a
// real number about a frame it spoiled itself. So the gap log is now opt-in (`?frametrace=log`),
// throttled, and prints a plain pre-built string rather than anything the console has to walk.
//
// The default path accumulates into bounded counters and says nothing. Read it when you want it:
//
//     __wenilla_frame_trace()          // the run so far, and reset
//
// ─────────────────────────────────────────────────────────────────────────────────────────────
//
// Nothing here is load-bearing. Every entry point is feature-detected and every failure is
// swallowed: a browser without `PerformanceObserver` or without the `longtask` entry type still
// plays exactly as it does today, it just reports less.

const MAX_LONGTASKS = 64;
/// Gaps at or above this are the ones worth counting apart: three frames at 60 Hz is already a
/// visible hitch, and the owner's reports are hundreds of milliseconds.
const LOUD_GAP_MS = 200;
/// How many of the worst gaps to keep in full. A handful names the shape of a session; the rest
/// are covered by the counters. Bounded on purpose — the first version kept EVERY gap in an array
/// that nothing ever trimmed, which at 60 Hz is a quarter of a million numbers an hour, sorted in
/// full on every read.
const KEEP_WORST = 16;
/// With `?frametrace=log`, no more than one line this often. Even a cheap line is not free when
/// an extension has overridden `console`.
const LOG_EVERY_MS = 5000;

/// Opt in from the page's own query string, the same way the client's own levers are reached.
const LOG_GAPS = (() => {
  try {
    return new URLSearchParams(location.search).get('frametrace') === 'log';
  } catch (_) {
    return false;
  }
})();

const state = {
  frames: 0,
  worst: 0,
  over100: 0,
  over200: 0,
  // A coarse histogram instead of every sample: buckets of 8 ms up to 256 ms, then one tail.
  buckets: new Uint32Array(33),
  worstGaps: [],
  longtasks: [],
  started: 0,
  last: 0,
  lastLog: 0,
};

function record(gap) {
  state.frames += 1;
  const b = Math.min(32, Math.floor(gap / 8));
  state.buckets[b] += 1;
  if (gap >= 100) state.over100 += 1;
  if (gap >= LOUD_GAP_MS) state.over200 += 1;
  if (gap > state.worst) state.worst = gap;
  if (gap >= LOUD_GAP_MS) {
    // Kept smallest-first so the cheap check below is against the weakest member.
    if (state.worstGaps.length < KEEP_WORST) {
      state.worstGaps.push(gap);
      state.worstGaps.sort((a, b2) => a - b2);
    } else if (gap > state.worstGaps[0]) {
      state.worstGaps[0] = gap;
      state.worstGaps.sort((a, b2) => a - b2);
    }
  }
}

function onFrame(now) {
  if (state.last) {
    const gap = now - state.last;
    record(gap);
    if (LOG_GAPS && gap >= LOUD_GAP_MS && now - state.lastLog >= LOG_EVERY_MS) {
      state.lastLog = now;
      // Named with what the browser blamed, if it blamed anything in the same window: a gap with a
      // long task against it is work; a gap with none is the tab not being scheduled at all, and
      // those two want opposite fixes. Built as ONE string: nothing here for the console to walk.
      const blame = state.longtasks.filter((t) => t.end > now - gap - 5 && t.start < now);
      const why = blame.length
        ? blame.map((t) => `${t.name}:${t.dur.toFixed(0)}ms`).join(' ')
        : 'no long task reported — the tab was simply not called';
      console.log(`frame gap ${gap.toFixed(0)} ms — ${why}`);
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

/// A quantile off the histogram. Bucket-resolution (8 ms), which is the right resolution for a
/// cadence question: the difference between 16 and 17 ms is noise, between 16 and 600 is the bug.
function quantile(q) {
  const want = state.frames * q;
  let seen = 0;
  for (let i = 0; i < state.buckets.length; i += 1) {
    seen += state.buckets[i];
    if (seen >= want) return i === 32 ? 256 : i * 8;
  }
  return 0;
}

/// The run so far, and a reset. Returned as data rather than logged so the harness can pull it out
/// of the page the same way it pulls the journal.
function summary() {
  const out = {
    frames: state.frames,
    seconds: state.last ? (state.last - state.started) / 1000 : 0,
    median_ms: quantile(0.5),
    p90_ms: quantile(0.9),
    p99_ms: quantile(0.99),
    worst_ms: state.worst,
    over_100ms: state.over100,
    over_200ms: state.over200,
    worst_gaps_ms: state.worstGaps.slice().reverse(),
    longtasks: state.longtasks.length,
    longtask_ms_total: state.longtasks.reduce((a, t) => a + t.dur, 0),
    longtask_worst_ms: state.longtasks.reduce((a, t) => Math.max(a, t.dur), 0),
  };
  state.buckets.fill(0);
  state.worstGaps = [];
  state.longtasks = [];
  state.frames = 0;
  state.worst = 0;
  state.over100 = 0;
  state.over200 = 0;
  return out;
}

try {
  state.started = performance.now();
  state.lastLog = state.started;
  startLongTasks();
  requestAnimationFrame(onFrame);
  globalThis.__wenilla_frame_trace = summary;
  console.info(
    LOG_GAPS
      ? 'frame trace: on, logging gaps (throttled). Read __wenilla_frame_trace().'
      : 'frame trace: on, silent. Read __wenilla_frame_trace(); ?frametrace=log to log gaps.',
  );
} catch (e) {
  console.warn('frame trace: not started', e);
}
