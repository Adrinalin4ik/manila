# pfUI / ClassicAPI / Nampower session handover

Prepared at the user's request on **2026-10-10**. This is a historical working-session
handover for another LLM session, not a declaration of full support or a replacement for the
repository's instructions. Implementation state below is at **`4cba7720`**; a later
documentation-only commit may contain this file. Recheck the checkout before acting.

## Start here

1. Read [AGENTS.md](AGENTS.md), [docs/METHOD.md](docs/METHOD.md),
   [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md), and [docs/MAP.md](docs/MAP.md).
   `MAP.md` is generated: do not edit it by hand.
2. Read this handover and [the extension README](crates/manila-pfui/README.md).
   The README is the maintained description of behavior and deviations; this file preserves
   session context, probes, failures, and proposed next steps.
3. Inspect `git status --short`, `git branch -vv`, `git log`, and actual remotes. Stay on the
   feature branch and preserve unrelated user changes.
4. Continue implementing and verifying actual missing pfUI behavior. Do not start by
   adding version flags, empty APIs, or repeating already completed fixes.

**Current outcome:** the complete pinned pfUI Turtle manifest loads and renders in the native
client; substantial equipment, bag, spell, focus, and addon functionality exists. Full pfUI
and full Nampower support are **still incomplete**. Both browser configurations compile,
but browser runtime behavior is unverified. There are known baseline gate failures.

## User intent and authorization

