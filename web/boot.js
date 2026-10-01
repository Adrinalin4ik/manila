// boot.js — the pre-login loading experience, shared by the dev page (web/index.html) and the
// realm page (crates/wenilla-realm/templates/play.html).
//
// Why this exists: the client cannot show its own boot. Everything a fresh tab waits on — the
// 15 MB (brotli) wasm download, instantiation, and the first update's catalog pile (~100+
// synchronous XHR chain reads in ONE rAF task, the "unresponsive tab" window) — happens before
// the first frame the app could draw. So the page owns a DOM overlay, and it does two jobs:
//
//   1. SHOW what is happening: a byte-counted wasm download bar (the counting stream is
//      re-wrapped in a Response so `WebAssembly.compileStreaming` still streams), a
//      "game data N/M" line for the prefetch, then indeterminate "Starting…" states driven by
//      the client's own `window.__wenilla_progress(stage)` calls (benilla-app/src/webprogress.rs)
//      until "ready" — the first glue screen — fades the overlay out.
//   2. SHRINK the unresponsive window: `/data/*` is served immutable (max-age=1y), and a sync
//      XHR is served from the HTTP cache when the same URL was fetch()ed first (verified live:
//      transferSize 0, deliveryType "cache"). So the overlay prefetches the boot read-set — a
//      checked-in manifest (boot-manifest.json) captured from a traced boot — with a small
//      fetch pool, in parallel with the wasm download. The catalog pile then costs parse, not
//      ~100 serial network round trips. Every miss (drift, missing manifest) degrades to
//      exactly today's behavior, and a warm-cache visitor flies through all of it: progress is
//      measured, never scripted.
//
// Page contract: `const w = await boot(init); bind(w.memory);` — bind must stay immediately
// after (the Lua WASI shims need the module's linear memory). Query switches: `?boottrace=1`
// arms window.__wenilla_boottrace and skips the prefetch (a manifest-capture boot must show the
// true read order); `?noprefetch=1` skips the prefetch only (the A/B lever).

// The wasm travels brotli-compressed: content-length counts .br bytes while the counted stream
// yields decompressed ones. Dividing the two on a real build gives ~5.0; it drifts a little per
// build and that is cosmetic — the bar is clamped at 99% until the stream actually ends.
// **The off-thread character-skin compositor** (web/skin_worker.js). Imported here, and not from
// the two pages, because both of them load boot.js and neither should have to remember a third
// file — "a page feature exists only where it was added" is the invariant this dodges. The import
// only defines `globalThis.__manila_skin_request` / `__manila_skin_take`; the Worker itself is not
// created until the client asks for its first composite, so a session that never sees a character
// pays nothing for it.
import './skin_worker.js';

// **The frame cadence as the browser sees it** (web/frame_trace.js). Imported here, statically and
// at boot, because the span it measures is the one the client cannot: `rapp` minus the seven render
// tiles - present - reached 480-520 ms on the owner's late freezes while the named tiles were 1.5%
// of it, and no measurement inside wasm can tell a busy GPU from a tab that was not called. It
// reports through `globalThis.__wenilla_frame_trace()`; nothing depends on it.
import './frame_trace.js';

const BR_RATIO = 5.0;
const READY_TIMEOUT_MS = 20000;
const PREFETCH_CONCURRENCY = 8;

export async function boot(init, opts = {}) {
  const q = new URLSearchParams(location.search);
  const ui = createBootUi();
  if (q.get('boottrace') === '1') window.__wenilla_boottrace = [];
  const skipPrefetch = q.get('boottrace') === '1' || q.get('noprefetch') === '1';
  installProgressHook(ui, skipPrefetch);

  const prefetch = skipPrefetch
    ? Promise.resolve()
    : prefetchManifest('./boot-manifest.json', (done, total) => ui.setData(done, total)).catch(
        () => ui.setData(0, 0)
      );

  // Compile immediately: merely fetching a Response leaves its body backpressured until
  // someone consumes it. Instantiation must still wait for the cache warmup because the
  // game's Startup performs synchronous reads of the prefetched catalogs.
  const wasm = compileWasm(new URL('./manila_bg.wasm', import.meta.url), (got, total) =>
    ui.setWasmProgress(got, total)
  );
  let w;
  try {
    // Attach rejection handlers to both operations now, including while prefetch is pending.
    const [module] = await Promise.all([wasm, prefetch]);
    w = await init({ module_or_path: module });
  } catch (e) {
    ui.fail('The client failed to start: ' + (e && e.message ? e.message : e));
    throw e;
  }
  // init() resolved = module instantiated and the plugin graph built; the heavy catalog frame
  // is still ahead. From here the client drives the stages; the timeout is the drift backstop
  // (an old bundle without webprogress under a new page must not strand the overlay).
  ui.setStage('Starting…');
  ui.armTimeout(opts.readyTimeoutMs ?? READY_TIMEOUT_MS);
  return w;
}

