# Running wenilla

How to get the client on screen — browser build and desktop build — and how to point it at a
realm. For what the project *is* and how a change ships, see [AGENTS.md](AGENTS.md).

Everything here was run and its output checked, except where a line says otherwise.

## Prerequisites

| you want | you need |
|---|---|
| browser build | `scripts/web-setup.sh` once (wasm target, wasm-bindgen-cli at `Cargo.lock`'s exact version, wasi-sdk, binaryen) |
| desktop build | a native toolchain; on Debian/Ubuntu also `pkg-config libasound2-dev libudev-dev`, or bevy will not link |
| either | a vanilla 1.12 `Data` directory |
| playing, not just the glue screens | a browser with **WebGPU**. Without an adapter the client aborts in `wgpu::create_bind_group_layout` at world entry — that is the missing adapter, not a bug. Linux Chrome needs `--enable-unsafe-webgpu`. |

## Browser build

Two steps: build the bundle, then serve it. The bundle only needs rebuilding when client Rust
(`benilla-*`, `wenilla`) changes — a change to `wenilla-host` needs a host restart, nothing more.

```bash
scripts/web-build.sh                      # → web/dist (WebGPU). WEB_DEBUG=1 keeps symbols.

cargo run --release -p wenilla-host -- \
  --www web/dist \
  --data /path/to/WoW/Data \
  --upstream logon.your-realm.example \
  --world-upstream 10.0.0.5              # see "Why --world-upstream" below
```

Then open **<http://127.0.0.1:8090/>**.

The build takes a few minutes and produces a ~75 MB wasm (~20 MB gzipped). Fine over LAN, painful
over mobile data.

### Changing realm from the login screen

By default the proxy dials `--upstream` and nothing the page says can move it, so the realmlist
box on the login screen sets the client's own idea of the address and changes nothing else. Add
`--follow-client-realmlist` and the box becomes what it looks like: the proxy dials the address
you typed, and `--upstream` is only the default for a session that names none.

```bash
cargo run --release -p wenilla-host -- \
  --www web/dist --data /path/to/WoW/Data \
  --upstream logon.your-realm.example \
  --follow-client-realmlist
```

Then type an address into **realmlist** on the login screen — or pass `?host=` in the URL, which
sets the same box — and log in. The proxy log's `dialed=` field names the address a session
actually went to, which is the one thing worth reading when a realm change appears to do nothing.

**`--world-upstream` is not needed with this flag.** The world address comes from the realm list
the login server sends, and the client puts it on the socket URL the same way; following the
client means following that too.

**It makes the host a TCP relay to ports 3724 and 8085 on any address a page names**, so it is off
by default and it says so at startup. On a loopback bind that reaches nothing you could not reach
anyway; on `--bind 0.0.0.0` it is a choice about the network you are on. The port allowlist still
holds either way: this widens *where* a session may go, never *what* it may reach there.
`wenilla-realm` has no such flag.

### Why `--world-upstream`

Only for a host running **without** `--follow-client-realmlist`. It is optional, and omitting it
means *"the world is on the same host as the login server"* — not
*"work it out"*. That is wrong for any realm whose realmd advertises a different address for the
world than the one it answers logins on.

The world address **is** discovered automatically, by the client, from the realm list. The client
then hands it to the proxy as `/ws/8085?host=<that address>`. The proxy **discards it** and dials
its configured upstream, deliberately: a page must not be able to aim the socket somewhere else on
the network (`ws.rs`, `upgrade`). `--world-upstream` is the supported way to say where the world
actually is — or turn that default off with `--follow-client-realmlist`.

Get it wrong and the failure is silent rather than loud: a DDoS front accepts a connection on every
port, so the world socket opens against the login host and then says nothing. The proxy log is what
tells them apart — `up=0 down=0` means nothing answered.

## Desktop build

```bash
WOW_DATA=/path/to/WoW/Data \
WOW_HOST=logon.your-realm.example \
WOW_USER=<account> WOW_PASS=<password> \
cargo run --release -p benilla
```

No proxy involved — the desktop client opens its own sockets, so the realm list's world address is
followed without any extra flag.

Never put credentials in a file. They go on the command line, and `WOW_PASS` is read the same way
by every entry point here.

## Configuration: one name, two spellings

wasm has no process environment, so the web build reads the **page's query string** instead. The
mapping is mechanical: `WOW_<NAME>` is the query key `<name>`, lowercased, prefix dropped
(`crates/benilla-app/src/webenv.rs`).

| desktop | browser | what it does |
|---|---|---|
| `WOW_DATA` | — | the `Data` directory (desktop only; the browser fetches `/data`) |
| `WOW_HOST` | `?host=` | the realmlist for this session. Also settable on the login screen; `$WOW_HOST` pins it and greys that control out. |
| `WOW_USER` / `WOW_PASS` | `?user=` / `?pass=` | credentials. The query spellings answer **only** when the page opts in with `dev_query_creds` — a production page never wants a password in a shareable URL. |
| `WOW_CHAR` | `?char=` | character to auto-pick |
| `WOW_WARDEN_MODULES` | `?warden_modules=` | `1` enables the Warden module lane — see below |
| `WOW_WARDEN_PROFILE` | `?warden_profile=` | a fixed scan encoding, for a server that agreed one instead of offering a module. Rarely needed now. |

**`?host=` changes what the client dials; whether the proxy follows is the host's choice.**
Without `--follow-client-realmlist` the socket still goes to `--upstream` and the realm is the
host's — see "Changing realm from the login screen".

## Warden

A server running Warden refuses a client that cannot answer it. This client can, for the part that
is answerable — but it needs the server's own module files.

1. Copy the server's `warden_modules/*.cr` into `<Data>/warden_modules/`. Only `.cr` is read;
   `.bin` and `.key` are the server's business. Copy **all** of them: the server picks a module at
   random per session, so one file covers roughly one session in seventy.
2. Start with the lane enabled:
   - desktop: `WOW_WARDEN_MODULES=1`
   - browser: `?warden_modules=1`

`wenilla-host` serves those files at `/data/warden_modules/<ID>.cr`, reading them loose from
`--data`. They cannot come from the patch chain — they sit beside the archives, not inside them.
`wenilla-realm` does **not** serve them; enabling that there is a deliberate change.

### What works, and the ceiling

The module handshake works end to end: the offer is accepted, the challenge answered from the
module's own table, both ciphers re-keyed, and the scan request read in the module's encoding.

Then it stops, and this is structural, not a bug to hunt. A stock scan table is mostly
`READ_MEMORY` and code-by-hash rows, which ask for bytes inside a `WoW.exe` process image. This
client is not one and has no such image, so those scans have no honest answer and the reply is
refused rather than padded — a filler byte would be a false answer to a specific question, since
the server matches results to scans positionally.

To get past it you must trim the server's `warden_scans` for this client's build: drop `type` 0, 2
and 3, and drop the two sanity rows that exist to catch a client answering "not found" to every
module lookup (`kernel32.dll`, expected present, and its code-scan twin). What is left and worth
keeping: `HASH_CLIENT_FILE`, `GET_LUA_VARIABLE`, `CHECK_TIMING_VALUES`.

### Probing a realm's Warden

To see what a server actually asks, without running the client:

```bash
cargo run --release -p benilla-protocol --example warden_probe -- \
  <logon-host> <account> <password> [module-dir]
```

It prints the module it was offered, whether the handshake completed, and every scan by name —
which is the list a trimmed scan table has to be built against. It needs no game install and no
bevy, so it builds where the full client will not.

## Reaching the host from another device

`--bind` defaults to loopback. Any other address serves `/data` — the whole game install — to
anyone who can reach it, with **no login**. The host says so at startup. Keep it on a network you
trust, and use `wenilla-realm` for actual players.

```bash
cargo run --release -p wenilla-host -- … --bind 0.0.0.0:8090
```

On **WSL2** that is not enough on its own: the distro sits behind NAT, so a phone on the LAN cannot
see the port. From an elevated PowerShell:

```powershell
netsh interface portproxy add v4tov4 listenport=8090 listenaddress=0.0.0.0 `
  connectaddress=<wsl-ip> connectport=8090
netsh advfirewall firewall add rule name="wenilla-host 8090" dir=in action=allow `
  protocol=TCP localport=8090
```

`<wsl-ip>` is `wsl -d <distro> -e bash -lc "hostname -I"`, and it **changes after
`wsl --shutdown`** — the entry has to be re-added with the new one. Remove it with
`netsh interface portproxy delete v4tov4 listenport=8090 listenaddress=0.0.0.0`.

Loopback needs none of this: WSL2 forwards `127.0.0.1` to Windows on its own.

## When it breaks

| symptom | look at |
|---|---|
| page loads, canvas stays black, console shows `RuntimeError: unreachable` | rebuild with `WEB_DEBUG=1`; check for a WebGPU adapter first; `?bridge=0` rules the bridge out |
| world never loads, glue screens fine | no WebGPU adapter |
| login hangs with no error, proxy log shows `up=0 down=0` | the world upstream is wrong — see "Why `--world-upstream`" |
| `no .cr for module <ID> in the module source` | that module's `.cr` is not in `<Data>/warden_modules/`. Copy all of them. |
| `mac module: its opcode table is all zeroes` | exactly one shipped module is the Mac build and is correctly refused; the other ~72 are the Windows ones |
| session reaches character select, then drops ~40 s later | the scan round landed. See "What works, and the ceiling". |
| a page feature works locally but not for realm players | `web-build.sh`'s `cp` line, then `play.html` vs `index.html` |
| wasm-bindgen glue errors at load | version drift — rerun `scripts/web-setup.sh` |

## Not supported yet

- **Choosing a realm on the realm service.** `--follow-client-realmlist` exists only on
  `wenilla-host`; players on `wenilla-realm` reach the realm it is configured for.
- **A realmlist on a non-standard port.** The proxy's port allowlist is `{3724, 8085}`
  (`crates/wenilla-host/src/lib.rs`, `ALLOWED_PORTS`), so `realm.example:3725` gets a 403 whatever
  the upstream flags say.
- **Warden in `wenilla-realm`.** The module route is mounted only by `wenilla-host`.
- **`GET_LUA_VARIABLE` on the net lane.** The Lua VM is a `!Send` resource on the Bevy main thread
  and cannot be read from the network thread, so such a scan reports a named gap rather than a
  wrong answer.
