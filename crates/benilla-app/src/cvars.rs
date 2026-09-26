//! benilla's **CVar registry** — the client's configuration store, host side (decisions 0954,
//! 2303). The reference keeps every player setting in one engine-side table
//! (`ConsoleVar.cpp`, a `TSHashTable<CVar>` of 0xc4-byte records: name, value, default, the
//! latch slot, and a change callback the owning subsystem hands `CVar::Register 0x63db90`).
//! This module is that table: [`Cvars`] holds the live value of every registered row, it is
//! what survives a VM replacement, and it is what `config.toml` is composed from. The script VM
//! carries a **mirror** of it for Lua's synchronous `GetCVar`/`SetCVar`
//! ([`benilla_ui::script::UiScript::seed_cvars`]); the mirror's writes ride a queue back here.
//!
//! - **The registered set** ([`REGISTERED`]): only vars something actually reads — a host knob,
//!   or (since 1140) a live Lua consumer. **A row's default is the REFERENCE's default**
//!   (decision 1804), and every row says where it stands against it — [`Registered::reference`],
//!   a mandatory column with no "unknown" variant. Two tests hold it: one checks each row's claim
//!   in both directions, the other pins the deviation set as a readable list. A row the reference
//!   **latches** (flag bit1 at its register site) says so with [`Registered::latched`]; a
//!   structural test insists every row has a reader somewhere in the source.
//!
//!   **The reference column has one source**: all 214 of the reference's `CVar::Register` sites,
//!   with name, help, flags, default string, callback, category and record global. A new row
//!   looks its answer up there rather than re-deriving it.
//!
//! - **The change callback is a Bevy observer** ([`CvarChanged`]). An accepted move of a row's
//!   applied value is triggered as one event, and the subsystem that owns the knob observes it
//!   beside the knob (`sound::on_cvar`, `video::on_cvar`, …), writing only its own resource — so
//!   `Res::is_changed()` on a knob is honest again, which the 32-resource bundle this replaced
//!   could not offer (it deref-mutted every knob on every write; three consumers carried the
//!   written workaround). The registry never applies anything itself: it has no knob, and no arm.
//!
//! - **The latch** ([`Row::pending`]): a write to a latched row is staged, `GetCVar` keeps
//!   answering the applied value, and nothing fires until [`Cvars::commit_latched`] — the
//!   reference's `0x639ec0`, `CVar::Commit 0x63e060` on the fourteen gx records, called from
//!   inside `RestartGx` (the video window's Okay). A staged value that is never committed is dropped at exit, as
//!   the reference's is (`SaveConfig` writes `rec+0x20`, the applied value).
//!
//! - **Boot**: read `benilla-config/config.toml` ([`crate::local_state`]) into the registry and
//!   fire the observers synchronously, before anything ordered after [`CvarLoad`] runs; when the
//!   UI VM exists, seed its mirror from the registry so `GetCVar` answers what the client is doing.
//! - **Sync**: per frame, drain the VM's registrations and writes into the registry, push the
//!   registry's own writes into the mirror, and trigger the accepted moves.
//! - **Save**: dirty + one quiet second → rewrite `config.toml` atomically (and flush on
//!   `AppExit`). The file holds **only values that moved off their default** — a diff, not a
//!   dump — plus any entries this build doesn't know (a newer build's keys, an addon's before it
//!   registers them: preserved verbatim, warned once).
//!
//! **Env overrides win for the session and never touch the file**: `WOW_UI_SCALE`/`WOW_FARCLIP`
//! and their siblings beat the loaded config (they exist to make taste iteration a relaunch —
//! pinning one into the config would make the A/B sticky), the session runs and saves around
//! them, and the file keeps whatever it already said for those keys ([`Cvars::own_for_session`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use bevy::prelude::*;

use crate::ui_script::VmMemo;
use benilla_ui::script::{SeededCvar, UiScript};

/// One host-backed CVar: its registered name, benilla's shipped default, and — the column that
/// exists so a divergence is a *decision* rather than an accident — **what the reference ships**
/// ([`Reference`]).
///
/// **The standard this table encodes: a benilla option's default IS the reference's.** Shipping
/// something else is allowed and sometimes right, but it costs a [`Reference::Deviates`] row
/// naming the reference's own value and the reason. `Reference` has no `Default` and no "unknown"
/// variant, so adding a row means answering the question; and because [`Reference::Same`] and
/// [`Reference::Deviates`] both carry the reference's value, the test
/// [`tests::defaults_stand_where_the_reference_column_says`] checks the claim in both directions —
/// a `Same` row that stopped matching fails, and so does a `Deviates` row that has quietly come
/// back into agreement.
pub(crate) struct Registered {
    /// The registered name, in the reference's own spelling.
    pub(crate) name: &'static str,
    /// What a fresh `benilla-config` runs at — seeded into the knob, and what `GetCVar` answers
    /// until the player moves it.
    pub(crate) default: &'static str,
    /// Where that value stands against the reference's.
    ///
    /// `#[allow(dead_code)]` because this column's readers are a **human** and
    /// [`tests::defaults_stand_where_the_reference_column_says`] — nothing at runtime consults
    /// it, and nothing should: it records what the *reference* does, which is an input to the
    /// choice above it, never a value this client acts on.
    #[allow(dead_code)]
    pub(crate) reference: Reference,
    /// The reference registers it with flag bit1 (`rec+0x1c & 0x2`, decision 2303): a write is
    /// staged in [`Row::pending`] and applied only at the latch boundary
    /// ([`Cvars::commit_latched`]). Read off each register site's `flags` argument (2 or 3); a test
    /// pins the set.
    pub(crate) latched: bool,
}

impl Registered {
    /// Mark the row latched — see [`Registered::latched`].
    pub(crate) const fn latched(self) -> Self {
        Self {
            latched: true,
            ..self
        }
    }
}