// Match wasm-bindgen's non-streaming fallback for servers without the WASM MIME type.
// Choose before consuming the body, avoiding a cloned response buffering the whole module.
async function compileWasm(url, onBytes) {
  const response = await fetchWasm(url, onBytes);
  if (!response.ok) throw new Error(`Client download failed (HTTP ${response.status})`);
  if (typeof WebAssembly.compileStreaming === 'function' &&
      response.headers.get('content-type')?.trim().toLowerCase() === 'application/wasm') {
    return WebAssembly.compileStreaming(response);
  }
  return WebAssembly.compile(await response.arrayBuffer());
}

/// Byte-counting fetch of the wasm, re-wrapped so streaming compilation survives. Returns a
/// Promise<Response> consumed immediately by compileWasm, before game instantiation.
async function fetchWasm(url, onBytes) {
  const r = await fetch(url);
  if (!r.ok || !r.body) return r; // compileWasm surfaces HTTP errors / handles a missing stream
  const contentLength = Number(r.headers.get('content-length')) || 0;
  const compressed = !!(r.headers.get('content-encoding') || '').trim();
  const total = contentLength ? contentLength * (compressed ? BR_RATIO : 1) : 0;
  let got = 0;
  const reader = r.body.getReader();
  const counted = new ReadableStream({
    async pull(c) {
      const { done, value } = await reader.read();
      if (done) {
        onBytes(got, got); // the honest total, now that we know it
        c.close();
        return;
      }
      got += value.byteLength;
      onBytes(got, total);
      c.enqueue(value);
    },
    cancel(reason) {
      return reader.cancel(reason);
    },
  });
  // Headers are copied so Content-Type: application/wasm survives — compileStreaming
  // refuses anything else.
  return new Response(counted, { status: r.status, statusText: r.statusText, headers: r.headers });
}

/// Warm the HTTP cache with a manifest's read-set: fetch the manifest, then a fixed-size pool of
/// full-body fetches. Failures are counted as done and otherwise ignored — a miss just means
/// that file loads the old way. A missing/invalid manifest resolves immediately (line hides).
/// Used twice: the boot set before `init()`, the world-entry set after `ready`.
// The mounted chain's fingerprint, appended to every /data/ URL as `?v=`.
//
// **It has to match what the client asks for, byte for byte.** The client version-pins its own
// reads (benilla-formats/src/chain.rs, `cache_key`), because /data/* is served immutable for a
// year and the path alone does not say WHICH install is mounted. A prefetch that omitted the
// pin would warm a different URL than the one the sync XHR then asks for, and this whole
// function's purpose — the sync reads finding a warm HTTP cache instead of ~100 serial round
// trips — would silently stop working. Fetched once; an older host without the route leaves it
// empty, which is exactly the URL shape both sides used before the pin existed.
let chainPin = null;
async function dataPin() {
  if (chainPin !== null) return chainPin;
  try {
    const r = await fetch('/data/__chain');
    const t = r.ok ? (await r.text()).trim() : '';
    chainPin = /^[a-f0-9]{1,32}$/.test(t) ? t : '';
  } catch (_) {
    chainPin = '';
  }
  return chainPin;
}
function dataUrl(name, pin) {
  const base = '/data/' + encodeURIComponent(name);
  return pin ? base + '?v=' + pin : base;
}

async function prefetchManifest(manifestUrl, onProgress) {
  const r = await fetch(manifestUrl);
  if (!r.ok) return onProgress(0, 0);
  const manifest = await r.json();
  const names = Array.isArray(manifest && manifest.names) ? manifest.names : [];
  if (!names.length) return onProgress(0, 0);
  let done = 0;
  onProgress(0, names.length);
  const pin = await dataPin();
  const queue = names.slice();
  async function worker() {
    for (;;) {
      const name = queue.shift();
      if (name === undefined) return;
      try {
        const resp = await fetch(dataUrl(name, pin));
        if (resp.ok) await resp.arrayBuffer(); // body fully read = body fully cached
      } catch (_) {
        /* offline blip, 404 drift — the sync XHR path will deal with it */
      }
      done += 1;
      onProgress(done, names.length);
    }
  }
  await Promise.all(Array.from({ length: PREFETCH_CONCURRENCY }, worker));
}

