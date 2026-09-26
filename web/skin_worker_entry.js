// skin_worker_entry.js — the Worker's own module: it instantiates `manila-skin` and answers
// composite requests. Its own wasm instance, its own linear memory; nothing is shared with the
// client's, which is exactly why this needs no atomics and no cross-origin isolation.
import init, { render_body } from './manila_skin.js';

let ready = null;
async function boot() {
  if (!ready) ready = init();
  return ready;
}

self.onmessage = async (e) => {
  const { id, planJson, prefix, suffix } = e.data || {};
  try {
    await boot();
    const out = await render_body(planJson, prefix, suffix);
    if (out && out.length) {
      // Transferred, not copied: a body atlas is megabytes and it is finished with here.
      self.postMessage({ id, bytes: out.buffer }, [out.buffer]);
      return;
    }
  } catch (_) {
    /* fall through: an id with no bytes tells the client to composite it itself */
  }
  self.postMessage({ id, bytes: null });
};
