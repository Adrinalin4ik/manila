// The playerDistance A/B, inside ONE run on ONE machine a minute apart — the comparison the
// wall's own module header argues for, because a comparison across runs measures the afternoon
// rather than the code.
import { launch, attach } from './cdp.mjs';
import { writeFileSync, rmSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [user, pass, char, out] = process.argv.slice(2); // legs follow at argv[6]+
const PROFILE = join(tmpdir(), 'manila-harness-profile');
mkdirSync(PROFILE, { recursive: true });
const q = new URLSearchParams({
  user, pass, char, warden_modules: '1',
  host: 'apac.capycraft.io', realm: 'Eversong Wilds', fps_journal: 'harness.csv',
});
const url = `http://127.0.0.1:8090/?${q.toString().replace(/\+/g, '%20')}`;

const { version } = await launch(url, PROFILE);
console.log('chrome:', version.Browser);
const s = await attach();
await s.start();
const line = (l) => l.replace(/%c/g, '').replace(/\s*color:.*$/, '').slice(0, 220);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

try {
  const t0 = Date.now();
  while ((Date.now() - t0) / 1000 < 600 && !s.console.some((l) => /net: in world as/.test(l))) await sleep(1000);
  console.log(`in world after ${((Date.now() - t0) / 1000).toFixed(0)}s`);
  await sleep(30000); // let the entry load and the first streaming burst settle

  const mark = async (cmd, secs) => {
    const at = await s.eval('window.__wenilla_fps_journal ? window.__wenilla_fps_journal.text().length : 0');
    if (cmd) { await s.eval(`window.wenilla.chat(${JSON.stringify(cmd)})`); console.log(`  [journal ${at} chars] ${cmd}`); }
    else console.log(`  [journal ${at} chars] baseline`);
    await sleep(secs * 1000);
    return at;
  };

  // The legs come from argv: `<cmd or ->` repeated. The camera is untouched throughout, which is
  // the point of a rig — and the first and last leg are meant to be the SAME setting, so a scene
  // that keeps streaming in (it does: entities went 24k -> 39k across one run) shows up as drift
  // between them instead of being mistaken for the effect.
  const legs = process.argv.slice(6);
  for (const leg of legs.length ? legs : ['-', '/console playerDistance 0', '/console playerDistance 777']) {
    await mark(leg === '-' ? null : leg, 45);
  }

  const csv = await s.eval('window.__wenilla_fps_journal.text()');
  writeFileSync(out, csv);
  console.log(`journal -> ${out} (${csv.split('\n').length} lines)`);
  // The console beside the journal, verbatim: an `/console` leg often reports through a log line
  // rather than a column (the archetype census, the crowd wall's own counter), and the tally
  // below groups by prefix and eats the numbers.
  try {
    const logPath = out.replace(/[.]csv$/, '') + '-console.log';
    writeFileSync(logPath, s.console.map(line).join(String.fromCharCode(10)));
    console.log('console -> ' + logPath);
  } catch (e) { console.log('console dump failed:', e.message); }


  console.log('\n--- what the wall itself reported ---');
  for (const l of s.console.filter((x) => /playerDistance|staticTransforms|console/i.test(x)).slice(-12)) console.log('  ', line(l));
} finally {
  for (const f of ['History', 'History-journal']) { try { rmSync(join(PROFILE, 'Default', f), { force: true }); } catch {} }
}
process.exit(0);