/// benilla's default, weighed against the reference's own.
///
/// **What "the reference's default" means here** is the **registered factory default** — the
/// string the real client's `CVar::Register` (`0x63db90`) call passes for that name, byte-read —
/// or, for a setting 1.12 keeps FrameXML-side instead of as a CVar, the value
/// `UIOptionsFrame.lua` boots it at. Two readings it deliberately is **not**, both of which have
/// misled a reader before:
///
/// - **Not what the reference install's `Config.wtf` says.** That file is one player's saved
///   *diff*: `SaveConfig 0x63d980` writes only what has moved off its default, so a line's mere
///   presence is proof the registered default is something *else*.
/// - **Not, by itself, what a fresh install ends up running at.** `hwDetect` rewrites sixteen
///   video CVars out of `VideoHardware.dbc` before the first frame, and the `useUiScale`-OFF leg
///   computes a UI scale of its own. Where the client's own boot code overrides the registered
///   string like that, benilla follows the *behaviour* and the row says so — that is
///   [`Reference::Overridden`], not a deviation.
///
/// Every row's provenance — the register-site VA, or the FrameXML line — belongs in the comment
/// above it. That is not decoration: it is what lets the next reader re-check the claim instead
/// of trusting this table.
#[allow(dead_code)] // read by a human and by the test — see `Registered::reference`
pub(crate) enum Reference {
    /// The reference registers this exact default string, and benilla ships it too. The copy is
    /// deliberate: the test compares the two, so an edit to `default` that forgets the reference
    /// fails here rather than shipping.
    Same(&'static str),
    /// The reference *registers* `registered`, but its own boot code overwrites that before the
    /// first frame, and benilla's `default` is what that code lands on — faithful to the client's
    /// behaviour, which is what the standard asks for. `why` is the override.
    Overridden {
        registered: &'static str,
        why: &'static str,
    },
    /// The reference ships `value`; benilla knowingly ships something else. `why` is the reason
    /// and the decision that ruled it. **This is the variant that answers "where do we differ?"**
    /// — every row here is a standing choice somebody made, reviewable as a list.
    Deviates {
        value: &'static str,
        why: &'static str,
    },
    /// The reference has no such setting to match: benilla's own knob (`renderScale`), or an era
    /// name for something 1.12 never made settable. `why` says which — and, where the reference
    /// still *behaves* some way, what that behaviour is.
    Ours(&'static str),
}

/// A row whose default is the reference's own registered string.
const fn same(name: &'static str, default: &'static str) -> Registered {
    Registered {
        name,
        default,
        reference: Reference::Same(default),
        latched: false,
    }
}

/// A row that follows the reference's *behaviour* where its own boot code overrides the
/// registered string — see [`Reference::Overridden`].
const fn overridden(
    name: &'static str,
    default: &'static str,
    registered: &'static str,
    why: &'static str,
) -> Registered {
    Registered {
        name,
        default,
        reference: Reference::Overridden { registered, why },
        latched: false,
    }
}

/// A row that knowingly leaves the reference's default — `value` is the reference's, `why` the
/// recorded reason.
const fn deviates(
    name: &'static str,
    default: &'static str,
    value: &'static str,
    why: &'static str,
) -> Registered {
    Registered {
        name,
        default,
        reference: Reference::Deviates { value, why },
        latched: false,
    }
}

/// A row the reference has no counterpart for.
const fn ours(name: &'static str, default: &'static str, why: &'static str) -> Registered {
    Registered {
        name,
        default,
        reference: Reference::Ours(why),
        latched: false,
    }
}

/// The table as the script VM's registrar wants it — `(name, default)` pairs, in table order.
///
/// The registrar has no use for the [`Reference`] column: that column is for *us* (and for the
/// test that holds the standard), never for the engine.
pub(crate) fn registered_pairs() -> impl Iterator<Item = (&'static str, &'static str)> {
    REGISTERED.iter().map(|r| (r.name, r.default))
}

/// The host-backed CVars. Grows one row per knob a settings page actually wires — never ahead of
/// the knob (see the module doc) — and every row states where its default stands against the
/// reference's ([`Registered`]).
///
/// Unregistered for want of a reader, though pfUI's `hdgraphic` writes them through `ConsoleExec`:
/// `lodDist` (`0x688524`, "100.0", read at `0x6afb1d` for the doodad LOD swap), `footstepBias`
/// (`0x6888b4`, "0.125", read at `0x68fcb6`), `mapObjLightLOD` (`0x6886ec`, "0") and `SkyCloudLOD`
/// (`0x6d1d33`, "0"). `DistCull` (`0x688570`) and `texLodBias` (`0x6885e2`, whose sink `0x672640`
/// is `ret 4`) have no reader in the reference either. `maxLOD` is no CVar: none of the 214
/// `CVar::Register` (`0x63db90`) sites names it.
pub(crate) const REGISTERED: &[Registered] = &[
    // The realm the session is on — a REAL 1.12 CVar (`0x83f2d0`, persisted: the client builds its
    // SavedVariables path from it through `0x5ab7d0`), and a live
    // Lua consumer in the strongest sense the honest-tree rule asks for. `Ace/AceState.lua:27` does
    // `ace.trim(GetCVar("realmName"))` inside `SetGameState`, which every Ace addon runs at
    // PLAYER_ENTERING_WORLD — so a nil there was `gsub(nil)` and took the whole Ace family down.
    // 18 corpus folders read the name.
    //
    // The default is EMPTY, deliberately and not as a guess: the value is written from the session's
    // real realm the moment addons load (`ui_script::addons::load_third_party`), so the default only
    // ever describes a client that has not connected. `0x85d684` holds a string
    // `"Last realm connected to"` beside the registration, but that reads like the CVar's HELP text
    // rather than its value and nothing here needs to resolve it — `""` is what `ace.trim` handles
    // cleanly, and inventing a realm name would be worse than admitting we have none yet.
    same("realmName", ""),
    // The address of the logon server — the reference's own CVar (register site `0x5ab6a6`; the
    // registration's string neighbours are `realmlist.wtf`, "Address of realm list server" and
    // `us.logon.worldofwarcraft.com:3724`).
    // A **string** row: the registry takes any string for it, and `realmlist::on_cvar` judges it.
    // The default diverges knowingly — see `realmlist::DEFAULT_REALMLIST`.
    deviates(
        crate::realmlist::CVAR_REALMLIST,
        crate::realmlist::DEFAULT_REALMLIST,
        "us.logon.worldofwarcraft.com:3724",
        "1667: that host has not resolved since 2019, so shipping it makes every first launch a \
         DNS failure; benilla dials the machine it is running on",
    ),
    // The implicit AFK clear (2088) — a REAL 1.12 CVar, byte-read off its own registration
    // (`0x5e24d4 push 0x82e748`, handle taken from the store AFTER the call at `0x5e24ef` into
    // `[0xc4d68c]`, whose single reader `0x5eb84b` tests `[cvar+0x28]` for non-zero). Registered
    // default `"1"`.
    //
    // It gates FIVE implicit clears, not one: any chat send whose type is not `0x14` (which is why
    // `/dnd` clears AFK before marking), plus Jump, forward/back, strafe and turn
    // (`0x513d36`/`0x514e23`/`0x514f0b`/`0x514fca`). With the CVar off the clear is a **total**
    // no-op — no echo, no mirror write, no packet.
    same("autoClearAFK", "1"),
    same("MasterVolume", "1"),
    same("SoundVolume", "1"),
    same("MusicVolume", "0.4"),
    same("AmbienceVolume", "0.6"),
    // The three 1.12 sound enables (registrar defaults all "1", `0x45737a`/`0x45739b`/`0x460a9d`):
    // `MasterSoundEffects` is the MASTER "Enable All Sound" checkbox (SoundOptionsFrame.lua
    // index 1 — its callback sets the engine-wide pause flag), NOT an SFX-only toggle; 1.12
    // has no `EnableSound`/`EnableSFX` at all.
    same("MasterSoundEffects", "1"),
    same("EnableMusic", "1"),
    same("EnableAmbience", "1"),
    // Error speech (1815) — the race/sex refusal lines your character says. A real 1.12 CVar
    // (`CVar::Register` at `0x457877`, registrar default `"1"`) and a real 1.12 checkbox:
    // SoundOptionsFrame.lua's `ENABLE_ERROR_SPEECH`, index 4, which the master enable greys along
    // with Ambience.
    same("EnableErrorSpeech", "1"),
    // Sound while the window is in the background (1847). **Not a 1.12 CVar and not a 1.12
    // checkbox**: none of the reference's 214 `CVar::Register` (`0x63db90`) sites names it, and
    // `SoundOptionsFrame.lua` declares seven checkboxes
    // (indices 1, 2, 4-8) and four sliders, none of them this. `CVar::Register` is the only
    // creation path, so `Config.wtf` can hold no such key either. The spelling is the later-era
    // engine's — the `autoLootDefault` / `nameplateShowEnemies` posture, where benilla's
    // persistence IS the CVar store (0954) and a setting 1.12 never made settable takes the era
    // name rather than an invented one.
    //
    // **`same`, not `ours`**, for the nameplate pair's reason: the reference has no CVar to match
    // but it very much has a *behaviour* to match, and it goes quiet in the background —
    // unconditionally, on `WM_ACTIVATE` → event-bus category 2 → `0x7a4860`'s
    // `FSOUND_SetMute(-3, active ? 0 : 1)`, music included. "0" IS the reference's own behaviour.
    // The knob is `SoundConfig::background_sound`, which carries the mechanism and the one
    // disclosed divergence.
    same("Sound_EnableSoundWhenGameIsInBG", "0"),
    // Zone reverb (1153). The binary registers this one `"1"` (`0x4573be`) and we register it
    // `"0"` — the only row here that knowingly leaves the registrar's default, because the
    // reference's reverb is EAX-over-hardware and that hardware has not existed since Vista:
    // `"1"` would ship audio the real client has never actually produced (bug B236).
    // `SoundConfig::reverb` carries the evidence.
    deviates(
        "SoundReverb",
        "0",
        "1",
        "1153: the reference's reverb is EAX-over-hardware and that hardware has not existed \
         since Vista, so \"1\" would ship audio the real client has never actually produced \
         (B236)",
    ),
    // The mix-ahead depth (1857) — 1.12's own `SoundBufferSize` (`0x4571ca`, flags 2: latched,
    // read once at sound-system init), "sound buffer size (milliseconds)": FMOD 3's mix-ahead
    // buffer, the distance the software mixer runs ahead of the output device. benilla's own
    // output has the same quantity — the render thread's ring ahead of the IO callback
    // (`sound::output`) — so the reference's dial drives it, in the reference's unit. The
    // registrar's default is a two-way host choice: `0x457520` returns "50" or "100" from an
    // OS-version probe (strings at `0x835e10`/`0x835e0c`, byte-read 2026-09-02). Ours is the
    // larger of its two, because the stall the crackle was measured from was a whole IO cycle
    // long and the depth exists to hide the next one. Applies at the next launch, like the
    // reference's.
    same("SoundBufferSize", "100").latched(),
    // The output limiter (1551) — benilla's own, not a 1.12 CVar. The reference needs no such DSP
    // (its mix is FMOD 3's and its headroom lives in the SFX-bus auto-duck); benilla sums into f32
    // behind a hard clamp, and every WoW SFX is mastered to full scale, so two overlapping kits
    // clip. Registered so the fix can be A/B'd live against the defect it fixes.
    ours(
        "SoundOutputLimiter",
        "1",
        "1551: benilla's own — the reference's FMOD 3 mix needs no such DSP; we sum into f32 \
         behind a hard clamp, and every WoW SFX is mastered to full scale",
    ),
    overridden(
        "uiScale",
        "0.9",
        "1.0",
        "a fresh reference client never consults this CVar: `useUiScale` registers \"0\" \
         (`0x48fce4`), and the OFF leg `0x492f70` computes clamp(768/height, 0.9, 1.0) instead — \
         0.9 at 854 px tall and up, which is every window we ship against. It is 1.0 at 768 and \
         below, where our flat 0.9 does diverge; `ui_script::DEFAULT_UI_SCALE` carries that. \
         See `useUiScale` below, whose row this one used to say did not exist",
    ),
    // **`useUiScale` (`0x8430c0`, default `"0"`)** — the switch the row above gates on.
    //
    // Its absence used to be argued for here as *"nothing reads it, and registering it would only
    // offer a switch whose ON path we do not implement"*, and the first half of that has been
    // false since the interface went stock: `ContainerFrame.lua:483` and `UIDropDownMenu.lua:525`
    // both branch on `GetCVar("useUiScale") == "1"`, and `OptionsFrame.lua:13` gives it a
    // checkbox. Nobody noticed because the only thing that said so was a host warning with
    // nowhere to go (decision 2135, which is how this was found).
    //
    // Registering it changes no behaviour today — `nil ~= "1"` and `"0" ~= "1"` take the same
    // branch — and makes the read the reference's read rather than an accident. The ON path
    // lands where the reference's does, because our `uiScale` default *is* the reference's OFF-leg
    // result: at `useUiScale = 1` both clients scale the bag frames and the dropdown list by
    // `GetCVar("uiscale")`, and both read 0.9 there on every window we ship against.
    same("useUiScale", "0"),
    same("farclip", "350"),
    // **`nearclip` — farclip's other half, and a knob we had been holding as a constant** (2163).
    // `0x68867a` passes name `0x84ffb0` `"nearclip"`, default string `0x84fb48` `"0.1"`, help
    // "Near clip plane distance", flags `1`, callback `0x688d90`, record `[0xc7f348]`.
    //
    // **The reader is the camera, and it re-reads every frame.** `0x511bc0` — the per-frame camera
    // outer, sole caller `0x483094` — stamps `[cam+0x38]` from this record's float before the
    // `[cam+0x48]` branch and unconditionally: `511bcf mov eax,[0xbe1078]; 511bd4 fld [eax+0x24];
    // 511bdc fstp [esi+0x38]`, with `[0xbe1078]` the handle `0x50b728` caches from a `"nearclip"`
    // Lookup. `farclip` is the next four instructions. `benilla_world::view::stamp_near_clip` is
    // that, and `ViewDistance` is the pair.
    //
    // **Why it was not registered for so long, and why that reasoning was wrong.** The near plane
    // was a `CAM_NEAR = 1.0/9.0` const documented as the reference's own, on the true finding that
    // the callback's *derived global* `[0xc7b480]` has one writer and no readers. The camera does
    // not read that global; it reads the record. So the ctor's
    // `0x3de38e39` = 1/9 is overwritten by the first frame's stamp and never reaches a picture —
    // a verified-but-partial mechanism, which docs/METHOD.md §4 names as the classic trap.
    //
    // pfUI's `hdgraphic` writes it (`ConsoleExec("nearClip " .. arg*2/100)`, 0.06..0.30 across its
    // extended stops) — every value inside the reference's own `[0.01, 0.33]`, which is why that
    // module could ask for it.
    same("nearclip", "0.1"),
    // The Controls-page trio (0961). `deselectOnClick`/`mouseInvertPitch` are 1.12's own
    // Interface Options CVars (UIOptionsFrame.lua indices 45/1); their defaults are the
    // reference behaviors benilla already shipped (empty-world click clears the target; no
    // pitch invert). `autoLootDefault` is era's — no 1.12 CVar exists, vanilla only had the
    // shift gesture — default off, like era's engine registrar.
    same("deselectOnClick", "1"),
    // *Block Trades* (decision 1764) — 1.12's own `BlockTrades` (`0x842fbc`), the General-box
    // checkbox at index 14 whose tooltip is "Block all incoming trade requests.". Registered
    // **"0"**: the reference's own `0x4bf7bc` leg only refuses when the CVar is set, so an
    // unset/absent value has to mean "trades allowed" — and a client that shipped with trades
    // blocked would refuse every trade until the player found the box. The knob is
    // [`crate::ui_trade::BlockTrades`], read by the incoming-request answerer.
    same("BlockTrades", "0"),
    // 1.12's own `autoSelfCast` — a friendly cast that binds nothing falls back to the caster.
    // The behaviour has been here since the cast arm landed, welded to a Resource default; it is a
    // CVar now because 1.12's `TOGGLEAUTOSELFCAST` binding is `GetCVar`/`SetCVar` over this exact
    // name and there was nothing for it to toggle (decision 1745).
    //
    // Register site `0x6e731d`, default string `"0"`, record `[0xceac34]`, one reader at
    // `0x6e53d7` (1804). **This row used to cite `0x870dc0` as the record; that is the NAME
    // string**.
    //
    // **benilla ships it ON and the reference registers "0"** — a named deviation that predates
    // this row (`cast_target::AutoSelfCast`): with it off, an unbindable friendly cast falls into
    // the reference's targeting-cursor machine, which is unmodeled, leaving no path at all. Flip
    // to the reference's default when that machine lands.
    deviates(
        "autoSelfCast",
        "1",
        "0",
        "1745: with it off, an unbindable friendly cast falls into the reference's \
         targeting-cursor machine, which is unmodeled — leaving no path at all. Flip when that \
         machine lands",
    ),
    // The five saved camera views and the live index (decision 1745) — the reference's own
    // sixteen names and its own shipped default strings, both read out of `WoW.exe` and owned by
    // [`crate::player::camera_view`], which is also the only writer. Registered here so they are
    // ordinary CVars: persisted as a diff like everything else, readable from a macro, and
    // reachable by `SetCVar` — which is what makes a `SaveView` survive a restart.
    same(
        crate::player::camera_view::CVAR_ACTIVE_VIEW,
        crate::player::camera_view::ACTIVE_VIEW_DEFAULT,
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[0][0],
        crate::player::camera_view::VIEW_DEFAULTS[0][0],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[0][1],
        crate::player::camera_view::VIEW_DEFAULTS[0][1],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[0][2],
        crate::player::camera_view::VIEW_DEFAULTS[0][2],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[1][0],
        crate::player::camera_view::VIEW_DEFAULTS[1][0],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[1][1],
        crate::player::camera_view::VIEW_DEFAULTS[1][1],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[1][2],
        crate::player::camera_view::VIEW_DEFAULTS[1][2],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[2][0],
        crate::player::camera_view::VIEW_DEFAULTS[2][0],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[2][1],
        crate::player::camera_view::VIEW_DEFAULTS[2][1],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[2][2],
        crate::player::camera_view::VIEW_DEFAULTS[2][2],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[3][0],
        crate::player::camera_view::VIEW_DEFAULTS[3][0],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[3][1],
        crate::player::camera_view::VIEW_DEFAULTS[3][1],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[3][2],
        crate::player::camera_view::VIEW_DEFAULTS[3][2],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[4][0],
        crate::player::camera_view::VIEW_DEFAULTS[4][0],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[4][1],
        crate::player::camera_view::VIEW_DEFAULTS[4][1],
    ),
    same(
        crate::player::camera_view::VIEW_CVARS[4][2],
        crate::player::camera_view::VIEW_DEFAULTS[4][2],
    ),
    same("mouseInvertPitch", "0"),
    ours(
        "autoLootDefault",
        "0",
        "0961: 1.12 has no auto-loot CVar at all — vanilla offers only the shift gesture, so OFF \
         IS the reference's own behaviour; the spelling is era's",
    ),
    // The overhead-name trio (0992): 1.12's own UnitName* CVars (UIOptionsFrame.lua indices
    // 21/30/67) over the nameplates module's gates. Defaults mirror `NameConfig::default()` and
    // are the binary's own, byte-read at the `0x6c7470` registrar (name string / default string
    // per row, folded into
    // mask `0xce8720`) — `UnitNamePlayer` `0x86c694` → `"1"` `0x82e748`, `UnitNameNPC` `0x86c6a4`
    // and `UnitNameOwn` `0x86c6b0` → `"0"` `0x82e570`. Corroborated the other way by the
    // reference install's own `Config.wtf`, which carries `SET UnitNameNPC "1"` and
    // `SET UnitNameOwn "1"`: `SaveConfig 0x63d980` writes only what has MOVED off its default, so
    // those two lines existing is proof the defaults are not "1".
    //
    // **npc and own shipped ON here from 2026-07-12 until 1804** — see `NameConfig`'s doc.
    same("UnitNamePlayer", "1"),
    same("UnitNameNPC", "0"),
    same("UnitNameOwn", "0"),
    // The fourth of the same registrar's five (2149): `UnitNamePlayerGuild` `0x86c680` -> `"1"`
    // `0x82e748`, mask bit `0x10`. It is NOT a show gate like the three above — `ShouldShowName
    // 0x6070a0` consults only bits `0x1/0x2/0x4` — it gates ONE LINE of the player stack, the a5
    // `"\n<%s>"` guild decoration at `0x609085`. Its fifth sibling `UnitNamePlayerPVPTitle` (bit
    // `0x20`, also `"1"`) has no row: a4's rank prefix needs a faction side `ui_unit` cannot
    // resolve for an arbitrary player, so there is no reader and 1134 §4 says no key.
    same("UnitNamePlayerGuild", "1"),
    // The two V-plate toggles over `VPlateMode` — the engine bitmask `[0xc4da34]`'s bit 0 and
    // bit 3. 1.12 registers NO nameplate CVar (the bitmask is a plain runtime
    // global, persisted FrameXML-side as the `RegisterForSave`'d `NAMEPLATES_ON` /
    // `FRIENDNAMEPLATES_ON`), so these take the LATER-era engine's names: the `autoLootDefault`
    // posture, where benilla's persistence IS the CVar store (0954) and a setting with no 1.12
    // CVar gets the era spelling rather than an invented one.
    //
    // **`same`, not `ours`, and that distinction is the point**: the reference has no CVar to
    // match, but it very much has a *setting* to match, and it boots both halves OFF —
    // `UIOptionsFrame_Init` assigns `NAMEPLATES_ON = nil` / `FRIENDNAMEPLATES_ON = nil` and
    // `UpdateNameplates` only calls `ShowNameplates()` on a truthy value (the install's
    // `Interface\FrameXML\UIOptionsFrame.lua` l.180-183 / l.769-775 — both of them the stock
    // file's own, off the chain since 2115; our copies of each are gone). A fresh 1.12 client
    // draws no plates until V is pressed. Enemy plates shipped ON here from 0167
    // until 1804 — `VPlateMode::default()` carries that history.
    same(crate::vplates::CVAR_ENEMIES, "0"),
    same(crate::vplates::CVAR_FRIENDS, "0"),
    // World detail (0992) — the ENVIRONMENT_DETAIL slider's 0..2, over the clutter-density knob.
    // 0 is the client's bare `frillDensity` baseline (×1 = 16 visits), each step +1×, so 0/1/2 are
    // the 16/32/48 `SetWorldDetail` itself writes.
    //
    // **Not a 1.12 CVar** (corrected 1804 — this row used to call it "1.12's video-panel var").
    // `WorldDetail` does not exist as a string in `WoW.exe` (scanned; positive control
    // `frillDensity` present), and `OptionsFrame.lua:27`'s `func = "WorldDetail"` is a *function
    // name suffix*: the slider calls the engine's `GetWorldDetail`/`SetWorldDetail`. So the
    // spelling is the API's — the `autoLootDefault` posture.
    //
    // **"1" IS the reference's own setting, and getting there took two goes.** `SetWorldDetail
    // 0x488dd0` writes **two** CVars per stop: `frillDensity` {16,32,48} and `SmallCull`
    // {0.07,0.04,0.01}. The registered pair is `frillDensity` **16** (stop 0) with `SmallCull`
    // **0.04** (stop **1**) — the reference boots an inconsistent pair — and `GetWorldDetail`
    // reads only `SmallCull`. So its own slider **reads Medium at boot**, which is this row.
    // Mid-1804 this was filed as a deviation against "0" on two true facts that are not the
    // answer: `OptionsFrame.lua:430`'s Defaults ladder yields 0, and `frillDensity` registers 16.
    // A partly-verified mechanism is not the mechanism (1804).
    //
    // What IS still divergent is the grass, and that is a *mapping* difference rather than a
    // default: our stop 1 scatters ×2 (32) where the reference's boot `frillDensity` is 16
    // registered, 24 after `hwDetect` reads `VideoHardware.dbc` (row 170 on any D3D9-class part,
    // 8 on the weakest). 24 is on no stop of ours; 1649 broke that tie toward the denser stop,
    // because erring sparse is the worse failure for a knob about ground cover. **That divergence
    // is a row of its own now** — 2151 registered the CVar it lives in, immediately below, so it
    // is on the deviation inventory instead of only in this paragraph.
    same("WorldDetail", "1"),
    // The SAME knob in the reference's own unit (2151), and the CVar 1.12 actually registers for
    // it: `0x68862e` passes name `0x8423d8` `"frillDensity"`, default string `0x864644` `"16"`,
    // help "Terrain frill density", flags `1`, callback `0x688de0`, record `[0xc7f2f4]`. The value
    // is **cells visited per chunk**: the
    // callback clamps `[1, 256]` and hands the number to `0x6725a0` → `[0xc7b494]`, which bounds
    // the detail-doodad scatter loop at `0x6bfcfb`/`0x6bff1c`. Our scatter is the byte-exact port
    // of that loop, so `frillDensity` is not a new dial — it is the unit
    // `benilla_formats::scatter_ground_doodads` has always counted in, and
    // `ClutterConfig::frill_density` is the conversion.
    //
    // **Two names for one knob is the reference's own shape, not ours.** `SetWorldDetail 0x488dd0`
    // writes this CVar per stop (16/32/48) alongside `SmallCull` — the row above is that stop,
    // this row is what the stop wrote. Writing either moves the same ground cover here, and each
    // keeps the clamp its own writer has: the stop's `[0, 2]`, the cells' `[1, 256]`. So a console
    // `frillDensity 200` is honoured, exactly as it is there, and the panel row then reads
    // off-grid — which is already this pair's stated posture for an off-grid multiplier.
    //
    // **It has a live Lua consumer, which is why it is registered now** (the module doc's rule):
    // pfUI's `hdgraphic` replaces `GetWorldDetail` with `tonumber(GetCVar("frillDensity")) > 48`,
    // and unregistered that is `nil > 48` — an error, not a fallback. Its extended arm drives the
    // knob the other way, `ConsoleExec("frillDensity " .. (arg+1)*16)` up to 256, which is the
    // whole reason the reference's range is wider than its slider.
    //
    // **The deviation is 1649's grass, finally visible as a row.** 1804 recorded it in prose and
    // could not table it, because the CVar it is a deviation *in* was not registered: our stop 1
    // scatters 32 where the reference's boot value is 16 registered, and 24 after `hwDetect`
    // (`0x639a60` CVar::Sets sixteen video CVars from the matched `VideoHardware.dbc` row; field
    // `+0x18` holds 8/12/16/24 across the table, 24 on the videoID 170 that the reference
    // install's own `Logs/gx.log` resolves to). 24 is on no stop of ours.
    deviates(
        "frillDensity",
        "32",
        "16",
        "1649/1804: the reference's registered 16 is stop 0 and its post-`hwDetect` 24 is on no \
         stop at all, so every stop diverges; Medium (32) is the nearest one no sparser than a \
         fresh install, and erring sparse is the worse failure for ground cover",
    ),
    // ── The combat log's display ranges: the reference's own `0x8629e0` table, in yards ─────────
    //
    // Eight rows, registered by the reference in ONE place — `0x626d00`, a loop over the
    // `{cvarName, defaultValue}` pairs at `0x8629e0` skipping the NULL/empty names, then one
    // unrolled call for the death range. They read as the CVar record's **float** (`+0x24`), unlike
    // the periodic gate below, which reads the int.
    //
    // **They are why a damage meter's range slider does something.** `BigWigs/Plugins/Range.lua`
    // and `DPSMate/DPSMate_DataBuilder.lua` both read and write all eight; unregistered, every
    // `SetCVar` here wrote nothing and every `GetCVar` answered nil. The reader was already built
    // — `ui_chat::combat::in_range` has run this exact table since 1571, off the compiled-in
    // defaults, with `UnitClass::range_cvar` parked under `#[cfg(test)]` waiting for this row.
    //
    // Classes 0 and 1 — you and your pet — have NO CVar in the reference's table (a NULL name and
    // the `100000.0` sentinel), so there is nothing to register for them and nothing to miss.
    same("CombatLogRangeParty", "50"),
    same("CombatLogRangePartyPet", "50"),
    same("CombatLogRangeFriendlyPlayers", "50"),
    same("CombatLogRangeFriendlyPlayersPets", "50"),
    same("CombatLogRangeHostilePlayers", "50"),
    same("CombatLogRangeHostilePlayersPets", "50"),
    same("CombatLogRangeCreature", "30"),
    // The one range CVar OUTSIDE that table (`0x626d5f`, default string `"60"` at `0x862e14`) —
    // and the only formatter with a range of its own. `0x62c160` reads it first and falls back to
    // the per-class getter only when the *lookup* fails, which a registered client never sees.
    same(crate::ui_chat::combat::DEATH_LOG_RANGE_CVAR, "60"),
    // ── The floating-combat-text gates, and the periodic one ────────────────────────────────────
    //
    // `CombatDamage` (`0x6032df`, record `[0xc4d944]`) is the MASTER: its only two readers are the
    // localized-WORD emitter `0x607140` and the `"%d"` NUMBER emitter `0x6128b0`, and both branch
    // targets are epilogues — so at "0" nothing floats over any unit from any source, words
    // included, despite the CVar's own help text saying "damage numbers". The two `Pet*` rows are
    // sub-gates below it, on the owned-by-you branch only; the self sub-case is unconditional.
    //
    // `PetSpellDamage` has no row in `UIOptionsFrameCheckButtons` — the *Show Pet Melee Damage*
    // box writes both (`UIOptionsFrame_Save` l.334-336) — which is why 2077's census, which reads
    // that table, could not see it while it saw its two siblings. Our own Pet Damage row carries
    // the same partner write (2180); all three are on the Combat page, under `CombatDamage`.
    same("CombatDamage", "1"),
    same("PetMeleeDamage", "1"),
    same("PetSpellDamage", "1"),
    // `CombatLogPeriodicSpells` (`0x6033b3`, handle deliberately DISCARDED — every use re-looks it
    // up by name). Read as the record's INT, unlike the ranges above, which read its float.
    same(crate::ui_chat::combat::LOG_PERIODIC_CVAR, "1"),
    // ── The three Sound-panel check buttons benilla had the machinery for and no key to ─────────
    //
    // All three are category-7 (sound) registrations that keep **no** `CVar::Register` handle: the
    // reference looks each up by name at the point of use. Each already had its reader here.
    //
    // `SoundListenerAtCharacter` (`0x457890`, "lock listener at character"): both of its branches
    // were already written in `update_audio_listener` — the at-character seat and the at-camera
    // one — with the camera path reachable only as a no-character fallback. This is the selector
    // they were missing.
    same("SoundListenerAtCharacter", "1"),
    // `EmoteSounds` (`0x4573b9`): the received text-emote voice kit, and only that.
    same("EmoteSounds", "1"),
    // `SoundZoneMusicNoDelay` (`0x4578b3`): `next_track_time`'s own comment named it as "the
    // immediate path, a \"0\" CVar we don't expose". Now exposed, at the reference's `"0"`.
    same("SoundZoneMusicNoDelay", "0"),
    // `assistAttack` (`0x48fc50`, record `[0xb4d8f8]`) — `/assist`'s opt-in second leg: select the
    // basis unit's target AND open the swing on it. Three references image-wide, two of them the
    // shared assist tails; `CanAssist 0x6066f0` is verified NOT on the path. The `"0"` default is
    // easy to misread: the *next* registration's default (`minimapZoom`) is `"3"`, and reading it
    // off instead is the `mov ds:` adjacency trap — so it is worth saying plainly here: stock
    // `/assist` selects and does not swing.
    same("assistAttack", "0"),
    // ── Mouse-look, per axis: the two CVars whose absence RAISED in the stock window ────────────
    //
    // `cameraYawMoveSpeed` is `UIOptionsFrameSliders` row 3, MOUSE_LOOK_SPEED (90…270 step 10) —
    // and it is what made stock `UIOptionsFrame_Load()` die: `slider:SetValue(GetCVar(value.cvar))`
    // is a shape-A binding (`0x790980`) that raises on a nil in the reference too, so the whole
    // window's `_Load` (and `_SetDefaults`, through `GetCVarDefault`) stopped at slider 3.
    // `cameraPitchMoveSpeed` has no row of its own: `UIOptionsFrame_Save` writes it as
    // `sliderValue / 2` beside the yaw one (l.352-356), which is exactly this 180/90 pair — and
    // the Controls page's Mouse Look Speed row carries that partner write (2180), so dragging it
    // keeps the two axes in the ratio the registrar ships them at.
    //
    // **Both defaults are the reference's, and the shipped feel does not change** — the two facts
    // are compatible only because the unit divergence is carried in `camera::LOOK_YAW_PER_SPEED`
    // instead of in these numbers. The reference integrates OS-accelerated `WM_MOUSEMOVE` pixels
    // (it imports no DirectInput at all) while we integrate winit's raw device delta, so its
    // `deg per pixel` is not our `deg per unit` and the factor between them is a per-machine
    // pointer setting. Anchoring the scale there and keeping the defaults here is what lets 1804
    // hold honestly rather than by picking a number that merely looks right.
    //
    // The validator is `0x50c000` → `0x50b330`, range [0.1, 360], and it **rejects rather than
    // clamps** — `player::camera::on_cvar` does the same, which is why these two do not use the
    // clamping shape every other numeric row uses.
    same("cameraYawMoveSpeed", "180"),
    same("cameraPitchMoveSpeed", "90"),
    // Mouse Sensitivity (1140): 1.12's own MOUSE_SENSITIVITY slider (`UIOptionsFrameSliders` row
    // 1, 0.5..1.5 step 0.05), a MULTIPLIER over the camera's own per-pixel rate — which was a
    // frozen constant until this row. Default "1" is the shipped feel exactly, welded to
    // `LookConfig::default()`.
    //
    // **The spelling is FrameXML's, not the binary's**, and that is deliberate: `WoW.exe` holds
    // `mouseSpeed` (capital S, register site `0x402c7b`) while `UIOptionsFrame.lua`'s slider
    // writes `cvar = "mousespeed"`. The reference reconciles them by looking CVars up
    // case-insensitively (`SStrCmpI` in `CVar::Lookup 0x63de30`), and so do we (the registry and
    // every observer lowercase first), so both spellings answer. We take the one the interface uses, because
    // that is the one an addon will type.
    //
    // **The VALUE agrees and the MECHANISM does not** (1804). The reference's default is not a
    // literal at all: it is `sprintf("%1.1f", SPI_GETMOUSESPEED × 0.1)`, which is `"1.0"` on a
    // stock Windows host — so "1" is the right number. But its record `[0x882704]` has **zero
    // readers**: the slider drives the *operating system's* pointer speed through
    // `SPI_SETMOUSESPEED` (callback `0x402ec0`, clamped [0.1, 2.0]), not an in-engine gain.
    // benilla will not reach out and repoint the OS mouse, so ours is a multiplier over our own
    // per-pixel rate — the same dial, the same range, the same resting value, a different thing
    // underneath. Recorded here rather than filed as a deviation because the default is the
    // question this table answers, and the default matches.
    same("mousespeed", "1"),
    // Max Camera Distance (1140): 1.12's `cameraDistanceMaxFactor` (its MAX_FOLLOW_DIST slider,
    // 1..2 step 0.1) over `cameraDistanceMax`'s 15 yd base. **"1", the reference's registrar
    // value** (`cameraDistanceMax` "15.0" `0x84fbd0`, `cameraDistanceMaxFactor` "1.0" `0x82e92c`)
    // — so the shipped ceiling is 15 yd, not the 30 this row registered from 1140 until 1804.
    // `ZoomLimit`'s doc carries why that changed; the slider still reaches 2.
    same("cameraDistanceMaxFactor", "1"),
    // Camera Following Style (1493, re-pinned by 1502): 1.12's `cameraSmoothStyle` — the
    // auto-return that swings the camera back behind the character. Registered "1" = Smart, which
    // is BOTH the reference's registrar default (byte-verified: the argument is loaded from
    // `[0x84f4f4]` -> "1" at the `0x50ba92` register site) and the director's call; benilla behaved
    // as Never unconditionally until this row. The enum is the ENGINE's — 0 Never, 1 Smart,
    // 2 Always — NOT the 1/2/3 the reference's own dropdown writes; see `FollowStyle`.
    same("cameraSmoothStyle", "1"),
    // Its sibling selector (1502), also registered "1": the reference reads THIS style instead
    // whenever the state mask contains Track or Fear — the externally-driven states — indexing the
    // same matrices. No row on any 1.12 panel, here or there; the reader is the host.
    same("cameraSmoothTrackingStyle", "1"),
    // The auto-follow's rate (1502), °/s — 1.12's own AUTO_FOLLOW_SPEED slider
    // (`UIOptionsFrameSliders`, 90..270 by 10), registered at the binary's "180.0" (`[0xbe1070]`).
    // It sets the transition's DURATION (`|dyaw| / rate * factor`), so it is an average rate, not a
    // slew. Its slider is the Controls page's Auto-Follow Speed row (2180), greyed while the
    // following style is Never — and it writes only this one, where the reference also writes
    // `cameraPitchSmoothSpeed` at a quarter of it: that name is deliberately unregistered here,
    // because `FollowRig` has a single rate and a key with no reader is 1134 §4's pretence.
    same("cameraYawSmoothSpeed", "180"),
    // **The four 1.12 camera-option toggles** (decision 2149) — the `UIOptionsFrame` checkboxes
    // FOLLOW_TERRAIN / HEAD_BOB / SMART_PIVOT / WATER_COLLISION, all four of which sat on the
    // unbacked-CVar census with a byte-level spec and no feature until now. Defaults are the
    // registrar's own, and two of them are **"1"** — which is why building them was not cosmetic:
    // benilla was the divergence on those, not the reference.
    //
    // `cameraPivot` `[0xbe10a4]` "1" (`0x50bda3`) — smart pivot. Mechanism: gate `0x510690`,
    // routing `0x50fee0`, release `0x5107f0`; ours is `player::camera_dynamics::SmartPivot`.
    same("cameraPivot", "1"),
    // Its two drag-shape thresholds, both read by that routing (`0x50fff5`/`0x510004`) and both
    // in RADIANS of camera rotation — the deltas they are compared against are already scaled by
    // `camera<Yaw|Pitch>MoveSpeed · π/180`, so unlike the sensitivity itself these two transfer
    // to benilla's raw-device units exactly (see `camera::LOOK_YAW_PER_SPEED`'s note).
    same("cameraPivotDXMax", "0.05"),
    same("cameraPivotDYMin", "0"),
    // The rate the pitch bias eases back on once the pivot lets go, deg/s (`[0xbe0fc8]`,
    // `0x512a50`'s `duration = |Δ| / (rate · π/180)`). No panel row here or there — the reader is
    // the host, exactly like `cameraSmoothTrackingStyle` above it.
    same("cameraTargetSmoothSpeed", "90"),
    // **`cameraWaterCollision`** `[0xbe1088]` "1" (`0x50bd63`, default string `0x82e748`) — one of
    // the two that ship ON, and this row is its THIRD life. It is **one CVar with two consumers**,
    // and this tree has now shipped each of them alone and broken the camera both times: 2149 the
    // pivot corridor without the trace (a 19/18 yd step reached continuously), 2170 the trace
    // without the corridor (the boom straddling a plane the pivot sits 11 mm under — 2173 §1).
    // Both halves are here now, and the row exists to say they may never again be separated.
    //
    // `0x50e5ec` produces one register. Its `0xf0000` nibble rides the trace mask to all three of
    // `0x50e570`'s queries, reaching `0x69cc13` through four direct calls; and `0x50e629` tests
    // the SAME register to admit the floor/cap block that lifts the sweep origin to
    // `surface + 2/9`. Readers: the camera boom's collision filter
    // (`benilla_world::collision::camera_filter`) and `player::camera_water`.
    same("cameraWaterCollision", "1"),
    // `cameraTerrainTilt` `[0xbe0fd4]` **"0"** (`0x50bcfd`) — Follow Terrain, and the one of the
    // four that ships OFF, so building it changed nothing until a player ticks the box. Mechanism:
    // the ahead-probe and the five-step staircase (`0x50d900`) and the arm (`0x50dbc0`); ours is
    // `player::camera_dynamics::TerrainTilt`.
    same("cameraTerrainTilt", "0"),
    // The ground channel's rate, deg/s (`[0xbe0fc0]`) and the duration bound its `Factor` scales
    // (`[0xbe1050]`/`[0xbe1054]`, seconds). The floor always binds — `20° / 7.5°/s` is 2.67 s
    // against a 3 s minimum — which is why a followed terrain leans rather than tracks.
    same("cameraGroundSmoothSpeed", "7.5"),
    same("cameraTerrainTiltTimeMin", "3"),
    same("cameraTerrainTiltTimeMax", "10"),
    // `cameraBobbing` `[0xbe10c0]` **"0"** (`0x50b76d`) — head bob, the fourth of the four and the
    // second that ships OFF. Mechanism: the kernel `0x511920` and its gate `0x5105e0`; ours is
    // `player::camera_dynamics::HeadBob`.
    same("cameraBobbing", "0"),
    // Its four numeric siblings. The two amplitudes are in the CVar's own units — the kernel
    // scales both by 1/36 (`[0x7ff9d0]`) to reach yards. `cameraBobbingSmoothSpeed` is the odd one
    // and its name is the trap: it is **not** a bob rate, it is the DECAY rate, and its single
    // image-wide read is in the disarm `0x51113a`, where `|largest component| / speed` becomes the
    // ramp's duration (~0.069 s at these defaults).
    same("cameraBobbingLRAmplitude", "2"),
    same("cameraBobbingUDAmplitude", "2"),
    same("cameraBobbingFrequency", "0.8"),
    same("cameraBobbingSmoothSpeed", "0.8"),
    // Status Text (1140): 1.12's `statusBarText`, the "always show value / max on a status bar"
    // switch. **No host knob** — its consumer is Lua (TextStatusBar.xml, decision 1082, which was
    // written waiting for this key and reads it on every repaint). Default "0": the reference's
    // out-of-box look is hover-only numerals.
    //
    // **Byte-read since 1804** — register site `0x48fc34`, default string `"0"`, record
    // `[0xb4d904]`, and a whole-image census finds that record has **no engine reader at all**:
    // this CVar is FrameXML's alone, which is exactly the shape this row was built for. It used to
    // concede "behaviour-derived, not byte-read"; that hedge is retired.
    same("statusBarText", "0"),
    // Enhanced Tooltips (B230): 1.12's `UberTooltips`, the *Enhanced Tooltips* checkbox
    // (`UIOptionsFrame.lua:15`, `USE_UBERTOOLTIPS`). **No host knob** — its consumers are Lua, and
    // there are three: PetActionBar.xml forks the whole tooltip on it (a token's own text with the
    // binding appended, vs the engine's pet-spell channel), the stock action and shapeshift buttons fork
    // their anchor. Registered "1" — byte-read, not behaviour-derived: WoW.exe `0x48fdd9`, default
    // string `0x82e748`, with the sibling rows `BlockTrades`→"0" and `UnitNameRenderMode`→"2"
    // confirming the layout. Those three Lua sites each carried the reference's fork in prose and
    // then collapsed it to this default, on the stated premise that benilla shipped no CVar state
    // for anything to move. That premise expired with 0954, and this row is what un-collapses them.
    same("UberTooltips", "1"),
    // The two chat-bubble switches (1139): 1.12's own registrar CVars over the bubble gate,
    // which held them as `const bool` from 0598 until this window had a page for them. Both are
    // the binary's own (registrar `0x603280`:
    // `ChatBubbles` "1", `ChatBubblesParty` "0"); the party half shipped ON from 0598 to 1804 on
    // the director's `/p` ask, and is a click away on the Chat page.
    same("ChatBubbles", "1"),
    same("ChatBubblesParty", "0"),
    // **The two text filters** (2077) — 1.12's own pair, and both are real features rather than
    // vestigial switches.
    // Registered `"1"` each, byte-read: `0x402e68` (`profanityFilter`, name `0x82e7f4`, callback
    // `0x403570`) and `0x402e8e` (`spamFilter`, name `0x82e7d4`, callback `0x4035b0`), both pushing
    // the shared `"1"` literal `0x82e748`, both category 4.
    //
    // `profanityFilter` masks matched spans of `ChatProfanity.dbc` in place, and it gates INSIDE
    // the shared masker (`0x4a1a66`), so all thirteen of its call sites are covered by the one
    // switch — the 14 social chat types, mail, the guild MOTD/info/rank names, item text and the
    // send path. `spamFilter` is a predicate over `SpamMessages.dbc` at the chat chokepoint that
    // **drops** a matching line silently. The knob for both is
    // [`crate::text_filter::TextFilterSwitches`]; the engine is `crate::text_filter`.
    same("profanityFilter", "1"),
    same("spamFilter", "1"),
    // **The loading-screen tip of the day** (2077) — 1.12's own pair, both registered lazily by
    // `CGlueMgr::EnterWorld` on its way to the config flush (`0x46b633` `gameTip` `"0"`,
    // `0x46b658` `showGameTips` `"1"`, both category 5, neither with a callback or a help string).
    //
    // `gameTip` is not a preference — it is the **cursor**, and it holds the NEXT row rather than
    // the one on screen, which is why the reference's own `Config.wtf` reads `SET gameTip "34"`
    // while showing row 33. It is registered here because that is how it persists: the file is
    // composed from the VM's live table, so the host's advance writes through it. `crate::game_tip`
    // is the law.
    same("gameTip", "0"),
    same("showGameTips", "1"),
    // *Detailed Loot Information* (1589, the Chat page) — 1.12's `showLootSpam`, whose subject is
    // group LOOT ROLLS (its own tooltip: "Uncheck this to hide individual loot roll messages and
    // only show the winner"). Registered `"1"`, **byte-read**: a census of `0xb4e2bc` has the
    // register site at `0x48fd1c`, name `0x8430a0`,
    // default string `0x82e748` = "1", **category** 5 — and exactly four references to the global,
    // one writer and three readers, all in the roll-line composers. The knob is
    // [`crate::ui_loot::LootConfig::show_loot_spam`], welded to that default below.
    same("showLootSpam", "1"),
    // *Guild Member Alert* (1589, the Chat page) — 1.12's `guildMemberNotify`, whose registered
    // help string says what it does: "Receive notification when guild members log on/off".
    //
    // Registered **`"0"`** — this is one of the few rows that ships a feature OFF, and it is
    // byte-read rather than chosen: the register site `0x5e24c7` pushes default `0x82e570` = "0".
    // A stock 1.12 client is
    // silent when a guildmate logs in, and a whole-image census of the record global `0xc4d3c4`
    // finds exactly two readers, both inside `SMSG_GUILD_EVENT`'s handler. The knob is
    // [`crate::ui_guild::GuildMemberNotify`]; the other three conjuncts of the line's display
    // condition live on `ui_guild::net::event`.
    same("guildMemberNotify", "0"),
    // The minimap's two zoom indices (1131). 1.12 CVars, both registered `"3"` (`0x48fc6c`,
    // `0x48fc88`, at the `RegisterCVar 0x63db90` argument slot). No options row drives these — the
    // +/- buttons on the minimap do, through `Minimap:SetZoom`, exactly as in the reference, where
    // `set_zoom` writes the live index and `CVar::Set`s the CVar in one breath. The knob is
    // [`crate::minimap::MinimapZoom`], the widget's live index is seeded from it at UI load.
    same("minimapZoom", "3"),
    same("minimapInsideZoom", "3"),
    // The addon version gate (decision 1292): 1.12's own `checkAddonVersion`, the *Load out of
    // date AddOns* checkbox INVERTED. Registrar default "1" = check enforced = box unticked
    // (`0x402c3b`; the key appears in Config.wtf exactly
    // while force-load is on and vanishes when it is turned off, `SaveConfig 0x63d980`'s
    // skip-default rule). No host knob: its consumers are the load walk (via the persisted value,
    // [`Cvars::addon_version_check`]) and the gate's live per-query read in the VM.
    same("checkAddonVersion", "1"),
    // **Which graphics API this run is actually on** (2151) — 1.12's own `gxApi`, byte-read at
    // `0x63a833`: name `0x842a64`, default string `0x864f7c` `"direct3d"`, help "graphics api",
    // flags `3` (registered | latched), callback `0x63b030`, record `[0xc4ea94]`. There it is a
    // real selector — `0x63a3c4` compares the live value case-insensitively against `"OpenGl"`
    // (`0x842a5c`) and `GxDevCreate` builds `CGxDeviceD3d` on anything else — but no shipped
    // `WTF` overrides it, so the stock client is always D3D9 and the whole GL arm is dead code
    // image-wide.
    //
    // **Here it DESCRIBES, it does not steer** — the `gxColorBits`/`gxDepthBits` posture. benilla
    // renders through wgpu, which has no D3D9 backend to name and no chooser to offer: the backend
    // is the adapter's, picked before the first frame, and this row is the honest report of it
    // (`wgpu::Backend::to_str` — `metal`, `vulkan`, `dx12`, `gl`). Answering `"direct3d"` on a Mac
    // would be a name with no behaviour behind it, which is the one thing 1203 forbids outright.
    //
    // **Default EMPTY, and pushed live** — the `realmName` posture (1140), for the same reason:
    // the value is a fact about the machine, written from `RenderAdapterInfo` the moment the VM's
    // table is seeded, so the default only ever describes a client with no render adapter (a
    // headless test). Inventing a backend for that case would be worse than admitting we have
    // none. And because it is the machine's fact rather than the player's choice, it is
    // **session-owned**: `SetCVar` consumes it and `config.toml` never carries it, so a GPU swap
    // or a `WGPU_BACKEND` run cannot leave a stale renderer name pinned in the file.
    //
    // Its live Lua consumer is pfUI's system panel — `panel.lua:185` does
    // `"|cffffffff" .. GetCVar("gxApi")` in a tooltip, which on a nil is a concat error rather
    // than a blank row.
    deviates(
        "gxApi",
        "",
        "direct3d",
        "2151: descriptive, not a selector — benilla renders through wgpu, which has no D3D9 \
         backend and no chooser; the value is the live adapter's own and is never persisted",
    )
    .latched(),
    // Vertical Sync — 1.12's own `gxVSync`, the Video Options checkbox at index 5
    // (`OptionsFrame.lua`'s `OptionsFrameCheckButtons["VERTICAL_SYNC"]`, in the install's
    // FrameXML). The knob is [`crate::video::VideoConfig::vsync`], which the window's
    // `present_mode` follows.
    //
    // Default "1" is BEHAVIOUR-derived, not byte-read: 1.12's registrar value for this var is
    // unconfirmed, and "1" is what benilla actually ships — the primary window is born at
    // `PresentMode::default()` (Fifo), and a test in `video.rs` welds the two together.
    //
    // Two knowing departures from the reference row, both stated on [`crate::video`]: its
    // `gxRestart = 1` does not apply (wgpu swaps the presentation interval live, so the box takes
    // effect on click), and `$WOW_NOVSYNC=1` overrides it session-only, below.
    same("gxVSync", "1").latched(),
    // MONKEY (advanced graphics): the one row on the Advanced Graphics page that is not a knob —
    // it is a NAME for a combination of the rows under it ([`LIGHTING_PRESETS`]). Writing a preset
    // name writes that preset's members; writing anything else is corrected on the next frame by
    // [`lighting_quality`], which re-DERIVES the name from the members those rows actually hold.
    // So the stored value is never stale: it is either a preset every member agrees with, or
    // `Custom`.
    //
    // A STRING row, not an int, so `config.toml` and `/console set lightingQuality Medium` both
    // read as what they mean — and so the ladder can gain a rung without renumbering a player's
    // saved value. `Row::numeric` is false for it (the default does not parse as a number), which
    // is what lets the registry accept a non-numeric write at all; `realmList` is the precedent.
    ours(
        "lightingQuality",
        "High",
        "benilla's own: the preset ladder over the dynamic light + shadow rows — Off / Low / \
         Medium / High / Ultra, or Custom when the members match none of them; the reference has no \
         realtime light or shadow system to preset",
    ),
    // MONKEY (presets): the Graphics Preset — `lightingQuality`'s posture one level up, a NAME
    // for every graphics row at once ([`GRAPHICS_PRESETS`]), re-derived every frame. "High" is
    // what a player with no saved preset boots into: [`Cvars::seed_graphics_preset`] writes the
    // High column over every row their `config.toml` does not carry, so the rows themselves keep
    // their per-lane defaults (Classic-leaning, and `farclip` the reference's 350) and a capture
    // or a test, which never seeds, keeps them too.
    ours(
        "graphicsQuality",
        "High",
        "benilla's own: the preset ladder over every graphics row — Classic / Low / Medium / High \
         / Ultra, or Custom when the rows match none of them; the reference's options have no \
         such preset",
    ),
    // MONKEY (volumetric fog): saved live tier; capture override stays session-only.
    ours("volumetricFog", "1", "benilla's own: near-field volumetric fog, 0 Off / 1 Low / 2 High"),
    // MONKEY (lampfog): opt-in point-light halos; High graphics preset value is 2.
    ours(
        "lampFog",
        "0",
        "benilla's own: lamps scattering through night fog, 0 Off / 1 Low (16 lamps) / 2 High (32 lamps)",
    ),
    // MONKEY (p0 skyDither): the FFXGlow combine's deband dither (was env WOW_DITHER only).
    // Default 0 = the reference look; the Graphics preset's High sets 1.
    ours(
        "skyDither",
        "0",
        "benilla's own: faint screen dither against sky and fog banding, 0 Off / 1 On",
    ),
    // MONKEY (post): tier 0 leaves every emissive site and the frame byte-identical.
    ours("bloom", "2", "benilla's own: HDR emissive bloom, 0 Off / 1 Low / 2 High"),
    ours("sunShafts", "1", "benilla's own: depth-occluded screen-space sun shafts"),
    ours("colorGrading", "1", "benilla's own: zone and day/night 32-cube colour grade"),
    // MONKEY (sky): the sky tier; the High graphics preset sets 2.
    ours(
        "skyQuality",
        "0",
        "benilla's own: sky quality, 0 Classic / 1 Enhanced (smooth gradient, sun glow, stars) / \
         2 High (+ detailed sun-lit clouds)",
    ),
    // MONKEY (wind): one tier controls the grass-only and grass-plus-tree receivers.
    ours(
        "foliageWind",
        "2",
        "benilla's own: foliage wind, 0 Off / 1 Grass / 2 Grass + trees",
    ),
    // MONKEY (fix-wind): the sway strength slider under Foliage Wind; not on the preset ladder.
    ours(
        "foliageWindStrength",
        "1",
        "benilla's own: foliage wind strength, a gain on the sway, 0.25..3 (1 = the shipped tuning)",
    ),
    // MONKEY (fog): the distance-fog model. 0 = the 1.12 linear fog (byte-identical), 1 = Modern
    // (exponential, sun/horizon colour, end-fog shift, fog end decoupled from farclip past 777).
    // Default 0; the Graphics preset's High sets 1.
    ours(
        "fogModel",
        "0",
        "benilla's own: distance fog, 0 Classic (1.12 linear) / 1 Modern (soft, sun-coloured horizon)",
    ),
    // MONKEY (wet): rain darkens and glosses sky-exposed surfaces and rings the water. Default 1
    // (only visible while it rains or the ground dries); the Graphics preset's High sets 1.
    ours(
        "rainSurfaces",
        "1",
        "benilla's own: rain wets sky-exposed surfaces and rings the water, 0 Off / 1 On",
    ),
    // MONKEY (ao): opt-in contact shadows; the High graphics preset value is 2.
    ours("ambientOcclusion", "0", "benilla's own: screen-space ambient occlusion, 0 Off / 1 Low / 2 High"),
    // MONKEY (skybox): the living player's zone skybox from `LightParams`; High sets 1.
    ours(
        "zoneSkyboxes",
        "0",
        "benilla's own: draw the zone skybox LightParams names for the living, 0 Off / 1 On; the \
         reference draws a DBC skybox only for the ghost",
    ),
    ours(
        "waterQuality",
        "1",
        "benilla's own: water quality, 0 Classic / 1 Enhanced / 2 High; mirror reflections are opt-in",
    ),
    ours(
        "lavaLightGain",
        "1",
        "benilla's own: brightness of lava lighting its surroundings, 0..4 (0 = no glow)",
    ),
    // Benilla's opt-in realtime shadow-map path, split into two INDEPENDENT lanes over one shared
    // shadow rig (one sun / one map). `worldShadows` = the static world (trees, buildings, foliage)
    // casts realtime shadows and baked MCSH terrain shadows switch off; `characterShadows` =
    // players/NPCs/creatures/mounts cast realtime silhouettes instead of the legacy oval blob.
    //
    // `ours(...)`, not `same(...)`: the reference has no realtime shadow at all — it bakes MCSH
    // into the terrain and draws an oval under every unit — so there is no registered default for
    // these two to agree with, and claiming `Same` would put a false entry in the one column
    // [`Reference`] exists to keep honest. (They read `same("…", "1")` until the 2303 registry
    // port; the default string is unchanged.)
    ours(
        "worldShadows",
        "1",
        "benilla's own: the static world (trees, buildings, alpha-tested foliage) casts a realtime \
         shadow and the baked MCSH terrain shadows stand aside; the reference bakes and has no \
         such switch",
    ),
    ours(
        "characterShadows",
        "1",
        "benilla's own: units cast a realtime silhouette instead of the reference's oval blob \
         decal, which is all 1.12 has and is therefore not a setting there",
    ),
    // Realtime-shadow render distance in yards (the shadow-map cascade range + caster reach).
    // benilla's own — the reference has no realtime shadow to size. Clamped to SHADOW_DISTANCE_RANGE.
    ours(
        "shadowDistance",
        "80",
        "1900: benilla's own realtime-shadow render-distance slider; the reference bakes MCSH and \
         has no cascade to size",
    ),
    // MONKEY (sun shadow perf): the five live dials over the sun lanes' ~5 ms/frame (RTX 3070,
    // 1080p, both lanes on: 45-47 fps, and 68-73 with both off). All benilla's own — the reference
    // bakes MCSH and has no realtime shadow to tune. `shadow_core`'s constants block holds the cost
    // split each one takes; every row is LIVE, so the whole set A/Bs from one chat line.
    ours(
        "shadowMapSize",
        "2048",
        "benilla's own: directional shadow-map edge in texels, 1024/2048/4096 (cost is quadratic)",
    ),
    ours(
        "shadowFilter",
        "1",
        "benilla's own: shadow PCF kernel: 0 hardware-2x2 (1 sample), 1 gaussian (9, the look)",
    ),
    ours(
        "characterShadowRate",
        "30",
        "benilla's own: Hz cap on the character shadow proxy re-skin+upload, 0..120 (0 = per frame)",
    ),
    ours(
        "worldShadowRate",
        "30",
        "benilla's own: Hz cap on the world lane's environment caster, 0..120 (0 = per frame)",
    ),
    ours(
        "shadowCasterReach",
        "1",
        "benilla's own: multiplier on the shadow caster-collection reach, 0.25..2 (1 = unchanged)",
    ),
    // MONKEY (moon shadows): how dark a MOON-shadowed fragment is allowed to get at night — the
    // dial over the night lane's own shadow term (`benilla_world::lighting::MoonShadowStrength`,
    // bridged by `dynamic_interior` beside the spell gain). `0` is the FAITHFUL null: every
    // consumer guards on it, so a night at 0 renders as the build before the feature did — which
    // is also what the Off and Low presets ask for.
    ours(
        "moonShadowStrength",
        "0.35",
        "benilla's own: how dark a moon-shadowed fragment gets at night, 0..1 (0 = no moon \
         shadow, the reference's own night)",
    ),
    // MONKEY (dynamic interiors): WMO interiors + their props light from the room's LIVE fixtures
    // instead of the MOCV bake / the baked prop probe (`static_gx.wgsl` `interior_room_light`;
    // bridged by `dynamic_interior`). The three numeric knobs are live-tunable from chat —
    // `/script SetCVar("interiorExposure", 2)` — which is how their defaults were found.
    ours(
        "interiorLight",
        "1",
        "benilla's own: fixture-lit WMO interiors (0 = the reference's baked interior path)",
    ),
    ours(
        "interiorAmbient",
        "0.015",
        "benilla's own: interior base ambient, 0..1",
    ),
    ours(
        "interiorFill",
        "0.08",
        "benilla's own: interior per-fixture bounce gain, 0..2",
    ),
    ours(
        "interiorExposure",
        "2.5",
        "benilla's own: interior light-budget multiplier before the soft rolloff, 0.25..8",
    ),
    // MONKEY (soft falloff): the live scale on every interior fixture's AUTHORED attenuation
    // window (WMO MOLT `+0x28/+0x2c`; M2 sources bucket by intensity instead — their authored pair
    // is a template default, not a reach). A fixture's EFFECTIVE RADIUS is `authored end × this`.
    // The artists' own ends — Goldshire inn 6.97-9.53 yd over 10 fixtures, its blacksmith 6.0,
    // NSabbey 4.17-5.56, Stormwind's 606 median 6.94 — are where FULL brightness ends, not where
    // light stops, so `1` drew a hard-edged disc at exactly that radius with black beyond it (the
    // abbey candelabra ring). **2.5** is the default: the pool now tails smoothly to 2.5× the
    // authored end, reading ~⅓ of its 1 yd brightness AT the authored end and ~8 % at twice it.
    // `>1.6` widens further, `<1.6` tightens, `0` switches the window off (the 48 yd lane), so the
    // whole shape A/Bs from chat.
    ours(
        "interiorAttenScale",
        "1.6",
        "benilla's own: scale on interior fixtures' authored attenuation window = their effective \
         radius, 0..8 (0 = no window, the old flat lane)",
    ),
    // MONKEY (room gate): whether an interior fixture may light only the rooms it CLAIMS — its
    // authored MOLR groups unioned with the interior groups whose MOGI bounding box it stands
    // inside (`LightLitRooms` carries the corpus evidence for why MOLR alone is far too sparse:
    // the Goldshire inn authors one on 2 of its 12 groups). Off restores the pre-gate behaviour:
    // every interior fixture in range lights every interior surface in range, so an inn's
    // ground-floor candles light its basement through the floor. Kept as a dial because a room the
    // gate leaves on ambient alone looks the same as a bug, and this tells the two apart in one
    // keystroke.
    ours(
        "interiorRoomGate",
        "1",
        "benilla's own: an interior fixture lights only the WMO groups it claims (0 = the old \
         leak-through-walls behaviour)",
    ),
    ours(
        "interiorShadows",
        "1",
        "benilla's own: interior fixtures cast real shadows (Stage B, the nearest few); needs \
         interiorLight",
    ),
    // MONKEY (outdoor torch shadows): the outdoor half of the same cube-map lane. Its own row
    // because it is its own audience (a night camp, a lit village) and its own cost profile — and
    // because "turn the outdoor shadows off" must not also turn the inn's candles' shadows off.
    ours(
        "exteriorShadows",
        "1",
        "benilla's own: outdoor fire lights (campfires, braziers, lampposts) cast real shadows at \
         night; no effect by day",
    ),
    // MONKEY (daylight: terrain torch casters): the ground as a torch caster.
    ours(
        "torchTerrainShadows",
        "0",
        "benilla's own: the ground casts into outdoor fire shadows (hills and banks block a fire's \
         light); needs exteriorShadows",
    ),
    // MONKEY (static torch cache): residency and per-frame work have separate live budgets.
    ours(
        "interiorShadowCasters",
        "12",
        "benilla's own: resident interior fixture shadow maps, 1..16",
    ),
    ours(
        "interiorShadowDynamic",
        "4",
        "benilla's own: nearest promoted fixtures with moving entity shadows, 0..16",
    ),
    // MONKEY (torch lane perf): the moving-caster REGATHER cadence. Its own row (and not folded
    // into `interiorShadowDynamic`) because it trades a different currency: `Dynamic` buys how
    // MANY fixtures overlay moving casters, this buys how OFTEN the one shared overlay mesh is
    // rebuilt. Neither of the count dials moved the frame time at all, so the cost was never per
    // map -- it was this gather + mesh mutation, paid once a frame no matter what the counts said.
    // `0` is the pre-feature every-frame behaviour, kept as the live A/B.
    ours(
        "interiorShadowEntityRate",
        "30",
        "benilla's own: how often (Hz) moving torch-shadow casters are regathered; 0 = every frame",
    ),
    // MONKEY (torch caster selection): the PCF tap radius on the torch maps. Candle clusters read
    // very hard-edged at 1 (a half-texel box on a 512² face); 2 is a visible softening for four
    // extra texel-neighbourhood taps' worth of cache pressure, no extra samples.
    ours(
        "interiorShadowSoft",
        "1.5",
        "benilla's own: torch-shadow edge softness — the PCF tap radius scale at a CONTACT, 0.5..3",
    ),
    // MONKEY (shadow floor): how BLACK a torch shadow is allowed to get. The lane's shadows were
    // the only occlusion in the direct term and took all of it, which is what made them read as
    // scars rather than as shadows; 0.7 leaves 30 % standing in place of the bounce light this
    // renderer does not have. `1` is the shipped look, `0` is off.
    ours(
        "torchShadowStrength",
        "0.7",
        "benilla's own: torch-shadow darkness — how much of the direct term a shadow removes, 0..1",
    ),
    ours(
        "interiorDebug",
        "0",
        "benilla's own: interior diagnostic overlay — 1 classification, 2 shadow, 3 caster count, 4 WMO lane map",
    ),
    // MONKEY (darkness gains): the two live dim dials. `nightGain` scales the EXTERIOR day/night
    // law (the packed ambient/diffuse/specular rows) by `mix(1, gain, night_w)`, so it is exactly
    // inert by day and full strength after dark; `interiorGain` scales the room lane's inputs (base
    // ambient, per-fixture fill, and every interior fixture's colour). Both fold in at PACK time in
    // `build_light_data`, so `SetCVar` moves the whole world on the very next frame — which is how
    // "20 % / 30 % darker" gets judged at all, and `1` on either is the restore.
    //
    // Night gain leaves fire lights alone; interior gain also scales interior fixtures,
    // including their candlelight. Outdoor fires retain their brightness under both dials.
    ours(
        "nightGain",
        "0.45",
        "benilla's own: exterior night brightness, 0.2..1.5 (1 = the reference's own night)",
    ),
    // MONKEY (lighting debug panel): weaker fresh interiors; persisted gains still win at boot.
    ours(
        "interiorGain",
        "0.5",
        "benilla's own: WMO interior brightness, 0.2..1.5 (1 = the pre-dial fixture-lit room)",
    ),
    // MONKEY (enclosed day floor): the daylight a room INSIDE A BUILDING gets by day, for the
    // doorways this renderer cannot locate in the data (the Goldshire inn's entry group authors no
    // portal, no EXT-class batch, no stitched vertex and no bake hot spot — there is nowhere to
    // stand a fixture). An additive ambient in `interiorAmbient`'s own units, scaled by the sun's
    // day envelope, so it is exactly 0 at night and the night look never moves. `0` is the restore.
    ours(
        "interiorDaylight",
        "0.0",
        "benilla's own: daylight floor for rooms inside a building, 0..1 (0 = none, the old look)",
    ),
    // MONKEY (fix-daylight): the district window split, gateable (default on = merged behaviour).
    ours(
        "daylightWindowSplit",
        "1",
        "benilla's own: split city window batches into window-sized daylight apertures (applies to newly loaded buildings)",
    ),
    // MONKEY (bake floor): the share of an interior batch's own MOCV bake that survives the live-
    // fixture lane. The lane throws the bake away and lets the fixtures decide, which leaves a room
    // no fixture reaches (the Lion's Pride Inn's east vestibule: MOLR 0, no claims, one faded
    // portal hop) rendering black between a sky-lit porch and a candle-lit hall — something the
    // reference client cannot do, because it draws every interior batch at its bake regardless of
    // lights. A fraction of the bake, inside the room law's rolloff, so a lit surface barely moves.
    ours(
        "interiorBakeFloor",
        "0.12",
        "benilla's own: share of an interior batch's baked light kept where no fixture reaches, 0..1 (0 = the old look)",
    ),
    // MONKEY (fire GO lights): the gain on lights SYNTHESISED from a model's flame emitter for the
    // ~410 fire props the artists never gave a light block (campfires, wall torches, magic
    // braziers, forges, candles). Live, like the interior knobs — and `0` is the kill switch for
    // the whole invented-light lane, which matters because unlike everything beside it this one is
    // a heuristic over content rather than a byte-verified mechanism.
    ours(
        "fireLightGain",
        "1",
        "benilla's own: brightness of lights synthesised from fire props' flame emitters (0 = off)",
    ),
    // MONKEY (spellLightGain): the same dial for the SPELL lane — a kit's aura glow, a missile's
    // core, an impact flash, a firework's burst. A separate knob from the one above it because the
    // two are separate judgements: that one tunes SCENERY (how bright is the invented campfire),
    // this one tunes COMBAT (how hard does a fight flash the room), and a spell light carries both
    // markers, so one dial over both would mean dimming the world's hearths to calm a fireball.
    // `0` is this lane's kill switch and reaches nothing else.
    ours(
        "spellLightGain",
        "1",
        "benilla's own: brightness of spell, missile and impact lights (0 = off)",
    ),
    // MONKEY (flame flicker): how hard every FLAME breathes — candles fast and shallow, bonfires
    // slow and shallower still (`benilla_world::lighting::FlameKind`). Live like the gain beside
    // it, and `0` restores the steady constants every fire had before the feature, which is the
    // escape hatch this needs precisely because "subtle" is a judgement call and a flicker that
    // reads as a strobe is worse than none.
    ours(
        "fireFlicker",
        "1",
        "benilla's own: how strongly fire lights flicker — 0 steady, 1 default, 2 pronounced",
    ),
    // **Display mode** (decisions 1627, 1650) — 1.12's own `gxWindow`, worn since 1650 as modern
    // Classic's two-entry *Display Mode* dropdown rather than 1.12's *Windowed Mode* checkbox: the
    // two states 1627 settled on ARE that client's two (its own `Graphics.lua` builds the list from
    // `VIDEO_OPTIONS_WINDOWED_FULLSCREEN` and `VIDEO_OPTIONS_WINDOWED`, and nothing else), and a
    // checkbox could only name one of them. The knob is [`crate::video::VideoConfig::display`],
    // which the window's `mode` follows.
    //
    // Default **"0" = not windowed**, which is the reference's own default and every shipped
    // game's — but "0" does NOT mean what it means in 1.12. The reference mode-sets the display;
    // we raise a **borderless** fullscreen window, and ship no exclusive mode at all.
    // [`crate::video`] carries the three-platform argument for why that is the whole of it (short
    // version: Wayland cannot do exclusive, X11's XRandR path cannot restore the desktop after a
    // crash, macOS has no such mode, and WoW itself dropped exclusive fullscreen in 8.0.1).
    //
    // Departs from the reference row's `gxRestart = 1` exactly like `gxVSync` above: ours applies
    // on the click.
    // Byte-read `"0"` at register site `0x63a889` — **for enUS**. Three defaults in this binary
    // are locale-conditional and this is one: `gxWindow` and `gxMaximize` (`0x63a8e0`) register
    // `"1"` on zhCN, `AutoInteract` (`0x603390`) `"1"` on koKR. benilla is enUS-only, so `"0"`
    // is the answer here; the note exists so the next reader does not take a locale-conditional
    // default for an unconditional one.
    same("gxWindow", "0").latched(),
    // The **windowed** size, `gxResolution` — 1.12's own CVar name, narrowed to half its job.
    // There it is the display mode *and* the backbuffer; here it is only what "windowed" means,
    // because fullscreen is the monitor's own size and we expose no mode list to pick from (the
    // deviation decision 1092 already records for `GxAspect`, unchanged by 1627).
    //
    // A **string** CVar, like it is in the reference — the registry takes any string for it and
    // `video::on_cvar` parses it. Default is the 1600×900 that was the client's only size before
    // 1627, so a windowed run is bit-for-bit where it was.
    deviates(
        "gxResolution",
        "1600x900",
        "640x480",
        "1627: narrowed to the WINDOWED size only — fullscreen is the monitor's own and we expose \
         no mode list, and 640x480 is not a window anyone would ship a client at",
    )
    .latched(),
    // The body panes' half-rate render (decision 1444) — **benilla's own CVar**, no 1.12
    // counterpart: the reference draws its doll inside the main pass (no second view exists to
    // rate-limit), while our RTT booths (1069) re-run the render graph per pane per frame. "1" =
    // the doll renders at half the frame rate while its pane is open; the knob is
    // [`crate::portrait::PaneRate`], and the default mirrors it (welded below).
    //
    // **Default ON (half-rate) — restored by 1607.** 1444 shipped it on; 1559 turned it off for
    // a smoother doll (a look-call); the 08-25 weak-GPU perf reports (B329) measured the cost —
    // ~1.6 ms at 1600×900, 7.6 ms at 4K, per frame while a body pane is open — and the director
    // retested the 30 fps doll as fine. Full-rate is one `SetCVar("boothHalfRate", 0)` away.
    ours(
        "boothHalfRate",
        "1",
        "1444/1607: benilla's own — the reference draws its doll inside the main pass and has no \
         second view to rate-limit",
    ),
    // The select screen's memory of who you last entered the world as (decision 1622) — 1.12's
    // own `lastCharacterIndex`, help string "Last character selected". **No host knob**: the live
    // value is the character screen's own state ([`crate::char_select::Roster::pending_index`]),
    // which this row only mirrors — the `statusBarText` posture, and why no observer watches it.
    //
    // Registered **"0"**, byte-read rather than chosen: `CVar::Register` at `0x402d93` pushes
    // default string `0x82e570` = "0", category 4, and caches the CVar* at `[0x882674]`. The value
    // is a **0-based** row (the engine's selection cell `[0x83856c]` under `"%d"`), so "0" is the
    // FIRST character and not a "no memory" sentinel — which is exactly why a stock `Config.wtf`
    // has no such line until you have played somebody other than your first character
    // (`SaveConfig 0x63d980` skips values equal to their default; [`Cvars::compose`] does the same).
    // Multisample antialiasing — 1.12's own `gxMultisample`, registered at `0x63a950` with help
    // "multisample antialiasing" and flags `3` = registered | **latched**. The knob is
    // [`benilla_world::view::MsaaSetting`], read once at the world camera's spawn; its doc carries
    // the full derivation.
    //
    // **Default "1" — off — and BYTE-DERIVED, unusually indirectly.** The reference registers no
    // literal here: the default string is `snprintf("%d")`'d at runtime from field 21 of the
    // `VideoHardware.dbc` row `DetectHardware` (`0x641260`) matched the GPU to. Across the shipped
    // 193-row table that field only ever holds 1 (144 rows) or 2 (49 rows), and the three rows the
    // fallback match can reach all hold 1 — so on any GPU the 2004-era table does not list, which
    // is every machine this client runs on now, the registered string is "1". A 1 is genuinely no
    // multisampling on both of its backends, not a one-sample mode. (Decision 1629.)
    //
    // Latched means a change is PENDING until the next launch — the reference's own callback
    // echoes "set pending gxRestart" — so this row persists and `GetCVar` answers it, while the
    // camera keeps what it was born with. `$WOW_MSAA` overrides it session-only, below.
    same("gxMultisample", "1").latched(),
    // The multisample triple's other two thirds. The reference's Video dropdown formats all three
    // into one row (`MULTISAMPLING_FORMAT_STRING` = "%d-bit color %d-bit depth %dx multisample")
    // and `GetCurrentMultisampleFormat 0x48c580` looks up all three by name to find which row is
    // selected — so without these registered that lookup can never match and the dropdown would
    // sit on entry 1 forever.
    //
    // **They describe, they do not steer.** benilla does not offer a colour or depth format to
    // choose: every format `benilla_world::view::MsaaFormats` publishes carries the same pair,
    // derived from the swapchain format and `Depth32Float`. `SetMultisampleFormat` writes them
    // from the chosen entry exactly like `0x48c640` does, which is a no-op in value and the right
    // shape to keep. The defaults here are the literals that pair matches on every target we ship;
    // if a target ever disagrees the dropdown's own row wins, because it is written from the live
    // enumeration.
    deviates(
        "gxColorBits",
        "32",
        "16",
        "1643: these describe, they do not steer — the pair is our swapchain's own, and every \
         format `MsaaFormats` publishes carries it",
    )
    .latched(),
    deviates(
        "gxDepthBits",
        "32",
        "16",
        "1643: as `gxColorBits` — the depth half of the same descriptive pair",
    )
    .latched(),
    // **The texture filter policy** — 1.12's own `trilinear` and `anisotropic`, over
    // `benilla_assets::TexFilterSetting`. The defaults are the reference's registered strings, and
    // benilla had neither CVar: it hardcoded trilinear + aniso 8 at every sampler it built, which
    // is mode 5 — the *top* of what these two can ask for — shipped as the thing you get before
    // asking. `tex_filter.rs` carries the derivation and the cost.
    //
    // Latched, exactly like `gxMultisample` above and for a harder reason: a sampler is baked into
    // the `Image` at load and lives in the uploaded texture, so a live change would mean rebuilding
    // every texture in the world. The reference's own UI says "enabled upon restart".
    // `$WOW_TRILINEAR` / `$WOW_ANISO` override session-only, below.
    // **`trilinear` registers "1", not the registrar's "0"** (decision 1645, correcting 1642).
    // The reference's `CVar::Register` string is `"0"`, but `hwDetect` — registered `"1"` — runs
    // `DetectHardware 0x641260` at boot and `CVar::Set`s sixteen video CVars from the matched
    // `VideoHardware.dbc` row before the first frame, then self-clears. Every GPU this client runs
    // on is unlisted in a 2004 table, so the row is the fallback scan's, whose reachable set is
    // exactly rows 168/169/170 — and `trilinear` is 1 on 169 and 170, at both CPU tiers, with no
    // CPU bias term. Measured as well as derived: the reference's own `WoW/Logs/gx.log` reads
    // `VID: 106b` → `DID: 2` → `videoID: 170`.
    //
    // This is the same shape as `gxMultisample` above, which also registers the value the hardware
    // table yields rather than a literal the registrar never emits on a modern machine.
    overridden(
        "trilinear",
        "1",
        "0",
        "1645: `hwDetect` sets it from `VideoHardware.dbc` field 9 before the first frame, and \
         that field is 1 on both fallback rows an unlisted modern GPU can reach — measured on the \
         reference's own `Logs/gx.log` (`videoID: 170`)",
    ),
    // `anisotropic` registers `"1"` — off — and here the registrar's string IS the answer: it is
    // **not** one of `hwDetect`'s sixteen (scan of `[0x639a60, 0x639b80)`: sixteen record-pointer
    // reads, `0xc7f2e4` absent), so nothing overwrites it on any path.
    same("anisotropic", "1"),
    // **Weather Intensity** — the video panel's slider 9 (`OptionsFrame.lua:29`,
    // `func = "weatherDensity"`, a real CVar name rather than an engine binding), and the nearest
    // of 2177 §10's named-not-done: benilla has had the feature since 0310 and only the switch was
    // missing. The reader is `benilla_world::weather::WeatherState::weather_density`, which scales
    // the rain/snow/mist spawn rate through the reference's own `0x67b870` quality table
    // {0.1, 0.33, 0.66, 1.0}. Rendering only — it never touches the wire grade, the two ramp
    // channels, or the storm/fog blend, so no server-visible behaviour rides it.
    //
    // The reference registers **`"2"`** at `0x67b806` (flags 0, callback `0x67b870`, name string
    // `0x8685ac`), and this row is among the twelve the reference install's `Config.wtf` moves off
    // its default.
    deviates(
        "weatherDensity",
        "3",
        "2",
        "2181: every precipitation rate in `benilla-world`'s own precipitation module was \
         derived and graded against the reference install's own apitrace captures, and that \
         install runs \
         `SET weatherDensity \"3\"` (K = 1.0) — so 3 is the value a benilla-vs-reference \
         side-by-side is correct at, and the registered 2 would thin every rate to 0.66 against \
         the only client we compare with. The slider is how a player takes it back down",
    ),
    // **Brightness** (decision 2182) — the reference's `gamma`, registered at `0x402d70` with
    // name `0x82e924` `"Gamma"`, default string `0x82e92c` **`"1.0"`** and flags **0** (not
    // latched, so its change callback `0x4034d0` applies on the write).
    //
    // There the callback builds `ramp[i] = __ftol(pow(i · 1/255, gamma) · 65535)` (`0x591680`) and
    // hands the 3×256 words to `GDI32!SetDeviceGammaRamp` — and **skips the upload windowed**
    // (`byte[dev+0x20b]` = `CGxFormat +0x07` = `gxWindow`), which is every mode benilla has. So the
    // reader here is not a ramp upload: it is [`crate::ui_gamma::DisplayGamma`], the same curve
    // applied to the same values one stage later, inside the pass that already owns the composited
    // image's single decode.
    //
    // 1.0 is the identity ramp — load-bearing rather than tidy: at the default this client's
    // output is what it was before the setting existed, so no visual golden moves.
    //
    // **Written `"1.000000"` rather than `"1.0"` or `"1"`, and that is not cosmetic.** Every value
    // this key ever receives comes through `SetGamma 0x4891f0`, whose
    // `SStrPrintf(buf, 0x10, "%f", …)` is six decimals — so the row that Restore Defaults produces
    // is `"1.000000"`, and a default
    // string in any other spelling would make it compare *moved* and write a `config.toml` line
    // holding the default value. The slider rows dodge this with their own trailing-zero strip
    // (`OptionsSlider_OnValueChanged`); a row whose store is an engine verb cannot, because the
    // verb owns the formatting. So the table speaks the verb's spelling instead, and
    // [`sync_cvars`] seeds it the same way. `Same` is still exact: the test parse-compares, and
    // the reference registers this value as `"1.0"` (`0x82e92c`).
    same("gamma", "1.000000"),
    // **Render scale** (decision 1639) — benilla's own CVar, no 1.12 counterpart, in the
    // `boothHalfRate` / `SoundOutputLimiter` mould: the reference has no such dial because it has
    // no second buffer to hang one on. The world renders into the composite lane's off-screen image
    // at `window × this` while the UI stays at native resolution; the knob is
    // [`crate::world_backdrop::RenderScale`], clamped to its `RENDER_SCALE_RANGE`.
    //
    // The era's nearest equivalent is `gxResolution`, which drops the interface along with the
    // world and, in fullscreen, mode-sets the display — the thing 1627 deliberately stopped doing.
    //
    // Default "1" is off, and that is load-bearing rather than cautious: at 1.0 the lane reproduces
    // its pre-1639 numbers bit-for-bit, so no visual golden in the tree moves. `$WOW_RENDER_SCALE`
    // overrides it session-only, below.
    ours(
        "renderScale",
        "1",
        "1639: benilla's own — the reference has no off-screen buffer to hang a resolution dial \
         on; its nearest equivalent, `gxResolution`, drops the interface with the world",
    ),
    // **The FPS journal** (decision 2008) — benilla's own, and the one instrument that ships:
    // `/console fpsJournal 1` appends a per-second row of position, frame cost and the GPU's
    // per-pass split to `benilla-config/Diagnostics/fps-journal.csv` in any build, which is how
    // a player on hardware we do not own measures for us. The knob is
    // [`crate::perf::FpsJournalSetting`]. Off by default; persisted like every row, so a
    // reporter who turns it on keeps it on until they turn it off — the file is theirs to
    // attach and theirs to delete.
    ours(
        "fpsJournal",
        "0",
        "2008: benilla's own — 1.12 has no player-side perf log; its nearest thing is the \
         Ctrl+R framerate label, a number with no file behind it",
    ),
    same(crate::char_select::CVAR_LAST_CHARACTER, "0"),
];

/// `config.toml`'s shape: a `[cvars]` table of `Name = "value"` strings (CVars are strings in
/// the reference too; consumers parse and clamp at their edge). BTreeMap so the file is stably
/// sorted on every save.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct LocalConfig {
    #[serde(default)]
    cvars: BTreeMap<String, String>,
}

// ─── The registry ────────────────────────────────────────────────────────────────────────────

/// **One accepted move of a CVar's applied value** — the reference's change callback, as the
/// Bevy event it is (decision 2303). Triggered by the registry's flushers for every write that
/// changed a row's applied value (a Lua `SetCVar`, a `/console` line, a host write, a committed
/// latch, and the loaded file at boot); observed by the subsystem that owns the knob, beside the
/// knob, writing only its own resource.
///
/// Not fired for a write that changed nothing, for a staged (latched) value, or for a session
/// override taking a row (the knob already read the env).
#[derive(Event, Clone, Debug, PartialEq, Eq)]
pub(crate) struct CvarChanged {
    /// The registered spelling (`MasterVolume`).
    pub(crate) name: String,
    pub(crate) old: String,
    pub(crate) new: String,
}

impl CvarChanged {
    /// Case-insensitive, like every lookup the client makes.
    pub(crate) fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    /// The lowercased name — what an observer's `match` arms are spelled in.
    pub(crate) fn key(&self) -> String {
        self.name.to_ascii_lowercase()
    }

