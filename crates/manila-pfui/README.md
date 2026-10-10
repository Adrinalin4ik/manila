# pfUI compatibility extension

This is an optional compatibility extension for **brues-code/pfUI**, including its optional
modules. **Full pfUI is not supported yet.** The native and browser entry crates expose the
`pfui` feature; ordinary builds retain the stock API. The extension includes a headless audit
of the complete addon manifest and leaves unsupported APIs absent.

The contract under investigation is:

- [pfUI source revision `40dd9dd6`](https://github.com/brues-code/pfUI/tree/40dd9dd6db1cb8d15d565ec5bdaf39e70175e611)
- [ClassicAPI reference revision `917b1564`](https://github.com/brues-code/ClassicAPI/tree/917b15640e2bdc2c8f24e0e0d4d520514d76ff52)

ClassicAPI is a DLL for the original Windows client. Manila must implement the API behavior
in its own engine; loading the DLL cannot provide those functions to its Lua VM.

## Isolation and integration

Most compatibility code lives in this crate. The engine connections are in new
`benilla-ui/src/script/pfui_host/` and `benilla-app/src/pfui.rs` files, with small
`#[cfg(feature = "pfui")]` hooks in the existing event, loader, item-cache, social-feed and
VM-creation paths. Attribute-bound frame hovers use a separate gated targeting module.
Auras use separate feeds beside the stock snapshots; cast and quest packets are observed through
additional handlers in the extension. Macro and reputation adapters live in new files beside
their stock modules and reuse their existing mutation and visible-row mechanisms.
The additional item metadata uses separate per-VM state, without extending stock item structs.
The vendored Lua parser has three guarded branches and `lmanila.h`; it reads a **per-VM
registry flag**, absent in ordinary clients. Building this crate as a workspace member does
not activate the extension on stock VMs. Tests create stock and extended VMs side by side.

The client installs the extension on every in-game VM, including reloads, before stock
FrameXML and third-party addons load. Addon-private tables and `(addonName, privateTable)`
arguments flow through TOC chunks, XML includes and load-on-demand dependencies. Stock
`this`, `event` and `argN` globals remain available alongside modern positional handler arguments.
Class, race, faction and spell metadata comes from the owner's localized DBCs.

Build the experimental native integration with:

```sh
cargo build -p benilla --features pfui
# For a matching Turtle installation instead of Vanilla:
cargo build -p benilla --features pfui-turtle
```

Place the separately obtained pfUI checkout under `benilla-config/AddOns/pfUI` (or
`$BENILLA_HOME/AddOns/pfUI`), outside the WoW install. The features select
`pfUI_ClassicAPI.toc` or `pfUI_Turtle.toc`, respectively. Browser builds can select the same
features on `wenilla` and need their normal wasm toolchain. The `webgpu,pfui-turtle`
combination passes a wasm compile check; browser runtime behavior remains unverified.

To run the native Turtle build, point it at the matching data and the state folder containing
`AddOns/pfUI`:

```sh
WOW_DATA='/path/to/Turtle_WoW/Data' WOW_HOST=localhost \
BENILLA_HOME='/path/to/benilla-config' \
cargo run -p benilla --features pfui-turtle
```

Log in normally and select a character. For Vanilla data, use `--features pfui` instead.
Keep the feature on the run command: an ordinary `cargo run -p benilla` builds the stock client.
Use `/pfui` in game to open its configuration. This is an experimental native setup; full
gameplay support and browser support remain unverified.

The ordinary `pfUI.toc` deliberately loads only `API_Check.lua`. ClassicAPI selects the full
flavor TOC in the original client. The full TOC still checks `CLASSIC_API_VERSION >= 11510`.
This partial extension **does not publish that version**, suppress the check, or claim that
ClassicAPI's complete API exists. The dependency notice therefore remains visible. Replacing it
requires a pfUI-specific capability check or cooperation with its maintainer, alongside real
engine support. The notice can say pfUI is disabled even though this build selected its full
manifest and loaded the UI. Acknowledge it with OK. The extension separately implements
pfUI's Nampower integration and returns `3, 0, 0` from `GetNampowerVersion`, which enables those
pfUI modules. `ManilaNampowerCompatibility.emulated` explicitly identifies the implementation;
`fullNampower` remains false. Installing Windows DLLs does not supply APIs to Manila.

Benilla's [contribution rules](../../docs/CONTRIBUTING.md) exclude new client-mod APIs and
extension-only engine hooks. Consequently, compatibility APIs belong in this extension/fork.
An independently demonstrated 1.12 bug fix can be submitted upstream separately. This is a
portability boundary, not a promise that upstream will merge the parser carry. The extension
builds against this checkout's benilla revision; upstream provides no stable extension API.

## Available primitives

| Surface | Behavior |
| --- | --- |
| Lua expressions | Opt-in `...`, `#`, `%`; Lua string methods; `string.match`/`gmatch`/`reverse`; `select`; missing math functions |
| Stock Lua behavior | Legacy `arg` tables and WoW's positional `string.format` stay usable; no filesystem or OS libraries are opened |
| Addon file calls | Shared private tables across TOC chunks, XML inclusions and demand-loaded addons; enclosing contexts are restored |
| Widgets and events | `SetSize`/`GetSize`, `SetShown`, `SetFormattedText`, `HookScript`, `hooksecurefunc`, `RegisterUnitEvent`, `IsEventRegistered`; numeric-string paint colours, modern arguments with legacy globals |
| Frame attributes | Case-insensitive storage, modifier lookups, recursive-safe `OnAttributeChanged`, target/assist/focus/menu/stop-cast actions, GUID-directed spells and named/inline macros through the ordered stock call stream; frame unit attributes publish real mouseover picks |
| `C_Timer` | `After`, `NewTimer`, `NewTicker`, cancellation, bounded iterations, next-frame dispatch; one anonymous frame, idle with no timers |
| `RunNextFrame` | A one-shot timer on the following UI frame |
| General helpers | `Mixin`, `CreateFromMixins`, `tInvert`, `SafePack`, `Round`, `Clamp`, `CopyTable`, object/texture pools |
| Creature metadata | Host DBC class names and `C_CreatureInfo.GetRaceInfo` / `GetFactionInfo` |
| Spells | Localized DBC metadata, legacy ten-value tuples with player/pet book and name/rank lookup, live power costs, cooldowns, usability and GUID-based range queries through the existing engine validators |
| Caster stats | Per-school damage modifiers from the player's descriptor and haste percentages from each visible unit's cast-speed multiplier |
| Known spells | Aura-effect searches over the complete known-spell set, including hidden talents, rather than only visible spellbook rows |
| Player states | Descriptor form IDs and mount state, real stealth auras, authorized mount/form cancellation from DBC effects; warrior stances remain active |
| Nampower integration | Explicit 3.0.0 compatibility identity, packet-backed spell/aura/heal events, raw aura slot and stack edges, event CVars, GUID-directed named casts, forced/no-queue spellbook overloads, GCD/non-GCD/on-swing queues, ground quickcasting, bounded server-rejection retries, prioritized queued scripts and pfUI queue indicator |
| Observed casts | Cast/channel queries over spell packets, remote channels confirmed by live descriptors, stable per-cast identifiers, pushback and modern start/success/stop/failure events |
| Auras | Seven pfUI-used queries, raw descriptor slots, pagination, fill buffers, polarity/source filters and authorized player-aura cancellation |
| Units and clock | Stock, focus, GUID and nameplate query tokens; local-player GUID checks, typed/raw power, NPC subtitles, party/raid membership and realm-local calendar |
| Focus | `FocusUnit`/`ClearFocus`, target-chain reads over visible-unit snapshots, real GUID targeting and focus identity/health/name/level/target events |
| Nameplates | Real pooled frames, stable unit-token slots, target chains and frame-valued creation, unit-added and readable unit-removed events |
| New items | Bag glow flags keyed by instance GUID, login baseline, move-stable flags, removal/clear operations and change-only notifications |
| Creature identity | Live NPC/pet template IDs, creature GUID fallback outside view, and no pet-number/entry confusion |
| Items | Cached 18-value item info, classification, qualities, levels, family, stack/sell limits, usability, equipped checks and async item helpers |
| Bags | Item IDs, charge counts, openable/instance-lock checks, paced atomic moves and swaps through the existing sender; counts include cold bank objects |
| Actions and equipment | `GetActionInfo`, `GetInventoryItemID` over stock inventory/inspect snapshots, player durability and equipment-slot constants |
| Social records | Structured friend and `/who` records, localized class tokens, online counts, real friend GUIDs, per-character notes and rate-limited silent name queries |
| Player cache | Opt-in `RememberPlayer`/`GetPlayerInfoByName`, real name-query replies, GUID and name collision handling, realm isolation and saved-variable persistence |
| Quests | Visible log IDs and cached server templates, including required items; accepted/removed events from real descriptor slots, successful turn-in events from server confirmations |
| Reputation | Structured visible rows and watched factions, retaining real IDs, folds, thresholds and synthetic-header exclusions |
| Macros | String-icon creation/editing using the existing macro table, caps, save mechanism and change events |
| Equipment sets | Per-character GUID snapshots, ignored slots, counts/locations, create/save/rename/delete, persistence, sequential atomic server swaps, draggable action buttons and summary tooltips with retained missing-item names |
| Inventory events | One `BAG_UPDATE_DELAYED` after stock bag handlers; `PLAYER_EQUIPMENT_CHANGED` compares actual instance GUIDs and publishes 1-or-nil presence |
| Cursor queries | `GetCursorInfo` reads the stock item, spell, macro, merchant or money payload and equipment sets; `CURSOR_CHANGED` records real payload transitions |
| Tooltip subjects | Item, spell and unit queries follow native builders, cleared callbacks, async spell views and hide; focus/GUID unit tooltips reuse the stock builder; `GameTooltip_Hide` closes the real tooltip for friend-note callbacks |
| Loot history | Bounded real roll history with player votes, observed dice, winner/pass completion, late name-query identities and modern notifications |
| Icons | MPQ and loose icon catalogs, filtered item/spell pools, spellbook and equipment extras |
| Map reveal | Current viewed zone and all its DBC overlays, actual BLP tile dimensions, partial edges and supported HD tile layouts |
| Chat bubbles | Pooled real frames over the native lifetime, distance, visibility and projection decisions, with source regions pfUI can style |
| Merchants | Actual buyback count/item-ID helpers and paced grey-item sales over current carried GUIDs, canceled on vendor changes |
| Addon registry | Existence and optional dependencies over the real loader registry |
| Colors | The pfUI-used `ColorMixin` methods, `CreateColor`, `C_ColorUtil.GenerateTextColorCode` |
| Callbacks | `CallbackRegistryMixin` and `EventRegistry` owner registration/removal, real frame-event callbacks, dispatch and reentrant subscriptions |
| `EventUtil` | `ContinueOnPlayerLogin`, `ContinueOnAddOnLoaded`; once per registration, immediate when already ready |
| `C_EncodingUtil` | Eight profile-sharing calls: CBOR/JSON serialization, zlib compression, Base64 encoding, and their inverses |

Frame spell actions reuse the native cast binder and validators with the clicked unit's GUID,
without changing selection or falling back to self-cast. Ground spells use the unit's position.
Named and inline macros temporarily select the clicked unit, run through an addon `RunMacro`
when present or the stock chat-line event otherwise, and restore the prior selection. Lua target
queries inside the macro use the committed selection, and unwind even on runner errors. An absent
clicked unit sends nothing. Named item actions and `C_Item.UseItemByName` resolve a carried
instance by ID, link, decorated name, GUID or explicit location. They retain the unit GUID and
re-resolve the item's slot before sending, including after bind confirmation. Missing targets,
pending inventory operations, transformed items and confirmations from an older VM refuse the
request. Explicit missing or unknown unit strings refuse rather than using another target;
the unknown-string refusal differs from ClassicAPI's documented no-target treatment.
Direct item uses bypass vendor, bank and auto-equip bag-click modes. Attribute values such as
`"0 1"` and deprecated `bag`/`slot` attributes retain ClassicAPI's documented stock bag-click
path. Item queues and cursor/unit placement helpers are outside this subset.
The player cache defaults
to disabled; explicit `RememberPlayer` calls and real name-query replies populate it when enabled.
Replies assign wire gender directly; getters return the 2/3 sex convention pfUI's race icons use.
The optional visible-object sweep remains absent. Notes and cache records use `ManilaPfuiDB` in the
client's own saved-variable file, outside the WoW install. Note keys include both realm and
character, while player-cache keys include the realm.
The focus GUID is retained in session-owned memory across a UI reload, including a focus change
immediately before `/reload`. The replacement VM announces the restored focus after its live
unit snapshot and interface are ready. Logout and disconnect clear that memory, so another character or session
does not inherit it. Focus is not written to saved variables.

`GetShapeshiftFormID` returns the descriptor's DBC form ID, including `0` outside a form;
it does not return the stance bar's position. `IsMounted` reads the mount display descriptor.
Deviation: `IsStealthed` scans helpful stealth auras and excludes a mounted player, rather than
using the DLL's visibility bit shared by mounts and some forms. Mount and form cancellation
match actual DBC aura effects and pass the stock cancellation authorization gate; warrior
stance cancellation is explicitly ignored. No spell-name or form-ID lists are hardcoded.

Atomic bag operations use a bounded queue and wait for both descriptor endpoints after each
server move. Whole-stack moves use the normal swap/merge packet: this server rejects splitting
an entire source stack. Counted moves reject a mismatched or overflowing destination before
sending, and a rejected or unanswered operation cancels the remaining batch. Deviation:
`BAG_UPDATE_DELAYED` waits until the extension's batch settles, so pfUI's placement phase reads
all consolidation results. Stock `BAG_UPDATE` events remain immediate. The stock cursor is
untouched; these APIs return acceptance into the queue, rather than network-send success.
Native probes exercise pfUI sorting after a real stack split, preserve item totals, and restore
the original slots and counts. Bank and specialized-bag sorting still need live coverage.

The aura feed retains actual descriptor slots, player durations when known, and unknown sources
as unknown. Successful observed casts supply aura ownership and DBC duration for their hit
targets; the player's server duration remains authoritative. Auras already present at login
retain unknown ownership. Turtle polarity overflow and crowd-control filters remain unimplemented. Cast observation supports
ordinary visible unit tokens, local channels and observed remote channels. Remote channel timing
starts with an observed `SMSG_SPELL_START`; `UNIT_CHANNEL_SPELL` confirms the channel and stops it
when the descriptor clears, changes spell or leaves view. A cast-time prelude waits for
`SMSG_SPELL_GO` before becoming a channel. Start, success and stop retain the cast identifier
across target, focus and nameplate aliases. Remote durations use the caster-level-scaled DBC;
remote talent modifiers and pushback are unavailable, and a channel first encountered midway
has no invented start time. Targeting/sent/quiet events, remaining focus/nameplate event coverage
and spell-interruption immunity still need integration. Interruption
immunity is returned as `nil` when unknown. Query availability alone does not establish full
Nampower behavior.

Nampower compatibility follows the addon-facing contracts in
[EVENTS.md](https://github.com/brues-code/nampower/blob/2c7f51cd436e9b0ce88271e84ce5a77c19f2c23b/EVENTS.md)
and [SCRIPTS.md](https://github.com/brues-code/nampower/blob/2c7f51cd436e9b0ce88271e84ce5a77c19f2c23b/SCRIPTS.md).
The optional extension owns its `NP_` settings in `ManilaPfuiDB`, independently of Benilla's
stock CVar table. The five pfUI event switches default to zero and pfUI enables them at login.
The ordinary spell queue defaults to a 500 ms window and 55 ms buffer, binds the target at
press time, replaces an earlier queued GCD spell, and re-enters the normal validator at release.
`QueueSpellByName` forces a request outside the window and retains it through a long cast.
`CastSpellByNameNoQueue` and `CastSpellNoQueue` bypass queuing for that individual request;
they still use the normal validator. Spellbook casts accept slots, learned names and
`spellId:123` strings, including pet spells and explicit unit targets. Self-directed casts
do not depend on an unrelated selection remaining streamed.
Explicit `target` tokens and their target chains resolve after preceding macro selections have
committed, then retain that GUID while queued. A missing explicit target never falls back to
the current selection or to self-casting.
Up to six pending non-GCD spells retain FIFO order and take priority over a queued GCD spell.
Nampower's pinned README specifies six entries, while its `QueueSpellByName` script reference
says five; this implementation follows the README pending an original-DLL capacity comparison.
A repeated spell replaces its own entry; replacing a shared nonzero start-recovery category is
opt-in through `NP_ReplaceMatchingNonGcdCategory`. Successful non-GCD sends apply the
`NP_NonGcdBufferTimeMs` spacing (100 ms by default). `SpellStopCasting` clears all spell queues
before release; session exit drops them. Auto-attack, item and profession actions continue through their
existing paths. Unknown `NP_` settings delegate to
stock handling; their existence is not advertised. `GetCastInfo` uses the same epoch as
`GetTime` and reads the actual GCD leg; `GetPlayerAuraDuration` uses raw slots and server timers.
`QueueScript` retains one replaceable script inside the ordinary queue window, with priority
1 before spells, 2 between the non-GCD and GCD lanes, or 3 after both. Idle requests and requests
outside the window execute immediately through the ordered call stream. Queued scripts wait
for the actual GCD even without a cast bar, read selection when they execute, and clear on
session exit or VM replacement. Invalid priorities raise instead of retaining an unexecutable
request. Scripts do not generate spell-queue icon events.
Terrain spells use `NP_QueueTargetingSpells` (default 1) and
`NP_TargetingQueueWindowMs` (500 ms). Queued terrain casts retain the request's finite world hit
and quickcast through the ordinary validator and terrain commit, regardless of the ordinary
quickcast setting. Explicit unit targets retain their GUID and use that unit's position at
release. `NP_QuickcastTargetingSpells` and `NP_QuickcastOnDoubleCast` default to zero; enabling
them also affects individual no-queue calls. A sky aim invents no coordinate. Stop-targeting
and world right presses cancel queued terrain requests; a held cursor payload pre-empts the
right-press cancellation. Terrain quickcasts emit cast types 4/5. Cursor movement timing still
needs comparison with the original DLL; ordinary manual terrain-click event coverage is pending.
The independent on-swing lane defaults off (`NP_QueueOnSwingSpells`). When enabled, it retains
one replaceable request until the server clears the existing melee spell and the cast validator
is ready, without estimating swing timers. `NP_OnSwingBufferCooldownMs` defaults to 500 ms and
prevents an immediate duplicate queue after a committed strike. Queue events use codes 0/1;
priority-3 scripts wait for this lane too. Strike combat behavior still needs a controlled
GM-off comparison; the command-stream tests do not establish that live behavior.
`NP_RetryServerRejectedSpells` defaults to one. Only our own successfully dispatched spell
requests can retry, and only after server reasons `ITEM_NOT_READY` (0x28), `NOT_READY` (0x3c)
or `SPELL_IN_PROGRESS` (0x61). Retries retain the resolved unit GUID or terrain wire coordinates,
pass through the stock validator and queue spacing, and yield to newer queued player requests.
Local refusals and explicit no-queue casts never retry. Acceptance cancels a waiting retry
without discarding ordinary player queues; stop-casting, terrain cancellation, session exit and
VM replacement discard the relevant retry state. Disabling the CVar also discards waiting retries.
Deviation: retries are limited to three resends within two seconds of the first send; the public
contract does not specify these bounds. Item/GO casts and ordinary manual terrain-click sends
remain outside this retry subset. Synthetic decoded-verdict tests verify this behavior through
the registered stock handlers and real outgoing commands; live transient rejection and original-DLL
retry timing still need comparison. The 1.12 verdict has no cast identifier, so same-spell
overlapping verdicts cannot be distinguished beyond the engine's single outstanding-cast slot.
The added functionality stays in extension files, with small feature-gated stock hooks.
Extension snapshots and event delivery participate in the stock `UiFeed` phase. Cast snapshots
and spell runtime data precede Nampower cast events; spell runtime data follows the stock unit,
spell-modifier and action feeds. Aura edges follow the visible-aura projection. Buffered events
wait for the in-game UI; logout/disconnect cleanup does not wait for it.

Equipment sets distinguish duplicate item IDs by their complete 64-bit GUID strings. The executor
re-resolves a GUID after each server update, so paired rings/trinkets need only one atomic swap.
Missing items and bank items are skipped, and empty saved slots do not unequip current items.
The equip executor leaves the cursor untouched. Deviation from ClassicAPI: `EQUIPMENT_SWAP_FINISHED` waits for observed
placement, rather than reporting dispatch success immediately; a rejected, deferred or unanswered
swap fails after ten seconds. Native probes exercise restoring real clothing, equipment-set
button dragging, rename/icon updates and persistence across reload. Bind-confirm, combat,
disconnection and insufficient-bag-space cases remain unverified.

Encoding uses standard CBOR, JSON, zlib and RFC 4648 Base64 rather than an executable Lua
serializer. Cycles, unsupported values and malformed input raise Lua errors. Profile payloads
are limited to 8 MiB and nesting to 64 levels. Nonempty contiguous integer-key tables become
arrays; mixed keys remain CBOR maps; empty tables become objects. JSON object keys must be
strings. These are the supported profile-data rules, not a claim of complete ClassicAPI
serialization parity. Live exchange with a DLL-generated pfUI profile still needs verification.
The native synthetic profile fixture round-trips nested tables, arrays, booleans, fractional
numbers, empty tables and UTF-8 text through the CBOR/zlib/Base64 pipeline. The current
original-client comparison is blocked before its codec probe: the isolated Wine client with
ClassicAPI and Nampower authenticates to localhost, then raises error 132 at `0x00477D15`
during character selection. That failed reference run is not evidence of codec compatibility
or incompatibility, and no player profile was imported or changed.
A separate login-screen probe confirms that `C_EncodingUtil` is absent from the original
client's glue VM, so the comparison requires entering the world.

Quest lifecycle feeds ignore login and reload bulk sync, wait for cached accepted rows, and
distinguish confirmed turn-ins from abandons. Deviation: a collapsed quest has no stock public
row index, so its accepted event carries index 0 and its real questID. A complete-to-incomplete
transition produces the modern accepted event; the stock client's chat announcement remains on
its existing new-quest/cleared-failure path.

Spell-damage bonuses currently use the server's player modifier fields. Servers that omit
computed gear or talent contributions need an additional client calculation; these reads do
not claim ClassicAPI's complete computed-value behavior on every realm. Haste uses the actual
unit cast-speed multiplier and returns zero when the field or unit is unavailable.

Equipment action bindings persist with their character's sets. They share the engine cursor
and grid, displace real actions through the ordinary server clear queue, and yield to server
actions changed by another client. A local cursor tag is rejected by the stock packet packer;
no new server action type or generated macro is used. Renames, icon updates and deletion reach
every bound button. Locked items and active casts prevent an action-button equipment swap.

Timers use the engine's `GetTime` clock and require its ordinary `UiScript::tick` calls.
Callbacks scheduled during a timer callback wait until the next frame. A long frame fires a
ticker once rather than running an unbounded catch-up loop. Timer callback errors are caught,
as ClassicAPI documents; other addon-load and session errors remain visible.
Compatibility validation retains Lua's original `error` function because pfUI replaces the
global with a chat printer; an invalid callback must still raise before it can be installed.

## Audit the complete addon

Obtain a separate source checkout outside the game install:

```sh
git clone https://github.com/brues-code/pfUI.git /tmp/pfUI
git -C /tmp/pfUI checkout 40dd9dd6db1cb8d15d565ec5bdaf39e70175e611
cargo run -p manila-pfui --features audit --bin pfui-audit -- /tmp/pfUI > /tmp/pfui-audit.json
```

To additionally load stock FrameXML and probe the first runtime blockers:

```sh
cargo run -p manila-pfui --features audit --bin pfui-audit -- \
  /tmp/pfUI --data '/path/to/your/WoW/Data' > /tmp/pfui-audit.json
```

By default the tool reads the complete `pfUI_ClassicAPI.toc` and recursively follows XML file
inclusions. Add `--flavor turtle` to select the full Turtle manifest. Its JSON report contains
the source revision, file/compiler errors for both VMs, and each
dotted `C_*`/`EventUtil` reference with its source files and function availability. The scanner
excludes comments and strings. Static references can include optional branches and miss dynamic
lookups; they are a dependency inventory, not verified API contracts. Globals and widget
methods require additional runtime probes.

With `--data`, the audit uses the owner's MPQ chain directly. It builds stock FrameXML with
the app-owned camera CVars needed by its dropdown OnLoads and an explicit 1024×768 virtual
viewport. A dirty stock load prevents the addon bootstrap, so setup errors are never attributed
to pfUI. It has no renderer, network
session, character state or saved-variable persistence, and cannot verify real inventory moves,
rendering or gameplay. `stock_ui_attempted` and `bootstrap_attempted` distinguish skipped stages
from clean ones; `gameplay_verified` is always false. The command exits nonzero on unresolved
API references or load failures. It never installs addons, edits manifests, logs in, or writes
into the game install.

## Engine work required for full pfUI

| Area | Backing behavior needed |
| --- | --- |
| Units and focus | Broader focus/plate aura and descriptor event feeds, further query/verb token integration |
| Auras | Ownership of pre-existing auras, crowd-control filters, Turtle overflow polarity and broader aura-change coverage; observed successful casts provide caster/duration information, while the 1.12 aura descriptors omit it |
| Casting and spells | Complete modern cast-event behavior and interruption classification; remote channel timing has no talent/pushback feed; runtime spell queries inherit existing engine validator limits |
| Remaining Nampower behavior | Item queues, bulk disenchanting and the broader Nampower API remain outside this subset; original-DLL retry timing, non-GCD capacity, terrain cursor timing and manual-click event coverage need comparison; the auto-attack component count is unavailable from the stock decoder; non-GCD/on-swing ordering is covered by command-capture tests, with combat behavior still unverified |
| Items and bags | Remaining instance helpers; broader multi-bag, bank and specialized-bag sorting verification |
| Equipment sets | Broader character/action-slot conflict verification; failures and bind-confirm behavior |
| Profile sharing | Live interoperability with the original ClassicAPI client; standard codec fixtures are covered |
| Browser | Live verification in a WebGPU browser; the Turtle feature passes the wasm compile check, while current live coverage is native |
| Remaining extras | Optional visible-player sweep, live vendor/group/totem checks and broader map/bubble validation |
| Widgets and globals | Further token-aware verbs, equipment IDs for uninspected players; broader click-casting and addon macro-runner live coverage |

The target is the complete pfUI manifest, including equipment management, sorting and sharing.
Implement only the functions/events that inventory and runtime probes demonstrate pfUI uses,
with real backing state. A namespace with empty functions would make the addon appear to load
while silently breaking features. Loading, drawing and using the UI need separate verification,
followed by reload/persistence checks and live tests on a dedicated account. All 108 dotted
API references discovered in the pinned manifest are available. That inventory excludes
widget methods, globals, event coverage and incomplete API behavior; complete inventory
coverage alone cannot prove full support.

## Checks

```sh
cargo test -p manila-pfui --all-features
cargo fmt -p manila-pfui -- --check
cargo clippy -p manila-pfui --all-features --all-targets --no-deps -- -D warnings
cargo check -p benilla-app --features pfui-turtle
# With the wasm target and WASI SDK installed (scripts/web-setup.sh):
WASI_SDK=/path/to/wasi-sdk cargo check --target wasm32-unknown-unknown \
  -p wenilla --no-default-features --features webgpu,pfui-turtle
```

Tests cover VM isolation, legacy and modern varargs, stock formatting, timer cancellation and
reentrancy, event readiness, pfUI-style callbacks, standard transport fixtures, malformed imports,
source scanning, XML/nested addon contexts, widget inheritance, numeric-string paint channels,
unit-event filtering, item-cache
arrivals, cold-bank counting and bag operation queues. Further checks cover action/equipment
queries, attribute-driven selection, recursive attribute handlers, social records, note persistence,
player-cache name collisions, realm isolation and `/who` rate limiting. The audit deliberately
reports unresolved engine APIs instead of hiding them behind compatibility-version flags.
Additional tests cover real overlay tile layouts and independent view tables, tooltip subject
clearing and late spell views, bubble replacement/recycling, loot result ordering, runtime spell
queries, quest bulk-sync suppression and turn-in/removal ordering, bag-event coalescing and
same-entry equipment-instance swaps. Additional tests cover nil aura slots, raw slot stability, filters/cancellation, cast identifiers,
typed mana/rage/energy, hidden known-spell queries, live/fallback creature IDs, reputation folds,
quest requirements, string-icon macros, plate token stability/removal/recycling, group rosters and GUID-based new-item tracking. Equipment tests retain duplicate GUIDs across saves,
resolve a paired swap with one send, wait for acknowledgements, fail on timeouts and reload saved
sets with character/realm isolation.
Equipment-button tests cover drag displacement, passive-spell refusal, rename/icon/delete
updates, cursor transition payloads, saved bindings, server action conflicts and locked-item
refusals through the actual swap queue.
Bag-worker tests cover chained partial/whole merges, split descriptor arrivals, placement over
confirmed slots, all-or-nothing counts, timeout and failure cancellation. Name-reply tests cover
opt-in writes, corrected wire gender, recycled names, realm isolation and late loot identities
without duplicate completion events.
Click-casting tests cover GUID snapshots, named/inline macro bodies, scoped Lua target reads,
selection restoration on errors, missing units, form cancellation and warrior stance retention.
Caster tests cover the complete legacy metadata tuple, both spellbooks, passive and exact-rank
lookups, school indexing, haste/slow conversion and invalid or removed unit descriptors.
Item-action tests cover references and exact locations, ordered target chains, moved GUIDs,
vendor/bank bypass, locked and missing items, one-shot bind confirmation, transformed-item
refusal, VM replacement and replacing an extension confirmation with an ordinary bag click.
Native live item probes use the dedicated warrior's existing Hearthstone through four paths:
ID, exact bag location, named frame action and bag-slot frame action. Each receives one server
cast start, is cancelled before completion, ends idle and preserves the inventory snapshot.
The event settings are restored. These self-directed probes do not establish targeted scroll,
bind-confirmation or combat behavior; those paths currently have command-capture coverage.
Queue tests drive the real ordered-call applier and capture its client commands: forced casts
outside the window, same-macro double casts, no-queue refusals, long waits, replacement,
non-GCD priority/spacing/category settings, cancellation, VM replacement and self-casts with a
vanished unrelated selection. Further checks cover explicit target chains in macro order,
retained GUIDs after selection changes, all three queued-script priorities, replacement,
reentrancy and real GCD waits without a cast bar. The headless client includes the optional plugin when enabled;
source audits exclude its modules when the feature is disabled.
Terrain checks cover the targeting window and switch, source/destination packet coordinates,
retained hits after cursor/selection changes, quickcast settings, no-queue validation, sky aims
and cancellation before a ready release. On-swing checks cover default-off behavior, the
post-send duplicate window, long server-held strikes, replacement and normal-lane isolation.
Focus checks replace actual Lua VMs sharing the session memory, verify a single restore event
after the live snapshot, and drive the logout/disconnect handlers to check character isolation.
Native live coverage includes focus GUID/name retention and a visible pfUI focus frame after a
real reload, plus a queued self-directed Flare after a heal: one push/pop, one terrain cast and
one server confirmation without a cast failure. Temporary spellbook/settings changes are restored.
The rebuilt retry-enabled native client also passes the five self-heal queue scenarios: forced
double cast, explicit no-queue, preserving a forced request across a refused no-queue attempt,
cancellation and spell-ID overload. Queue icons finish hidden and the temporary queue-window
setting is restored. These self-directed checks do not establish hostile or GM-off combat behavior.
Auto-attack checks keep the ordinary attack toggle working while a cast is pending, without
creating a spell-queue entry.
Retry regressions dispatch decoded verdicts through the registered stock handlers and capture
outgoing commands. They cover all three transient reasons, terminal and unowned failures,
no-queue and CVar controls, resolved GUID and terrain retention, missing units, self-cast fallback,
minimum spacing, the resend/lifetime limits, late acceptance, newer user requests and session/VM
cleanup. Fresh scripts and pop-event callbacks can cancel a released request before it reaches
the sender; ordinary queued casts restore the selection present when they execute.

All implementation in this crate is independently written under MIT OR Apache-2.0.
ClassicAPI's GPL implementation and the player's FrameXML are not bundled here.
