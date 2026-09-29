// One measured run: log in, reach the world, arm the journal, sit, pull it out.
//
// Credentials arrive on argv. They ride the query string because the login screen here is Rust
// and offers no Lua entry to type into; the profile's History file is deleted on the way out, so
// they do not outlive the run. Everything else about the profile is KEPT: a fresh profile means a
// cold HTTP cache, and re-downloading the chain every run is both slow and a different machine
// from the one we are trying to measure.
import { launch, attach } from './cdp.mjs';
import { writeFileSync, rmSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [user, pass, char, secondsStr, outPath, ...consoleCmds] = process.argv.slice(2);
if (!user || !pass) { console.error('usage: run.mjs <user> <pass> <char> <seconds> <out.csv> [/console cmd]...'); process.exit(2); }
const seconds = Number(secondsStr ?? 90);
const out = outPath ?? 'run.csv';
const PROFILE = join(tmpdir(), 'manila-harness-profile');
mkdirSync(PROFILE, { recursive: true });

const q = new URLSearchParams({
  user, pass, warden_modules: '1',
  host: process.env.WOW_HOST ?? 'apac.capycraft.io',
  realm: process.env.WOW_REALM ?? 'Eversong Wilds',
  // `journal.env_path.is_some() || setting.0` arms it, so any non-empty value turns the journal on
  // from boot - which beats sending `/console fpsJournal 1` through the chat box and hoping.
  fps_journal: 'harness.csv',
});
if (char) q.set('char', char);
// `%20`, not `+`: the client decodes with `decodeURIComponent`, which leaves `+` alone. A realm
// named "Eversong Wilds" arrived as "Eversong+Wilds" and matched nothing, which looked exactly
// like a realm refusing to be picked.
const url = `http://127.0.0.1:8090/?${q.toString().replace(/\+/g, '%20')}`;

const { version } = await launch(url, PROFILE);
console.log('chrome:', version.Browser);
const s = await attach();
await s.start();
const line = (l) => l.replace(/%c/g, '').replace(/\s*color:.*$/, '').slice(0, 200);
const seen = (re) => s.console.some((l) => re.test(l));

try {
  // Progress, so a slow run is distinguishable from a stuck one: the first two failures here
  // were driver defects that both read as "the client did not boot".
  const t0 = Date.now();
  let stage = 'boot';
  while ((Date.now() - t0) / 1000 < 600) {
    if (seen(/net: in world as/)) { stage = 'world'; break; }
    if (seen(/FATAL|LoginFailed|login refused/)) { stage = 'refused'; break; }
    const e = ((Date.now() - t0) / 1000) | 0;
    if (e % 30 === 0 && e) {
      const last = s.console.filter((l) => /crates\/benilla/.test(l)).slice(-1).map(line)[0] ?? '(silent)';
      console.log(`  ${e}s … ${last.slice(0, 120)}`);
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
  console.log(`${stage} after ${((Date.now() - t0) / 1000).toFixed(0)}s`);
  for (const l of s.console.filter((l) => /net: (realm|parked|connected|in world)/.test(l)).slice(-5)) console.log('  ', line(l));

  if (stage === 'world') {
    // Let the entry load settle before the journal starts, or the first rows are the load burst.
    await new Promise((r) => setTimeout(r, 25000));
    for (const c of consoleCmds) {
      await s.eval(`window.wenilla.chat(${JSON.stringify(c)})`);
      console.log('  sent:', c);
      await new Promise((r) => setTimeout(r, 1500));
    }
    console.log(`sitting ${seconds}s…`);
    await new Promise((r) => setTimeout(r, seconds * 1000));
    const csv = await s.eval('window.__wenilla_fps_journal ? window.__wenilla_fps_journal.text() : null');
    if (csv) { writeFileSync(out, csv); console.log(`journal -> ${out} (${csv.split('\n').length} lines)`); }
    else {
      console.log('the journal global never appeared; what the client said about it:');
      for (const l of s.console.filter((x) => /journal/i.test(x)).slice(-6)) console.log('  ', line(l));
    }
  }

  console.log('\n--- client warnings and errors ---');
  const bad = s.console.filter((l) => /ERROR|WARN|panic/.test(l)).map(line);
  const tally = new Map();
  for (const l of bad) { const k = l.slice(0, 110); tally.set(k, (tally.get(k) ?? 0) + 1); }
  for (const [k, n] of [...tally].sort((a, b) => b[1] - a[1]).slice(0, 15)) console.log(`  ${n}x ${k}`);
} finally {
  // The credentialed URL is in the profile's history; the cache is not, and the cache is why the
  // profile is kept.
  for (const f of ['History', 'History-journal']) { try { rmSync(join(PROFILE, 'Default', f), { force: true }); } catch {} }
}
process.exit(0);
