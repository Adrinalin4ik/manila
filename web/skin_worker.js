// skin_worker.js — the page half of the off-thread character-skin compositor.
//
// Why: a body composite measured 248 ms at the median and 410 ms at p90, 437 of them in a
// 357-second capture — 110 seconds, 31% of wall time, on the thread that draws. It is the hitch
// the player sees whenever a character comes into view. `crates/manila-skin` is the same blit
// stack compiled as its own wasm module; this file owns the Worker it runs in and the little
// request table the client polls.
//
// **Poll, not callback.** The client asks from inside a Bevy system, which cannot await and cannot
// be re-entered from JS. So a request returns nothing, and the answer is collected on a later
// frame by id. That is also why a result is handed over exactly once: `take` removes it, so a
// second poll for the same id cannot hand the same buffer to two callers.
//
// A worker that fails to start is not an error the game should care about: `request` then reports
// failure for every id, and the client composites on the main thread exactly as it did before.

let worker = null;
let broken = false;
const done = new Map();   // id -> Uint8Array
const failed = new Set(); // ids the worker could not render

function ensure() {
  if (worker || broken) return worker;
  try {
    worker = new Worker(new URL('./skin_worker_entry.js', import.meta.url), { type: 'module' });
    worker.onmessage = (e) => {
      const { id, bytes } = e.data || {};
      if (typeof id !== 'number') return;
      if (bytes) done.set(id, new Uint8Array(bytes));
      else failed.add(id);
    };
    worker.onerror = () => { broken = true; worker = null; };
  } catch (_) {
    broken = true;
  }
  return worker;
}

/// Queue one composite. Returns false when there is no worker to take it, which tells the client
/// to do the work itself rather than wait for an answer that is never coming.
export function request(id, planJson, prefix, suffix) {
  const w = ensure();
  if (!w) return false;
  try {
    w.postMessage({ id, planJson, prefix, suffix });
    return true;
  } catch (_) {
    return false;
  }
}

/// The finished atlas for `id`, once. `null` while it is still being made; an empty array when
/// the worker gave up, so the caller can stop waiting.
export function take(id) {
  const b = done.get(id);
  if (b) { done.delete(id); return b; }
  if (failed.has(id)) { failed.delete(id); return new Uint8Array(0); }
  return null;
}

// The client reaches these through `wasm_bindgen`; a module export is not visible to it.
globalThis.__manila_skin_request = request;
globalThis.__manila_skin_take = take;