/// `window.__wenilla_progress(stage)` — the client's calls land here (webprogress.rs).
///
/// `ready` also starts the SECOND prefetch: the world-entry read-set. Entering the world used
/// to be one frozen frame of ~200 synchronous sprite reads (184 `Interface\…\*.blp` on the
/// first UI draw, plus a few quest-marker models and zone sounds) — 206 s on a cold 100 ms-RTT
/// cache. That set is location-independent, so `world-manifest.json` (captured the same way
/// as the boot manifest, minus the boot set) is warmed while the player is at the glue screens,
/// with a corner line so it is visible but never in the way. Whatever is still in flight when
/// they click Enter World just loads the old way.
function installProgressHook(ui, skipPrefetch) {
  window.__wenilla_progress = (stage) => {
    if (stage === 'startup') ui.setStage('Loading interface…');
    else if (stage === 'ready') {
      ui.hide();
      if (!skipPrefetch) {
        const corner = createCornerLine();
        prefetchManifest('./world-manifest.json', (done, total) =>
          corner.set(total > 0 && done < total ? `preparing world data — ${done}/${total}` : '')
        )
          .catch(() => {})
          .finally(() => corner.remove());
      }
    }
  };
}

/// The unobtrusive bottom-left status line the world prefetch reports on.
function createCornerLine() {
  const el = document.createElement('div');
  el.id = 'wenilla-corner';
  el.style.cssText =
    'position:fixed;left:.6rem;bottom:.5rem;z-index:15;font:12px system-ui,sans-serif;' +
    'color:#888;background:rgba(0,0,0,.55);padding:.2rem .5rem;border-radius:4px;' +
    'pointer-events:none;transition:opacity .4s ease';
  document.body.appendChild(el);
  return {
    set(text) {
      el.textContent = text;
      el.style.opacity = text ? '1' : '0';
    },
    remove() {
      el.style.opacity = '0';
      setTimeout(() => el.remove(), 500);
    },
  };
}

/// The overlay itself. Same dark theme as the pages' own chrome; no dependencies, no images.
function createBootUi() {
  const root = document.createElement('div');
  root.id = 'wenilla-boot';
  root.innerHTML = `
    <style>
      #wenilla-boot { position: fixed; inset: 0; z-index: 20; display: grid; place-items: center;
        background: #000; color: #ccc; font: 15px system-ui, sans-serif;
        transition: opacity .4s ease; }
      #wenilla-boot.gone { opacity: 0; pointer-events: none; }
      #wenilla-boot .box { width: min(420px, 80vw); }
      #wenilla-boot .stage { margin-bottom: .6rem; text-align: center; }
      #wenilla-boot .bar { height: 6px; background: #222; border-radius: 3px; overflow: hidden; }
      #wenilla-boot .bar > i { display: block; height: 100%; width: 0; background: #c9a227;
        border-radius: 3px; transition: width .15s linear; }
      #wenilla-boot .bar.pulse > i { width: 30% !important; animation: wboot-slide 1.2s ease-in-out infinite; }
      @keyframes wboot-slide { 0% { margin-left: 0 } 50% { margin-left: 70% } 100% { margin-left: 0 } }
      #wenilla-boot .data { margin-top: .5rem; text-align: center; font-size: 12px; color: #888;
        min-height: 1.2em; }
      #wenilla-boot .err { color: #e07070; text-align: center; }
    </style>
    <div class="box">
      <div class="stage">Loading…</div>
      <div class="bar"><i></i></div>
      <div class="data"></div>
    </div>`;
  document.body.appendChild(root);
  const stageEl = root.querySelector('.stage');
  const barEl = root.querySelector('.bar');
  const fillEl = root.querySelector('.bar > i');
  const dataEl = root.querySelector('.data');
  let gone = false;
  let timeout = 0;
  const mb = (n) => (n / (1024 * 1024)).toFixed(0);
  return {
    setWasmProgress(got, total) {
      if (gone) return;
      if (total > 0) {
        const pct = Math.min(99, Math.floor((got / total) * 100));
        const done = got >= total;
        stageEl.textContent = done
          ? 'Downloading client — done'
          : `Downloading client — ${pct}% (${mb(got)} MB)`;
        fillEl.style.width = (done ? 100 : pct) + '%';
      } else {
        stageEl.textContent = `Downloading client — ${mb(got)} MB`;
        barEl.classList.add('pulse');
      }
    },
    setData(done, total) {
      if (gone) return;
      dataEl.textContent = total > 0 ? `game data — ${done}/${total}` : '';
    },
    setStage(text) {
      if (gone) return;
      stageEl.textContent = text;
      barEl.classList.add('pulse');
    },
    fail(text) {
      clearTimeout(timeout);
      barEl.remove();
      dataEl.remove();
      stageEl.className = 'err';
      stageEl.textContent = text;
    },
    armTimeout(ms) {
      timeout = setTimeout(() => this.hide(), ms);
    },
    hide() {
      if (gone) return;
      gone = true;
      clearTimeout(timeout);
      root.classList.add('gone');
      setTimeout(() => root.remove(), 500);
    },
  };
}