The user wants [brues-code/pfUI](https://github.com/brues-code/pfUI), which depends on
[ClassicAPI](https://github.com/brues-code/ClassicAPI), running in Manila/Benilla. They
explicitly selected **full pfUI, including equipment manager, bag sorting, profile sharing,
and extras**, and subsequently requested full pfUI/Nampower support.

Implement the APIs pfUI actually needs first, rather than porting all of ClassicAPI. Keep
the integration modular, optional, and cheap to carry across upstream changes. The user
expects that upstream Benilla may reject the compatibility extension. Independently found
Benilla/Manila bugs should be fixed in **separate commits** for possible upstream submission.
The user also requested rebasing the work onto Manila's upstream changes and making commits
on their branch so they can prepare a PR. That rebase and commit split have been performed.

The local WoW server is on localhost. The user authorized testing on their local server and
creating test characters. Repository unattended-login rules still apply: use the checkout's
declared identity, not a player's active account. **Do not print or commit credentials.**
The user supplied an original-client launcher for comparison:

```sh
cd /home/thorsten/Games
./start-wow.sh turtle-wow laptop
```

The darker-night / patch-N / Octo-HD discussion in the earlier conversation was a separate
topic; the user explicitly redirected work to pfUI. It is not a pending pfUI implementation task.

## Git state and PR preparation

At handover creation, the implementation tree was clean on **`feat/pfui-compatibility`**.
There were **13 commits above `origin/main`**, with code HEAD `4cba7720`. Nothing from this
work was pushed, merged, deployed, or submitted as a PR.

| Local ref / remote | Observed value |
| --- | --- |
| `origin` | `git@github.com:Adrinalin4ik/manila.git` |
| `fork` | `git@github.com:Thorsten42/manila.git` |
| `origin/main`, `fork/main` | `c06e2f19` at the last fetch |
| Local `main` | `f482d712`, behind `origin/main`; intentionally left alone |
| Backup before rebase | `backup/pfui-before-rebase-20261010` at `69229c4d` |

The feature work was rebased onto **Manila's `origin/main`**, preserving the fork's existing
upstream merge history. This does not change the rule that syncing Benilla into the fork
uses a real merge, as described in [docs/UPSTREAM.md](docs/UPSTREAM.md).
Remote tips are historical observations: fetch before a future rebase/PR if appropriate.
Do not reset local `main`, copy upstream content without ancestry, or force-push `main`.

Some inherited instructions mention `Arnesen/wenilla` as the PR target. This checkout's
actual remotes and the user's intended Manila PR must be reconciled before publishing;
do not blindly create a PR in the repository from the inherited example. No PR is needed
merely to resume local implementation.

Commits, oldest first:

| Commit | Purpose |
| --- | --- |
| `6e6c4aa9` | Camera zoom test uses shared binding input queue |
| `430e154c` | Test Manila's debug-stat diagnostic string |
| `fd4664d4` | App fixture repairs for current Bevy / async APIs |
| `0713767f` | Keep unit snapshots current without a GUID index |
| `fcc9781b` | Reset chat profiling counters when the UI VM changes |
| `fb634770` | Relearn held cast bindings after a UI reload |
| `cb1e1330` | Optional Lua expressions per VM for addon extensions |
| `19498579` | Main opt-in pfUI / Nampower implementation, including earlier rounds |
| `8f5b2cb8` | Close tooltip tuple in options test fixture |
| `d3297c34` | Document browser compile check and runtime limit |
| `cdf58308` | Independent aura-expiration clock bug fix |
| `a2f805bb` | Observed remote channels for pfUI cast bars |
| `4cba7720` | Aura filters, ownership, refresh notifications, group fallback |

`19498579` is large: 144 files, roughly 20,767 insertions. Most added code lives in extension
files. Do not assume the entire branch is a small upstreamable patch. The ordinary bug fixes
are separate; extension hooks and the optional parser carry may remain fork-only.

## Constraints that matter during implementation

- Keep compatibility in its own crate/files and small `#[cfg(feature = "pfui")]` hooks.
  Feature-off builds must retain stock behavior. Do not globally enable the new Lua dialect.
- Implement real backing state, command dispatch, and events. A present function or a clean
  static audit does not establish correct behavior. Leave unsupported capabilities absent.
- The implementation is independently written under MIT OR Apache-2.0. Use pfUI call sites,
  public API contracts, packet facts, DBC fields, and the owner's runtime data as evidence.
  Do not copy ClassicAPI/Nampower DLL algorithms or bundle GPL implementation code.
- The WoW install is **read-only**, including FrameXML. No logs, screenshots, cache, profiles,
  addon edits, or scratch files go there. Separately obtained addon sources and client state
  live outside it. Do not commit game assets or reference-client binaries.
- Use `bevy::platform::time::Instant` / appropriate web clocks for wasm-reachable code.
  `std::time::Instant`, `SystemTime`, threads, and `env::vars_os` are browser hazards.
  `Duration` arithmetic is fine. A wasm compile pass does not prove browser startup.
- Read live preflight numbers before accepting a result. A screenshot proves visible
  existence; logs/packet timestamps establish timing and event order.
- Every unattended run needs `WOW_UNATTENDED=1` and `WOW_NOSOUND=1`, a bounded lifetime,
  and the identity declared in `.probe-identity`. Never print that file or the environment.
- **No unattended combat probes.** Hostility / faction / nameplate-color conclusions require
  verified GM-off state, not merely setting an environment variable.
- Run the repository's required checks, report baseline failures honestly, and keep fixes
  atomic. Do not blanket-format upstream files, inflate ambiguity ceilings, or weaken tests
  just to make this branch green.

## Source contracts and local checkouts

The investigation was pinned to these revisions. `/tmp` checkouts are disposable and may
not exist in a future session. Recreate pinned sources separately if needed; do not silently
change the tested addon/API revisions.

| Project | Revision | Local checkout |
| --- | --- | --- |
| pfUI | `40dd9dd6db1cb8d15d565ec5bdaf39e70175e611` | `/tmp/manila-pfui` |
| ClassicAPI | `917b15640e2bdc2c8f24e0e0d4d520514d76ff52` | `/tmp/manila-classicapi` |
| Nampower | `2c7f51cd436e9b0ce88271e84ce5a77c19f2c23b` | `/tmp/manila-nampower` |

Useful contracts are ClassicAPI `docs/API.md` and Nampower `SCRIPTS.md`, `EVENTS.md`,
and `README.md`. The pinned Nampower README and QueueSpellByName reference disagree on
non-GCD capacity (six versus five); the implementation currently follows six and documents it.
Public vmangos spell aura/mechanic enum definitions were also consulted for numeric DBC facts.
This is not evidence of original-DLL behavioral parity.

## Code map

| Area | Start reading |
| --- | --- |
| Extension installer, Lua helpers, codecs, audit | [crates/manila-pfui/src/lib.rs](crates/manila-pfui/src/lib.rs), neighboring `.rs` / `.lua` files |
| Engine plugin, per-VM install, session focus, snapshots | [crates/benilla-app/src/pfui.rs](crates/benilla-app/src/pfui.rs) |
| Additional packet-handler entry | [crates/benilla-app/src/net/pfui.rs](crates/benilla-app/src/net/pfui.rs) |
| Host data / command bridge | [crates/benilla-ui/src/script/pfui_host.rs](crates/benilla-ui/src/script/pfui_host.rs), neighboring `pfui_host/` modules |
| Cast snapshots and remote channels | [crates/benilla-app/src/pfui/casts.rs](crates/benilla-app/src/pfui/casts.rs), [host casts](crates/benilla-ui/src/script/pfui_host/casts.rs) |
| Nampower packet / raw aura events | [app Nampower](crates/benilla-app/src/pfui/nampower.rs), [extension Nampower](crates/manila-pfui/src/nampower.rs), [Lua ABI](crates/manila-pfui/src/nampower.lua) |
| Spell queues, retries, queued scripts | [spell_queue.rs](crates/benilla-app/src/pfui/spell_queue.rs), [spell_retry.rs](crates/benilla-app/src/pfui/spell_retry.rs), [script_queue.rs](crates/benilla-app/src/pfui/script_queue.rs) |
| Item use and bag operations | [item_use.rs](crates/benilla-app/src/pfui/item_use.rs), [app bags](crates/benilla-app/src/pfui/bags.rs), [extension bags](crates/manila-pfui/src/bags.rs) |
| Aura projection and clock | [ui_aura/pfui.rs](crates/benilla-app/src/ui_aura/pfui.rs), [host auras](crates/benilla-ui/src/script/pfui_host/auras.rs), [ui_aura/clock.rs](crates/benilla-app/src/ui_aura/clock.rs) |
| Unit tokens, focus, nameplates | [host units](crates/benilla-ui/src/script/pfui_host/units.rs), neighboring `focus.rs`, `nameplates.rs`, `unit_events.rs` |
| Equipment sets and buttons | `crates/manila-pfui/src/equipment*.lua`, `crates/benilla-ui/src/script/pfui_host/equipment*.rs` |
| Loader / selected manifest | `crates/benilla-app/src/ui_script/{addons,manifest,lifecycle}.rs` |
| Optional Lua grammar | [third_party/lua-src/BENILLA.md](third_party/lua-src/BENILLA.md), `lua-5.1.5/{lparser.c,lmanila.h}` under that directory |
| Live probe variables | [capture/probe_env.rs](crates/benilla-app/src/capture/probe_env.rs) |

The grammar carry is in **`third_party/lua-src`**, not `third_party/mlua-sys`.
The registry key `manila.pfui.lua51` enables `...`, `#`, `%` per VM. Stock VMs preserve the
reference dialect. Legacy `arg` tables, stock table iteration, and positional formatting
must continue working beside modern addon code. Do not replace this with source rewriting.

Feature wiring: native `benilla` and browser `wenilla` expose `pfui` and `pfui-turtle`;
`benilla-app/pfui` enables the optional `manila-pfui` dependency, and `benilla-ui/pfui` owns
host seams. `pfui-turtle` includes `pfui` and selects the Turtle manifest.

## Loading, running, and the dependency popup

Use the owner's matching Turtle data at `/home/thorsten/Games/Turtle_WoW/Data`.
The latest isolated state folder is `/tmp/manila-pfui-reviewed-state`, with `AddOns/pfUI`
already present at handover. Check this before reuse; `/tmp` is not durable storage.
Normal persistent state is `benilla-config/` or a folder selected with `BENILLA_HOME`.

```sh
cargo build -p benilla --features pfui-turtle
WOW_DATA=/home/thorsten/Games/Turtle_WoW/Data WOW_HOST=localhost \
BENILLA_HOME=/tmp/manila-pfui-reviewed-state \
cargo run -p benilla --features pfui-turtle
```

This is the interactive launch; do not embed credentials. For a scripted launch follow the
unattended rules above and use a dedicated character. For Vanilla data select `--features pfui`.
An ordinary run without the feature rebuilds the stock client. `/pfui` opens configuration.

Important load facts:

- Ordinary `pfUI.toc` intentionally loads only `API_Check.lua`. The extension selects
  **`pfUI_ClassicAPI.toc`** or **`pfUI_Turtle.toc`** to load the complete addon.
- The full manifest still checks `CLASSIC_API_VERSION >= 11510`. We deliberately do **not**
  publish that broad capability. The dependency popup can claim pfUI is disabled even though
  the selected manifest loaded it. Acknowledge with OK; this is not itself a load failure.
- Resolving that notice properly needs a pfUI-specific capability check / maintainer
  cooperation or genuinely complete API support. Do not just suppress it or edit the install.
- `GetNampowerVersion()` returns `3, 0, 0` to enable the implemented pfUI integration.
  `ManilaNampowerCompatibility.emulated = true` identifies Manila and `fullNampower = false`
  stays honest. Windows DLLs cannot provide their APIs to Manila's own Lua VM.
- Install runs on **every in-game VM**, including reloads, before FrameXML/addons. Addon
  `(addonName, privateTable)` contexts survive XML includes / demand loads and unwind properly.

## What already exists

See the README's full available-primitives table before implementing another namespace.
High-level coverage includes:

- Lua compatibility, widget helpers, unit events, hooks, timers, pools, mixins, callback
  registries, and addon-private tables.
- DBC spell metadata and creature information; runtime power/cooldown/usability/range;
  forms, mounts, stealth, hidden known spells, descriptor-backed caster stats.
- Stock/focus/GUID/nameplate tokens, real focus targeting, reload retention, pooled nameplates,
  aura queries, observed cast/channel queries and modern events.
- Cached item metadata and item locations; carried and cold-bank counts; paced atomic bag
  operations; real item use through existing validators/sender; inventory and cursor events.
- Per-character equipment sets, full-GUID identity, sequential swaps, draggable action buttons,
  tooltips, binding persistence, and rename/icon/delete updates.
- Profile CBOR/JSON/zlib/Base64 helpers, malformed-data limits, social notes/player cache,
  quests, reputation, macros, loot history, icons, map reveal, bubbles, merchant grey sales.
- Nampower event switches and packet events; ordinary/forced/no-queue spell APIs; GCD,
  non-GCD and on-swing lanes; queued scripts; ground quickcasting; bounded transient retries.

These implementations have different levels of evidence. Do not infer that every helper
above has been compared live with the original client, or tested in every gameplay state.

## Implementation learnings and invariants

### Event ordering, reloads, focus, and commands

Snapshots must be readable **inside** callbacks. Engine projections participate in `UiFeed`:
cast/runtime spell data precedes Nampower cast events; runtime data follows stock unit,
spell-modifier and action feeds; raw aura edges follow the visible-aura projection.
Buffered events wait for the in-game UI; session cleanup must not wait for it.

Focus belongs to session memory, survives a real `/reload`, and announces restoration only
after the replacement VM and live snapshot are ready. Logout/disconnect clear it. It is not
a saved-variable setting. Per-VM memo state must reset on VM replacement; stale cast bindings
and stale chat profiling counters were independent reload bugs already fixed.

Targeted spell/item/macro actions must use the existing **ordered call stream** and sender.
An explicit target chain resolves after earlier macro selections commit, then retains its GUID
while queued. Missing explicit units refuse rather than silently casting on another unit.
Re-resolve item GUID locations at dispatch; a moved item is not necessarily a different item.
Macro selection restores after execution/error, without undoing unrelated later player input.

### Spell queues and retries

- The ordinary queue defaults to a 500 ms window and 55 ms buffer; channel window is 1500 ms,
  cooldown window 250 ms. `QueueSpellByName` forces retention outside the ordinary window.
  The two no-queue APIs bypass queuing for that request but still use stock validation.
- One GCD slot is replaceable. Up to six non-GCD entries retain FIFO order, run before GCD,
  and apply 100 ms default spacing. Shared-category replacement defaults off.
- `QueueScript` has one replaceable body: priority 1 before spells, 2 between non-GCD/GCD,
  3 after them (including the on-swing lane). GCD-only waits must work without a cast bar.
  Scripts do not invent queue-icon events. New scripts/pop callbacks may cancel released work
  before it reaches the sender, so release paths must recheck state.
- Terrain requests retain a finite world hit or resolved unit GUID. A sky aim invents no
  coordinate. Queue targeting defaults on; immediate/double-cast quickcasting defaults off.
  Right-click/stop-targeting cancellation must respect an existing cursor payload.
- On-swing defaults off, waits on the real server-held melee spell and validators, and uses
  a 500 ms post-send duplicate guard. Do not substitute guessed swing timers.
- Retry only our successfully sent requests on reasons `ITEM_NOT_READY` (`0x28`),
  `NOT_READY` (`0x3c`), or `SPELL_IN_PROGRESS` (`0x61`). No-queue/local refusals do not retry.
  Keep resolved GUID/terrain coordinates; newer player requests win. Current deviation:
  at most three resends within two seconds. The wire verdict has no cast ID, so overlapping
  same-spell replies have an inherent correlation limit.
- Stop/cancel, VM replacement, logout/disconnect and disabling retry settings discard the
  relevant pending state. Auto-attack remains an ordinary toggle, not a queued spell.
- Unknown `NP_` CVars delegate to stock handling; do not advertise unsupported settings.
  The five pfUI event switches default to zero and pfUI enables them at login. Settings live
  in `ManilaPfuiDB`, not the stock CVar table.

### Remote cast/channel support added last

`a2f805bb` implements observed remote channels. Start timing comes from observed
`SMSG_SPELL_START` plus finite level-scaled DBC duration; `UNIT_CHANNEL_SPELL` confirms it.
A positive cast-time prelude waits for successful `SMSG_SPELL_GO` before channel activation.
Keep a stable cast GUID through prelude/start/success/stop and fan out to current readable
aliases (target, focus, nameplate, etc.). Raw GUIDs are query tokens, not event aliases.

Clear/change of channel descriptor, expiry, despawn or session exit ends it. Matching failure
or replacement discards a pending candidate. An unrelated instant START must **not** end
an active channel; a late failure for the replaced spell must not end the replacement.
Channels do not emit `UNIT_SPELLCAST_INTERRUPTED`. Use `crate::net::Objects` lookup rather
than rebuilding a full object map per frame.

A channel first observed midway has **unknown timing**. Remote talents and pushback lack a
timing feed. Do not invent these values from descriptor presence or claim complete parity.
Tests in `cast_channel_tests.rs` drive registered handlers, real host queries, event callbacks,
preludes, replacement, unrelated instants, late failure, missing START, despawn, and cleanup.

### Auras, polarity, ownership, and group fallback

`4cba7720` adds `CROWD_CONTROL` / `!CROWD_CONTROL`, combinations with player/dispellable
filters, ownership retention, refresh notifications, source aliases, and group fallback.

- Raw descriptor slots are zero-based and are **not** filtered-list positions. Ordinary
  harmful slots sort before Turtle overflow harmful slots. Pagination and fill buffers must
  preserve the documented slots and continuation behavior.
- Missing filter defaults HELPFUL. An explicit filter without a polarity token considers
  both polarities; whole tokens, negation and case-insensitivity matter.
- DBC active-effect flags use reverse bits: effect 0 = 8, effect 1 = 4, effect 2 = 2.
  Roster-only flags zero leave all DBC effects as candidates. Use only active effects when
  actual flags are available.
- Known overflow harmful/control auras use DBC `AURA_IS_DEBUFF` (`0x04000000`), control
  effects, DOT/leech effects, and mechanics. Cancelable buffs and ordinary negative-range
  classification remain authoritative. The wire has no general polarity field: ambiguous
  or custom Turtle spells still use slot classification. This is conservative support,
  not complete polarity parity.
- Only an observed successful cast supplies ownership/duration for its hit targets.
  Auras already present on login have unknown caster; do not label them as the player's.
  Preserve annotations on unchanged `(slot, spell)` snapshots; clear them on removal/reuse.
- Recasts refresh estimated remote timing. Player server duration packets remain
  authoritative. Notify `UNIT_AURA` after updating readable state and only on actual change.
  Source lookup now handles party/raid pets, focus and nameplates through the shared token
  resolver. Shared event-alias enumeration is in `pfui_host/units.rs`.
- Stock `PLAYER_AURAS_CHANGED` deliberately ignores countdown-only changes, so modern
  duration refreshes need their own notifications. Suppress sub-ms join rounding noise;
  do not use an epsilon to hide genuine timing drift.
- Visible-unit cleanup previously erased the roster fallback when a party/raid member
  streamed out. Keep the fallback while a real group or group-pet alias owns the GUID;
  let the stock roster feed own its lifecycle. Ordinary streamed-out units still disappear.

New tests live in `ui_aura/pfui_tests.rs` and `manila-pfui/src/aura_tests.rs`; existing
Nampower slot-reuse tests also guard against stale ownership.

### Independent aura clock bug

`cdf58308` is deliberately separate and suitable for independent upstream review.
Aura feeding precedes the VM tick. Joining current-frame Bevy elapsed time to last-frame
`script.now()` shifted absolute Lua expiration by variable frame deltas, producing timer
jitter and repeated modern aura notifications. Live before-fix measurement saw approximately
216 extra updates in the idle observation window.

The fix projects `Time<Real>::last_update()` through the atomic `UiClock { anchor, ui_now }`
pair, using a signed delta and falling back when a standalone fixture has no clock/frame.
Tests cover uneven frames, a hitch, duration refresh and first-frame/no-clock fallback.
Live after-fix measurement found stable expiration and **zero extra updates over two idle
seconds**. Preserve this clock relationship when changing schedules or reload behavior.

### Bags, equipment, item use, and profiles

Atomic bag operations are bounded and await **both descriptor endpoints** after each move.
The local server rejects splitting an entire stack: whole-stack moves use normal swap/merge.
Reject mismatched/overflowing counted moves before send; a rejected/unanswered move cancels
the batch. `BAG_UPDATE_DELAYED` waits for the extension batch to settle so pfUI placement sees
consolidation results; stock `BAG_UPDATE` remains immediate. Preserve cursor and item totals.

Equipment uses complete 64-bit GUID strings, not item IDs or Lua floating-point numbers.
Re-resolve locations after each acknowledgement; paired rings/trinkets can settle with one
atomic swap. Missing/bank items are skipped; empty saved slots do not unequip current gear.
`EQUIPMENT_SWAP_FINISHED` reports observed placement rather than immediate dispatch, with a
10-second failure timeout. Existing native checks restore clothing and test button operations
and persistence; bank, bind-confirm, space shortage and wider conflicts remain to verify.

Named/direct item uses keep their target and re-resolve instance locations, including after
bind confirmation. They bypass vendor/bank/auto-equip click modes. Deprecated `bag`/`slot`
frame attributes intentionally keep stock bag-click semantics. Existing Hearthstone probes
were cancelled before completion and preserved inventory; targeted consumables are not live-proven.

Profile helpers implement standard CBOR, JSON, zlib and Base64, with 8 MiB / 64-level limits.
Cycles/unsupported values/malformed imports raise errors. Empty tables become objects; mixed
keys stay CBOR maps; JSON keys must be strings. Synthetic round-trips and standard fixtures
pass, but they do **not** prove compatibility with a DLL-generated pfUI profile. Never load
an encoded profile as executable Lua. Keep a copy of genuine reference output when obtained.

pfUI replaces global `error` with a chat printer. Compatibility validation captures the
original Lua `error` before addon load so invalid callbacks still fail. Error reporting must
also inspect the client script log, not rely on the visible chat printer alone.

## Verification baseline at code HEAD 4cba7720

The following results were observed on Linux. They are historical results, not a request to
rerun every gate for this handover-only change. Re-run relevant checks after implementation.
Logs below are in `/tmp` and may be lost across reboots.

| Check | Result | Log basename under `/tmp/` |
| --- | --- | --- |
| UI + extension, all features, no install | 1,720 UI pass, 1 ignored; 100 extension pass | `manila-pfui-gap-ui-tests-verified.log` |
| Focused app pfUI tests with Turtle data | 42 pass | `manila-pfui-gap-turtle-tests-verified.log` |
| Full app lib suite, feature on, no install | 4,433 pass, 8 fail, 9 ignored | `manila-pfui-gap-app-verified.log` |
| Extension strict clippy | Pass | `manila-pfui-gap-clippy.log` |
| Native Turtle build | Pass | `manila-pfui-gap-build-verified.log` |
| Browser check with `webgpu,pfui-turtle` | Pass | `manila-pfui-gap-wasm-feature.log` |
| Browser check with `webgpu` only | Pass | `manila-pfui-gap-wasm-stock.log` |
| Extension formatting + scoped new-file rustfmt, diff whitespace | Pass | Commands / commit evidence |
| `scripts/check.sh` | Fails on existing repository formatting, before later gates | `manila-pfui-gap-check.log` |

Earlier post-rebase feature-off app baseline had **4,379 pass, the same 8 fail, 9 ignored**
in `manila-pfui-rebased-stock-app-tests.log`. Feature-on before the last eight tests had
4,425 pass / 8 fail / 9 ignored. Realm tests also passed after the rebase; see
`manila-pfui-rebased-realm-tests.log` (9 + 7 + 1 + 2 passing tests across its targets).

Known full-app failing tests:

1. `dynamic_interior::tests::water_and_lava_bridge_publishes_changes_without_dirtying_idle_frames`
2. `entities::attach::redress::tests::a_gear_change_restamps_what_the_visual_is_dressed_with`
3. `game_plugins::schedule_tests::the_schedules_have_no_more_undeclared_orders_than_the_ceilings_say`
4. `game_tip::tests::a_pick_paints_the_node_the_loading_screen_spawns`
5. `game_tip::tests::the_dismiss_leaves_the_node_reshapeable`
6. `game_tip::tests::the_tip_outlives_a_second_raise_and_dies_with_the_screen`
7. `perf::journal::tests::the_header_ends_with_the_gpu_cells_in_bucket_order`
8. `ui_script::shape_gate::every_query_binding_answers_the_reference_s_return_arity`

Observed details: water default assertion expected High/2 but saw 1 before `app.update()`;
the schedule ceiling was 4927 versus 4965 reported actionable pairs; journal expected trailing
GPU cells but saw newer diagnostics; `GetDebugStats` intentionally returns a Manila string
while the reference arity fixture expects zero. Do not silently raise ceilings or relabel
these failures as new compatibility regressions without a fresh feature-off comparison.

**Data trap:** the complete app suite assumes pristine 1.12 fixtures in many places. Running
all of it against Turtle data produced 3,893 pass / 545 fail / 9 ignored in an exploratory
run, while new channel/aura tests passed. That result is not a useful Vanilla baseline.
Use `WOW_DATA=` for the known no-install baseline and matching Turtle data for the focused
extension/data tests. Data-gated tests must use `wow_data_or_skip!`; skipped tests are not
evidence of DBC behavior.

Whole-tree formatting currently has many pre-existing upstream hunks (roughly 72 files in
the last check). `ui_aura.rs` also has unrelated existing formatting differences; those were
left intact. Do not include a formatting sweep in a pfUI commit.

Useful commands:

```sh
WOW_DATA= cargo test -p manila-pfui -p benilla-ui --all-features --lib
WOW_DATA= cargo test -p benilla-app --features pfui-turtle --lib
WOW_DATA=/home/thorsten/Games/Turtle_WoW/Data \
  cargo test -p benilla-app --features pfui-turtle --lib pfui::
cargo fmt -p manila-pfui -- --check
cargo clippy -p manila-pfui --all-features --all-targets --no-deps -- -D warnings
cargo build -p benilla --features pfui-turtle
WASI_SDK=/tmp/manila-wasi-sdk cargo check --target wasm32-unknown-unknown \
  -p wenilla --no-default-features --features webgpu,pfui-turtle
WASI_SDK=/tmp/manila-wasi-sdk cargo check --target wasm32-unknown-unknown \
  -p wenilla --no-default-features --features webgpu
```

The wasm Rust target and WASI SDK 27 were installed; `/tmp/manila-wasi-sdk` was usable.
If gone, follow `scripts/web-setup.sh` and verify wasm-bindgen crate/CLI version agreement.
Native and wasm caches may need permission outside the workspace. No sandbox review rejection
remained unresolved at handover. Do not assume those approvals persist in a new environment.

## Latest live probe: valid result and reproduction notes

Local character **Pfuihealer** was a level-60 Human priest in Northshire, approximately map 0
`[-8950, -132.5, 83.5]`, with 1,717 HP. **Pfuitest** was used for earlier warrior/item probes.
These are dedicated test characters; recheck their current state and identity before reuse.
No character or server state should be assumed from a stale `/tmp` folder alone.

Probe artifacts at handover:

- `/tmp/manila-pfui-gaps-live.py`: native launch wrapper; reads `.probe-identity` privately.
- `/tmp/manila-pfui-gaps-live-serial.lua`: corrected serial probe chunk.
- `/tmp/manila-pfui-gaps-live-4.log`: final valid client/probe log.
- `/tmp/manila-pfui-gaps-live-run-4.log`: wrapper result, client exit 0.
- `/tmp/manila-pfui-gaps-live-4.png`: screenshot, inspected; pfUI target and focus First Aid
  bars visible near `0.35 / 6.00`, around 60 FPS.

The wrapper and probe are scratch experiments, not general-purpose safe automation. Inspect
their current contents before running; they may overwrite those log names. In particular,
the probe assumes Renew is initially unknown on this dedicated priest and unlearns it on
cleanup. Do not run that cleanup on another character with a legitimate Renew spellbook.
Prefer a new output basename to preserve evidence.

The valid sequence was bounded to about 73 seconds, began the Lua chunk around 10 seconds
after world entry, and used `C_Timer` polling for real server/spellbook/aura transitions:

1. Save target/focus and Nampower start/go event settings; enable those events temporarily.
2. Temporarily learn Renew rank 1 (`139`) on the dedicated priest; cast on self and wait for
   actual owned aura state, not just a timer after sending chat.
3. Recast after about 2.2 seconds; require expiration to advance. Observe two idle seconds:
   expiration stays stable and owned `UNIT_AURA` count does not grow.
4. Cancel via **`C_Spell.CancelSpellByID(139)`**, wait for disappearance, unlearn the temporary
   spell, and wait for the actual spellbook removal.
5. Target nearby friendly NPC **Deputy Willem** and focus it. Send **`.cast self 746`**:
   this server command makes the selected NPC channel First Aid on itself for six seconds.
   It does not mean that the player casts First Aid. No combat, consumable, or new spawn
   was needed.
6. Observe real `SPELL_START_OTHER` / GO packets and modern channel events. Both target/focus
   start callbacks must read active channel data; stop callbacks must read nil.
7. Restore target/focus and event settings; confirm Renew absent/unlearned and script error
   total zero. The valid run reported successful restoration and exited normally.

Useful final log evidence (UTC, 2026-10-10):

```text
17:54:27 aura-first=true source=0x0000000000000006
17:54:31 aura-refreshed=true idle-expiry-stable=true idle-updates=0
17:54:35 channel-start=target readable=true
17:54:35 channel-start=focus readable=true
17:54:35 remote-packet=0xF130000337013846 type=1 duration=6000
17:54:41 channel-stop=target cleared=true
17:54:41 channel-stop=focustarget cleared=true
17:54:41 channel-stop=focus cleared=true
17:54:45 remote-result=true starts=2 stops=3 packets=1 goes=1
17:54:48 restored=true
17:54:48 script-errors=0
```

The additional `focustarget` stop is a currently readable alias, not an extra remote cast.
Measured start-to-stop wall time was approximately 6.0055 seconds. This is a live implementation
check, not an original-DLL timing comparison. The dependency popup and initial pfUI setup
wizard were still visible in the screenshot; that does not invalidate the observed bars.

**GM caveat:** the admin/test login retained GM ON / faction 35 even with `WOW_GM=off` set.
`probe_shield.rs` only automatically manages accounts named `probe` followed by digits, so
the environment setting is not proof that this account changed GM state. Self healing and
friendly NPC channel checks are valid; no hostile reaction, enemy plate color, threat, damage,
or GM-off combat support was established. Keep unattended combat prohibited.

Earlier failed experiments and what they taught:

- `manila-pfui-gaps-live.log`: reproduced the aura clock jitter, but the probe used a nonexistent
  global `CancelSpellByID`, so cleanup failed. This was a probe namespace error.
- `...live-2.log`: clock fix yielded stable expiry / zero extra idle updates, but had that same
  cancellation error. Do not claim this as a clean full run.
- `...live-3.log`: corrected namespace; Renew and cleanup worked. `.cast back 746` is not
  supported by this server and returned syntax help; remote-channel result was false.
- `...live-4.log`: **`.cast self 746`** gave the successful channel result and restored state.
  This is the final valid proof. Earlier temporary learned-spell state was cleaned up.

Some existing startup warnings remain (Turtle `ItemButtonTemplate`, `ShowVKeyCastbar`,
`SetOwner` / BOTTOMRIGHT handling). Zero Lua errors does not mean zero warnings. Raw logs
include the full quoted Lua probe source; a blind search for the word `error` also matches
that source. Inspect actual script-error diagnostics and the script log counter instead.

## Original client comparison: current blocker

An isolated original Windows client with ClassicAPI and Nampower authenticated to localhost
but crashed with **error 132 at `0x00477D15` during character selection**, before entering
the world. Its login-screen glue VM does not expose `C_EncodingUtil`. Therefore login-screen
codec probes cannot settle profile interoperability, and the failed run establishes neither
compatibility nor incompatibility.

The user-provided `start-wow.sh turtle-wow laptop` path remains the intended comparison
route. Use an isolated copy/configuration for experiments, protect player sessions, and leave
the install unchanged. Find a concrete reason to fix/reconfigure the reference run or obtain
a real exported profile fixture; repeatedly launching the same failing Wine setup adds no
evidence. No real player profile was imported or modified by this investigation.

## Remaining work and suggested acceptance checks

This is proposed work order, not evidence that any of these tasks is already complete.
Check actual pfUI call sites before broadening the API surface.

| Gap | Next action and proof needed |
| --- | --- |
| Nampower item queues and bulk disenchanting | Read public contracts and `pfUI/modules/nampower.lua`; `DisenchantAll` is genuinely absent and its pfUI slash command is conditional. Add a separate real worker using instance GUIDs, stock item/spell paths, acknowledgement/cancellation, VM/session cleanup and documented semantics. First test captured commands and failure paths; do not destroy player items to get a live result. Broad support does not authorize unattended destructive inventory tests. |
| Profile sharing | Enter world on a working original ClassicAPI client or obtain a genuine pfUI export. Test export/import in both directions, nested/mixed/empty data, malformed input and limits; use temporary profiles and restore state. Standard codec unit tests alone cannot close this gap. |
| Cast/event completeness | Investigate sent/targeting/quiet events, interruption classification/immunity, manual terrain clicks, remote timing modifiers and wider focus/plate aliases. Preserve stable IDs, snapshot-before-event ordering and no invented unknown state. |
| Aura completeness | Compare ambiguous/custom Turtle overflow spells and broader change/stack/source cases live. Already covered: basic CC filters, known overflow DBC cases, refresh stability, source retention, reused slots and group fallback. Pre-existing caster ownership is absent from the 1.12 descriptor; do not promise to infer it perfectly. |
| Bag sorting | Extend safe coverage to multi-bag, specialized bags, bank access, full bags and rejection/timeouts. Preserve exact item GUID/count totals and original layout; wait for server confirmations. |
| Equipment manager | Wider characters, bank/missing items, action-slot conflicts, reload/disconnect, bind-confirm and insufficient-space cases. Check outgoing commands and resulting placement/persistence, not only acceptance return values. |
| Nampower comparison | Resolve five/six non-GCD capacity discrepancy, retry timing, cursor timing and on-swing behavior against the reference. The stock decoder lacks the auto-attack component count; do not invent one. Current combat lanes have command-capture coverage, not live combat proof. |
| Unit/widget integration | More token-aware verbs, focus/plate descriptor/aura notifications, uninspected equipment IDs, click-casting and addon macro-runner live paths. |
| Extras | Optional visible-player cache sweep, group/vendor/totem probes and more map/bubble validation. |
| Browser | Build and run with a real WebGPU adapter; test login/world entry, addon load, reload, persistence, UI interaction and representative features. Both compile modes pass; no browser runtime claim yet. |
| Dependency notice | Seek a genuine pfUI-specific capability contract or maintainer agreement. Do not claim complete ClassicAPI merely to remove the popup. |

For a browser run, `wenilla-host` serves the dev page on port 8090 and relays localhost
realmd:3724 / mangosd:8085. Linux Chrome may need `--enable-unsafe-webgpu`; adapter absence
can abort at `wgpu::create_bind_group_layout`. Test adapter availability first. Browser
page changes must reach both `web/index.html` and `crates/wenilla-realm/templates/play.html`,
and the `scripts/web-build.sh` copy list determines shipped assets. Check the build script's
actual feature forwarding before assuming its default output includes pfUI.

## Audit workflow and interpretation

The complete pinned manifest's static inventory found **108 dotted API references**, now
available. That deliberately excludes widget methods, ordinary globals, dynamic lookups,
event coverage, gameplay behavior and some optional branches. Do not call this full support.

```sh
cargo run -p manila-pfui --features audit --bin pfui-audit -- \
  /tmp/manila-pfui --flavor turtle \
  --data /home/thorsten/Games/Turtle_WoW/Data > /tmp/pfui-handover-audit.json
```

The default audit flavor is ClassicAPI, so specify Turtle when matching the live setup.
The audit reads MPQs and loads stock FrameXML with app camera CVars and a 1024x768 virtual
viewport before attempting pfUI. A dirty stock load must not be attributed to the addon.
`stock_ui_attempted` / `bootstrap_attempted` distinguish skipped phases. It has no renderer,
network, real inventory or persistence; `gameplay_verified` is always false. Its nonzero exit
is meaningful; do not conceal failures by exporting a compatibility version.

## Other reusable local evidence

If `/tmp` survives, earlier named artifacts may help avoid repeating work:

- `manila-pfui-review-*`: earlier audits, clean-load checks, cast before/after screenshots,
  stock UI/app baselines, and extension checks.
- `manila-pfui-rebased-*`: rebase-era feature-on/off native/wasm checks and realm tests.
- `manila-nampower-focus-reload-live.log`: actual focus reload comparison.
- `manila-nampower-ground-queue-live.log`: queued self-directed Flare after a heal.
- `manila-nampower-queue-apis-live.log`: forced/no-queue/cancellation/ID-overload self-heal
  scenarios; icon cleanup and temporary queue settings restored.
- `manila-nampower-retry-*`: decoded transient verdict / real command tests and rebuild logs.
- `manila-nampower-reviewed-*`: earlier reviewed live outputs and summary.

Prefer the final `*-verified.log` and `...live-4.log` artifacts for the latest state. Earlier
files may contain failed intermediate probes or builds. Do not overwrite evidence casually
or treat a scratch wrapper as reviewed production automation. No game data, credentials,
reference binaries or full third-party addon code belong in a handover commit.

## Addendum: second session, 2026-10-10

Written by a later session that continued from the text above. Code state for this addendum is
the commit that adds it; the seven commits before it are that session's work. Where this
section and the text above disagree, this section is newer.

**What changed.** The earlier audit counted only dotted `C_*` references. A live inventory
looked up every global, upper-case constant and widget method pfUI's source names in the running
VM, and compared every event pfUI registers with `reference/1.12-events.tsv` and the events this
client fires. It found about thirty missing ClassicAPI globals, a set of constants and labels,
six unfired events and two tooltip methods. They are now implemented; the list, deviations and
evidence are in [the extension README](crates/manila-pfui/README.md).

| Commit subject | Purpose |
| --- | --- |
| Add the ClassicAPI globals pfUI calls outside its C_ namespaces | Globals, tracking, equipment flyout query, constants, labels, colour methods |
| Fire the ClassicAPI state events pfUI registers for | Form, durability, reputation, hearth bind, global mouse |
| Add repair cost, mail item links and unit speed for pfUI | Durability panel, mail hooks, tooltip speed line |
| Name each session event the pfUI bind handler registers for | Satisfies the handler registry test |
| Size a tooltip in the call that shows it | **Engine fix, ungated, kept separate for upstream review** |
| Add hyperlink item comparison and tooltip re-anchoring for pfUI | Shift-hover comparison |

**Corrections to the text above.**

- Everything under `/tmp` was lost. The pinned sources were re-fetched and the installed
  `benilla-config/AddOns/pfUI` was confirmed identical to pfUI `40dd9dd6`. Scratch probes and
  logs of this session were also temporary.
- `.probe-identity` declares the warrior **Pfuitest** (level 1, Northshire), not the priest.
- With Turtle data the headless audit reports three stock errors in Turtle's own FrameXML
  (`Turtle_TransmogUI`, `Turtle_ShopUI`, `EverlookBroadcastingCo`) and therefore never
  bootstraps pfUI: `bootstrap_attempted` is false. Use the native client for that data.
- The dependency popup is more than a cosmetic notice: its `OnShow` focuses an edit box, so
  after every login keyboard input goes to that box until the popup is dismissed. A scripted
  run that presses keys must first call `StaticPopup_Hide("PFUI_CLASSICAPI_REQUIRED")`; a
  movement probe that skipped this read a false zero.
- `WOW_PROBE_BINDER=1` fails its own step 3 in this build (`BENILLA_GOSSIP_ICONS.binder`
  resolves to nothing) and stops before binding. It still teleports the character to Dolanaar;
  return it with `WOW_PROBE_CHAT=".go xyz -8950 -132.5 83.5 0"`.

**Baselines at the end of the session (Linux).**

| Check | Result |
| --- | --- |
| Extension tests, all features | 116 pass |
| UI tests, with and without the feature | 1,721 pass, 1 ignored |
| Full app lib suite, feature on, `WOW_DATA=` | 4,434 pass, the same 8 failures, 9 ignored |
| Extension strict clippy | Pass |
| Native Turtle build; stock native check | Pass |
| Browser check, `webgpu,pfui-turtle` and `webgpu` | Pass |

The schedule-ceiling failure now reports 4,968 actionable pairs against the ceiling of 4,927;
it was 4,965 before this session, and three of the difference are this session's new systems.

**How the live checks were run.** Each used `WOW_UNATTENDED=1`, `WOW_NOSOUND=1`, the declared
identity, a copy of `benilla-config` as `BENILLA_HOME`, `WOW_PROBE_EXIT_AT`, and a
`WOW_PROBE_LUA` chunk reporting through `ProbeLog`. A small recording addon in that state
folder's `AddOns` registered for the new events at load, which is what showed that login and a
cross-map teleport fire none of them. Temporary changes were a learned Defensive Stance, a
reputation offset of 50 and an unequipped main hand; each was undone and confirmed.

**Still open, in the order I would take them.**

1. **Inline `|T…|t` textures: done.** `crates/benilla-app/src/pfui/inline_texture.rs` swaps
   each escape for a private-use placeholder before the markup parser sees it, so the stock
   decoder in `benilla-ui/src/markup.rs` is untouched and upstream's "no texture escape" tests
   still pass. The upstream files carry only `#[cfg(feature = "pfui")]` hooks: the engine
   field and glyphless check (`ui_text/engine/mod.rs`), width, block height and the four
   substitution points (`ui_text/layout/measure.rs`, `layout/mod.rs`), the unit scale
   (`ui_text/measurer.rs`, `ui_script/extract/text.rs`) and the shadow filter. Seen live in a
   font string, a tooltip and a chat line. A second pass matched ClassicAPI's documented
   behaviour (`docs/InlineTextureEscapes.md` in its repository): raw markup in EditBoxes,
   grown chat rows, same-frame loading, `r:g:b` tint, outline ink, stripped player-typed
   escapes (`ui_chat/feed.rs` hook) and `GetCoinTextureString`. Left: chat rows grow by whole
   font heights, and `|A…|a` atlas markup needs an atlas registry first.
2. **`PLAYER_SWING`.** The decoded attack log already feeds `AUTO_ATTACK_SELF`; the swing clock
   needs weapon speed, next-swing spells and ranged shots. Live proof needs GM-off combat, which
   is not allowed unattended.
3. **`DisenchantAll` and item queues**, as described above; destructive, so command-capture only.
4. **Comparison stat lines** for `SetHyperlinkCompareItem`.
5. **Hearthstone bind live check**, once the binder probe passes or by a manual bind.
6. Profile exchange with a real ClassicAPI client, the browser runtime, and the combat-side
   Nampower comparisons remain exactly as described above.

**Decided by the owner after this addendum was first written.** The extension now publishes
`CLASSIC_API_VERSION = 11510`, so the dependency popup and its focus steal are gone, and the
instructions above that say never to publish it are superseded. The surface is still partial:
`ManilaClassicAPICompatibility.fullClassicAPI` is false. The owner also allows reasonable
changes to the main client where pfUI support needs them, as long as they stay easy to carry
across upstream merges.

## Suggested prompt for the next session

> Read PFUI_HANDOVER.md, AGENTS.md, the required Benilla docs, and
> crates/manila-pfui/README.md. Continue full pfUI/Nampower compatibility on
> feat/pfui-compatibility, preserving the optional extension structure and separate commits
> for ordinary engine bugs. Start from actual missing behavior and close one coherent gap
> with real state/commands, meaningful tests and valid live evidence where safe. Keep the
> install read-only and use the declared dedicated test identity. Do not fake complete
> ClassicAPI/Nampower, repeat already fixed aura/channel work, or claim browser/profile
> interoperability without proof. Report baseline failures and remaining limitations clearly.
