// A zero-dependency CDP driver for the browser client.
//
// Node 22 ships a global WebSocket, so this needs no npm install — which matters, because the
// repo has no browser automation at all and adding a toolchain to get one measurement would cost
// more than the measurement.
//
// Chrome runs HEADED on purpose: the world needs a WebGPU adapter and headless Windows Chrome
// does not reliably have one. A missing adapter aborts the client in `create_bind_group_layout`
// at world entry, which reads like our bug and is not.
import { spawn } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const CHROME = 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const PORT = Number(process.env.CDP_PORT ?? (9400 + (process.pid % 100)));

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export async function launch(url, profileDir) {
  const profile = profileDir ?? mkdtempSync(join(tmpdir(), 'manila-cdp-'));
  const child = spawn(CHROME, [
    `--remote-debugging-port=${PORT}`,
    `--user-data-dir=${profile}`,
    '--enable-unsafe-webgpu',
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-background-timer-throttling',
    // The client is one long-running page; a backgrounded tab would stop rAF and every
    // measurement with it.
    '--disable-backgrounding-occluded-windows',
    '--disable-renderer-backgrounding',
    url,
  ], { detached: false, stdio: 'ignore' });

  // Wait for the debugger to answer rather than sleeping a guess.
  let version = null;
  for (let i = 0; i < 60 && !version; i++) {
    try {
      version = await (await fetch(`http://127.0.0.1:${PORT}/json/version`)).json();
    } catch { await sleep(500); }
  }
  if (!version) throw new Error('chrome did not open a debugging port');
  return { child, profile, version };
}

// Pick the target by URL, not "the first page". A Chrome started beside the user's own opens
// more than one page target (new-tab, about:blank), and attaching to the wrong one waits forever
// for a `window.session` that was never going to appear there - which is exactly what the first
// run did, and it read like a boot failure rather than a driver defect.
export async function attach(match = '127.0.0.1:8090') {
  let last = [];
  for (let i = 0; i < 60; i++) {
    const list = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
    last = list.map((t) => `${t.type} ${t.url}`);
    const page = list.find((t) => t.type === 'page' && t.webSocketDebuggerUrl && t.url.includes(match));
    if (page) return new Session(page.webSocketDebuggerUrl);
    await sleep(500);
  }
  throw new Error(`no page target matching ${match}; saw: ` + last.join(' | '));
}

class Session {
  constructor(wsUrl) {
    this.ws = new WebSocket(wsUrl);
    this.id = 0;
    this.pending = new Map();
    this.console = [];
    this.ready = new Promise((res, rej) => {
      this.ws.addEventListener('open', () => res());
      this.ws.addEventListener('error', (e) => rej(e));
    });
    this.ws.addEventListener('message', (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id !== undefined) {
        const p = this.pending.get(m.id);
        if (p) { this.pending.delete(m.id); m.error ? p.rej(new Error(JSON.stringify(m.error))) : p.res(m.result); }
        return;
      }
      // Both channels: `console.log` from page script and the browser's own log entries.
      if (m.method === 'Runtime.consoleAPICalled') {
        this.console.push(m.params.args.map(a => a.value ?? a.description ?? '').join(' '));
      } else if (m.method === 'Log.entryAdded') {
        this.console.push(`[${m.params.entry.level}] ${m.params.entry.text}`);
      }
    });
  }
  async send(method, params = {}) {
    await this.ready;
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((res, rej) => this.pending.set(id, { res, rej }));
  }
  async start() {
    await this.send('Runtime.enable');
    await this.send('Log.enable');
    await this.send('Page.enable');
  }
  async eval(expression) {
    const r = await this.send('Runtime.evaluate', {
      expression, awaitPromise: true, returnByValue: true,
    });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.text + ' ' + (r.exceptionDetails.exception?.description ?? ''));
    return r.result?.value;
  }
  /// Poll an expression until it is truthy, or give up — never a fixed sleep, because the load
  /// time here varies by minutes between a cold and a warm cache.
  async waitFor(expression, { timeoutMs = 300000, everyMs = 1000, label = expression } = {}) {
    const t0 = Date.now();
    while (Date.now() - t0 < timeoutMs) {
      try { if (await this.eval(expression)) return (Date.now() - t0) / 1000; } catch {}
      await sleep(everyMs);
    }
    throw new Error(`timed out after ${(timeoutMs / 1000) | 0}s waiting for: ${label}`);
  }
}