    /// The new value as a number. **The registry refuses an unparseable write to a numeric
    /// row** ([`Cvars::set`]), so on a row whose default is a number this is never a fallback;
    /// on a string row it is `0`, and an observer for a string row reads [`Self::new`] instead.
    pub(crate) fn num(&self) -> f32 {
        self.new.trim().parse().unwrap_or(0.0)
    }

    /// The new value as the client's flag: int-parse, then `!= 0`.
    pub(crate) fn flag(&self) -> bool {
        self.num() != 0.0
    }
}

/// One row of the live registry — the reference's `CVar` record, the parts benilla keeps.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    /// The registered spelling.
    pub(crate) name: String,
    pub(crate) default: String,
    /// The **applied** value: what `GetCVar` answers and what the file is composed from.
    pub(crate) value: String,
    /// A latched row's staged value (`rec+0x38`), applied by [`Cvars::commit_latched`].
    pub(crate) pending: Option<String>,
    /// [`Registered::latched`] — the reference's flag bit1.
    pub(crate) latched: bool,
    /// Declared by an addon's `RegisterCVar` rather than by [`REGISTERED`] (decision 1195): it
    /// persists like any other row, and it is re-seeded into every later VM so the addon's own
    /// re-declaration finds it and no-ops.
    pub(crate) addon: bool,
}

impl Row {
    /// A row whose default parses as a number holds numbers — the registry refuses a write
    /// that does not parse ([`Cvars::set`]), so a numeric observer can trust [`CvarChanged::num`].
    fn numeric(&self) -> bool {
        self.default.trim().parse::<f32>().is_ok()
    }
}

/// What a write did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetOutcome {
    /// No such row (warned).
    Unknown,
    /// A numeric row, and the value does not parse — refused, the applied value stands (warned).
    Refused,
    /// Already the applied value (or already the staged one).
    Unchanged,
    /// A latched row: staged, not applied; nothing fires until the commit.
    Staged,
    /// Applied: a [`CvarChanged`] is queued for the next flush and the config is dirty.
    Changed,
}

/// **The registry** — the engine-side CVar table. See the module doc.
#[derive(Resource)]
pub(crate) struct Cvars {
    rows: Vec<Row>,
    /// Lowercased name → row.
    index: HashMap<String, usize>,
    /// The file's `[cvars]` entries, verbatim spelling — the merge base every save starts from:
    /// unknown keys ride through untouched, session-owned keys keep their stored value.
    file: BTreeMap<String, String>,
    /// Lowercased names this SESSION owns rather than the player — never saved, and the file's
    /// own entry for them is left exactly as it was found.
    ///
    /// Almost all of them are env levers (`$WOW_UI_SCALE`, `$WOW_MSAA`, `$WOW_HOST`, …): a value
    /// that stuck in `config.toml` would make an A/B or an instrument run sticky across
    /// relaunches. `gxApi` (2151) is the member that is not — it is owned by the session because
    /// it is a fact about the *machine* (the render adapter's backend), which is nobody's setting
    /// to persist.
    session_owned: HashSet<String>,
    /// Accepted moves not yet triggered — flushed by [`sync_cvars`] every frame, synchronously by
    /// the boot load and the session-edge fold, and by any caller that wants its observers to
    /// have run before its own dependents ([`Cvars::take_events`]).
    events: Vec<CvarChanged>,
    /// Host-side writes the VM's mirror has not seen yet — pushed by [`sync_cvars`] as host
    /// writes (no echo). Cleared by a seed, which carries the whole table anyway.
    outbox: Vec<(String, String)>,
    /// A change since the last save; `last_change` drives the one-quiet-second debounce.
    dirty: bool,
    last_change: Option<Instant>,
}

