<div align="center">

<img src="screenshots/Screenshot%202026-10-09%20093829.png" alt="Screenshot" width="100%">

<h1>manila</h1>

<p><b>World of Warcraft 1.12.1 and Turtle WoW 1.18.1, in a browser tab or on the desktop, with modern lighting, water and sky.</b></p>

<p>
Nothing is installed: this is a from-scratch Rust client compiled to WebAssembly and drawn with
WebGPU. The same code also builds natively for Windows, Linux and macOS.
</p>

<p>
<a href="LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="License"></a>
<img src="https://img.shields.io/badge/client-1.12.1%20%7C%20Turtle%201.18.1-c79c6e?style=for-the-badge" alt="1.12.1 | Turtle 1.18.1">
<img src="https://img.shields.io/badge/runs%20in-browser%20%7C%20Windows%20%7C%20Linux%20%7C%20macOS-4b8bbe?style=for-the-badge" alt="browser | Windows | Linux | macOS">
</p>

</div>

## What this is

A complete World of Warcraft client written from scratch in Rust and [Bevy](https://bevy.org):
every file format, the network protocol and a FrameXML/Lua engine that runs the game's own
interface and its addons. It reads the game data from your own install and connects to a server
over the original protocol, with no original client code and no bundled game assets.

It plays in a browser tab with nothing installed, or as a native application, and it can render
either the original image or a modern one: dynamic light and shadows, volumetric fog, new water and
sky, each behind its own switch.

The projects this one is built on and tracks: [`forks.md`](forks.md).

## Screenshots

<table>
  <tr>
    <td width="50%"><img src="screenshots/Screenshot%202026-10-09%20091642.png" alt="Screenshot"></td>
    <td width="50%"><img src="screenshots/Screenshot%202026-10-09%20094153.png" alt="Screenshot"></td>
  </tr>
  <tr>
    <td><img src="screenshots/Screenshot%202026-10-09%20220031.png" alt="Screenshot"></td>
    <td><img src="screenshots/Screenshot%202026-10-09%20220205.png" alt="Screenshot"></td>
  </tr>
  <tr>
    <td><img src="screenshots/Screenshot%202026-10-09%20103552.png" alt="Screenshot"></td>
    <td><img src="screenshots/Screenshot%202026-10-10%20002351.png" alt="Screenshot"></td>
  </tr>
  <tr>
    <td colspan="2"><img src="screenshots/Screenshot%202026-10-10%20081926.png" alt="Screenshot"></td>
  </tr>
</table>

## Features

### The game

The same in the browser and natively:

- Login, realm list, character select and the world, on vanilla 1.12.1 and Turtle WoW 1.18.1 realms
  alike, over the original protocol
- Questing and professions, dungeons and raids, battlegrounds, groups, guilds, trade, mail and the
  auction house
- The interface your install ships, run from its own FrameXML and Lua — the stock 1.12 one, or
  Turtle WoW's own with its options window, ESC menu and server prelude
- Your addons: Questie, pfUI, Bagnon, Bartender2 and most other 1.12 addons run

### In the browser

The browser build is not a port of a different program: it is the same client compiled to
WebAssembly, with only what a browser cannot do replaced.

- Raw TCP becomes a WebSocket through `wenilla-host`
- The MPQ chain becomes single files fetched over HTTP; the MPQs never leave the host, and server
  content missing from an archive's listfile still loads
- An addon's own art loads in the browser too
- Character skins are composited in a separate wasm module in a Web Worker
  ([`crates/manila-skin`](crates/manila-skin)), off the thread that draws

### Native builds

The same tree builds and runs natively on **Windows, Linux and macOS**, with the same graphics, the
same interface and the same servers. No WebGPU requirement there, no host in between, and the game
data is read straight from your install.

### Servers: vanilla, Turtle WoW, Warden

- **Vanilla 1.12.1** — any server the original client could connect to:
  [vmangos](https://github.com/vmangos/core), cMaNGOS and the other 1.12.1 cores
- **Turtle WoW 1.18.1** — Turtle-derived realms, played from a Turtle client's data:
  - its two extra races and its modified DBC layouts
  - the server's own FrameXML prelude (`Globals.lua`, `Overrides.lua`) loads when the install has
    it, so the character sheet and the world map run without script errors
  - character creation accepts the race/class combinations Turtle adds (57 against vanilla's 40)
    and lays out all ten races
  - Turtle's replaced options window (no `OptionsFrameSliders`) and its own ESC menu, Donation
    Rewards button included, work with the Graphics rows and the menu adapters
  - login: native builds try build 5875 first, then 7272 and 12340 when the server refuses; the
    browser logs in with 12340
- **Warden, partially** — the client half of the anticheat exchange. A module offer is accepted by
  reading the server's `.cr` module data instead of executing it, the challenge is answered from
  that module's own table, and every scan the client can honestly witness about itself is answered:
  hashes of the game files it holds, Lua globals from its own VM, timing. Scans that read memory
  inside a `WoW.exe` process cannot be answered by a from-scratch client and are left unanswered,
  never faked; the client never drops the session itself, so the server decides. Setup in
  [`run.md`](run.md)

### Graphics

Every feature has its own switch under **Options → Advanced Graphics**, ordered on a preset ladder
**Classic / Low / Medium / High / Ultra / Custom**. **Classic** is the original 1.12 image, and it is
what the browser boots into — opt in to more from the preset list.

- **Lighting and shadows** — realtime sun and moon shadows, foliage included; moonlight at night;
  building interiors lit by their own fixtures; torches, braziers and lamps that flicker and cast
  cube-map shadows; daylight through doors and windows; spell and lava glow; SSAO
- **Sky** — smooth dithered gradient, sun glow, a procedural star field with the Milky Way, sun-lit
  clouds, zone skyboxes
- **Fog and atmosphere** — a modern fog model, volumetric haze, sun and moon shafts marched through
  the shadow map, lamps glowing through fog at night with their count and brightness on sliders,
  render distance up to 1497 yards
- **Water** — refraction, caustics, depth colour, planar reflections, Gerstner waves with whitecaps,
  enhanced city and building water
- **Weather and nature** — wet ground, puddles and rings on water in the rain, shelter under roofs;
  grass and trees in the wind, grass parting around characters
- **Post-processing** — HDR bloom, per-zone day/night colour grading
- **Saved presets** — save the Advanced Graphics page under a name and get it back in the Graphics
  Preset list

Details: [`LIGHTING.md`](LIGHTING.md), [`WATER.md`](WATER.md).

### Options and tools

- **Press and Hold Casting** — a held action-bar key casts again each time its slot is ready
  (Options → Controls, off by default)
- **Built for the browser's budget** — addons loaded one per frame instead of in a single
  multi-second freeze, split collider builds, budgeted arrivals, a collapsed material key,
  frustum-parked doodad animation, ETag-revalidated and version-pinned asset requests
- **A measuring toolkit** — an FPS journal downloaded as CSV from the page, a frame trace naming
  what blocked each frame, archetype and material censuses, and switches that turn a lane off so
  the frame says what it was worth

## Technologies

| area | what |
|---|---|
| language | Rust 1.98 |
| engine | [Bevy](https://bevy.org) 0.18 (ECS, scheduling, assets) |
| rendering | wgpu 27 — WebGPU in the browser; Vulkan, DirectX 12 or Metal natively; WGSL shaders |
| interface | Lua 5.1 via mlua, built from source and patched to the 1.12 client's dialect; a FrameXML engine of its own |
| audio | kira 0.12 over cpal; Web Audio in the browser |
| browser build | WebAssembly (`wasm32-unknown-unknown`), wasm-bindgen, web-sys, wasi-sdk for the C Lua, binaryen; Web Workers, WebSocket, brotli/gzip-precompressed bundle |
| servers | axum 0.8 and tokio, tokio-tungstenite for the WebSocket relay; Askama templates and sqlx (SQLite) in the realm service |
| formats | MPQ, BLP, DBC, ADT, WDT, WDL, M2, WMO — all read by our own crates, no third-party WoW libraries |

## Running it

You need a client install for the game data — manila only reads it — and a server that matches it:

- an English **1.12.1 (build 5875)** client and a 1.12.1 server, or
- a **Turtle WoW 1.18.1** client and a Turtle-derived realm.

Pick either route below; both are supported. The browser needs **WebGPU**: Chrome or Edge on
Windows and macOS, Safari 26+, Firefox 141+; Linux Chrome needs `--enable-unsafe-webgpu`. For a
server that runs Warden, see the Warden section of [`run.md`](run.md).

### In the browser

```bash
scripts/web-setup.sh            # once per machine: wasm target, wasm-bindgen, wasi-sdk, binaryen (no sudo)
scripts/web-build.sh            # → web/dist
cargo run --release -p wenilla-host -- --www web/dist --data /path/to/WoW/Data --upstream 127.0.0.1
# open http://127.0.0.1:8090/
```

`wenilla-host` serves the game files you point it at with no login, so it binds to loopback by
default: keep it there or on a private network, never on the open internet. Hosting other players
is [`wenilla-realm`](crates/wenilla-realm/README.md), which puts everything behind a session.

[`run.md`](run.md) is the full guide — realm selection from the login screen, reaching the host from
another machine, and the one check that proves a tab is running the build you just made.
[`web/README.md`](web/README.md) explains how the port works.

### On the desktop (Windows, Linux, macOS)

The native build is fully supported and plays the same game as the browser one.

```sh
WOW_DATA=/path/to/WoW/Data cargo run --release -p benilla
```

```powershell
$env:WOW_DATA="C:\path\to\WoW\Data"; cargo run --release -p benilla
```

The server defaults to `localhost:3724`; set `WOW_HOST` or use the Realmlist button on the login
screen. Native builds try login build 5875 first and fall back to 7272 and 12340 for Turtle-derived
and custom realms; `WOW_REALMD_BUILD=<build>` pins one. Settings, screenshots and addons live in
`benilla-config/` (addons in `benilla-config/AddOns/`). A C compiler is required, because the
client's Lua is built from source; on Linux also `pkg-config libasound2-dev libudev-dev`.

Optional, locally built skybox and colour-grading data:
[`Optional/sky-and-grading/`](Optional/sky-and-grading/README.md).

## Repository map

| path | what |
|---|---|
| `crates/benilla-*` | the client: formats, world, models, UI engine, protocol, app |
| `crates/wenilla` | the wasm entry crate |
| `crates/wenilla-host` | the local dev server: static files, `/data/*`, the `/ws/*` relay |
| `crates/wenilla-realm` | the realm service: login, play page, admin panel |
| `crates/manila-skin` | the skin compositor as its own wasm module for a Web Worker |
| `web/` | the page side: `index.html`, `boot.js`, `platform.js`, `bridge.js`, `frame_trace.js` |
| `scripts/` | `web-setup.sh` and `web-build.sh` |

[`docs/MAP.md`](docs/MAP.md) maps every client crate and subsystem, generated from the code.
[`AGENTS.md`](AGENTS.md) is the working map of the repository and the rules that bite here.

## Contributing

Issues and pull requests are welcome. [`docs/METHOD.md`](docs/METHOD.md) and
[`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) say how a change is made and judged.

## Legal

This is an independent fan project, not affiliated with or endorsed by Blizzard Entertainment. It
ships **no Blizzard content**: no art, models, sounds, maps, MPQ contents or FrameXML. You provide
your own legally obtained client, and its interface runs off its own FrameXML at runtime. World of
Warcraft is a trademark of Blizzard Entertainment, Inc.

The code is licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.
The vendored components under `third_party/` keep their own licenses, alongside each; code ported
from other projects is listed in [`THIRD-PARTY.md`](THIRD-PARTY.md).
