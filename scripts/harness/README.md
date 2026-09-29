# The browser harness

Drives the real client in a real Chrome over CDP: log in, reach the world, run `/console`
commands, pull the FPS journal out. Zero dependencies — Node 22 ships a global `WebSocket`, and
adding a browser-automation toolchain to get one measurement would have cost more than the
measurement.

```
node scripts/harness/run.mjs <user> <pass> <char> <seconds> <out.csv>
node scripts/harness/ab.mjs  <user> <pass> <char> <out.csv> '-' '/console playerDistance 0' '/console playerDistance 777'
```

`ab.mjs` runs its legs inside ONE session on ONE machine a minute apart, which is the only
comparison that means anything here: the same street gave 739 players on one run and 947 on the
next, so a comparison ACROSS runs measures the afternoon rather than the code. Make the first and
last leg the same setting — the scene keeps streaming in (entities went 25k → 39.7k inside one
four-minute run), and that drift has to be visible rather than mistaken for the effect.

## What it needs

- `wenilla-host` already serving on `127.0.0.1:8090`.
- Chrome on Windows at the path in `cdp.mjs`. It runs HEADED on purpose: the world needs a WebGPU
  adapter and headless Windows Chrome does not reliably have one. Without it the client aborts in
  `wgpu::create_bind_group_layout` at world entry, which reads like our bug and is not.

## Things that cost an hour each, so they are written down

- **Credentials ride the query string** because the login screen is Rust here and offers no Lua
  entry to type into. The profile's `History` is deleted on the way out; the rest of the profile
  is KEPT, because a fresh profile means a cold HTTP cache — re-downloading the chain every run is
  both slow and a different machine from the one being measured.
- **`%20`, never `+`.** The client decodes the query with `decodeURIComponent`, which leaves `+`
  alone (turning it into a space is form-encoding, not URI decoding). A realm named
  "Eversong Wilds" arrives as "Eversong+Wilds" and matches nothing, and the client then sits at
  the realm list looking exactly like a realm refusing to be picked.
- **Pick the CDP target by URL**, not "the first page": a Chrome started beside the user's own has
  other page targets, and attaching to one waits forever for a client that was never there.
- **`window.session` does not exist in this client.** Readiness is the client's own first log line.
- **Arm the journal from the URL** (`?fps_journal=<name>`): `journal.env_path.is_some() ||
  setting.0`, so any non-empty value turns it on from boot. Sending `/console fpsJournal 1` through
  the chat box instead leaves `ui_us` reading zero, because the CVar arm is what also sets
  `ui_cost` — two ways to switch the journal on, two different sets of columns.
- **Git Bash rewrites a leading `/console` into a Windows path.** Run the driver with
  `MSYS_NO_PATHCONV=1` and pass the output as a bare filename, since that variable also stops the
  output path being translated.

## What it cannot do

Judge anything visual, and it should not try: the owner has said so, and on the day this was
written he found a frame-layout defect and a run of red Lua errors by looking, in seconds, while
the harness was still being debugged.