impl Default for Cvars {
    fn default() -> Self {
        let mut cvars = Self {
            rows: Vec::with_capacity(REGISTERED.len()),
            index: HashMap::with_capacity(REGISTERED.len()),
            file: BTreeMap::new(),
            session_owned: HashSet::new(),
            events: Vec::new(),
            outbox: Vec::new(),
            dirty: false,
            last_change: None,
        };
        for r in REGISTERED {
            cvars.insert_row(Row {
                name: r.name.to_string(),
                default: r.default.to_string(),
                value: r.default.to_string(),
                pending: None,
                latched: r.latched,
                addon: false,
            });
        }
        cvars
    }
}

impl Cvars {
    fn insert_row(&mut self, row: Row) {
        let key = row.name.to_ascii_lowercase();
        debug_assert!(
            !self.index.contains_key(&key),
            "{}: registered twice",
            row.name
        );
        self.index.insert(key, self.rows.len());
        self.rows.push(row);
    }

    fn slot(&self, name: &str) -> Option<usize> {
        self.index.get(&name.to_ascii_lowercase()).copied()
    }

    /// The row, matched case-insensitively.
    pub(crate) fn row(&self, name: &str) -> Option<&Row> {
        self.slot(name).map(|i| &self.rows[i])
    }

    /// Every row, in registration order.
    pub(crate) fn rows(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter()
    }

    /// The applied value.
    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.row(name).map(|r| r.value.as_str())
    }

    /// The applied value as a number, `None` for an unknown row or a non-numeric value.
    pub(crate) fn num(&self, name: &str) -> Option<f32> {
        self.get(name).and_then(|v| v.trim().parse().ok())
    }

    /// The applied value as the client's flag (int-parse, `!= 0`), `None` for an unknown row.
    pub(crate) fn flag(&self, name: &str) -> Option<bool> {
        self.num(name).map(|v| v != 0.0)
    }

    /// The registered default.
    pub(crate) fn default_of(&self, name: &str) -> Option<&str> {
        self.row(name).map(|r| r.default.as_str())
    }

    /// Whether this session owns the row rather than the player.
    pub(crate) fn is_session_owned(&self, name: &str) -> bool {
        self.session_owned.contains(&name.to_ascii_lowercase())
    }

    /// The persisted `checkAddonVersion` (decision 1292) — what the addon load walk gates on.
    /// The registrar default is check ON.
    pub(crate) fn addon_version_check(&self) -> bool {
        self.flag("checkAddonVersion").unwrap_or(true)
    }

    fn touch(&mut self) {
        self.dirty = true;
        self.last_change = Some(Instant::now());
    }

    /// The write, from either side of the VM boundary. `from_vm` is a write the mirror has
    /// already made (a Lua `SetCVar`, `ConsoleExec`, an engine verb), so it is not echoed back;
    /// a refusal IS pushed back, because the mirror stored what this refused.
    fn write(&mut self, name: &str, value: &str, from_vm: bool) -> SetOutcome {
        let Some(i) = self.slot(name) else {
            warn!("cvar {name}: not registered — write ignored");
            return SetOutcome::Unknown;
        };
        let row = &mut self.rows[i];
        if row.numeric() && value.trim().parse::<f32>().is_err() {
            warn!(
                "cvar {}: unparseable value '{value}' refused (still {:?})",
                row.name, row.value
            );
            if from_vm {
                self.outbox.push((row.name.clone(), row.value.clone()));
            }
            return SetOutcome::Refused;
        }
        if row.latched {
            // The reference's `Set 0x63df50` on flag bit1: `latchedValue` takes the string and
            // `InternalSet` does not run — no dirty, no callback. Staging the applied value
            // back clears the stage.
            // The stage lives HERE, not in the mirror: the VM only ever learns applied values
            // (a host write into the mirror IS a commit there), so a host-side stage is not
            // echoed — `GetCVar` keeps answering the applied value either way, and the commit
            // below is what reaches the VM.
            let staged = (value != row.value).then(|| value.to_string());
            if row.pending == staged {
                return SetOutcome::Unchanged;
            }
            row.pending = staged;
            return if row.pending.is_some() {
                SetOutcome::Staged
            } else {
                SetOutcome::Unchanged // the stage cleared: the boundary has nothing to do
            };
        }
        if row.value == value {
            return SetOutcome::Unchanged;
        }
        let old = std::mem::replace(&mut row.value, value.to_string());
        let name = row.name.clone();
        self.events.push(CvarChanged {
            name: name.clone(),
            old,
            new: value.to_string(),
        });
        if !from_vm {
            self.outbox.push((name, value.to_string()));
        }
        self.touch();
        SetOutcome::Changed
    }

    /// **A host-side write** — the counterpart of a Lua `SetCVar`, for the engine's own values:
    /// the minimap zoom, the camera views, the remembered character, the loading screen's tip
    /// cursor. Mirrored into the VM, persisted, and observed like any other write.
    pub(crate) fn set(&mut self, name: &str, value: &str) -> SetOutcome {
        let outcome = self.write(name, value, false);
        if name.eq_ignore_ascii_case("lightingQuality") {
            apply_lighting_preset(self, value);
        }
        if name.eq_ignore_ascii_case("graphicsQuality") {
            apply_graphics_preset(self, value);
        }
        outcome
    }

    /// A write the VM's mirror already made — drained from its change queue.
    pub(crate) fn set_from_vm(&mut self, name: &str, value: &str) -> SetOutcome {
        let outcome = self.write(name, value, true);
        // Apply at the write's position in the queue: a later member edit must win,
        // including a re-selection of the currently displayed preset.
        if name.eq_ignore_ascii_case("lightingQuality") {
            apply_lighting_preset(self, value);
        }
        if name.eq_ignore_ascii_case("graphicsQuality") {
            apply_graphics_preset(self, value);
        }
        outcome
    }

    /// MONKEY (presets): **the first boot's Graphics Preset** — a player whose `config.toml`
    /// names no `graphicsQuality` (a new player, or one from before the ladder) gets the
    /// [`GRAPHICS_DEFAULT`] column written over every governed row the file does not carry and the
    /// session does not own. Rows the file carries are the player's and stay; the lighting rung
    /// is left alone, because High on that ladder IS the registered defaults
    /// (`the_high_preset_is_the_registered_defaults`). Returns how many rows moved.
    pub(crate) fn seed_graphics_preset(&mut self) -> usize {
        let carried = |cvars: &Self, k: &str| {
            cvars.file.keys().any(|f| f.eq_ignore_ascii_case(k)) || cvars.is_session_owned(k)
        };
        if carried(self, "graphicsQuality") {
            return 0;
        }
        let col = graphics_column(GRAPHICS_DEFAULT).expect("the default is a rung");
        let mut moved = 0;
        for (k, values) in GRAPHICS_PRESETS {
            if k.eq_ignore_ascii_case("lightingQuality") || carried(self, k) {
                continue;
            }
            if self.set(k, values[col]) == SetOutcome::Changed {
                moved += 1;
            }
        }
        moved
    }

    /// MONKEY (followups): **re-apply the saved presets to rows the file does not carry.** A saved
    /// rung name implies every governed row matched it when the file was written (the label is
    /// re-derived every frame), and a row at its registered default is not saved. So a governed
    /// row ABSENT from the file is one that was registered after the save (or held a rung value
    /// equal to its default): it takes the saved rung's value, not its own registered default.
    /// Rows the file carries and session-owned rows are never touched; `Custom` (or no saved
    /// name) writes nothing. The lighting rung is the file's own `lightingQuality`, or else the
    /// saved Graphics rung's. Returns how many rows moved.
    pub(crate) fn reapply_saved_presets(&mut self) -> usize {
        let carried = |cvars: &Self, k: &str| {
            cvars.file.keys().any(|f| f.eq_ignore_ascii_case(k)) || cvars.is_session_owned(k)
        };
        let saved = |cvars: &Self, k: &str| {
            if cvars.is_session_owned(k) {
                return None;
            }
            cvars
                .file
                .iter()
                .find(|(f, _)| f.eq_ignore_ascii_case(k))
                .map(|(_, v)| v.clone())
        };
        let mut moved = 0;
        let col = saved(self, "graphicsQuality").and_then(|v| graphics_column(v.trim()));
        if let Some(col) = col {
            for (k, values) in GRAPHICS_PRESETS {
                if k.eq_ignore_ascii_case("lightingQuality") || carried(self, k) {
                    continue;
                }
                if self.set(k, values[col]) == SetOutcome::Changed {
                    moved += 1;
                }
            }
        }
        let lighting = saved(self, "lightingQuality").or_else(|| {
            let col = col?;
            GRAPHICS_PRESETS
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("lightingQuality"))
                .map(|(_, values)| values[col].to_string())
        });
        let members = lighting.and_then(|name| {
            LIGHTING_PRESETS
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name.trim()))
                .map(|(_, members)| *members)
        });
        for (k, v) in members.unwrap_or(&[]) {
            if carried(self, k) {
                continue;
            }
            if self.set(k, v) == SetOutcome::Changed {
                moved += 1;
            }
        }
        moved
    }

    /// **The table follows a value the engine already applied** — a mirror, not a write: the
    /// applied value moves, the config dirties, the VM's mirror learns it, and **no observer
    /// fires**, because the knob is already there. For a second registered spelling of one knob
    /// (`WorldDetail`/`frillDensity`, 2151) — an observer that answered its sibling's move with a
    /// full write would queue an event that lands one flush later, by which time the row may
    /// have moved again, and the stale event would win. Returns whether the row moved.
    pub(crate) fn mirror(&mut self, name: &str, value: &str) -> bool {
        let Some(i) = self.slot(name) else {
            warn!("cvar {name}: not registered — mirror ignored");
            return false;
        };
        let row = &mut self.rows[i];
        if row.value == value {
            return false;
        }
        row.value = value.to_string();
        row.pending = None;
        self.outbox.push((row.name.clone(), value.to_string()));
        self.touch();
        true
    }

    /// **The latch boundary `RestartGx` crosses** — the reference's `0x639ec0`, fourteen
    /// `CVar::Commit 0x63e060` calls on fourteen gx records (`[0xc4ea90]` … `[0xc4eab4]`,
    /// `0x639ec0..0x639f5a`): each of THOSE staged values becomes the applied one, fires,
    /// persists, and reaches the mirror. Returns how many moved.
    ///
    /// **Only the `gx*` rows.** A latched row outside the video set is not on that list and
    /// nothing else commits it: `SoundBufferSize`'s register site (`0x4571ca`) discards the
    /// record pointer `0x63db90` returns, so no global holds it for any commit to name. It
    /// holds its stage until exit and loses it there, as the reference's does. Until 2343 this
    /// walked every row, and a console `SoundBufferSize` rode the next video Okay into the file.
    ///
    /// The reference runs a gx CVar's change callback at `SetCVar` time (`0x63df50`), and the
    /// commit fires nothing; our observers fire here instead. Nothing reads the difference
    /// today — the appliers are the device rebuild's, and that runs after this boundary.
    pub(crate) fn commit_latched(&mut self) -> usize {
        let mut moved = 0;
        for row in &mut self.rows {
            if !row.name.starts_with("gx") {
                continue;
            }
            let Some(staged) = row.pending.take() else {
                continue;
            };
            if staged == row.value {
                continue;
            }
            let old = std::mem::replace(&mut row.value, staged.clone());
            self.events.push(CvarChanged {
                name: row.name.clone(),
                old,
                new: staged.clone(),
            });
            self.outbox.push((row.name.clone(), staged));
            moved += 1;
        }
        if moved > 0 {
            self.touch();
        }
        moved
    }

    /// **This session owns the row**: an env lever or the machine's own fact took it, so it is
    /// never saved and the file's entry for it is left alone. `value` is what the row answers
    /// this session — `None` marks the row without moving it (the resource is absent, so the
    /// registered default is the truth). No observer fires: the knob already read the env.
    pub(crate) fn own_for_session(&mut self, name: &str, value: Option<&str>) {
        let key = name.to_ascii_lowercase();
        self.session_owned.insert(key.clone());
        let Some(value) = value else {
            return;
        };
        let Some(&i) = self.index.get(&key) else {
            warn!("cvar {name}: not registered — session value ignored");
            return;
        };
        let row = &mut self.rows[i];
        if row.value != value {
            row.value = value.to_string();
            row.pending = None;
            self.outbox.push((row.name.clone(), value.to_string()));
        }
    }

    /// An addon's `RegisterCVar` (decision 1195), reported by the VM: a row of its own, starting
    /// at the file's value for that name when it carries one (the 1291 bridge, now on the store
    /// that survives the VM), else at the declared default. A name already registered — the
    /// client's own, or the same addon's earlier declaration — is the no-op it always was.
    pub(crate) fn learn_addon_row(&mut self, name: &str, default: &str) {
        if self.slot(name).is_some() {
            return;
        }
        let saved = self
            .file
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone());
        self.insert_row(Row {
            name: name.to_string(),
            default: default.to_string(),
            value: saved.unwrap_or_else(|| default.to_string()),
            pending: None,
            latched: false,
            addon: true,
        });
    }

    /// Fold the file in: every known, player-owned key becomes its row's applied value (an
    /// accepted move, so the observers hear it — the reference's `Register` on a record
    /// `Config.wtf` already created calls the callback with the file's value); an unknown key is
    /// preserved for the save and warned once; a session-owned key is skipped and the file keeps
    /// it. Loading is not a change: nothing is dirtied.
    fn load_file(&mut self, file: BTreeMap<String, String>) {
        for (name, value) in &file {
            let key = name.to_ascii_lowercase();
            let Some(&i) = self.index.get(&key) else {
                warn!("config: unknown cvar '{name}' — preserved, not applied");
                continue;
            };
            if self.session_owned.contains(&key) {
                info!("config: {name} is owned by this session, not the file (file value kept)");
                continue;
            }
            let row = &mut self.rows[i];
            if row.numeric() && value.trim().parse::<f32>().is_err() {
                warn!("config: {name}: unparseable value '{value}' ignored");
                continue;
            }
            if row.value == *value {
                continue;
            }
            let old = std::mem::replace(&mut row.value, value.clone());
            self.events.push(CvarChanged {
                name: row.name.clone(),
                old,
                new: value.clone(),
            });
        }
        self.file = file;
    }

    /// The file's entries no row claims — a newer build's keys, or an addon's before it
    /// registers them this session. Handed to the VM as its saved base, so an addon's
    /// `RegisterCVar` starts at the player's value (decision 1291).
    pub(crate) fn orphans(&self) -> Vec<(String, String)> {
        self.file
            .iter()
            .filter(|(k, _)| !self.index.contains_key(&k.to_ascii_lowercase()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// The whole table as a VM's mirror is seeded from it.
    pub(crate) fn vm_seed(&self) -> Vec<SeededCvar> {
        self.rows
            .iter()
            .map(|r| SeededCvar {
                name: r.name.clone(),
                value: r.value.clone(),
                default: r.default.clone(),
                latched: r.latched,
            })
            .collect()
    }

    /// Whether a flush has something to trigger — read before taking, so a quiet frame never
    /// deref-muts the registry.
    pub(crate) fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// The accepted moves since the last flush — the caller triggers each one.
    pub(crate) fn take_events(&mut self) -> Vec<CvarChanged> {
        std::mem::take(&mut self.events)
    }

    fn has_outbox(&self) -> bool {
        !self.outbox.is_empty()
    }

    fn take_outbox(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.outbox)
    }

    /// Compose the file to save: the previous file as the merge base, every row that moved off
    /// its default written (the **applied** value — a staged one is not the player's setting
    /// yet), every one back at its default removed — session-owned keys and unknown keys
    /// untouched.
    fn compose(&self) -> BTreeMap<String, String> {
        let mut out = self.file.clone();
        for row in &self.rows {
            let key = row.name.to_ascii_lowercase();
            if self.session_owned.contains(&key) {
                continue;
            }
            // Match any existing entry case-insensitively so a hand-edited spelling doesn't fork.
            let existing = out
                .keys()
                .find(|k| k.eq_ignore_ascii_case(&row.name))
                .cloned();
            if row.value == row.default {
                if let Some(k) = existing {
                    out.remove(&k);
                }
            } else {
                out.insert(
                    existing.unwrap_or_else(|| row.name.clone()),
                    row.value.clone(),
                );
            }
        }
        out
    }

    /// A registry that already holds one stored value — a launch whose `config.toml` said so,
    /// without a file. For the tests that drive a real consumer over a real remembered row
    /// (`char_select`'s restore test) rather than a copy of its logic.
    #[cfg(test)]
    pub(crate) fn with_value(name: &str, value: &str) -> Self {
        let mut cvars = Self::default();
        cvars.load_file(BTreeMap::from([(name.to_string(), value.to_string())]));
        cvars.events.clear();
        cvars
    }
}

/// How long a dirty config sits before the save fires — long enough to coalesce a slider drag,
/// short enough that a crash loses one gesture, not a session ("write-on-change, debounced").
const SAVE_QUIET: std::time::Duration = std::time::Duration::from_secs(1);

/// The startup fold of `config.toml` into the registry and, through the observers, the knobs
/// ([`load_config`]).
///
/// A set rather than a bare system because one knob is **read once and never again**: the world
/// camera takes its `Msaa` at spawn (decision 1629, the reference's latched `gxMultisample`), so
/// `setup_player` must not be able to run before the file has been folded in. Every other knob is
/// live-read and does not care.
///
/// This removes a **race, not an observed bug**. Measured: with the constraint deleted, a
/// `gxMultisample = "4"` in `config.toml` still reached the camera — and it did so despite
/// `PlayerPlugin` being added *before* `CvarPlugin` (`lib.rs`), i.e. the order that happened to
/// hold was the executor's choice out of an unconstrained graph, not insertion order and not
/// anything we could point at. The failure it prevents is silent (the player's setting is simply a
/// launch late) and would surface as a bug report nobody could reproduce.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CvarLoad;

pub(crate) struct CvarPlugin;

impl Plugin for CvarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Cvars>()
            .add_systems(
                Startup,
                (load_config, publish_filter_policy)
                    .chain()
                    .in_set(CvarLoad),
            )
            // After the tick (decision 2304): a `SetCVar` the interface made this frame reaches
            // the registry — and its observers — before the frame's drains read it. The video
            // window's Okay is the case: `SetCVar` per changed row, then `RestartGx()`, in one
            // handler; `video::drain_restart_gx` orders after this so the commit finds the stage.
            .add_systems(Update, sync_cvars.after(crate::ui_script::UiInput));
        // **The flush is on the exit edge, not beside its feed** (decision 1528). It used to be
        // `(sync_cvars, save_config).chain()` in `Update`, which made the "or the app exiting"
        // half of its own gate dead on the exit a player actually causes: the close button's
        // `AppExit` is not written until `PostUpdate`, so the last second of slider drags went
        // with the process. `Last` still runs after `sync_cvars` — schedule order does what the
        // `.chain()` did — and now also after every announcement.
        crate::shutdown::on_app_exit(app, save_config.into_configs());
    }
}

// ─── MONKEY (advanced graphics): the lighting preset ladder ──────────────────────────────────

/// **The one place the presets are written down** — the Advanced Graphics page's `lightingQuality`
/// ladder, as (name, members) pairs in the order [`derive_lighting_quality`] tries them.
///
/// A preset is a NAME FOR A SET OF ROWS, not a knob: applying one writes each member through the
/// ordinary registry path ([`Cvars::set`]), so every observer, clamp, bridge and save that a
/// hand-typed `/console set` would run, runs. That is why this table lives beside the registry
/// rather than in the options window's Lua: the page is one of four ways to reach it (the others
/// being `/console`, the lighting debug panel, and a hand-edited `config.toml`), and a table
/// carried by the window would be a table three of them cannot see.
///
/// **A preset names only the members it decides.** `Off` says nothing about `shadowMapSize` — with
/// both shadow lanes off there is no map to size — and Low says nothing about the torch counts,
/// which its `interiorShadows 0` has already made inert. Derivation matches on exactly the members
/// a preset lists, so a row nobody's preset mentions never pushes the ladder to `Custom`.
///
/// Order matters twice: the ladder reads Off → High for a human, and the first preset whose whole
/// member list matches wins. The four are mutually exclusive on `characterShadows` /
/// `worldShadows` / `exteriorShadows`, so no value can answer to two of them — a property
/// `every_preset_derives_back_to_its_own_name` holds rather than a reader has to check.
pub(crate) const LIGHTING_PRESETS: &[(&str, &[(&str, &str)])] = &[
    // Everything this whole system invented, off — the client as it rendered before any of it
    // existed. Not "cheap": OFF, so a machine that cannot afford the lanes can say so in one
    // click, and so a bug report can be split from a taste report in one click too.
    (
        "Off",
        &[
            ("characterShadows", "0"),
            ("worldShadows", "0"),
            ("interiorLight", "0"),
            ("interiorShadows", "0"),
            ("exteriorShadows", "0"),
            ("spellLightGain", "0"),
            ("waterQuality", "0"),
            // MONKEY (volumetric fog): Off preset restores the original atmosphere.
            ("volumetricFog", "0"),
            // MONKEY (post): no HDR lift or post pass is the byte-identical baseline.
            ("bloom", "0"),
            ("sunShafts", "0"),
            ("colorGrading", "0"),
            ("lavaLightGain", "0"),
            ("fireLightGain", "0"),
            ("nightGain", "1.0"),
            ("interiorGain", "1.0"),
            ("moonShadowStrength", "0"),
            ("fireFlicker", "0"),
            // MONKEY (wind): Off is the exact zero-displacement path.
            ("foliageWind", "0"),
        ],
    ),
    // Character silhouettes and lit rooms, and nothing that costs a second shadow pass: no world
    // lane, no torch maps indoors or out, no moon term, and the smallest sun map on the ladder.
    (
        "Low",
        &[
            ("characterShadows", "1"),
            ("worldShadows", "0"),
            ("shadowMapSize", "1024"),
            ("interiorLight", "1"),
            ("interiorShadows", "0"),
            ("exteriorShadows", "0"),
            ("spellLightGain", "1"),
            ("waterQuality", "1"),
            // MONKEY (volumetric fog): preset atmosphere uses the default cheap tier.
            ("volumetricFog", "1"),
            // MONKEY (post): quarter-resolution halo.
            ("bloom", "1"),
            ("sunShafts", "0"),
            ("colorGrading", "0"),
            ("lavaLightGain", "1"),
            ("fireLightGain", "1"),
            ("nightGain", "0.45"),
            ("interiorGain", "0.5"),
            ("moonShadowStrength", "0"),
            ("fireFlicker", "1"),
            // MONKEY (wind): Low animates the denser grass lane only.
            ("foliageWind", "1"),
        ],
    ),
    // Both sun lanes, indoor torch shadows at half the residency, and the moon term — but no
    // OUTDOOR torch shadows, which are the one lane whose cost is paid in the open world where the
    // sun lanes are already paying.
    (
        "Medium",
        &[
            ("characterShadows", "1"),
            ("worldShadows", "1"),
            ("shadowMapSize", "2048"),
            ("interiorLight", "1"),
            ("interiorShadows", "1"),
            ("interiorShadowCasters", "6"),
            ("interiorShadowDynamic", "2"),
            ("exteriorShadows", "0"),
            ("spellLightGain", "1"),
            ("waterQuality", "1"),
            // MONKEY (volumetric fog): preset atmosphere uses the default cheap tier.
            ("volumetricFog", "1"),
            // MONKEY (post): quarter-resolution halo.
            ("bloom", "1"),
            ("sunShafts", "0"),
            ("colorGrading", "0"),
            ("lavaLightGain", "1"),
            ("fireLightGain", "1"),
            ("nightGain", "0.45"),
            ("interiorGain", "0.5"),
            ("moonShadowStrength", "0.35"),
            ("fireFlicker", "1"),
            // MONKEY (wind): Medium and High include classified trees.
            ("foliageWind", "2"),
        ],
    ),
    // **High IS the shipped default**, member for member — which is a property, not a coincidence:
    // `the_high_preset_is_the_registered_defaults` welds the two, so a fresh `benilla-config`
    // reads "High" on this row rather than "Custom".
    (
        "High",
        &[
            ("characterShadows", "1"),
            ("worldShadows", "1"),
            ("shadowMapSize", "2048"),
            ("interiorLight", "1"),
            ("interiorShadows", "1"),
            ("interiorShadowCasters", "12"),
            ("interiorShadowDynamic", "4"),
            ("exteriorShadows", "1"),
            ("spellLightGain", "1"),
            // Mirror reflections stay opt-in until their cost is measured.
            ("waterQuality", "1"),
            // MONKEY (volumetric fog): preset atmosphere uses the default cheap tier.
            ("volumetricFog", "1"),
            // MONKEY (post): half-resolution halo.
            ("bloom", "2"),
            ("sunShafts", "1"),
            ("colorGrading", "1"),
            ("lavaLightGain", "1"),
            ("fireLightGain", "1"),
            ("nightGain", "0.45"),
            ("interiorGain", "0.5"),
            ("moonShadowStrength", "0.35"),
            ("fireFlicker", "1"),
            // MONKEY (wind): High preset = grass plus classified trees.
            ("foliageWind", "2"),
        ],
    ),
    // MONKEY (presets): High plus every lane at its maximum — the 4096 sun map, all sixteen
    // resident torch maps with eight of them tracking movers, mirror-reflection water and the
    // High fog march. Disjoint from High on `shadowMapSize`, so derivation never confuses them.
    (
        "Ultra",
        &[
            ("characterShadows", "1"),
            ("worldShadows", "1"),
            ("shadowMapSize", "4096"),
            ("interiorLight", "1"),
            ("interiorShadows", "1"),
            ("interiorShadowCasters", "16"),
            ("interiorShadowDynamic", "8"),
            ("exteriorShadows", "1"),
            ("spellLightGain", "1"),
            ("waterQuality", "2"),
            ("volumetricFog", "2"),
            ("bloom", "2"),
            ("sunShafts", "1"),
            ("colorGrading", "1"),
            ("lavaLightGain", "1"),
            ("fireLightGain", "1"),
            ("nightGain", "0.45"),
            ("interiorGain", "0.5"),
            ("moonShadowStrength", "0.35"),
            ("fireFlicker", "1"),
            ("foliageWind", "2"),
        ],
    ),
];

/// What the ladder shows when the members match no preset. Not a preset: selecting it writes
/// nothing, and the next derivation puts back whatever the members really say.
pub(crate) const LIGHTING_CUSTOM: &str = "Custom";

/// Two registry values are the SAME setting when they are the same number — `"0"`, `"0.00"` and
/// `"0.0"` all mean a lane that is off, and a preset whose member reads `"1"` must not be called
/// `Custom` because a slider wrote `"1.00"`. Falls back to a case-insensitive string compare for
/// the rows that hold words.
fn same_value(a: &str, b: &str) -> bool {
    match (a.trim().parse::<f32>(), b.trim().parse::<f32>()) {
        (Ok(x), Ok(y)) => (x - y).abs() < 1e-4,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// **The name the members currently spell** — the first preset every one of whose members matches
/// the live registry, or [`LIGHTING_CUSTOM`]. This is the only thing `lightingQuality` is ever
/// allowed to say, which is what keeps the row from going stale behind a `/console` write.
pub(crate) fn derive_lighting_quality(cvars: &Cvars) -> &'static str {
    for (name, members) in LIGHTING_PRESETS {
        // MONKEY (reviewfix-a): a session-owned row (an env lever) is not the player's choice and
        // must not turn the saved label Custom; it is skipped, as the seed skips it.
        let matched = members
            .iter()
            .filter(|(k, _)| !cvars.is_session_owned(k))
            .all(|(k, v)| cvars.get(k).is_some_and(|live| same_value(live, v)));
        if matched {
            return name;
        }
    }
    LIGHTING_CUSTOM
}

/// Write one preset's members. Ordinary registry writes: observers, clamps and the save all run.
/// Unknown names (including `Custom`) write nothing and say so by answering `false`.
pub(crate) fn apply_lighting_preset(cvars: &mut Cvars, name: &str) -> bool {
    let Some((_, members)) = LIGHTING_PRESETS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
    else {
        return false;
    };
    for (k, v) in *members {
        cvars.set(k, v);
    }
    true
}

/// Derive after all ordered writes, before the mirror/observer flush. File loading bypasses
/// `set`, so saved members remain authoritative even when the saved preset name disagrees.
fn lighting_quality(cvars: &mut Cvars) {
    let derived = derive_lighting_quality(cvars);
    // A derived label is a mirror, never another request to apply a preset.
    cvars.mirror("lightingQuality", derived);
    // MONKEY (presets): after the lighting label, which the graphics ladder reads as a member.
    let graphics = derive_graphics_quality(cvars);
    cvars.mirror("graphicsQuality", graphics);
}

// ─── MONKEY (presets): the Graphics Preset ladder ─────────────────────────────────────────────

/// The Graphics Preset's rungs, in the order [`derive_graphics_quality`] tries them and the
/// column order of [`GRAPHICS_PRESETS`].
pub(crate) const GRAPHICS_PRESET_NAMES: [&str; 5] = ["Classic", "Low", "Medium", "High", "Ultra"];

/// The rung a player with no saved preset boots into ([`Cvars::seed_graphics_preset`]).
pub(crate) const GRAPHICS_DEFAULT: &str = "High";

/// MONKEY (reviewfix-a): **the rows where the first-boot seed leaves the reference** — the
/// [`GRAPHICS_DEFAULT`] column value a new player actually boots with, against the reference's
/// own default. The registered row keeps its `same(...)` (the registry default IS the
/// reference's; a capture or a test, which never seeds, runs at it), so this list is where the
/// deviation of the *seeded* boot value is declared, `Deviates`-style: `(row, reference, why)`.
/// `seeded_column_deviates_only_where_declared` walks the whole column against the reference
/// rows, both ways.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const SEEDED_DEVIATIONS: &[(&str, &str, &str)] = &[(
    "farclip",
    "350",
    "owner decision: the High preset is the first-boot default, and High's view distance is the \
     reference's own slider maximum, 777 yd; Classic keeps 350",
)];

/// **The one place the Graphics Preset is written down**: one line per governed row, one column
/// per rung of [`GRAPHICS_PRESET_NAMES`]. `lightingQuality` is itself a row — writing its rung
/// writes [`LIGHTING_PRESETS`]' members — so every lighting, water, fog, post and wind row the
/// lighting ladder owns is governed here without a second copy. The other rows are disjoint from
/// that ladder, which is what lets both labels be right at once.
///
/// Classic is the reference client: every lane off, `farclip` at its registered 350. High is
/// each lane's documented High (LIGHTING.md, WATER.md); Ultra is High at every maximum, and the
/// only rung past the reference's 777 yd (`FARCLIP_RANGE`); Low and Medium keep the practically
/// free lanes (sky, dither, modern fog, wet surfaces) and leave the costly ones to High.
///
/// A row that lands later is ONE line here, and only once it is registered (an unregistered
/// member would never match, and the ladder would read Custom forever).
#[rustfmt::skip]
pub(crate) const GRAPHICS_PRESETS: &[(&str, [&str; 5])] = &[
    //                        Classic  Low    Medium    High    Ultra
    ("lightingQuality",      ["Off",  "Low", "Medium", "High", "Ultra"]),
    ("farclip",              ["350",  "350", "477",    "777",  "1497"]),
    ("skyQuality",           ["0",    "1",   "1",      "2",    "2"]),
    ("skyDither",            ["0",    "1",   "1",      "1",    "1"]),
    ("fogModel",             ["0",    "1",   "1",      "1",    "1"]),
    ("rainSurfaces",         ["0",    "1",   "1",      "1",    "1"]),
    ("torchTerrainShadows",  ["0",    "0",   "0",      "1",    "1"]),
    ("daylightWindowSplit",  ["0",    "1",   "1",      "1",    "1"]),
    // MONKEY (integration): the three rows that landed in round 3. Low keeps them off (AO and
    // the lamp halos are GPU passes; the zone skybox is a look change), Medium takes the Low tiers.
    ("ambientOcclusion",     ["0",    "0",   "1",      "2",    "2"]),
    ("zoneSkyboxes",         ["0",    "0",   "1",      "1",    "1"]),
    ("lampFog",              ["0",    "0",   "1",      "2",    "2"]),
];

/// The column of a rung, matched case-insensitively; `None` for `Custom` or anything else.
fn graphics_column(name: &str) -> Option<usize> {
    GRAPHICS_PRESET_NAMES
        .iter()
        .position(|n| n.eq_ignore_ascii_case(name))
}

/// The rung every governed row currently spells, or [`LIGHTING_CUSTOM`] (the one "Custom" both
/// ladders show). Reads `lightingQuality`'s DERIVED label, so run it after that is mirrored.
pub(crate) fn derive_graphics_quality(cvars: &Cvars) -> &'static str {
    for (col, name) in GRAPHICS_PRESET_NAMES.iter().enumerate() {
        // MONKEY (reviewfix-a): session-owned rows are skipped, as in the lighting ladder above.
        let matched = GRAPHICS_PRESETS
            .iter()
            .filter(|(k, _)| !cvars.is_session_owned(k))
            .all(|(k, v)| cvars.get(k).is_some_and(|live| same_value(live, v[col])));
        if matched {
            return name;
        }
    }
    LIGHTING_CUSTOM
}

/// Write one rung's column through [`Cvars::set`]; `false` (and nothing written) for an unknown
/// name, `Custom` included.
pub(crate) fn apply_graphics_preset(cvars: &mut Cvars, name: &str) -> bool {
    let Some(col) = graphics_column(name) else {
        return false;
    };
    for (k, values) in GRAPHICS_PRESETS {
        cvars.set(k, values[col]);
    }
    true
}

/// **What the environment took for this session, and what it set it to** — read off the knobs
/// the env levers already seeded (`RenderScale::default()` reads `$WOW_RENDER_SCALE`, and so
/// on), so the registry answers `GetCVar` with the value the client is actually running at.
///
/// This is the (iv) residue 2265 named: each lever here is one the env still reaches through a
/// resource's `Default` rather than through a `category: debug` row of this table, and each
/// line goes the day its lever does. A lever whose resource is absent (a stripped test app)
/// still marks its row session-owned — the file must not apply over an env the caller set.
fn session_values(world: &World) -> Vec<(&'static str, Option<String>)> {
    let set = |k: &str| std::env::var_os(k).is_some();
    let flag = |b: bool| if b { "1" } else { "0" }.to_string();
    let mut out: Vec<(&'static str, Option<String>)> = Vec::new();
    if set("WOW_UI_SCALE") {
        let v = world.get_resource::<crate::ui_script::UiScaleCvar>();
        out.push(("uiScale", v.map(|s| s.0.to_string())));
    }
    if set("WOW_FARCLIP") {
        let v = world.get_resource::<benilla_world::view::ViewDistance>();
        out.push(("farclip", v.map(|s| s.farclip.to_string())));
    }
    // The clutter A/B env drives the same knob WorldDetail lands on — same session-only law, over
    // BOTH of that knob's spellings ([`CLUTTER_DENSITY_CVARS`]). An off-grid multiplier seeds
    // off-grid honestly: the dropdown shows the raw number and checks nothing (the 0959
    // out-of-range posture, dropdown-flavored).
    if set("WOW_CLUTTER_DENSITY") {
        let v = world.get_resource::<benilla_world::clutter::ClutterConfig>();
        out.push(("WorldDetail", v.map(|c| (c.density - 1.0).to_string())));
        out.push(("frillDensity", v.map(|c| c.frill_density().to_string())));
    }
    // `$WOW_NOVSYNC=1` is the measurement uncap: session-only, exactly like the taste-iteration
    // overrides above. Pinning it into the config would make an instrument run sticky.
    if crate::video::novsync_env() {
        let v = world.get_resource::<crate::video::VideoConfig>();
        out.push(("gxVSync", v.map(|c| flag(c.vsync))));
    }
    // The filter policy's A/B levers, under the same law: pricing mode 3 against mode 5 on one
    // machine in one session is exactly what these are for, and a value that stuck in
    // `config.toml` would silently denominate every later reading.
    let tex = world.get_resource::<benilla_assets::TexFilterSetting>();
    if set("WOW_TRILINEAR") {
        out.push(("trilinear", tex.map(|t| flag(t.trilinear))));
    }
    if set("WOW_ANISO") {
        out.push(("anisotropic", tex.map(|t| t.aniso.to_string())));
    }
    // `$WOW_WIN`, a capture scenario, or any instrumented run owns the window's geometry for the
    // session (decision 1627), so the two CVars that would otherwise move it mid-run are
    // session-only under exactly the same law as the levers above.
    if crate::video::windowed_env() {
        let v = world.get_resource::<crate::video::VideoConfig>();
        out.push((
            "gxWindow",
            v.map(|c| flag(c.display == crate::video::DisplayMode::Windowed)),
        ));
        out.push((
            "gxResolution",
            v.map(|c| format!("{}x{}", c.windowed.x, c.windowed.y)),
        ));
    }
    // `$WOW_MSAA` is the multisampling A/B lever (1629); `$WOW_RENDER_SCALE` the render-scale one
    // (1639), doubly session-only because it is also the supersampling instrument this machine
    // prices pixels with, and an instrument run that pinned 4× into the file would come back at
    // 4× the next time the client opened.
    if set("WOW_MSAA") {
        let v = world.get_resource::<benilla_world::view::MsaaSetting>();
        out.push(("gxMultisample", v.map(|m| m.samples.to_string())));
    }
    if set("WOW_RENDER_SCALE") {
        let v = world.get_resource::<crate::world_backdrop::RenderScale>();
        out.push(("renderScale", v.map(|r| r.0.to_string())));
    }
    // `$WOW_HOST` is the realmlist for the session (1667) — every probe, smoke run and harness leg
    // sets it, and a value pinned into the file would silently repoint the player's client at
    // whatever a test dialed. `Realmlist::default()` has already taken it; this keeps it off disk.
    if set("WOW_HOST") {
        let v = world.get_resource::<crate::realmlist::Realmlist>();
        out.push((
            crate::realmlist::CVAR_REALMLIST,
            v.map(|r| r.address().to_string()),
        ));
    }
    out
}

/// Startup: mark what the session owns, read `benilla-config/config.toml` (absent file = all
/// defaults, not an error) into the registry, and **fire the observers here and now** — this is
/// exclusive so that everything ordered after [`CvarLoad`] finds its knob already written, the
/// way the reference's subsystems find their callback already run by the time they read a
/// record. The VM does not exist yet; [`sync_cvars`] seeds its mirror when it does.
fn load_config(world: &mut World) {
    let session = session_values(world);
    let stored = stored_config();
    let stored_kind = match &stored {
        StoredConfig::Absent => StoredKind::Absent,
        StoredConfig::Table(_) => StoredKind::Table,
        StoredConfig::Bad(_) => StoredKind::Bad,
    };
    let events = {
        let mut cvars = world.resource_mut::<Cvars>();
        for (name, value) in session {
            cvars.own_for_session(name, value.as_deref());
        }
        // The one member with no env var behind it (2151): `gxApi` reports the render adapter's
        // backend, which is a fact about the machine rather than a setting the player chose. It
        // is pushed live by [`sync_cvars`] once the adapter exists, so persisting it could only
        // ever write a name that the next launch overwrites — or, worse, a stale one that
        // outlives the GPU it described.
        cvars.own_for_session("gxApi", None);
        match stored {
            StoredConfig::Absent => {} // no file, capture without a fixture, or no install
            StoredConfig::Bad(msg) => {
                // A malformed file is preserved, not clobbered: nothing loads, but nothing
                // saves over it either until a change actually happens — and the warn names
                // the file.
                warn!("{msg}");
            }
            StoredConfig::Table(table) => {
                cvars.load_file(table);
                // MONKEY (followups): rows registered after the save follow the saved preset.
                cvars.reapply_saved_presets();
            }
        }
        // MONKEY (presets): a player's own run (never a capture, never a malformed file) boots
        // into the default Graphics Preset over whatever its file leaves unsaid.
        if seeds_graphics_preset(&stored_kind) {
            cvars.seed_graphics_preset();
        }
        cvars.take_events()
    };
    for event in events {
        world.trigger(event);
    }
}

/// MONKEY (presets): which [`StoredConfig`] arm a boot took, kept past the table's move.
enum StoredKind {
    Absent,
    Table,
    Bad,
}

/// MONKEY (presets): whether this boot seeds the default Graphics Preset — a run that reads and
/// saves a player's `config.toml` and found it absent or well-formed. A capture (hermetic, or on
/// an explicit fixture) keeps the registered defaults so its A/B stays exact, and a malformed
/// file is left for the player rather than papered over.
fn seeds_graphics_preset(kind: &StoredKind) -> bool {
    let players_file = std::env::var_os("WOW_CAPTURE").is_none() && config_read_path().is_some();
    players_file && matches!(kind, StoredKind::Absent | StoredKind::Table)
}

/// What the one read of `config.toml` found.
enum StoredConfig {
    /// No file, no install, or a capture without an explicit fixture — use registered defaults.
    Absent,
    /// The file's `[cvars]` table.
    Table(BTreeMap<String, String>),
    /// The file is there but unreadable or malformed. The string is what [`load_config`] warns
    /// with — carried rather than logged, because this read happens before the `App` (and so
    /// before `LogPlugin`) exists.
    Bad(String),
}

/// Read `config.toml`.
///
/// **One parser, two callers at two different times** — [`load_config`] at `Startup`, and the
/// primary window literal in [`crate::run`], which has to know `gxWindow`/`gxResolution` *before*
/// the window exists ([`crate::video::boot_window_mode`] carries why booting windowed and flipping
/// a frame later is not good enough).
///
/// Deliberately **not** cached in a `OnceLock`, though it was written that way first. Three reads
/// of a sub-kilobyte file at process start is not a cost worth a global, and a process-wide cache
/// is actively wrong: every test that lays a config down and then runs `load_config` would be
/// answered from whatever the *first* test in the binary happened to see, and `local_state`'s home
/// law can legitimately move under a run. The thing worth having exactly one of is this function,
/// not its result.
fn stored_config() -> StoredConfig {
    let Some(path) = config_read_path() else {
        return StoredConfig::Absent; // capture without a fixture, or no install — session-only
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return StoredConfig::Absent,
        Err(e) => return StoredConfig::Bad(format!("config: cannot read {}: {e}", path.display())),
    };
    match toml::from_str::<LocalConfig>(&text) {
        Ok(cfg) => StoredConfig::Table(cfg.cvars),
        Err(e) => StoredConfig::Bad(format!(
            "config: {} is malformed ({e}) — running on defaults",
            path.display()
        )),
    }
}

/// MONKEY (integration): a capture may opt into one explicit, read-only CVar fixture.
/// MONKEY (reviewfix-a): through its own variable, `WOW_CAPTURE_CVARS=<config.toml path>`, not
/// `BENILLA_HOME` (the general local-state override a player shell may carry, which would load
/// that player's settings into a baseline). Without it a capture reads no config at all. All
/// other local state stays hermetic and [`save_config`] still sees no path.
fn config_read_path() -> Option<std::path::PathBuf> {
    if std::env::var_os("WOW_CAPTURE").is_some() {
        return std::env::var_os("WOW_CAPTURE_CVARS").map(std::path::PathBuf::from);
    }
    crate::local_state::config_path()
}

/// One CVar as `config.toml` holds it, matched case-insensitively — **before the `App` exists**
/// (decision 1627).
///
/// Every other consumer wants [`Cvars::get`], which answers from the registry once it is a
/// resource and stays current across a VM replacement (1291). This one exists for the
/// single caller that cannot wait for a resource: the primary window has to be *built* with its
/// display mode already resolved.
pub(crate) fn boot_cvar(name: &str) -> Option<String> {
    match stored_config() {
        StoredConfig::Table(t) => t
            .into_iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v),
        StoredConfig::Absent | StoredConfig::Bad(_) => None,
    }
}

/// Per frame: seed the VM's mirror once it exists (the registered set at its live values, so
/// `GetCVar` reflects env overrides and the loaded config alike), drain the VM's registrations
/// and writes into the registry, push the registry's own writes into the mirror, and trigger
/// every accepted move for its observers.
pub(crate) fn sync_cvars(
    script: Option<NonSendMut<UiScript>>,
    mut cvars: ResMut<Cvars>,
    adapter: Option<Res<bevy::render::renderer::RenderAdapterInfo>>,
    msaa_formats: Option<Res<benilla_world::view::MsaaFormats>>,
    mut seeded: Local<VmMemo<bool>>,
    mut commands: Commands,
) {
    // **The machine's, not the player's** (2151): the live render backend, so `GetCVar` and
    // pfUI's system tooltip answer what this run is actually on. Absent in a headless app with
    // no renderer, where the registered `""` stands and says so.
    if let Some(adapter) = adapter.as_deref() {
        let backend = adapter.backend.to_str();
        if cvars.get("gxApi") != Some(backend) {
            cvars.own_for_session("gxApi", Some(backend));
        }
    }
    let Some(mut script) = script else {
        lighting_quality(&mut cvars);
        // Nothing to mirror into; a later VM is seeded from the table, which carries it all.
        if cvars.has_outbox() {
            cvars.take_outbox();
        }
        if cvars.has_events() {
            for event in cvars.take_events() {
                commands.trigger(event);
            }
        }
        return;
    };
    // **The VM's writes first, the seed second.** A fresh VM loads the whole interface before
    // this system's first turn against it, and an addon's `SetCVar` at load is already in the
    // queue by then; seeding first would overwrite the mirror with the registry's older value
    // and then apply the write to the registry alone, leaving `GetCVar` a frame behind for the
    // rest of the session. Registrations before writes: an addon declares a row and sets it in
    // the same breath.
    let registrations = script.take_cvar_registrations();
    let changes = script.take_cvar_changes();
    if !registrations.is_empty() || !changes.is_empty() {
        for (name, default) in registrations {
            cvars.learn_addon_row(&name, &default);
        }
        for (name, value) in changes {
            cvars.set_from_vm(&name, &value);
        }
    }
    lighting_quality(&mut cvars);
    if seeded.claim(&script) {
        // The file's unclaimed entries go in FIRST (decision 1291): an addon's `RegisterCVar`
        // later starts its key at the saved value. Then the table itself, at its live values.
        script.set_cvar_saved_base(cvars.orphans());
        script.seed_cvars(cvars.vm_seed());
        if cvars.has_outbox() {
            cvars.take_outbox(); // the seed just carried everything
        }
        // The Video dropdown's menu — what this device actually accepts, enumerated once at
        // `finish()` by `view::MsaaSupportPlugin` (decision 1631) and handed over whole. Pushed
        // here rather than owned by the VM because the list is a fact about the render adapter,
        // which `benilla-ui` has no way to ask and should not grow one.
        script.set_multisample_formats(
            msaa_formats
                .as_deref()
                .map(|f| {
                    f.formats
                        .iter()
                        .map(|&(color_bits, depth_bits, samples)| {
                            benilla_ui::script::MultisampleFormat {
                                color_bits,
                                depth_bits,
                                samples,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );
        // **What `GetVideoCaps` answers with** (decision 2177) — the seven values the stock video
        // window's `OptionsFrame_Load` destructures. Pushed beside the multisample list because it
        // is the same kind of fact: what this client's device and presentation path really offer,
        // which the VM has no way to ask.
        //
        // Six of the seven are properties of the client rather than of the adapter, and each is
        // true here by construction:
        //   * shaders — wgpu has no non-programmable path; there is no fixed-function fallback to
        //     be missing. The reference asked because 2004 hardware could genuinely lack them.
        //   * trilinear and anisotropy — `benilla_assets::tex_filter` builds every sampler with
        //     both available; `ANISO_RANGE`'s top is the ceiling the `anisotropic` CVar clamps to
        //     and is reported raw, because `OptionsFrame.lua:124` matches it against
        //     `ANISOTROPIC_VALUES = {"1","2","4","8","16"}` with `tonumber` and ignores a value
        //     that is not one of them.
        //   * the hardware cursor — `crate::cursor` composites the reference's own
        //     `Interface\Cursor\*.blp` into an OS cursor on every target (an `NSCursor` on macOS,
        //     winit's `CursorIcon::Custom` elsewhere).
        //   * triple buffering — **false, and it is the one that does visible work**. wgpu's
        //     surface decides its own buffering and benilla exposes no knob, so the reference's own
        //     `OptionsFrame_Load` hides check button 13 and re-seats button 6 against button 5
        //     (`OptionsFrame.lua:168-175`). Answering `true` would light a checkbox writing a CVar
        //     nothing reads — 2115 §2's wrong answer that succeeds.
        script.set_video_caps(benilla_ui::script::VideoCaps {
            anisotropic: true,
            pixel_shaders: true,
            vertex_shaders: true,
            trilinear: true,
            triple_buffering: false,
            max_anisotropy: *benilla_assets::ANISO_RANGE.end(),
            hardware_cursor: true,
        });
    }
    if cvars.has_outbox() {
        for (name, _) in cvars.take_outbox() {
            // A preset may have queued this before a newer VM member edit. Publish
            // the final registry value, never that older queued snapshot.
            if let Some(value) = cvars.get(&name) {
                script.set_cvar_host(&name, value);
            }
        }
    }
    if cvars.has_events() {
        for event in cvars.take_events() {
            commands.trigger(event);
        }
    }
}

/// Fold the dying VM's last writes into the registry — the session edge's half of decision
/// 1291's bridge (the seed in [`sync_cvars`] is the other). Called from
/// [`crate::ui_script::end_ui_session`] **after** the shutdown events (an addon's
/// `PLAYER_LOGOUT` handler may `SetCVar`, and in the reference that write lands in an
/// engine-side store that survives) and **before** the VM is replaced.
///
/// The registry IS the store that survives, so there is nothing to copy back: only the writes
/// the per-frame sync never got to see — a `SetCVar` in the final frame would otherwise be
/// overwritten by the stale value when the next VM's seed runs. The observers fire here, so the
/// knobs are current before the next VM is even built.
pub(crate) fn fold_dying_vm_cvars(world: &mut World) {
    // A world with no registry has no file to bridge — a test world or a stripped scenario that
    // never added the plugin.
    if !world.contains_resource::<Cvars>() {
        return;
    }
    let (registrations, changes) = {
        let Some(mut script) = world.get_non_send_resource_mut::<UiScript>() else {
            return;
        };
        (script.take_cvar_registrations(), script.take_cvar_changes())
    };
    let events = {
        let mut cvars = world.resource_mut::<Cvars>();
        for (name, default) in registrations {
            cvars.learn_addon_row(&name, &default);
        }
        for (name, value) in changes {
            cvars.set_from_vm(&name, &value);
        }
        lighting_quality(&mut cvars);
        if cvars.has_outbox() {
            cvars.take_outbox(); // the VM this was for is going away
        }
        if cvars.has_events() {
            cvars.take_events()
        } else {
            Vec::new()
        }
    };
    for event in events {
        world.trigger(event);
    }
}

/// The file's header comment — where these values come from and where the law lives.
const HEADER: &str = "\
# benilla local config (decision 0954) — CVar values that moved off their defaults.
# Managed by the client; hand edits are read on next launch and preserved on save.
";

/// Dirty + one quiet second (or the app exiting) → rewrite `config.toml` atomically, from the
/// registry — the store, not the VM's mirror, so a session with no VM at all (the glue screens,
/// a headless run) saves exactly what it changed.
fn save_config(mut cvars: ResMut<Cvars>, mut exits: MessageReader<AppExit>) {
    let exiting = exits.read().next().is_some();
    if !cvars.dirty {
        return;
    }
    let quiet = cvars.last_change.is_none_or(|t| t.elapsed() >= SAVE_QUIET);
    if !(quiet || exiting) {
        return;
    }
    let Some(path) = crate::local_state::config_path() else {
        cvars.dirty = false; // hermetic/session-only: nothing to write, stop retrying
        return;
    };
    let file = cvars.compose();
    let body = toml::to_string(&LocalConfig {
        cvars: file.clone(),
    })
    .expect("string map serializes");
    match crate::local_state::write_atomic(&path, &format!("{HEADER}{body}")) {
        Ok(()) => {
            cvars.file = file;
            cvars.dirty = false;
        }
        Err(e) => {
            warn!("config: cannot write {}: {e}", path.display());
            cvars.dirty = false; // don't retry every frame into the same error
        }
    }
}

/// Freeze the texture filter policy for the process, and say what it resolved to.
///
/// **A separate system, chained after [`load_config`], deliberately.** `load_config` returns early
/// on an absent or malformed file, and the policy has to be published on every one of those paths:
/// the sampler lanes are an async `AssetLoader` and a set of ordinary systems, none of which can
/// read a resource the others own, so a run that never published would be reading
/// [`benilla_assets::tex_filter`]'s fallback while a player's `config.toml` said otherwise.
///
/// The log line is not decoration — it is the same reasoning as `video::log_display_session`
/// (1627). Every filtering report this client will get comes from a machine nobody here can run,
/// and "which mode was that run actually in" must be readable off the log a player pastes rather
/// than reasoned about.
fn publish_filter_policy(filter: Res<benilla_assets::TexFilterSetting>) {
    benilla_assets::publish_tex_filter(*filter);
    let mode = filter.mode();
    let name = match mode {
        3 => "bilinear + nearest-mip select, aniso off",
        4 => "trilinear, aniso off",
        _ => "trilinear + aniso",
    };
    info!(
        "texture filter: mode {mode} ({name}) — trilinear={} anisotropic={}",
        u8::from(filter.trilinear),
        filter.aniso
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_bubble::BubbleConfig;
    use crate::minimap::MinimapZoom;
    use crate::nameplates::NameConfig;
    use crate::player::camera::{
        FollowConfig, FollowStyle, LookConfig, ZoomLimit, FOLLOW_SPEED_RANGE,
    };
    use crate::portrait::PaneRate;
    use crate::sound::SoundConfig;
    use crate::target::ClickConfig;
    use crate::ui_loot::LootConfig;
    use crate::ui_script::{UiScaleCvar, DEFAULT_UI_SCALE};
    use crate::video::VideoConfig;
    use crate::vplates::VPlateMode;
    use crate::world_backdrop::{RenderScale, RENDER_SCALE_RANGE};
    use benilla_ui::widget::MINIMAP_ZOOM_LEVELS;
    use benilla_world::clutter::ClutterConfig;
    use benilla_world::view::{MsaaSetting, ViewDistance, FARCLIP_RANGE, MSAA_RANGE};

    /// **The standard, enforced: a benilla option's default IS the reference's** (decision 1804)
    /// — every row's [`Reference`] column stands up.
    ///
    /// This is the half that could not be a convention. Before it, the reference's value for a row
    /// lived only in the prose above that row, which meant a default could be *chosen* without
    /// anyone establishing what the client it imitates does — and five of them were: NPC and own
    /// overhead names, enemy V-plates, party chat bubbles and the camera's max-distance factor all
    /// shipped ON or raised while a stock 1.12 client ships them off or low, each one a reasonable
    /// call on its own day and none of them visible as a *set* until somebody went looking. A
    /// third column, mandatory and typed, is what makes the question unskippable; this test is
    /// what makes the answer stay true.
    ///
    /// It checks **both directions**, which is the part that matters over years:
    /// - a [`Reference::Same`] row whose default has drifted off the reference's fails — you
    ///   cannot edit a default and leave the claim behind;
    /// - a [`Reference::Deviates`] or [`Reference::Overridden`] row that has quietly come back
    ///   into agreement *also* fails, because a stale deviation note is worse than none: it hides
    ///   that we are already faithful and invites the next reader to "restore" a divergence.
    ///
    /// Values are parse-compared where both sides are numeric, so `"1"` and `"1.0"` are one claim.
    #[test]
    fn defaults_stand_where_the_reference_column_says() {
        /// One value against another — numeric when both parse, textual otherwise (`gxResolution`,
        /// `realmList`).
        fn agrees(ours: &str, theirs: &str) -> bool {
            match (ours.parse::<f32>(), theirs.parse::<f32>()) {
                (Ok(a), Ok(b)) => a == b,
                _ => ours == theirs,
            }
        }
        for row in REGISTERED {
            let name = row.name;
            match &row.reference {
                Reference::Same(value) => assert!(
                    agrees(row.default, value),
                    "{name}: the row claims the reference registers {value:?} and we ship the \
                     same, but our default is {:?}. If the reference really does differ, this is \
                     a `deviates` row and owes a reason.",
                    row.default,
                ),
                Reference::Deviates { value, why } => {
                    assert!(
                        !agrees(row.default, value),
                        "{name}: a `deviates` row that no longer deviates — our {:?} IS the \
                         reference's. Demote it to `same`; a stale deviation hides that we are \
                         faithful again.",
                        row.default,
                    );
                    assert!(!why.trim().is_empty(), "{name}: a deviation owes a reason");
                }
                Reference::Overridden { registered, why } => {
                    assert!(
                        !agrees(row.default, registered),
                        "{name}: an `overridden` row whose default is just the registered string \
                         {registered:?} — that is `same`, and saying otherwise buries a real \
                         override behind a false one.",
                    );
                    assert!(
                        !why.trim().is_empty(),
                        "{name}: an override owes its mechanism"
                    );
                }
                Reference::Ours(why) => assert!(
                    !why.trim().is_empty(),
                    "{name}: a CVar the reference does not have owes the reason it exists",
                ),
            }
        }
    }

    /// **The inventory** — the exact set of options benilla ships at something other than what a
    /// stock 1.12 client ships, as one readable list.
    ///
    /// The list is the deliverable, not the assertion: a reviewer (or the director) reads *this*
    /// to answer "where do we differ, and is each one still worth it?", and growing it is a
    /// deliberate edit rather than a side effect of adding a row. [`Reference::Overridden`] and
    /// [`Reference::Ours`] are deliberately **not** here — those rows follow the reference's
    /// behaviour, or have no reference behaviour to follow.
    ///
    /// Every name below is argued at its own row; this is the index, and the reason it is sorted
    /// is that the table's order is a load order, not a ranking.
    #[test]
    fn the_options_that_leave_the_reference_are_this_list_and_no_other() {
        let mut names: Vec<&str> = REGISTERED
            .iter()
            .filter(|r| matches!(r.reference, Reference::Deviates { .. }))
            .map(|r| r.name)
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "SoundReverb",
                "autoSelfCast",
                "frillDensity",
                "gxApi",
                "gxColorBits",
                "gxDepthBits",
                "gxResolution",
                "realmList",
                "weatherDensity",
            ],
        );
    }

    /// Every registered default IS the code constant it mirrors — parse-compared so "1" vs
    /// "1.0" cannot fail it, welded so neither side can drift alone.
    ///
    /// **The numeric ones**, which was every one of them until `realmName` — the first
    /// string-valued CVar in the table, and the reason this now filters rather than unwraps. It is
    /// asserted on its own terms in
    /// [`the_only_string_valued_cvar_is_the_realm_and_it_defaults_empty`]; a `parse::<f32>()` over
    /// the whole table would either panic (it did) or quietly need every future string CVar to be
    /// numeric.
    #[test]
    fn registered_defaults_mirror_the_code_truths() {
        let d: BTreeMap<&str, f32> = REGISTERED
            .iter()
            .filter_map(|r| r.default.parse::<f32>().ok().map(|f| (r.name, f)))
            .collect();
        let sound = SoundConfig::default();
        assert_eq!(d["MasterVolume"], sound.master);
        assert_eq!(d["SoundVolume"], sound.sfx);
        assert_eq!(d["MusicVolume"], sound.music);
        assert_eq!(d["AmbienceVolume"], sound.ambience);
        assert_eq!(d["MasterSoundEffects"] != 0.0, sound.enabled);
        assert_eq!(d["EnableMusic"] != 0.0, sound.music_enabled);
        assert_eq!(d["EnableAmbience"] != 0.0, sound.ambience_enabled);
        assert_eq!(d["EnableErrorSpeech"] != 0.0, sound.error_speech);
        assert_eq!(
            d["Sound_EnableSoundWhenGameIsInBG"] != 0.0,
            sound.background_sound
        );
        assert!(
            !sound.background_sound,
            "the reference goes quiet in the background and offers no way out (decision 1847)"
        );
        // Welded like the rest — and deliberately NOT the binary's registrar "1" (1153).
        assert_eq!(d["SoundReverb"] != 0.0, sound.reverb);
        assert_eq!(d["SoundOutputLimiter"] != 0.0, sound.limiter);
        assert!(sound.limiter, "the output limiter ships on (decision 1551)");
        assert!(!sound.reverb, "zone reverb ships off (decision 1153)");
        assert_eq!(d["uiScale"], DEFAULT_UI_SCALE);
        // ViewDistance::default() reads $WOW_FARCLIP; the registered default mirrors the
        // env-less 350 literal (view.rs doc: "Default 350" — the reference's own, 1624).
        assert_eq!(d["farclip"], 350.0);
        // `nearclip` welds to the const the off-world spawners use, so the viewer, the depth probe
        // and the player's camera cannot open on three different near planes again (2163).
        assert_eq!(d["nearclip"], benilla_world::view::NEARCLIP_DEFAULT);
        assert_eq!(
            d["nearclip"],
            ViewDistance::default().nearclip,
            "the registered default and the resource's own must be one number"
        );
        // Same shape as farclip: `MsaaSetting::default()` reads $WOW_MSAA, so the registered
        // default mirrors the env-less literal — 1, the reference's own (1629).
        assert_eq!(d["gxMultisample"], 1.0);
        // The Controls trio (0961) welds to its knob Defaults the same way.
        assert_eq!(
            d["deselectOnClick"] != 0.0,
            ClickConfig::default().deselect_on_click
        );
        assert_eq!(
            d["mouseInvertPitch"] != 0.0,
            LookConfig::default().invert_pitch
        );
        assert_eq!(d["mousespeed"], LookConfig::default().sensitivity);
        assert_eq!(d["cameraDistanceMaxFactor"], ZoomLimit::default().factor());
        // The follow trio (1493/1502) welds to FollowConfig's own defaults — and all three ARE
        // the binary's registrar values, byte-verified: "1"/"1"/"180.0". The one corner of this
        // arc that agrees with the reference outright.
        let follow = FollowConfig::default();
        assert_eq!(
            d["cameraSmoothStyle"],
            follow.style.cvar().parse::<f32>().unwrap()
        );
        assert_eq!(
            d["cameraSmoothTrackingStyle"],
            follow.tracking_style.cvar().parse::<f32>().unwrap()
        );
        assert_eq!(d["cameraYawSmoothSpeed"], follow.yaw_speed);
        assert_eq!(FollowStyle::default(), FollowStyle::Smart);
        assert_eq!(d["autoLootDefault"] != 0.0, LootConfig::default().auto_loot);
        // The roll-detail switch (1589) welds to the same knob's default — and that default IS
        // the binary's registered "1", so this row agrees with the reference on both sides.
        assert_eq!(
            d["showLootSpam"] != 0.0,
            LootConfig::default().show_loot_spam
        );
        // …and the one row that ships a feature OFF, byte-read at `0x5e24c7`.
        assert_eq!(
            d["guildMemberNotify"] != 0.0,
            crate::ui_guild::GuildMemberNotify::default().0
        );
        assert_eq!(d["guildMemberNotify"], 0.0, "the binary registers \"0\"");
        // Block Trades (1764) welds the same way, and its "0" is behaviour: the refusal leg only
        // fires when the CVar is SET, so an unset value must read as "trades allowed".
        assert_eq!(
            d["BlockTrades"] != 0.0,
            crate::ui_trade::BlockTrades::default().0
        );
        assert_eq!(d["BlockTrades"], 0.0, "an unset BlockTrades allows trades");
        // The name trio (0992) welds to NameConfig's defaults the same way — and all three are
        // the binary's own registrar values now (1804), not two director pins over one.
        let names = NameConfig::default();
        assert_eq!(d["UnitNamePlayer"] != 0.0, names.player);
        assert_eq!(d["UnitNameNPC"] != 0.0, names.npc);
        assert_eq!(d["UnitNameOwn"] != 0.0, names.own);
        assert_eq!(d["UnitNamePlayerGuild"] != 0.0, names.player_guild);
        assert!(
            names.player && !names.npc && !names.own && names.player_guild,
            "the binary registers UnitNamePlayer \"1\", NPC \"0\", Own \"0\", \
             PlayerGuild \"1\""
        );
        // The camera options weld to `CameraOptions::default()` the same way (2149) — and the one
        // that matters here is the one registered "1": a `cameraPivot` that shipped OFF would be
        // benilla diverging from the reference on a feature it now has.
        let camera_opts = crate::player::camera_dynamics::CameraOptions::default();
        assert_eq!(d["cameraPivot"] != 0.0, camera_opts.pivot);
        assert!(camera_opts.pivot, "the binary registers cameraPivot \"1\"");
        assert_eq!(
            d["cameraWaterCollision"] != 0.0,
            camera_opts.water_collision
        );
        assert!(
            camera_opts.pivot && camera_opts.water_collision,
            "the binary registers cameraPivot and cameraWaterCollision both \"1\""
        );
        assert_eq!(d["cameraTerrainTilt"] != 0.0, camera_opts.terrain_tilt);
        assert!(
            !camera_opts.terrain_tilt,
            "the binary registers cameraTerrainTilt \"0\""
        );
        assert_eq!(
            d["cameraGroundSmoothSpeed"],
            camera_opts.ground_smooth_speed
        );
        assert_eq!(d["cameraTerrainTiltTimeMin"], camera_opts.tilt_time_min);
        assert_eq!(d["cameraTerrainTiltTimeMax"], camera_opts.tilt_time_max);
        assert_eq!(d["cameraBobbing"] != 0.0, camera_opts.bobbing);
        assert!(
            !camera_opts.bobbing && !camera_opts.terrain_tilt,
            "the binary registers cameraBobbing and cameraTerrainTilt both \"0\""
        );
        assert_eq!(d["cameraBobbingLRAmplitude"], camera_opts.bob_lr_amplitude);
        assert_eq!(d["cameraBobbingUDAmplitude"], camera_opts.bob_ud_amplitude);
        assert_eq!(d["cameraBobbingFrequency"], camera_opts.bob_frequency);
        assert_eq!(d["cameraBobbingSmoothSpeed"], camera_opts.bob_smooth_speed);
        assert_eq!(d["cameraPivotDXMax"], camera_opts.pivot_dx_max);
        assert_eq!(d["cameraPivotDYMin"], camera_opts.pivot_dy_min);
        assert_eq!(
            d["cameraTargetSmoothSpeed"],
            camera_opts.target_smooth_speed
        );
        // The V-plate pair welds to VPlateMode's defaults — both OFF, which is the reference's
        // own boot state on both of its halves (the `[0xc4da34]` bitmask and FrameXML's
        // `NAMEPLATES_ON = nil`). Enemy plates were the 0167 director pin until 1804.
        // Weather Intensity (2181): the CVar's default and the weather driver's own must be
        // the same rain, or a fresh config writes a row the world does not agree with. This is
        // also where the DEVIATION is held honest — the reference registers "2" and the row
        // above says why we ship 3; the weld makes sure it is 3 in both places.
        assert_eq!(
            d["weatherDensity"],
            f32::from(benilla_world::weather::WeatherState::default().weather_density)
        );
        let plates = VPlateMode::default();
        assert_eq!(d[crate::vplates::CVAR_ENEMIES] != 0.0, plates.enemies);
        assert_eq!(d[crate::vplates::CVAR_FRIENDS] != 0.0, plates.friends);
        assert!(
            !plates.enemies && !plates.friends,
            "a fresh 1.12 client draws no plates until V is pressed"
        );
        // ClutterConfig::default() reads $WOW_CLUTTER_DENSITY; the registered default mirrors
        // the env-less ×2 literal (clutter.rs: "Default ×2 = Medium", 1649) on the panel's 0..2
        // scale. The weld is the point: the CVar's default and the engine's must be the same
        // ground cover, or a fresh config writes a row the world does not agree with.
        assert_eq!(d["WorldDetail"], 1.0);
        // …and its twin in the reference's own unit (2151) welds to it, not beside it: the two
        // rows are one knob read two ways, so a default that disagreed would ship a client whose
        // panel stop and whose cells-per-chunk describe different ground.
        assert_eq!(
            d["frillDensity"],
            (d["WorldDetail"] + 1.0) * benilla_formats::FRILL_DENSITY as f32
        );
        // The bubble pair (1139) welds to BubbleConfig's defaults — both the binary's own since
        // 1804 (`ChatBubbles` "1", `ChatBubblesParty` "0"; the party half was 0598's director pin).
        let bubbles = BubbleConfig::default();
        assert_eq!(d["ChatBubbles"] != 0.0, bubbles.all);
        assert_eq!(d["ChatBubblesParty"] != 0.0, bubbles.party);
        assert!(bubbles.all && !bubbles.party, "the binary's own pair");
        // The minimap pair (1131) welds to the widget's own `MINIMAP_DEFAULT_ZOOM`, which is the
        // byte-verified registration default `"3"` — one truth, mirrored in three places.
        let zoom = MinimapZoom::default();
        assert_eq!(d["minimapZoom"], f32::from(zoom.outdoor));
        assert_eq!(d["minimapInsideZoom"], f32::from(zoom.inside));
        assert_eq!(zoom.outdoor, benilla_ui::widget::MINIMAP_DEFAULT_ZOOM);
        // VSync welds to the video knob, which in turn welds to the window literal's boot
        // mode (`video::tests`) — so the registered "1" cannot drift from what we ship.
        assert_eq!(d["gxVSync"] != 0.0, VideoConfig::default().vsync);
        // ── MONKEY (lighting): the weld for the dynamic light + shadow system's whole row set ──
        //
        // **All 34, as one census, with the count asserted.** Every one of them lands on
        // `VideoConfig` and is read from there per frame (`shadow_core::update_shadows`,
        // `dynamic_interior::bridge`, `torch_shadow`), so a registered default that drifts from
        // the struct's literal is a setting that reads one way in `config.toml` and renders
        // another — invisible until someone compares a fresh install against a configured one.
        // Spelling the table out rather than asserting a favourite handful is what makes "added
        // a row, forgot its weld" fail HERE: the length check below is the gate.
        let shadows = VideoConfig::default();
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        // MONKEY (volumetric fog): include the atmospheric tier in this fixed-size default table.
        let lighting: [(&str, f32); 38] = [
            ("waterQuality", shadows.water_quality as f32),
            // MONKEY (volumetric fog): weld registry and renderer defaults.
            ("volumetricFog", shadows.volumetric_fog as f32),
            // MONKEY (lampfog): opt-in point-light fog.
            ("lampFog", shadows.lamp_fog as f32),
            // MONKEY (post): weld registry and renderer defaults.
            ("bloom", shadows.bloom as f32),
            ("sunShafts", flag(shadows.sun_shafts)),
            ("colorGrading", flag(shadows.color_grading)),
            ("lavaLightGain", shadows.lava_light_gain),
            // The two sun lanes and the cascade they share.
            ("worldShadows", flag(shadows.world_shadows)),
            ("characterShadows", flag(shadows.character_shadows)),
            ("shadowDistance", shadows.shadow_distance),
            // MONKEY (sun shadow perf): the five cost dials.
            ("shadowMapSize", shadows.shadow_map_size as f32),
            ("shadowFilter", shadows.shadow_filter as f32),
            ("characterShadowRate", shadows.character_shadow_rate as f32),
            ("worldShadowRate", shadows.world_shadow_rate as f32),
            ("shadowCasterReach", shadows.shadow_caster_reach),
            // MONKEY (moon shadows): the night lane's own shadow darkness.
            ("moonShadowStrength", shadows.moon_shadow_strength),
            // MONKEY (dynamic interiors): the room lane's switch and its light law.
            ("interiorLight", flag(shadows.interior_light)),
            ("interiorAmbient", shadows.interior_ambient),
            ("interiorFill", shadows.interior_fill),
            ("interiorExposure", shadows.interior_exposure),
            ("interiorAttenScale", shadows.interior_atten_scale),
            ("interiorRoomGate", flag(shadows.interior_room_gate)),
            ("interiorDebug", shadows.interior_debug as f32),
            // MONKEY (torch shadows): the cube-map lane, indoors and out.
            ("interiorShadows", flag(shadows.interior_shadows)),
            ("exteriorShadows", flag(shadows.exterior_shadows)),
            // MONKEY (daylight: terrain torch casters)
            ("torchTerrainShadows", flag(shadows.torch_terrain_shadows)),
            (
                "interiorShadowCasters",
                shadows.interior_shadow_casters as f32,
            ),
            (
                "interiorShadowDynamic",
                shadows.interior_shadow_dynamic as f32,
            ),
            (
                "interiorShadowEntityRate",
                shadows.interior_shadow_entity_rate as f32,
            ),
            ("interiorShadowSoft", shadows.interior_shadow_soft),
            ("torchShadowStrength", shadows.torch_shadow_strength),
            // MONKEY (darkness gains) / (enclosed day floor) / (bake floor): the four level dials.
            ("nightGain", shadows.night_gain),
            ("interiorGain", shadows.interior_gain),
            ("interiorDaylight", shadows.interior_daylight),
            ("interiorBakeFloor", shadows.interior_bake_floor),
            // MONKEY (fire GO lights) / (spellLightGain) / (flame flicker): the invented lanes.
            ("fireLightGain", shadows.fire_light_gain),
            ("spellLightGain", shadows.spell_light_gain),
            ("fireFlicker", shadows.fire_flicker),
        ];
        for (name, want) in lighting {
            assert_eq!(d[name], want, "{name}: registered default left the knob");
        }
        // …and the census is the VideoConfig-backed row set. A name here that nothing registers
        // would weld against a row the client does not have; the length is the other half — 38
        // VideoConfig rows, 38 welds (MONKEY lampfog: + lampFog). MONKEY (wind): `foliageWind` is a world resource bridge.
        let welded: std::collections::BTreeSet<&str> = lighting.iter().map(|(n, _)| *n).collect();
        // MONKEY (volumetric fog): the atmospheric tier joins the default-consumer weld.
        assert_eq!(welded.len(), 38, "the lighting lane welds 38 distinct rows");
        for name in &welded {
            assert!(
                REGISTERED.iter().any(|r| r.name == *name),
                "{name}: welded but not registered"
            );
        }
        // Three of them are CALIBRATED rather than chosen, so the weld alone is not enough — it
        // would pass just as happily if a tuning session left the struct's literal wherever it was
        // last dragged in the debug panel. These pin the measured numbers themselves.
        assert_eq!(d["interiorGain"], 0.5, "MONKEY (lighting debug panel)");
        // MONKEY (bake floor): the inn's black door band measured up to 0.108 x tex with
        // candle-lit surfaces moving under +10 % — only true at this number.
        assert_eq!(d["interiorBakeFloor"], 0.12);
        assert_eq!(d["spellLightGain"], 1.0, "the spell lane ships neutral");
        assert_eq!(d["waterQuality"], 1.0, "mirror reflections stay opt-in");
        assert_eq!(d["lavaLightGain"], 1.0);
        // MONKEY (ao): registry and renderer agree, and the lane ships Off.
        assert_eq!(d["ambientOcclusion"], shadows.ambient_occlusion as f32);
        assert_eq!(d["ambientOcclusion"], 0.0, "ambient occlusion is opt-in");
        // `lightingQuality` and MONKEY (wind) `foliageWind` deliberately are not in that census:
        // neither is a `VideoConfig` knob. The former names the rows above; its own weld is
        // `the_high_preset_is_the_registered_defaults`. The latter bridges directly to the
        // world-owned `FoliageWind` resource in `monkey_gfx`.
        // ── end MONKEY (lighting) ─────────────────────────────────────────────────────────────
        // The pane half-rate (1444) welds to the portrait knob's shipped default.
        assert_eq!(d["boothHalfRate"] != 0.0, PaneRate::default().half);
        // Render scale (1639) welds to OFF. Not a taste default: the whole tree of visual
        // goldens is denominated in a 1:1 backdrop, so a registered value other than 1 would
        // silently re-render every one of them through a resample.
        assert_eq!(d["renderScale"], 1.0);
    }

    // ── MONKEY (advanced graphics): the preset ladder ─────────────────────────────────────────

    /// A registry holding nothing but the registered defaults — the state a fresh
    /// `benilla-config` boots into, which is what every claim below is measured against.
    fn fresh_registry() -> Cvars {
        Cvars::default()
    }

    /// **The round trip, for every rung**: apply a preset, derive, and get that preset's own name
    /// back. This is the property the whole ladder rests on — without it the dropdown would show
    /// `Custom` the instant after the player picked something, which is the exact failure the
    /// "derived, never stored stale" rule exists to prevent.
    #[test]
    fn every_preset_derives_back_to_its_own_name() {
        for (from, _) in LIGHTING_PRESETS {
            for (name, _) in LIGHTING_PRESETS {
                let mut cvars = fresh_registry();
                apply_lighting_preset(&mut cvars, from);
                cvars.set("shadowDistance", "120");
                cvars.set("interiorShadowSoft", "2.5");
                assert!(apply_lighting_preset(&mut cvars, name), "{name}: applied");
                assert_eq!(derive_lighting_quality(&cvars), *name, "{from} -> {name}");
                assert_eq!(cvars.get("shadowDistance"), Some("120"));
                assert_eq!(cvars.get("interiorShadowSoft"), Some("2.5"));
                // MONKEY (volumetric fog): Off must remove atmosphere as well as enhanced water.
                for member in ["fireLightGain", "waterQuality", "volumetricFog", "lavaLightGain", "nightGain", "interiorGain"] {
                    let expected = if *name == "Off" {
                        if matches!(member, "fireLightGain" | "waterQuality" | "volumetricFog" | "lavaLightGain") { "0" } else { "1.0" }
                    } else if *name == "Ultra" && matches!(member, "waterQuality" | "volumetricFog") {
                        "2" // MONKEY (presets): Ultra's two maxima over the defaults.
                    } else {
                        cvars.default_of(member).unwrap()
                    };
                    assert!(same_value(cvars.get(member).unwrap(), expected), "{name}/{member}");
                }
            }
        }
    }

    /// **High is the shipped default, member for member.** A fresh config must read "High" on this
    /// row rather than "Custom" — otherwise the very first thing a player sees on the page is a
    /// word that says their settings are a hand-made combination when they have touched nothing.
    #[test]
    fn the_high_preset_is_the_registered_defaults() {
        let cvars = fresh_registry();
        assert_eq!(derive_lighting_quality(&cvars), "High");
        assert_eq!(
            cvars.get("lightingQuality"),
            Some("High"),
            "the registered default agrees with what a fresh registry derives"
        );
        // …and the table is not merely CONSISTENT with the defaults, it IS them: a member whose
        // registered default moved without its preset entry moving would still derive "High"
        // above (both sides move together), so the weld is checked against the row's own default.
        let (_, members) = LIGHTING_PRESETS
            .iter()
            .find(|(n, _)| *n == "High")
            .expect("the High rung");
        for (k, v) in *members {
            assert!(
                same_value(cvars.default_of(k).expect("a registered member"), v),
                "{k}: the High preset says {v}, the registry's default says {:?}",
                cvars.default_of(k)
            );
        }
    }

    /// **One member off the rung is `Custom`** — for every rung, and for every member of it, which
    /// is the half that keeps the derivation from being a lucky match on a favourite row.
    #[test]
    fn one_changed_member_derives_custom() {
        for (name, members) in LIGHTING_PRESETS {
            for (k, v) in *members {
                let mut cvars = fresh_registry();
                apply_lighting_preset(&mut cvars, name);
                // Somewhere else on the row's own scale: the flags flip, the numbers move by one.
                let moved = match v.trim().parse::<f32>() {
                    Ok(x) if x == 0.0 => "1".to_string(),
                    Ok(x) if x == 1.0 => "0".to_string(),
                    Ok(x) => (x + 1.0).to_string(),
                    Err(_) => format!("{v}x"),
                };
                assert_eq!(cvars.set(k, &moved), SetOutcome::Changed, "{name}/{k}");
                assert_eq!(
                    derive_lighting_quality(&cvars),
                    LIGHTING_CUSTOM,
                    "{name}: {k} moved to {moved} and the ladder still says {name}"
                );
            }
        }
    }

    /// A value that says the same NUMBER is the same setting: the options window's sliders write
    /// `"1"`, a hand-edited config may say `"1.0"`, and `%.2f` formatting elsewhere says `"1.00"`.
    /// A ladder that compared strings would call every one of those `Custom`.
    #[test]
    fn the_ladder_compares_numbers_not_spellings() {
        let mut cvars = fresh_registry();
        apply_lighting_preset(&mut cvars, "High");
        for spelling in ["1.0", "1.00", "1"] {
            cvars.set("spellLightGain", spelling);
            assert_eq!(derive_lighting_quality(&cvars), "High", "{spelling}");
        }
        cvars.set("moonShadowStrength", "0.350");
        assert_eq!(derive_lighting_quality(&cvars), "High");
    }

    /// `Custom` is not a rung: picking it writes nothing, so the members — and therefore the name
    /// the next derivation lands on — are exactly where they were.
    #[test]
    fn picking_custom_writes_nothing() {
        let mut cvars = fresh_registry();
        apply_lighting_preset(&mut cvars, "Medium");
        assert!(!apply_lighting_preset(&mut cvars, LIGHTING_CUSTOM));
        assert!(!apply_lighting_preset(&mut cvars, "Epic"));
        assert_eq!(derive_lighting_quality(&cvars), "Medium");
    }

    // ── MONKEY (presets): the Graphics Preset ladder ──────────────────────────────────────────

    /// Every governed row is registered and names no row the lighting ladder already owns — the
    /// disjointness that lets both labels be right at once.
    #[test]
    fn graphics_rows_are_registered_and_disjoint_from_the_lighting_ladder() {
        assert_eq!(GRAPHICS_PRESETS.len(), 11, "add the row count with the row");
        let cvars = fresh_registry();
        for (k, values) in GRAPHICS_PRESETS {
            assert!(cvars.get(k).is_some(), "{k}: not registered");
            for (rung, members) in LIGHTING_PRESETS {
                assert!(
                    members.iter().all(|(m, _)| !m.eq_ignore_ascii_case(k)),
                    "{k} is also a member of lighting {rung}"
                );
            }
            if *k == "lightingQuality" {
                for v in values {
                    assert!(
                        LIGHTING_PRESETS.iter().any(|(n, _)| n == v),
                        "{v}: a lighting rung"
                    );
                }
            }
        }
        // The rungs are distinct columns, so no two can derive to the same name.
        for a in 0..GRAPHICS_PRESET_NAMES.len() {
            for b in a + 1..GRAPHICS_PRESET_NAMES.len() {
                assert!(
                    GRAPHICS_PRESETS
                        .iter()
                        .any(|(_, v)| !same_value(v[a], v[b])),
                    "{} and {} are the same column",
                    GRAPHICS_PRESET_NAMES[a],
                    GRAPHICS_PRESET_NAMES[b]
                );
            }
        }
    }

    /// **Preset → rows → the same preset**, from every rung to every rung, through the ordinary
    /// `set` path — with the two rows no preset decides left where the player put them.
    #[test]
    fn every_graphics_preset_derives_back_to_its_own_name() {
        for from in GRAPHICS_PRESET_NAMES {
            for name in GRAPHICS_PRESET_NAMES {
                let mut cvars = fresh_registry();
                cvars.set("graphicsQuality", from);
                cvars.set("shadowDistance", "120");
                cvars.set("interiorShadowSoft", "2.5");
                cvars.set("graphicsQuality", name);
                lighting_quality(&mut cvars);
                assert_eq!(derive_graphics_quality(&cvars), name, "{from} -> {name}");
                assert_eq!(cvars.get("graphicsQuality"), Some(name));
                let col = graphics_column(name).unwrap();
                assert_eq!(
                    cvars.get("lightingQuality"),
                    Some(GRAPHICS_PRESETS[0].1[col])
                );
                assert_eq!(cvars.get("shadowDistance"), Some("120"));
                assert_eq!(cvars.get("interiorShadowSoft"), Some("2.5"));
            }
        }
    }

    /// Classic is the reference client: the lighting ladder's Off, every programme lane off and
    /// `farclip` at the reference's registered 350.
    #[test]
    fn the_classic_preset_is_every_lane_off_at_the_reference_view_distance() {
        let mut cvars = fresh_registry();
        assert!(apply_graphics_preset(&mut cvars, "Classic"));
        assert_eq!(cvars.get("lightingQuality"), Some("Off"));
        assert_eq!(cvars.get("farclip"), cvars.default_of("farclip"));
        for (k, values) in GRAPHICS_PRESETS {
            if !matches!(*k, "lightingQuality" | "farclip") {
                assert!(same_value(values[0], "0"), "{k}: Classic leaves it on");
            }
        }
        // Ultra is the one rung past the reference's 777, and inside the extended clamp.
        let ultra = GRAPHICS_PRESETS
            .iter()
            .find(|(k, _)| *k == "farclip")
            .unwrap()
            .1[4];
        let ultra: f32 = ultra.parse().unwrap();
        assert!(ultra > 777.0 && FARCLIP_RANGE.contains(&ultra));
    }

    /// **Editing one governed row is Custom** — for every rung and every row of it, the lighting
    /// rung included (moved by one of ITS members, as the page's lighting rows would).
    #[test]
    fn one_changed_graphics_row_derives_custom() {
        for name in GRAPHICS_PRESET_NAMES {
            let col = graphics_column(name).unwrap();
            for (k, values) in GRAPHICS_PRESETS {
                let mut cvars = fresh_registry();
                apply_graphics_preset(&mut cvars, name);
                let (row, moved) = if *k == "lightingQuality" {
                    let to = if values[col] == "Off" { "1" } else { "0" };
                    ("characterShadows", to.to_string())
                } else {
                    let v: f32 = values[col].parse().unwrap();
                    let to = if *k == "farclip" {
                        v + 60.0
                    } else if v == 0.0 {
                        1.0
                    } else {
                        0.0
                    };
                    (*k, to.to_string())
                };
                assert_eq!(cvars.set(row, &moved), SetOutcome::Changed, "{name}/{row}");
                lighting_quality(&mut cvars);
                assert_eq!(
                    cvars.get("graphicsQuality"),
                    Some(LIGHTING_CUSTOM),
                    "{name}/{row}"
                );
                // …and picking the rung again puts every row back.
                assert!(apply_graphics_preset(&mut cvars, name));
                lighting_quality(&mut cvars);
                assert_eq!(
                    cvars.get("graphicsQuality"),
                    Some(name),
                    "{name}/{row} re-pick"
                );
            }
        }
        let mut cvars = fresh_registry();
        assert!(!apply_graphics_preset(&mut cvars, LIGHTING_CUSTOM));
        assert!(!apply_graphics_preset(&mut cvars, "Epic"));
    }

    /// **A new player boots into High**: the seed writes the High column over a registry with no
    /// file, and the registered default of the label agrees, so it never reaches `config.toml`.
    #[test]
    fn a_fresh_player_is_seeded_to_the_high_graphics_preset() {
        assert_eq!(
            fresh_registry().default_of("graphicsQuality"),
            Some(GRAPHICS_DEFAULT)
        );
        let mut cvars = fresh_registry();
        assert!(cvars.seed_graphics_preset() > 0);
        lighting_quality(&mut cvars);
        assert_eq!(cvars.get("graphicsQuality"), Some("High"));
        assert_eq!(cvars.get("lightingQuality"), Some("High"));
        assert_eq!(cvars.get("farclip"), Some("777"));
        assert!(
            !cvars.compose().contains_key("graphicsQuality"),
            "the default label is not saved"
        );
        // A second boot over what the first one saved seeds nothing.
        let mut again = fresh_registry();
        again.load_file(cvars.compose());
        assert_eq!(again.seed_graphics_preset(), 0);
        lighting_quality(&mut again);
        assert_eq!(again.get("graphicsQuality"), Some("High"));
    }

    /// MONKEY (followups): a saved Graphics rung fills the governed rows the file lacks (a row
    /// registered after the save) with the rung's value; rows the file carries stay; Custom and
    /// no saved name write nothing.
    #[test]
    fn a_saved_graphics_preset_fills_rows_missing_from_the_file() {
        // An Ultra save made before `ambientOcclusion` / `lampFog` existed.
        let file = BTreeMap::from([
            ("graphicsQuality".to_string(), "Ultra".to_string()),
            ("lightingQuality".to_string(), "Ultra".to_string()),
            ("farclip".to_string(), "1497".to_string()),
            ("zoneSkyboxes".to_string(), "0".to_string()), // the player's own later edit
        ]);
        let mut cvars = fresh_registry();
        cvars.load_file(file);
        assert!(cvars.reapply_saved_presets() > 0);
        assert_eq!(cvars.get("ambientOcclusion"), Some("2"));
        assert_eq!(cvars.get("lampFog"), Some("2"));
        assert_eq!(cvars.get("farclip"), Some("1497"));
        assert_eq!(cvars.get("zoneSkyboxes"), Some("0"), "a saved row stays");
        // The lighting members follow Ultra too.
        assert_eq!(cvars.get("shadowMapSize"), Some("4096"));
        assert_eq!(cvars.get("volumetricFog"), Some("2"));

        // Medium without a saved lighting rung: the Graphics rung's lighting rung applies.
        let mut medium = fresh_registry();
        medium.load_file(BTreeMap::from([(
            "graphicsQuality".to_string(),
            "Medium".to_string(),
        )]));
        medium.reapply_saved_presets();
        assert_eq!(medium.get("ambientOcclusion"), Some("1"));
        assert_eq!(medium.get("farclip"), Some("477"));
        lighting_quality(&mut medium);
        assert_eq!(medium.get("lightingQuality"), Some("Medium"));
        assert_eq!(medium.get("graphicsQuality"), Some("Medium"));

        // Custom writes nothing; neither does a file with no preset name.
        for file in [
            BTreeMap::from([("graphicsQuality".to_string(), LIGHTING_CUSTOM.to_string())]),
            BTreeMap::from([("skyQuality".to_string(), "1".to_string())]),
        ] {
            let mut custom = fresh_registry();
            custom.load_file(file);
            assert_eq!(custom.reapply_saved_presets(), 0);
            assert_eq!(
                custom.get("ambientOcclusion"),
                custom.default_of("ambientOcclusion")
            );
        }

        // A session-owned row is left to the session.
        let mut owned = fresh_registry();
        owned.own_for_session("farclip", Some("500"));
        owned.load_file(BTreeMap::from([(
            "graphicsQuality".to_string(),
            "Ultra".to_string(),
        )]));
        owned.reapply_saved_presets();
        assert_eq!(owned.get("farclip"), Some("500"));
        assert_eq!(owned.get("ambientOcclusion"), Some("2"));
    }

    /// MONKEY (followups): a saved Lighting Quality rung fills the lighting members the file
    /// lacks; members the file carries stay; Custom writes nothing.
    #[test]
    fn a_saved_lighting_preset_fills_members_missing_from_the_file() {
        let mut cvars = fresh_registry();
        cvars.load_file(BTreeMap::from([
            ("lightingQuality".to_string(), "Ultra".to_string()),
            ("volumetricFog".to_string(), "1".to_string()),
        ]));
        assert!(cvars.reapply_saved_presets() > 0);
        assert_eq!(cvars.get("shadowMapSize"), Some("4096"));
        assert_eq!(cvars.get("interiorShadowCasters"), Some("16"));
        assert_eq!(cvars.get("volumetricFog"), Some("1"), "a saved member stays");
        // No Graphics rung saved: the graphics rows keep their defaults.
        assert_eq!(
            cvars.get("ambientOcclusion"),
            cvars.default_of("ambientOcclusion")
        );

        let mut off = fresh_registry();
        off.load_file(BTreeMap::from([(
            "lightingQuality".to_string(),
            "Off".to_string(),
        )]));
        off.reapply_saved_presets();
        assert_eq!(off.get("worldShadows"), Some("0"));
        lighting_quality(&mut off);
        assert_eq!(off.get("lightingQuality"), Some("Off"));

        let mut custom = fresh_registry();
        custom.load_file(BTreeMap::from([(
            "lightingQuality".to_string(),
            LIGHTING_CUSTOM.to_string(),
        )]));
        assert_eq!(custom.reapply_saved_presets(), 0);
    }

    /// The seed never overrides the player: a saved preset stops it outright, a saved row keeps
    /// its value, and a session-owned row (an env lever) is left to the session.
    #[test]
    fn the_graphics_seed_leaves_saved_and_session_rows_alone() {
        let mut saved = fresh_registry();
        saved.load_file(BTreeMap::from([(
            "graphicsQuality".into(),
            "Classic".into(),
        )]));
        assert_eq!(saved.seed_graphics_preset(), 0);
        assert_eq!(saved.get("skyQuality"), saved.default_of("skyQuality"));

        let mut row = fresh_registry();
        row.load_file(BTreeMap::from([("skyQuality".into(), "1".into())]));
        row.own_for_session("farclip", Some("500"));
        row.seed_graphics_preset();
        lighting_quality(&mut row);
        assert_eq!(row.get("skyQuality"), Some("1"));
        assert_eq!(row.get("farclip"), Some("500"));
        assert_eq!(row.get("fogModel"), Some("1"));
        assert_eq!(row.get("graphicsQuality"), Some(LIGHTING_CUSTOM));
    }

    /// Ultra's `farclip` survives the real observer (the extended clamp), and every governed
    /// row's rung values do too.
    #[test]
    fn graphics_rows_pass_their_observers_unclamped() {
        // MONKEY (reviewfix-a): every governed row, every rung — not only `farclip`. Each value
        // is driven through the real setter and read back off the resource its renderer reads.
        let mut app = cvar_app();
        for (col, name) in GRAPHICS_PRESET_NAMES.iter().enumerate() {
            for (k, values) in GRAPHICS_PRESETS {
                if *k == "lightingQuality" {
                    continue; // its members are welded by the lighting-preset census test
                }
                let value = values[col];
                apply(&mut app, k, "-1");
                apply(&mut app, k, value);
                let video = res::<VideoConfig>(&app);
                let flag = |b: bool| if b { 1.0 } else { 0.0 };
                let applied = match *k {
                    "farclip" => res::<ViewDistance>(&app).farclip,
                    "skyQuality" => video.sky_quality as f32,
                    "skyDither" => flag(res::<benilla_world::ffx_glow::SkyDither>(&app).0),
                    "fogModel" => {
                        flag(res::<benilla_world::lighting::FogModelSetting>(&app).modern())
                    }
                    "rainSurfaces" => flag(res::<benilla_world::weather::RainSurfaces>(&app).0),
                    "torchTerrainShadows" => flag(video.torch_terrain_shadows),
                    "daylightWindowSplit" => flag(video.daylight_window_split),
                    "ambientOcclusion" => video.ambient_occlusion as f32,
                    "zoneSkyboxes" => flag(res::<benilla_world::skybox::ZoneSkyboxes>(&app).0),
                    "lampFog" => video.lamp_fog as f32,
                    _ => panic!("add the observer readback for {k}"),
                };
                assert_eq!(applied, value.parse::<f32>().unwrap(), "{name}/{k}");
            }
        }
    }

    /// Every member every rung names is a REGISTERED row — a typo'd member would otherwise make
    /// its whole preset underivable (a `cvars.get` that answers `None` never matches), and the
    /// only symptom would be a dropdown stuck on `Custom`.
    #[test]
    fn every_preset_member_is_registered_and_inside_its_observers_clamp() {
        let mut app = cvar_app();
        for (name, members) in LIGHTING_PRESETS {
            for (k, value) in *members {
                assert!(
                    REGISTERED.iter().any(|r| r.name == *k),
                    "{name}: member {k} is not registered"
                );
                // Drive the real setter: a clamp, integer conversion or map-size snap
                // must leave every preset value unchanged.
                apply(&mut app, k, "-1");
                apply(&mut app, k, value);
                let video = res::<VideoConfig>(&app);
                let applied = match *k {
                    "characterShadows" => video.character_shadows as u32 as f32,
                    "worldShadows" => video.world_shadows as u32 as f32,
                    "interiorLight" => video.interior_light as u32 as f32,
                    "interiorShadows" => video.interior_shadows as u32 as f32,
                    "exteriorShadows" => video.exterior_shadows as u32 as f32,
                    "shadowMapSize" => video.shadow_map_size as f32,
                    "interiorShadowCasters" => video.interior_shadow_casters as f32,
                    "interiorShadowDynamic" => video.interior_shadow_dynamic as f32,
                    "spellLightGain" => video.spell_light_gain,
                    "waterQuality" => video.water_quality as f32,
                    // MONKEY (volumetric fog): verify presets reach the renderer.
                    "volumetricFog" => video.volumetric_fog as f32,
                    // MONKEY (post): the post lane's live tier.
                    "bloom" => video.bloom as f32,
                    "sunShafts" => video.sun_shafts as u32 as f32,
                    "colorGrading" => video.color_grading as u32 as f32,
                    // MONKEY (integration): wind is bridged directly to its world resource rather
                    // than through VideoConfig, but it is still a preset member and needs the same
                    // real-observer clamp weld as every VideoConfig-backed row above.
                    "foliageWind" => res::<benilla_world::wind::FoliageWind>(&app).0 as f32,
                    "lavaLightGain" => video.lava_light_gain,
                    "fireLightGain" => video.fire_light_gain,
                    "moonShadowStrength" => video.moon_shadow_strength,
                    "fireFlicker" => video.fire_flicker,
                    "nightGain" => video.night_gain,
                    "interiorGain" => video.interior_gain,
                    _ => panic!("add the observer readback for {k}"),
                };
                assert_eq!(applied, value.parse::<f32>().unwrap(), "{name}/{k}");
            }
        }
    }

    #[test]
    fn preset_then_member_edit_survives_sync_observers_and_later_flushes() {
        for seeded in [false, true] {
            for flush_preset_first in [false, true] {
                let mut app = cvar_app();
                app.world_mut().non_send_resource_mut::<UiScript>()
                    .register_cvars(registered_pairs());
                // Run the production Update schedule without Startup's on-disk config load.
                if seeded {
                    app.world_mut().run_schedule(Update);
                }
                app.world_mut().non_send_resource_mut::<UiScript>()
                    .run("SetCVar('lightingQuality', 'Low')").unwrap();
                if flush_preset_first {
                    app.world_mut().run_schedule(Update);
                    assert_eq!(res::<VideoConfig>(&app).shadow_map_size, 1024);
                }
                app.world_mut().non_send_resource_mut::<UiScript>()
                    .run("SetCVar('shadowMapSize', '4096')").unwrap();
                for _ in 0..3 {
                    app.world_mut().run_schedule(Update);
                    let cvars = res::<Cvars>(&app);
                    assert_eq!(cvars.get("shadowMapSize"), Some("4096"));
                    assert_eq!(cvars.get("lightingQuality"), Some(LIGHTING_CUSTOM));
                    assert_eq!(res::<VideoConfig>(&app).shadow_map_size, 4096);
                    let script = app.world().non_send_resource::<UiScript>();
                    assert_eq!(script.cvar("shadowMapSize").as_deref(), Some("4096"));
                    assert_eq!(script.cvar("lightingQuality").as_deref(), Some(LIGHTING_CUSTOM));
                }
            }
        }
    }

    #[test]
    fn fire_spell_and_lava_gains_above_two_reach_the_video_observer() {
        let mut app = cvar_app();
        for value in ["3.25", "4"] {
            apply(&mut app, "fireLightGain", value);
            apply(&mut app, "spellLightGain", value);
            apply(&mut app, "lavaLightGain", value);
            let expected = value.parse::<f32>().unwrap();
            assert_eq!(res::<VideoConfig>(&app).fire_light_gain, expected);
            assert_eq!(res::<VideoConfig>(&app).spell_light_gain, expected);
            assert_eq!(res::<VideoConfig>(&app).lava_light_gain, expected);
        }
    }

    #[test]
    fn water_and_lava_observers_clamp_and_water_high_is_opt_in() {
        let mut app = cvar_app();
        for (value, water, lava) in [("-1", 0, 0.0), ("1.75", 1, 1.75), ("2", 2, 2.0), ("9", 2, 4.0)] {
            apply(&mut app, "waterQuality", value);
            apply(&mut app, "lavaLightGain", value);
            assert_eq!(res::<VideoConfig>(&app).water_quality, water);
            assert_eq!(res::<VideoConfig>(&app).lava_light_gain, lava);
        }
        let mut cvars = fresh_registry();
        cvars.set("waterQuality", "2");
        assert_eq!(derive_lighting_quality(&cvars), LIGHTING_CUSTOM);
        apply_lighting_preset(&mut cvars, "High");
        assert_eq!(cvars.get("waterQuality"), Some("1"));
    }

    /// **Every arm, through the registry and its observers.** The old central `apply_to_knobs`
    /// match is gone (2303); each arm lives beside the knob it writes, and this drives the whole
    /// set through a real `App` — a host write, the observers firing synchronously — so a knob
    /// whose observer forgot an arm, or clamps differently from what its row promises, fails here.
    ///
    /// `apply` crosses the latch boundary at once, so the arms are what is under test; the latch
    /// itself has its own test.
    #[test]
    fn the_observers_apply_every_arm() {
        let mut app = cvar_app();
        apply(&mut app, "MusicVolume", "0.7");
        assert_eq!(res::<SoundConfig>(&app).music, 0.7);
        // The second string-valued row (1667): it must reach the knob rather than being rejected
        // by the numeric parse every other row goes through, and a value that is not an address
        // must be consumed (known key) while leaving the knob's truth alone.
        apply(&mut app, "realmList", "logon.example.org:3724");
        assert_eq!(
            res::<crate::realmlist::Realmlist>(&app).address(),
            "logon.example.org:3724"
        );
        apply(
            &mut app,
            "realmlist",
            r#"SET realmlist "elsewhere.example.org""#,
        );
        assert_eq!(
            res::<crate::realmlist::Realmlist>(&app).address(),
            "elsewhere.example.org"
        );
        apply(&mut app, "realmList", "not an address");
        assert_eq!(
            res::<crate::realmlist::Realmlist>(&app).address(),
            "elsewhere.example.org",
            "a known key with a bad value is consumed, and the resource keeps its truth",
        );
        // Clamps are the knob's own: volume to [0,1], farclip to FARCLIP_RANGE.
        apply(&mut app, "mastervolume", "7");
        assert_eq!(res::<SoundConfig>(&app).master, 1.0);
        apply(&mut app, "farclip", "50");
        assert_eq!(res::<ViewDistance>(&app).farclip, *FARCLIP_RANGE.start());
        // `nearclip` clamps to the reference's own callback bounds `[0.01, 0.33]` (`0x688d90`),
        // both ends. pfUI's extended stops write 0.06..0.30, so its whole range passes untouched.
        apply(&mut app, "nearclip", "0.001");
        assert_eq!(
            res::<ViewDistance>(&app).nearclip,
            0.01,
            "[0x8029d0], the callback's low bound"
        );
        apply(&mut app, "nearclip", "9");
        assert_eq!(
            res::<ViewDistance>(&app).nearclip,
            0.33,
            "[0x808300], its high bound"
        );
        apply(&mut app, "nearclip", "0.3");
        assert_eq!(res::<ViewDistance>(&app).nearclip, 0.3);
        // Multisampling clamps to the reference's own [1, 16] and takes an int the way its `atoi`
        // does — the value reaching the camera is a sample COUNT, where 1 is none (1629).
        apply(&mut app, "gxMultisample", "4");
        assert_eq!(res::<MsaaSetting>(&app).samples, 4);
        // The filter policy's two rows: `anisotropic` takes the reference's own [1, 16] clamp,
        // `trilinear` is a flag. Both write the pending value; the process policy is already
        // published by the time either can be typed (1642).
        apply(&mut app, "anisotropic", "99");
        assert_eq!(
            res::<benilla_assets::TexFilterSetting>(&app).aniso,
            *benilla_assets::ANISO_RANGE.end()
        );
        apply(&mut app, "anisotropic", "0");
        assert_eq!(
            res::<benilla_assets::TexFilterSetting>(&app).aniso,
            *benilla_assets::ANISO_RANGE.start()
        );
        // Both directions: the knob starts at what ships (on), so only the flip to 0 proves the
        // arm does anything.
        apply(&mut app, "trilinear", "0");
        assert!(!res::<benilla_assets::TexFilterSetting>(&app).trilinear);
        apply(&mut app, "trilinear", "1");
        assert!(res::<benilla_assets::TexFilterSetting>(&app).trilinear);
        // **The DEVICE's ceiling, not the reference's** (decision 1643). 99 clamps to the
        // reference's 16 and then to the 4 this GPU offers — before 1643 it stopped at 16 and the
        // camera was handed a sample count wgpu refuses, killing the render thread on frame one.
        apply(&mut app, "gxmultisample", "99");
        assert_eq!(res::<MsaaSetting>(&app).samples, 4);
        // The realistic route in: a config written where 8x exists, opened where it does not.
        apply(&mut app, "gxMultisample", "8");
        assert_eq!(
            res::<MsaaSetting>(&app).samples,
            4,
            "a device that stops at 4x must never be handed an 8"
        );
        // A count the device DOES offer is untouched.
        apply(&mut app, "gxmultisample", "2");
        assert_eq!(res::<MsaaSetting>(&app).samples, 2);
        apply(&mut app, "gxmultisample", "0");
        assert_eq!(res::<MsaaSetting>(&app).samples, *MSAA_RANGE.start());
        // Render scale takes a fraction and clamps to its own range at both ends (1639).
        apply(&mut app, "renderScale", "0.75");
        assert_eq!(res::<RenderScale>(&app).0, 0.75);
        apply(&mut app, "renderscale", "9");
        assert_eq!(res::<RenderScale>(&app).0, *RENDER_SCALE_RANGE.end());
        apply(&mut app, "renderscale", "0");
        assert_eq!(res::<RenderScale>(&app).0, *RENDER_SCALE_RANGE.start());
        // The FPS journal switch (2008): a flag, case-insensitive, off as shipped.
        assert!(!res::<crate::perf::FpsJournalSetting>(&app).0);
        apply(&mut app, "fpsJournal", "1");
        assert!(res::<crate::perf::FpsJournalSetting>(&app).0);
        apply(&mut app, "fpsjournal", "0");
        assert!(!res::<crate::perf::FpsJournalSetting>(&app).0);
        // Enable flags: any nonzero is on, zero is off (the client's int-parse + != 0).
        apply(&mut app, "EnableMusic", "0");
        assert!(!res::<SoundConfig>(&app).music_enabled);
        apply(&mut app, "mastersoundeffects", "1");
        assert!(res::<SoundConfig>(&app).enabled);
        // The Controls trio lands on its knobs (case-insensitive like everything else).
        apply(&mut app, "deselectonclick", "0");
        assert!(!res::<ClickConfig>(&app).deselect_on_click);
        apply(&mut app, "MouseInvertPitch", "1");
        assert!(res::<LookConfig>(&app).invert_pitch);
        // The sensitivity multiplier clamps to the 1.12 slider's range at the knob.
        apply(&mut app, "mousespeed", "1.4");
        assert_eq!(res::<LookConfig>(&app).sensitivity, 1.4);
        apply(&mut app, "mousespeed", "9");
        assert_eq!(res::<LookConfig>(&app).sensitivity, 1.5);
        // The following style lands as the ENGINE's enum (0 Never / 1 Smart / 2 Always), and the
        // "3" the reference's own dropdown writes for Never still means Never.
        apply(&mut app, "cameraSmoothStyle", "0");
        assert_eq!(res::<FollowConfig>(&app).style, FollowStyle::Never);
        apply(&mut app, "camerasmoothstyle", "2");
        assert_eq!(res::<FollowConfig>(&app).style, FollowStyle::Always);
        apply(&mut app, "cameraSmoothStyle", "3");
        assert_eq!(res::<FollowConfig>(&app).style, FollowStyle::Never);
        apply(&mut app, "cameraSmoothStyle", "1");
        assert_eq!(res::<FollowConfig>(&app).style, FollowStyle::Smart);
        // Its two siblings land on the same knob — the tracking selector, and the rate, which
        // clamps to 1.12's AUTO_FOLLOW_SPEED slider range.
        apply(&mut app, "cameraSmoothTrackingStyle", "2");
        assert_eq!(
            res::<FollowConfig>(&app).tracking_style,
            FollowStyle::Always
        );
        assert_eq!(
            res::<FollowConfig>(&app).style,
            FollowStyle::Smart,
            "and only that one"
        );
        apply(&mut app, "cameraYawSmoothSpeed", "270");
        assert_eq!(res::<FollowConfig>(&app).yaw_speed, 270.0);
        apply(&mut app, "cameraYawSmoothSpeed", "9000");
        assert_eq!(
            res::<FollowConfig>(&app).yaw_speed,
            *FOLLOW_SPEED_RANGE.end()
        );
        // The max-orbit factor lands as YARDS on the knob (base 15 x factor), clamped to 1..2.
        apply(&mut app, "cameraDistanceMaxFactor", "1");
        assert_eq!(res::<ZoomLimit>(&app).max, 15.0);
        apply(&mut app, "cameradistancemaxfactor", "5");
        assert_eq!(res::<ZoomLimit>(&app).max, 30.0);
        apply(&mut app, "autoLootDefault", "1");
        assert!(res::<LootConfig>(&app).auto_loot);
        apply(&mut app, "showLootSpam", "0");
        assert!(!res::<LootConfig>(&app).show_loot_spam);
        // Guild Member Alert (1589) — the row that ships OFF, so its ON is the interesting write.
        apply(&mut app, "guildMemberNotify", "1");
        assert!(res::<crate::ui_guild::GuildMemberNotify>(&app).0);
        // Block Trades (1764) — the other row that ships OFF; its ON is what refuses a trade.
        apply(&mut app, "BlockTrades", "1");
        assert!(res::<crate::ui_trade::BlockTrades>(&app).0);
        // The name trio lands on its gates (0992).
        apply(&mut app, "UnitNameNPC", "0");
        assert!(!res::<NameConfig>(&app).npc);
        apply(&mut app, "unitnameown", "1");
        assert!(res::<NameConfig>(&app).own);
        // …and the plate pair on the two bits of the bitmask, either casing.
        apply(&mut app, crate::vplates::CVAR_ENEMIES, "0");
        assert!(!res::<VPlateMode>(&app).enemies);
        apply(&mut app, "nameplateshowfriends", "1");
        assert!(res::<VPlateMode>(&app).friends);
        // The bubble pair lands on the spawn gate's own knob (1139).
        apply(&mut app, "ChatBubbles", "0");
        assert!(!res::<BubbleConfig>(&app).all);
        apply(&mut app, "chatbubblesparty", "0");
        assert!(!res::<BubbleConfig>(&app).party);
        // WorldDetail: panel 0/1/2 → density ×1/×2/×3, clamped to the 1.12 slider's range.
        apply(&mut app, "WorldDetail", "0");
        assert_eq!(res::<ClutterConfig>(&app).density, 1.0);
        apply(&mut app, "worlddetail", "7");
        assert_eq!(res::<ClutterConfig>(&app).density, 3.0);
        // frillDensity: the SAME field in the reference's cells-per-chunk (2151), and the two
        // arms' clamps are deliberately different — the stop's `[0, 2]` above, the cells'
        // `[1, 256]` here (callback `0x688de0`). The stops round-trip through both spellings,
        // which is the property that makes them one knob rather than two that agree by habit.
        apply(&mut app, "frillDensity", "48");
        assert_eq!(res::<ClutterConfig>(&app).density, 3.0);
        apply(&mut app, "frilldensity", "16");
        assert_eq!(res::<ClutterConfig>(&app).density, 1.0);
        // Past the top stop is HONOURED, not clamped to it — pfUI's `hdgraphic` drives exactly
        // this, `ConsoleExec("frillDensity " .. (arg+1)*16)` for arg up to 15.
        apply(&mut app, "frillDensity", "256");
        assert_eq!(res::<ClutterConfig>(&app).density, 16.0);
        // …and the reference's own bounds hold at both ends. `0` is NOT clutter-off: the callback
        // pins it to 1, and turning grass off stays the `$WOW_CLUTTER_DENSITY` instrument's.
        apply(&mut app, "frillDensity", "9000");
        assert_eq!(res::<ClutterConfig>(&app).density, 16.0);
        apply(&mut app, "frillDensity", "0");
        assert_eq!(res::<ClutterConfig>(&app).density, 1.0 / 16.0);
        // The row `GetCVar` answers is the same field seen the other way round.
        apply(&mut app, "WorldDetail", "1");
        assert_eq!(res::<ClutterConfig>(&app).density, 2.0);
        // Weather Intensity (2181): the panel's four stops land whole, an off-grid value
        // truncates toward zero the way every int-valued row here does, and both ends clamp.
        for (wrote, want) in [
            ("0", 0u8),
            ("1", 1),
            ("2", 2),
            ("3", 3),
            ("2.9", 2),
            ("9", 3),
            ("-4", 0),
        ] {
            apply(&mut app, "weatherDensity", wrote);
            assert_eq!(
                res::<benilla_world::weather::WeatherState>(&app).weather_density,
                want,
                "weatherDensity {wrote}"
            );
        }
        // Brightness (2182): the CVar's own unit is the ramp exponent, NOT the slider's offset —
        // `SetGamma` does the `1 - v` on the way in, so what arrives here is already `gamma`.
        // Both ends of the stock slider land whole, and the consumer's clamp holds the values the
        // reference accepts without one (`SetGamma(5)` writes -4 there).
        for (wrote, want) in [("1.000000", 1.0), ("0.500000", 0.5), ("1.500000", 1.5)] {
            apply(&mut app, "gamma", wrote);
            assert_eq!(
                res::<crate::ui_gamma::DisplayGamma>(&app).0,
                want,
                "gamma {wrote}"
            );
        }
        apply(&mut app, "gamma", "-4.000000");
        assert_eq!(
            res::<crate::ui_gamma::DisplayGamma>(&app).0,
            *crate::ui_gamma::GAMMA_RANGE.start(),
            "a negative exponent clamps at the consumer, where it cannot blank the screen"
        );
        apply(&mut app, "gamma", "99");
        assert_eq!(
            res::<crate::ui_gamma::DisplayGamma>(&app).0,
            *crate::ui_gamma::GAMMA_RANGE.end()
        );
        assert_eq!(res::<ClutterConfig>(&app).frill_density(), 32.0);
        // Both spellings reach the same field, from a state neither of them holds — and
        // `$WOW_CLUTTER_DENSITY` takes both for the session (`session_values`), so the lever
        // cannot ride into `config.toml` through the other name (2151). Two different values,
        // because a write of the sibling's mirrored value is the no-op it should be.
        for (key, value) in [
            (benilla_ui::script::CVAR_WORLD_DETAIL, "2"),
            (benilla_ui::script::CVAR_FRILL_DENSITY, "16"),
        ] {
            app.world_mut().resource_mut::<ClutterConfig>().density = 0.5;
            assert_eq!(apply(&mut app, key, value), SetOutcome::Changed);
            assert_ne!(
                res::<ClutterConfig>(&app).density,
                0.5,
                "{key}: reached no knob"
            );
        }
        // **The engine verbs' half of the same weld** (2163). `SetWorldDetail`/`GetWorldDetail` live
        // in `benilla-ui`, which cannot see this table, so the two CVar names and the stop table it
        // writes are consts there — and if either name stopped being registered, or the reference's
        // {16, 32, 48} stopped being `frillDensity`'s unit times the stop, the verbs would write
        // into nothing and only this assertion would say so.
        assert!(REGISTERED
            .iter()
            .any(|r| r.name == benilla_ui::script::CVAR_WORLD_DETAIL));
        assert!(REGISTERED
            .iter()
            .any(|r| r.name == benilla_ui::script::CVAR_FRILL_DENSITY));
        // …and the display-gamma pair's (2182), for exactly the same reason: `GetGamma` answers
        // `1 - <this CVar>` and `SetGamma` writes `1 - v` into it, both from a crate that cannot
        // see this table, so an unregistered name would make the getter answer a constant 0 and
        // the setter write into nothing.
        assert!(REGISTERED
            .iter()
            .any(|r| r.name == benilla_ui::script::CVAR_GAMMA));
        // **`RestoreVideoDefaults`' row list, welded the same way** (2177). It lives in
        // `benilla-ui` beside the binding that walks it and cannot see this table, so a rename or
        // a retirement here would turn one of its rows into a silent skip — the verb would restore
        // eleven settings out of twelve and say nothing. This is the only place that can notice.
        for key in benilla_ui::script::VIDEO_DEFAULT_CVARS {
            assert!(
                REGISTERED.iter().any(|r| r.name.eq_ignore_ascii_case(key)),
                "{key}: RestoreVideoDefaults would restore it, and nothing registers it"
            );
        }
        for (n, frill) in benilla_ui::script::WORLD_DETAIL_STOPS.iter().enumerate() {
            assert_eq!(
                *frill,
                benilla_formats::FRILL_DENSITY * (n as u32 + 1),
                "stop {n}: the reference's own 0x804518 entry must be this knob's unit times the stop"
            );
            // Either spelling of the stop lands on the same ground cover — which is what lets the
            // setter write `frillDensity` and the getter read `WorldDetail` without disagreeing.
            apply(&mut app, "WorldDetail", &n.to_string());
            let by_stop = res::<ClutterConfig>(&app).density;
            // Off the stop first — the mirror already wrote this spelling — then the stop's
            // own cells through it.
            apply(&mut app, "frillDensity", "1");
            app.world_mut().resource_mut::<ClutterConfig>().density = 0.5;
            apply(&mut app, "frillDensity", &frill.to_string());
            assert_eq!(
                res::<ClutterConfig>(&app).density,
                by_stop,
                "stop {n}: the two spellings disagree"
            );
            assert_eq!(res::<ClutterConfig>(&app).frill_density(), *frill as f32);
        }
        // Back to the shipped stop, so the rows after this one read the default ground cover.
        apply(&mut app, "WorldDetail", "1");
        assert_eq!(res::<ClutterConfig>(&app).density, 2.0);
        // The zoom pair (1131): each index lands on its own field, clamped like `set_zoom`.
        apply(&mut app, "minimapZoom", "5");
        assert_eq!(res::<MinimapZoom>(&app).outdoor, 5);
        assert_eq!(
            res::<MinimapZoom>(&app).inside,
            3,
            "the two indices are independent"
        );
        apply(&mut app, "minimapinsidezoom", "9");
        assert_eq!(res::<MinimapZoom>(&app).inside, MINIMAP_ZOOM_LEVELS - 1);
        apply(&mut app, "minimapZoom", "-2");
        assert_eq!(res::<MinimapZoom>(&app).outdoor, 0);
        // A bad value on a numeric row is REFUSED by the registry (2303) — the resource keeps
        // its truth and the row keeps its value; an unknown name is reported as such.
        assert_eq!(apply(&mut app, "uiScale", "banana"), SetOutcome::Refused);
        assert_eq!(res::<UiScaleCvar>(&app).0, 0.9);
        assert_eq!(apply(&mut app, "bogus", "1"), SetOutcome::Unknown);
    }

    #[test]
    fn compose_writes_the_diff_and_preserves_what_it_does_not_own() {
        let mut cvars = Cvars::default();
        cvars.own_for_session("uiScale", Some("1.2")); // env-overridden this session
        cvars.load_file(
            [
                ("FutureKnob".to_string(), "3".to_string()), // a newer build's key: preserved
                ("uiScale".to_string(), "0.8".to_string()),  // the file's own, kept as found
                ("farclip".to_string(), "400".to_string()),  // will return to default
            ]
            .into(),
        );
        cvars.set("MusicVolume", "0.7"); // moved: written
        cvars.set("farclip", "350"); // back to default: removed
        let out = cvars.compose();
        assert_eq!(out.get("MusicVolume").map(String::as_str), Some("0.7"));
        assert!(!out.contains_key("MasterVolume"), "at default: absent");
        assert_eq!(out.get("uiScale").map(String::as_str), Some("0.8"));
        assert!(!out.contains_key("farclip"));
        assert_eq!(out.get("FutureKnob").map(String::as_str), Some("3"));
    }

    /// End to end on a real App: a pre-written `config.toml` loads into the knobs at Startup, a
    /// Lua `SetCVar` drains into the knobs and — on the exit flush — lands back in the file as a
    /// diff (the moved value present, the untouched ones absent). This is the whole 0954 slice-1
    /// loop in one place: file → knobs → VM table → Lua write → knobs → file.
    #[test]
    fn a_lua_setcvar_lands_in_config_toml_end_to_end() {
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};
        let _l = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = std::env::temp_dir().join(format!("benilla-cvar-e2e-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let _c = EnvGuard::unset("WOW_CAPTURE");
        let _u = EnvGuard::unset("WOW_UI_SCALE");
        let _f = EnvGuard::unset("WOW_FARCLIP");
        let _d = EnvGuard::unset("WOW_CLUTTER_DENSITY");
        let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());
        crate::local_state::write_atomic(
            &tmp.join("config.toml"),
            "[cvars]\nMusicVolume = \"0.1\"\n",
        )
        .unwrap();

        let mut app = cvar_app();

        // Startup: the file's MusicVolume reaches the knob; Update: the VM table seeds from it.
        app.update();
        assert_eq!(app.world().resource::<SoundConfig>().music, 0.1);
        assert_eq!(
            app.world_mut()
                .non_send_resource_mut::<UiScript>()
                .cvar("MusicVolume")
                .as_deref(),
            Some("0.1")
        );

        // The Lua write (what a settings slider will do) reaches the knob on the next frame…
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run(r#"SetCVar("MusicVolume", 0.75)"#)
            .unwrap();
        app.update();
        assert_eq!(app.world().resource::<SoundConfig>().music, 0.75);

        // …and the exit flush writes the diff: the moved value, nothing at its default.
        app.world_mut().write_message(AppExit::Success);
        app.update();
        let text = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(text.contains("MusicVolume = \"0.75\""), "{text}");
        assert!(!text.contains("MasterVolume"), "defaults stay out:\n{text}");
        let back: LocalConfig = toml::from_str(&text).unwrap();
        // MONKEY (presets): the file named no `graphicsQuality`, so the first boot seeded the
        // High rungs over the governed rows; those are the only other entries, and the label
        // itself, at its default, stays out.
        let player: Vec<&String> = back
            .cvars
            .keys()
            .filter(|k| !GRAPHICS_PRESETS.iter().any(|(g, _)| g.eq_ignore_ascii_case(k)))
            .collect();
        assert_eq!(player.len(), 1, "a diff, not a dump: {text}");
        assert!(!text.contains("graphicsQuality"), "{text}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A client whose CVar host is real: every knob resource an observer writes, every
    /// observer, [`CvarPlugin`] itself, and a VM for the mirror to live in. The end-to-end tests below
    /// each stand a whole client up, and the census is one row per knob — copied per test, adding
    /// a knob meant editing every copy.
    fn cvar_app() -> App {
        let mut app = App::new();
        app.add_plugins(bevy::MinimalPlugins)
            .insert_resource(SoundConfig::default())
            .insert_resource(UiScaleCvar(DEFAULT_UI_SCALE))
            .insert_resource(ViewDistance {
                farclip: 350.0,
                nearclip: benilla_world::view::NEARCLIP_DEFAULT,
            })
            .insert_resource(MsaaSetting { samples: 1 })
            // Literal for the same reason (1642): TexFilterSetting::default() reads
            // $WOW_TRILINEAR / $WOW_ANISO. These are what ships (1645).
            .insert_resource(benilla_assets::TexFilterSetting {
                trilinear: true,
                aniso: 1,
            })
            // The device menu the Video dropdown reads. A real-shaped list, not empty: these
            // tests exercise `GetCurrentMultisampleFormat`'s lookup, which needs rows to find.
            .insert_resource(benilla_world::view::MsaaFormats {
                formats: vec![(32, 32, 1), (32, 32, 2), (32, 32, 4)],
            })
            .init_resource::<LookConfig>()
            .init_resource::<crate::player::camera_dynamics::CameraOptions>()
            .init_resource::<crate::ui_chat::combat::CombatLogRanges>()
            .init_resource::<crate::combat_text::DamageTextGates>()
            .init_resource::<crate::ui_chat::combat::LogPeriodicSpells>()
            .init_resource::<benilla_world::weather::WeatherState>()
            .init_resource::<crate::ui_gamma::DisplayGamma>()
            .init_resource::<ClickConfig>()
            .init_resource::<crate::target::AssistAttack>()
            .init_resource::<LootConfig>()
            .init_resource::<NameConfig>()
            .init_resource::<VPlateMode>()
            .init_resource::<ClutterConfig>()
            .init_resource::<MinimapZoom>()
            .init_resource::<BubbleConfig>()
            .init_resource::<ZoomLimit>()
            .init_resource::<FollowConfig>()
            .init_resource::<VideoConfig>()
            // Literal, not Default: RenderScale::default() reads $WOW_RENDER_SCALE.
            .insert_resource(RenderScale(1.0))
            // Literal for the same reason again (1667): Realmlist::default() reads $WOW_HOST.
            .insert_resource(crate::realmlist::Realmlist::unpinned(
                crate::realmlist::DEFAULT_REALMLIST,
            ))
            .init_resource::<PaneRate>()
            .init_resource::<crate::ui_guild::GuildMemberNotify>()
            .init_resource::<crate::ui_trade::BlockTrades>()
            .init_resource::<crate::spell::AutoSelfCast>()
            .init_resource::<crate::perf::FpsJournalSetting>()
            .init_resource::<crate::text_filter::TextFilterSwitches>()
            .init_resource::<crate::game_tip::GameTipSetting>()
            .add_plugins(CvarPlugin);
        for observer in ALL_OBSERVERS {
            observer(&mut app);
        }
        app.insert_non_send_resource(UiScript::new().unwrap());
        app
    }

    /// Every CVar observer in the crate, as the plugins register them — the census this test
    /// module stands a client up with. A new observer is one line here and one
    /// `add_observer` in its plugin.
    const ALL_OBSERVERS: &[fn(&mut App)] = &[
        |app| {
            app.add_observer(crate::sound::on_cvar);
        },
        |app| {
            app.add_observer(crate::ui_script::on_cvar);
        },
        |app| {
            app.add_observer(crate::video::on_cvar);
        },
        // MONKEY (p0 graphics programme)
        |app| {
            app.init_resource::<benilla_world::ffx_glow::SkyDither>();
            // MONKEY (fog)
            app.init_resource::<benilla_world::lighting::FogModelSetting>();
            // MONKEY (wind)
            app.init_resource::<benilla_world::wind::FoliageWind>();
            app.init_resource::<benilla_world::wind::FoliageWindStrength>(); // MONKEY (fix-wind)
            app.init_resource::<benilla_world::weather::RainSurfaces>(); // MONKEY (wet)
            app.add_observer(crate::monkey_gfx::on_cvar);
        },
        // MONKEY (reviewfix-a): the zone-skybox bridge, so every governed row has its observer.
        |app| {
            app.insert_resource(crate::zone_skybox::ZoneSkyboxOverride(None));
            app.init_resource::<benilla_world::skybox::ZoneSkyboxes>();
            app.add_observer(crate::zone_skybox::on_cvar);
        },
        |app| {
            app.add_observer(crate::player::camera::on_cvar);
        },
        |app| {
            app.add_observer(crate::player::camera_dynamics::on_cvar);
        },
        |app| {
            app.add_observer(crate::target::on_cvar);
        },
        |app| {
            app.add_observer(crate::spell::cast_target::on_cvar);
        },
        |app| {
            app.add_observer(crate::combat_text::on_cvar);
        },
        |app| {
            app.add_observer(crate::ui_chat::combat::on_cvar);
        },
        |app| {
            app.add_observer(crate::ui_loot::on_cvar);
        },
        |app| {
            app.add_observer(crate::nameplates::on_cvar);
        },
        |app| {
            app.add_observer(crate::vplates::on_cvar);
        },
        |app| {
            app.add_observer(crate::game_tip::on_cvar);
        },
        |app| {
            app.add_observer(crate::text_filter::on_cvar);
        },
        |app| {
            app.add_observer(crate::chat_bubble::on_cvar);
        },
        |app| {
            app.add_observer(crate::ui_guild::on_cvar);
        },
        |app| {
            app.add_observer(crate::ui_trade::on_cvar);
        },
        |app| {
            app.add_observer(crate::minimap::on_cvar);
        },
        |app| {
            app.add_observer(crate::portrait::on_cvar);
        },
        |app| {
            app.add_observer(crate::perf::on_cvar);
        },
        |app| {
            app.add_observer(crate::realmlist::on_cvar);
        },
        |app| {
            app.add_observer(crate::ui_gamma::on_cvar);
        },
        |app| {
            app.add_observer(crate::world_backdrop::on_cvar);
        },
    ];

    /// A host write, its latch committed at once, and its observers run before this returns.
    fn apply(app: &mut App, name: &str, value: &str) -> SetOutcome {
        let world = app.world_mut();
        let (outcome, events) = {
            let mut cvars = world.resource_mut::<Cvars>();
            let outcome = cvars.set(name, value);
            cvars.commit_latched();
            (outcome, cvars.take_events())
        };
        for event in events {
            world.trigger(event);
        }
        outcome
    }

    fn res<T: Resource>(app: &App) -> &T {
        app.world().resource::<T>()
    }

    /// **The reported bug, end to end** (decision 1622): "char screen doesn't remember the last
    /// logged in char, the ref does". Two launches over one `benilla-config/`, with the real
    /// [`CvarPlugin`] and the real [`crate::char_select`] systems in between — entering the world
    /// as somebody has to survive the quit and bring the screen back to them.
    ///
    /// The seam this covers and the per-module tests cannot: the screen's host write has to
    /// reach the file through the registry's own dirty/compose path. Before 2303 a knobless CVar
    /// fell through the central arm match to `_ => return false`, reached the VM's table, read
    /// back correctly all session, and was silently dropped at the save — this bug again, one
    /// layer down.
    #[test]
    fn entering_the_world_survives_the_quit_and_comes_back_selected() {
        use crate::char_select::{ClientState, Roster};
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};
        let _l = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = std::env::temp_dir().join(format!("benilla-lastchar-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let _c = EnvGuard::unset("WOW_CAPTURE");
        let _u = EnvGuard::unset("WOW_UI_SCALE");
        let _f = EnvGuard::unset("WOW_FARCLIP");
        let _d = EnvGuard::unset("WOW_CLUTTER_DENSITY");
        let _w = EnvGuard::unset("WOW_CHAR");
        let _s = EnvGuard::unset("WOW_CHARSELECT_PICK");
        let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());
        let roster = || {
            (1..=4)
                .map(|g| crate::char_select::test_character(g, &format!("Char{g}")))
                .collect::<Vec<_>>()
        };

        // ── Launch 1: the roster lands, and the player enters the world as the third row. ────
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut app = cvar_app();
        app.add_plugins(bevy::state::app::StatesPlugin);
        crate::char_select::add_test_systems(&mut app, tx);
        app.update(); // Startup loads the (absent) file; the first Update seeds the VM table
        app.world_mut().write_message(crate::net::CharListMessage {
            characters: roster(),
            realm: None,
        });
        app.update();
        assert_eq!(
            app.world().resource::<Roster>().selected(),
            Some(0),
            "nothing remembered yet, so the first row — the behaviour that was already right",
        );
        app.world_mut().resource_mut::<Roster>().pending_pick = Some(3); // guid 3 = row 2
        app.update();
        app.world_mut().write_message(AppExit::Success);
        app.update();

        let text = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(
            text.contains("lastCharacterIndex = \"2\""),
            "entering the world must reach the file, 0-based like Config.wtf:\n{text}"
        );

        // ── Launch 2: a fresh client over the same folder, and the roster arrives. ───────────
        let (tx, _rx2) = crossbeam_channel::unbounded();
        let mut app = cvar_app();
        app.add_plugins(bevy::state::app::StatesPlugin);
        crate::char_select::add_test_systems(&mut app, tx);
        app.update();
        app.world_mut().write_message(crate::net::CharListMessage {
            characters: roster(),
            realm: None,
        });
        app.update();

        assert_eq!(
            app.world().resource::<Roster>().selected(),
            Some(2),
            "the second launch must stand the SAME character on the stage — the whole report",
        );
        assert_eq!(
            *app.world().resource::<State<ClientState>>().get(),
            ClientState::CharSelect,
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The minimap's zoom rides the same loop, driven from the **engine** rather than a Lua
    /// `SetCVar` (decision 1131): the `+`/`-` buttons call `Minimap:SetZoom`, which writes the live
    /// index and its CVar together — and that has to reach the knob and the file exactly like a
    /// settings row's write does, or the level is forgotten at the next launch.
    #[test]
    fn a_minimap_setzoom_reaches_the_knob_and_the_file() {
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};
        let _l = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = std::env::temp_dir().join(format!("benilla-mmzoom-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let _c = EnvGuard::unset("WOW_CAPTURE");
        let _u = EnvGuard::unset("WOW_UI_SCALE");
        let _f = EnvGuard::unset("WOW_FARCLIP");
        let _d = EnvGuard::unset("WOW_CLUTTER_DENSITY");
        let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());
        // The previous session left the outdoor map zoomed right in.
        crate::local_state::write_atomic(
            &tmp.join("config.toml"),
            "[cvars]\nminimapZoom = \"5\"\n",
        )
        .unwrap();

        let mut app = cvar_app();
        app.update();

        // Startup restored the knob, and the VM's table answers with it — which is what the UI-load
        // seam hands to `set_minimap_zoom` when the widget is born.
        assert_eq!(app.world().resource::<MinimapZoom>().outdoor, 5);
        assert_eq!(app.world().resource::<MinimapZoom>().inside, 3);
        let seed = {
            let z = app.world().resource::<MinimapZoom>();
            (z.outdoor, z.inside)
        };
        {
            let mut script = app.world_mut().non_send_resource_mut::<UiScript>();
            assert_eq!(script.cvar("minimapZoom").as_deref(), Some("5"));
            // The UI-load seam's own order: the widget is born (at its `MinimapState` default),
            // THEN the persisted level is pushed into it. Seeding a widget that does not exist yet
            // is a no-op — which is exactly why that call sits after `load_ingame_ui`.
            script.run(r#"m = CreateFrame("Minimap", "Mini")"#).unwrap();
            script.set_minimap_zoom(seed.0, seed.1);
            assert_eq!(script.eval::<u8>("return m:GetZoom()").unwrap(), 5);
            // The player zooms out two notches with the minimap's own buttons.
            script.run("m:SetZoom(m:GetZoom() - 2)").unwrap();
        }
        app.update();
        assert_eq!(app.world().resource::<MinimapZoom>().outdoor, 3);

        app.world_mut().write_message(AppExit::Success);
        app.update();
        let text = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(
            !text.contains("minimapZoom"),
            "back at the registered default 3, so it leaves the diff entirely:\n{text}"
        );

        // …and one more notch out is a real diff again.
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("m:SetZoom(1)")
            .unwrap();
        app.update();
        app.world_mut().write_message(AppExit::Success);
        app.update();
        let text = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(text.contains("minimapZoom = \"1\""), "{text}");
        assert_eq!(app.world().resource::<MinimapZoom>().outdoor, 1);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_toml_round_trips() {
        let cfg = LocalConfig {
            cvars: [("MusicVolume".to_string(), "0.7".to_string())].into(),
        };
        let text = format!("{HEADER}{}", toml::to_string(&cfg).unwrap());
        let back: LocalConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.cvars, cfg.cvars);
        // The header survives as comments; a hand edit with comments parses too.
        let hand = "# my note\n[cvars]\nFarclip = \"500\"\n";
        let parsed: LocalConfig = toml::from_str(hand).unwrap();
        assert_eq!(parsed.cvars.get("Farclip").map(String::as_str), Some("500"));
    }

    /// **The table's string-valued CVars, named — and each one's default asserted on its own
    /// terms.**
    ///
    /// Pinned as a closed list so a new string CVar has to come here and think about the numeric
    /// test above rather than silently widening it. That is exactly what happened at 1627, when
    /// this test still said "the ONE": `gxResolution` arrived and the list grew by one, on purpose.
    ///
    /// **`realmName` defaults EMPTY** — empty rather than a guess: the value is written from the
    /// session's real realm by `set_realm_name`, so the default only ever describes a client that
    /// has not connected. `0x85d684` holds `"Last realm connected to"` beside the registration, but
    /// that reads like the CVar's HELP text rather than its value, and nothing here needs it
    /// resolved — `""` is what `Ace/AceState.lua:27`'s `ace.trim(GetCVar("realmName"))` handles
    /// cleanly, and inventing a realm name would be worse than admitting we have none yet.
    ///
    /// **`gxResolution` defaults to the pre-1627 window** (decision 1627), and **`realmList` to
    /// `localhost`** (1667). These are the rows the registry takes any string for (a row's
    /// default decides whether it is numeric — [`Cvars::set`]), so each default is asserted
    /// through the parser its observer uses on the live value — a spelling this table accepts
    /// but [`crate::video::parse_resolution`] or [`crate::realmlist::normalize`] rejects would
    /// otherwise ship as a silent fall back.
    ///
    /// The list itself is the load-bearing half: a new string-valued row has to come here and
    /// name its parser, which is what makes adding one impossible to do quietly.
    #[test]
    fn the_string_valued_cvars_are_the_realm_and_the_windowed_size() {
        let mut strings: Vec<&str> = REGISTERED
            .iter()
            .filter(|r| r.default.parse::<f32>().is_err())
            .map(|r| r.name)
            .collect();
        strings.sort_unstable(); // the list is the claim, not where the rows sit in the table
        assert_eq!(
            strings,
            vec![
                "graphicsQuality",
                "gxApi",
                "gxResolution",
                "lightingQuality",
                "realmList",
                "realmName"
            ]
        );
        let default_of = |name: &str| {
            REGISTERED
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.default)
                .expect("registered")
        };
        assert_eq!(default_of("realmName"), "");
        // **`gxApi` defaults EMPTY on the same argument** (2151): the value is the render
        // adapter's backend, written by `sync_cvars` on every launch, so the default only ever
        // describes a client with no renderer. Naming one — `"direct3d"` least of all, which is
        // the reference's and is a backend wgpu does not have — would be a claim about a machine
        // we have not looked at.
        assert_eq!(default_of("gxApi"), "");
        assert_eq!(
            crate::video::parse_resolution(default_of("gxResolution")),
            Some(crate::video::DEFAULT_WINDOWED)
        );
        // Same posture for the third row (1667): a default this table accepts but
        // `realmlist::normalize` rejects would ship as a client that silently cannot dial.
        assert_eq!(
            crate::realmlist::normalize(default_of(crate::realmlist::CVAR_REALMLIST)).as_deref(),
            Some(crate::realmlist::DEFAULT_REALMLIST),
        );
    }

    /// A registry with the file's entries and the session's env values, no disk: the pure
    /// half of the load, for the tests below.
    fn registry(file: &[(&str, &str)]) -> Cvars {
        let mut cvars = Cvars::default();
        cvars.load_file(
            file.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        cvars
    }

    /// **The latch** (decision 2303): a write to a row the reference registers with flag bit1
    /// is staged, the applied value stands, nothing fires and nothing dirties — the reference's
    /// `Set 0x63df50` storing to `latchedValue` with `InternalSet` skipped — until the boundary
    /// commits it (`CVar::Update 0x63e060`), at which point it fires, persists and reaches the
    /// mirror. Staging the applied value back clears the stage; a stage never committed is not
    /// what the file gets.
    #[test]
    fn a_latched_row_stages_the_write_until_the_boundary_commits_it() {
        let mut cvars = Cvars::default();
        assert!(
            cvars.row("gxVSync").unwrap().latched,
            "the reference's flags=3 row"
        );
        assert_eq!(cvars.set("gxVSync", "0"), SetOutcome::Staged);
        assert_eq!(cvars.get("gxVSync"), Some("1"), "applied value stands");
        assert_eq!(cvars.row("gxVSync").unwrap().pending.as_deref(), Some("0"));
        assert!(!cvars.has_events(), "nothing fires before the boundary");
        assert!(!cvars.dirty, "nothing to save before the boundary");
        assert_eq!(
            cvars.set("gxVSync", "0"),
            SetOutcome::Unchanged,
            "same stage"
        );
        assert!(
            !cvars.compose().contains_key("gxVSync"),
            "a stage that is never committed never reaches the file"
        );
        // The boundary.
        assert_eq!(cvars.commit_latched(), 1);
        assert_eq!(cvars.get("gxVSync"), Some("0"));
        assert_eq!(cvars.row("gxVSync").unwrap().pending, None);
        assert_eq!(
            cvars.take_events(),
            vec![CvarChanged {
                name: "gxVSync".into(),
                old: "1".into(),
                new: "0".into()
            }]
        );
        assert!(cvars.dirty);
        assert_eq!(
            cvars.compose().get("gxVSync").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            cvars.take_outbox(),
            vec![("gxVSync".to_string(), "0".to_string())]
        );
        // Staging the applied value back over a stage clears the stage, and the next boundary
        // has nothing to do.
        cvars.set("gxVSync", "1");
        assert_eq!(cvars.set("gxVSync", "0"), SetOutcome::Unchanged);
        assert_eq!(cvars.row("gxVSync").unwrap().pending, None);
        assert_eq!(cvars.commit_latched(), 0);
        // A latched row outside the gx set is not the video restart's to commit (2343).
        assert_eq!(cvars.set("SoundBufferSize", "200"), SetOutcome::Staged);
        assert_eq!(cvars.commit_latched(), 0);
        assert_eq!(cvars.get("SoundBufferSize"), Some("100"));
        assert_eq!(
            cvars.row("SoundBufferSize").unwrap().pending.as_deref(),
            Some("200")
        );
        assert!(!cvars.compose().contains_key("SoundBufferSize"));
        // An unlatched row applies at once, fires, and dirties.
        assert_eq!(cvars.set("MusicVolume", "0.7"), SetOutcome::Changed);
        assert_eq!(cvars.get("MusicVolume"), Some("0.7"));
        assert_eq!(cvars.take_events().len(), 1);
    }

    /// **A numeric row refuses what does not parse** — the applied value stands, the mirror
    /// that already stored the garbage is corrected through the outbox, and nothing fires. The
    /// old central parse consumed the value and let it into the file; the registry does not.
    /// A string row takes any string, and its observer is the one that judges it.
    #[test]
    fn a_numeric_row_refuses_what_does_not_parse_and_corrects_the_mirror() {
        let mut cvars = Cvars::default();
        assert_eq!(cvars.set_from_vm("uiScale", "banana"), SetOutcome::Refused);
        assert_eq!(cvars.get("uiScale"), Some("0.9"));
        assert!(!cvars.has_events());
        assert!(!cvars.dirty);
        assert_eq!(
            cvars.take_outbox(),
            vec![("uiScale".to_string(), "0.9".to_string())],
            "the VM stored 'banana' synchronously; the host writes the truth back"
        );
        assert_eq!(cvars.set("uiScale", "banana"), SetOutcome::Refused);
        assert!(
            cvars.take_outbox().is_empty(),
            "a host write has no mirror to correct"
        );
        assert_eq!(
            cvars.set("realmList", "not an address"),
            SetOutcome::Changed
        );
        assert_eq!(cvars.set("nosuchrow", "1"), SetOutcome::Unknown);
    }

    /// **An addon-declared row persists like the client's own** (decisions 1195, 1291, 2303):
    /// the VM reports the registration, the registry gives it a row starting at the file's
    /// value when the file carries one, a write to it dirties the config, and the save writes it
    /// as a diff against the addon's default. Before the registry the write reached the VM's
    /// table and nothing dirtied, so an addon-only change was saved only if something else
    /// happened to move.
    #[test]
    fn an_addon_row_persists_like_the_clients_own() {
        let mut cvars = registry(&[("myAddonKnob", "3")]);
        assert_eq!(
            cvars.orphans(),
            vec![("myAddonKnob".to_string(), "3".to_string())],
            "unclaimed until the addon declares it — the VM's saved base"
        );
        cvars.learn_addon_row("myAddonKnob", "1");
        assert_eq!(
            cvars.get("myAddonKnob"),
            Some("3"),
            "starts at the saved value"
        );
        assert_eq!(cvars.default_of("myAddonKnob"), Some("1"));
        assert!(cvars.orphans().is_empty());
        cvars.learn_addon_row("myAddonKnob", "9");
        assert_eq!(
            cvars.default_of("myAddonKnob"),
            Some("1"),
            "a re-declaration is a no-op"
        );
        assert!(!cvars.dirty);
        assert_eq!(cvars.set_from_vm("myAddonKnob", "5"), SetOutcome::Changed);
        assert!(cvars.dirty, "an addon-only change is a change");
        assert_eq!(
            cvars.compose().get("myAddonKnob").map(String::as_str),
            Some("5")
        );
        cvars.set_from_vm("myAddonKnob", "1");
        assert!(
            !cvars.compose().contains_key("myAddonKnob"),
            "back at the addon's default, it leaves the diff"
        );
        // A newer build's key stays an orphan and rides through the save verbatim.
        let cvars = registry(&[("FutureKnob", "3")]);
        assert_eq!(
            cvars.compose().get("FutureKnob").map(String::as_str),
            Some("3")
        );
    }

    /// **A session-owned row answers the env and never reaches the file**: the value the knob
    /// is running at is what `GetCVar` answers, the file's own entry for the key is neither
    /// applied nor rewritten, and a player's write to it during the session still works — it
    /// just stays out of the diff.
    #[test]
    fn a_session_owned_row_answers_the_env_and_never_reaches_the_file() {
        let mut cvars = Cvars::default();
        cvars.own_for_session("uiScale", Some("1.2"));
        cvars.load_file(BTreeMap::from([("uiScale".to_string(), "0.8".to_string())]));
        assert_eq!(
            cvars.get("uiScale"),
            Some("1.2"),
            "the env's, not the file's"
        );
        assert!(
            !cvars.has_events(),
            "the knob already read the env — nothing to apply"
        );
        assert_eq!(cvars.set("uiScale", "1.4"), SetOutcome::Changed);
        assert_eq!(
            cvars.compose().get("uiScale").map(String::as_str),
            Some("0.8"),
            "the file keeps what it said"
        );
        // A lever whose resource is absent still marks the row, so the file cannot apply over
        // the env — the registered default stands as the truth.
        cvars.own_for_session("farclip", None);
        cvars.load_file(BTreeMap::from([("farclip".to_string(), "500".to_string())]));
        assert_eq!(cvars.get("farclip"), Some("350"));
        assert!(cvars.is_session_owned("FARCLIP"));
    }

    /// **The loaded file reaches a knob synchronously, before anything ordered after
    /// [`CvarLoad`]** — the observers fire inside the exclusive load, which is what lets the
    /// camera read `gxMultisample` at spawn (1629) and every other `Startup` reader of a knob
    /// see the player's value rather than the default.
    #[test]
    fn the_loaded_file_reaches_a_knob_before_anything_ordered_after_cvar_load() {
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};
        let _l = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = std::env::temp_dir().join(format!("benilla-cvar-load-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let _c = EnvGuard::unset("WOW_CAPTURE");
        let _u = EnvGuard::unset("WOW_UI_SCALE");
        let _f = EnvGuard::unset("WOW_FARCLIP");
        let _d = EnvGuard::unset("WOW_CLUTTER_DENSITY");
        let _m = EnvGuard::unset("WOW_MSAA");
        let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());
        crate::local_state::write_atomic(
            &tmp.join("config.toml"),
            "[cvars]\nMusicVolume = \"0.1\"\ngxMultisample = \"4\"\n",
        )
        .unwrap();
        #[derive(Resource, Default)]
        struct SeenAtStartup(Option<(f32, u32)>);
        fn after_load(
            sound: Res<SoundConfig>,
            msaa: Res<MsaaSetting>,
            mut seen: ResMut<SeenAtStartup>,
        ) {
            seen.0 = Some((sound.music, msaa.samples));
        }
        let mut app = cvar_app();
        app.init_resource::<SeenAtStartup>()
            .add_systems(Startup, after_load.after(CvarLoad));
        app.update();
        assert_eq!(
            app.world().resource::<SeenAtStartup>().0,
            Some((0.1, 4)),
            "both the plain row and the latched one are applied by the time CvarLoad is over"
        );
        assert_eq!(
            app.world().resource::<Cvars>().get("gxMultisample"),
            Some("4"),
            "a file value is applied, not staged: the reference's LoadFile runs before Register"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// MONKEY (integration): deterministic graphics A/B captures may read one explicit fixture,
    /// but the local-state law still exposes no writable capture path.
    #[test]
    fn a_capture_can_read_an_explicit_cvar_fixture_without_enabling_writes() {
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};
        let _l = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = std::env::temp_dir().join(format!(
            "benilla-capture-cvar-fixture-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&tmp).ok();
        crate::local_state::write_atomic(
            &tmp.join("config.toml"),
            "[cvars]\nbloom = \"0\"\nsunShafts = \"0\"\ncolorGrading = \"0\"\n",
        )
        .unwrap();
        let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());
        // MONKEY (reviewfix-a): the fixture is named by its own variable, not BENILLA_HOME.
        let _x = EnvGuard::set("WOW_CAPTURE_CVARS", tmp.join("config.toml").to_str().unwrap());
        let _c = EnvGuard::set("WOW_CAPTURE", "post-lava-searing");

        assert_eq!(boot_cvar("bloom").as_deref(), Some("0"));
        assert_eq!(boot_cvar("sunShafts").as_deref(), Some("0"));
        assert_eq!(boot_cvar("colorGrading").as_deref(), Some("0"));
        assert_eq!(
            crate::local_state::config_path(),
            None,
            "capture fixtures are read-only; persistence remains hermetic"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// **Every registered row has a reader** — the honest-tree rule (module doc, 1140), made
    /// structural now that the registry has no central arm to answer `false` from. A row's
    /// reader is a string literal naming it somewhere in the crate's code or `benilla-ui`'s (an
    /// observer's arm in the lowercased spelling, a `cvars.get("…")`, a `const` the row is
    /// registered through), or the row is named in [`LUA_ONLY`] with the stock file that reads
    /// it. Test files are not readers.
    #[test]
    fn every_registered_row_has_a_reader_in_the_source() {
        /// Rows whose only reader is the stock interface, by decision.
        const LUA_ONLY: &[(&str, &str)] = &[
            (
                "statusBarText",
                "TextStatusBar.lua reads it on CVAR_UPDATE (1140)",
            ),
            (
                "UberTooltips",
                "GameTooltip's binding-line gate, stock and pfUI (1316)",
            ),
            ("gxApi", "pfUI's system tooltip names the backend (2151)"),
            (
                "graphicsQuality",
                "MONKEY (presets): the Advanced Graphics page's Graphics Preset row; the host \
                 writes and derives it here in cvars.rs",
            ),
            (
                "useUiScale",
                "UIOptionsFrame.lua and OptionsFrame.lua branch on it to gate the uiScale slider",
            ),
        ];
        let app_src = crate::test_support::src_dir();
        let ui_src = app_src
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/")
            .join("benilla-ui")
            .join("src");
        let mut code = String::new();
        for file in crate::test_support::rust_files(&app_src)
            .into_iter()
            .chain(crate::test_support::rust_files(&ui_src))
        {
            let rel = file.to_string_lossy().replace('\\', "/");
            if rel.ends_with("/cvars.rs") && rel.contains("benilla-app") || rel.contains("tests") {
                continue;
            }
            let text = std::fs::read_to_string(&file).expect("source is readable");
            for line in text.lines() {
                if !line.trim_start().starts_with("//") {
                    code.push_str(line);
                    code.push('\n');
                }
            }
        }
        let mut orphans = Vec::new();
        for row in REGISTERED {
            if LUA_ONLY.iter().any(|(n, _)| *n == row.name) {
                continue;
            }
            let exact = format!("\"{}\"", row.name);
            let lower = format!("\"{}\"", row.name.to_ascii_lowercase());
            if !(code.contains(&exact) || code.contains(&lower)) {
                orphans.push(row.name);
            }
        }
        assert!(
            orphans.is_empty(),
            "registered rows nothing in the source reads (an observer arm, a `cvars.get`, or a \
             `LUA_ONLY` entry with its stock reader): {orphans:?}"
        );
        for (name, _) in LUA_ONLY {
            assert!(
                REGISTERED.iter().any(|r| r.name == *name),
                "{name}: named Lua-only but not registered"
            );
        }
    }

    /// The eight rows the reference latches that benilla registers, and no other — read off their
    /// register sites (`flags` 2 or 3): the sound-init row, and the `gx*` block `RestartGx`
    /// commits. `trilinear`/`anisotropic`/`farclip` register with `flags = 1` and apply live, so
    /// they are deliberately not here.
    #[test]
    fn the_latched_rows_are_the_references_own() {
        let mut latched: Vec<&str> = REGISTERED
            .iter()
            .filter(|r| r.latched)
            .map(|r| r.name)
            .collect();
        latched.sort_unstable();
        assert_eq!(
            latched,
            vec![
                "SoundBufferSize",
                "gxApi",
                "gxColorBits",
                "gxDepthBits",
                "gxMultisample",
                "gxResolution",
                "gxVSync",
                "gxWindow",
            ]
        );
    }

    // ─── MONKEY (reviewfix-a) ────────────────────────────────────────────────────────────────

    /// The value the reference boots a row at, as its [`Reference`] column records it; `None`
    /// for benilla's own rows (nothing to match).
    fn reference_boot_value(row: &Registered) -> Option<&'static str> {
        match &row.reference {
            Reference::Same(v) => Some(v),
            // The reference's own boot code lands where our default does.
            Reference::Overridden { .. } => Some(row.default),
            Reference::Deviates { value, .. } => Some(value),
            Reference::Ours(_) => None,
        }
    }

    /// **The seeded High column against every reference row** (review #6): a governed row a new
    /// player boots off the reference's value must be declared in [`SEEDED_DEVIATIONS`] with
    /// that reference value and a reason, and a declared row must really deviate.
    #[test]
    fn seeded_column_deviates_only_where_declared() {
        let col = graphics_column(GRAPHICS_DEFAULT).unwrap();
        for (k, values) in GRAPHICS_PRESETS {
            if k.eq_ignore_ascii_case("lightingQuality") {
                continue; // High on that ladder IS the registered defaults (its own test)
            }
            let row = REGISTERED.iter().find(|r| r.name == *k).expect("governed row registered");
            let declared = SEEDED_DEVIATIONS.iter().find(|(n, _, _)| n == k);
            match reference_boot_value(row) {
                Some(reference) if !same_value(values[col], reference) => {
                    let (_, value, why) = declared.unwrap_or_else(|| {
                        panic!(
                            "{k}: the seeded {GRAPHICS_DEFAULT} value {} leaves the reference's \
                             {reference}; declare it in SEEDED_DEVIATIONS",
                            values[col]
                        )
                    });
                    assert!(same_value(value, reference), "{k}: declared reference is stale");
                    assert!(!why.trim().is_empty(), "{k}: a deviation owes a reason");
                }
                _ => assert!(
                    declared.is_none(),
                    "{k}: declared in SEEDED_DEVIATIONS but the seed does not deviate"
                ),
            }
        }
        for (n, _, _) in SEEDED_DEVIATIONS {
            assert!(
                GRAPHICS_PRESETS.iter().any(|(k, _)| k == n),
                "{n}: declared but not governed"
            );
        }
    }

    /// Review #14: an env lever owns a governed row for the session; the label must not derive
    /// (and so persist) `Custom` because of it.
    #[test]
    fn a_session_owned_row_does_not_derive_custom() {
        let mut cvars = fresh_registry();
        apply_graphics_preset(&mut cvars, "High");
        lighting_quality(&mut cvars);
        assert_eq!(derive_graphics_quality(&cvars), "High");
        cvars.own_for_session("farclip", Some("500"));
        assert_eq!(cvars.get("farclip"), Some("500"));
        assert_eq!(derive_graphics_quality(&cvars), "High");
        // A lighting member owned by the session is skipped the same way.
        cvars.own_for_session("waterQuality", Some("0"));
        assert_eq!(derive_lighting_quality(&cvars), "High");
    }

    /// Review #15: help strings are one line — a lost `\` continuation leaves a literal `\n` or a
    /// run of indentation spaces in the `/console` text.
    #[test]
    fn help_strings_have_no_broken_continuations() {
        for row in REGISTERED {
            let why = match &row.reference {
                Reference::Ours(why)
                | Reference::Deviates { why, .. }
                | Reference::Overridden { why, .. } => *why,
                Reference::Same(_) => continue,
            };
            assert!(
                !why.contains('\n') && !why.contains("\\n") && !why.contains("   "),
                "{}: broken line continuation in {why:?}",
                row.name
            );
        }
    }

    /// Review #13: a capture reads CVars only from the dedicated `WOW_CAPTURE_CVARS` fixture,
    /// never from a `BENILLA_HOME` the shell happens to carry.
    #[test]
    fn a_capture_reads_cvars_only_from_its_fixture() {
        use crate::local_state::test_env::{EnvGuard, ENV_LOCK};
        let _l = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _c = EnvGuard::set("WOW_CAPTURE", "overlook-noon");
        let _h = EnvGuard::set("BENILLA_HOME", "player-home");
        let _x = EnvGuard::unset("WOW_CAPTURE_CVARS");
        assert_eq!(config_read_path(), None);
        let _x = EnvGuard::set("WOW_CAPTURE_CVARS", "fixture/config.toml");
        assert_eq!(
            config_read_path(),
            Some(std::path::PathBuf::from("fixture/config.toml"))
        );
    }
}
