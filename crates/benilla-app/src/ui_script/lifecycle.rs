//! The UI session's lifecycle, all edges and nothing per-frame: `Startup` installs a boot VM
//! (strings, emote tokens, fonts); world entry builds a fresh VM and loads the in-game UI and
//! every addon onto it; leaving the world runs the reference's shutdown tail and installs a boot
//! VM again; `ReloadUI()` is the two edges back to back without leaving the world.

use bevy::prelude::*;

use benilla_assets::LockRecover;
use benilla_ui::script::UiScript;

use super::manifest::silenced_ui_load;
use super::{
    load_ingame_ui, CursorPayloadHeld, PlayerUiHover, UiClock,
    UiKeyboardCapture,
};
use crate::ui_script::addons;
use crate::ui_script::addons::Addon;

pub(crate) fn setup_script(world: &mut World) {
    install_boot_vm(world);
}

/// Build and install a boot VM: strings, emote tokens and the font-object registry, no frames.
/// Installed at `Startup`, at every [`end_ui_session`] and at every world entry, so no login
/// shares a session id (and so a [`super::VmMemo`]) with the character screen before it. The
/// registry is here because the glyph atlas bakes from it on the first `Update`; the reference
/// re-loads `Fonts.xml` per rebuild too, as a font object dies with its session (the native
/// `CSimpleFont` with the frame-script owner `0x7839c0`, torn down at `0x490c97`).
fn install_boot_vm(world: &mut World) {
    let Some(script) = new_vm(world) else {
        return;
    };
    load_global_strings(world, &script);
    load_emote_tokens(world, &script);
    world.insert_non_send_resource(script);
}

/// A fresh VM on the process clock with the addon asset probes wired, or `None` (the world's VM
/// removed) when the VM cannot start.
fn new_vm(world: &mut World) -> Option<UiScript> {
    let mut script = match UiScript::new() {
        Ok(s) => s,
        Err(e) => {
            error!("ui_script: VM init failed: {e}");
            world.remove_non_send_resource::<UiScript>();
            return None;
        }
    };
    seed_vm_clock(world, &mut script);
    install_addon_asset_resolvers(world, &mut script);
    info!("ui_script: new VM, session {}", script.session());
    Some(script)
}

/// Start the new VM's `GetTime()` where the process already is and re-anchor [`UiClock`] to it,
/// both off the `Time<Real>` it anchors on. The reference's `GetTime` is `GetTickCount` × 0.001
/// (`0x515ea0`, via `0x42c010` → `0x42b790`), an OS clock that never restarts; ours lives in the
/// VM, rebuilt at every login and `ReloadUI`, and a clock back at zero puts every converted
/// cooldown start in the past, which stock `CooldownFrame_SetTimer` hides (`Cooldown.lua:3`).
fn seed_vm_clock(world: &mut World, script: &mut UiScript) {
    let (anchor, elapsed) = world
        .get_resource::<Time<bevy::time::Real>>()
        .map_or_else(Default::default, |t| {
            (t.last_update(), t.elapsed_secs_f64())
        });
    script.set_now(elapsed);
    if let Some(mut clock) = world.get_resource_mut::<UiClock>() {
        *clock = UiClock {
            anchor: anchor.unwrap_or_else(bevy::platform::time::Instant::now),
            ui_now: elapsed,
        };
    }
}

/// Wire `Interface\AddOns\` art and fonts: the decoder's loose-file root and the VM's texture,
/// size and font probes, so the path forms of `SetTexture` and `SetFont` answer the reference's
/// 1|nil load verdict, and a region with no authored size on an axis takes it from its art as the
/// client's virtual size getters do. Each probe resolves [`addons::root`] through the store its
/// renderer reads, so the answer is what draws; they test existence, not decode.
fn install_addon_asset_resolvers(world: &mut World, script: &mut UiScript) {
    let root = addons::root();
    let Some(mut assets) = world.get_resource_mut::<benilla_assets::WorldAssets>() else {
        return;
    };
    assets.set_loose_addon_root(root.clone());
    let chain = assets.chain.clone();
    let size_chain = chain.clone();
    let size_root = root.clone();
    let font_chain = chain.clone();
    let font_root = root.clone();
    script.set_texture_probe(Box::new(move |path| {
        benilla_assets::sprite_candidates(path).iter().any(|c| {
            chain.lock_recover().contains(c)
                || root
                    .as_deref()
                    .is_some_and(|r| benilla_assets::loose_addon_file(r, c).is_some())
        })
    }));
    // Keyed by the path exactly as the region carries it, so a hit allocates nothing (this is
    // asked per zero-size textured region per layout pass); misses are cached too.
    let sizes: std::cell::RefCell<std::collections::HashMap<String, Option<(u32, u32)>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    script.set_texture_size_probe(Box::new(move |path| {
        if let Some(cached) = sizes.borrow().get(path) {
            return *cached;
        }
        let measured = benilla_assets::sprite_dimensions(&size_chain, size_root.as_deref(), path);
        sizes.borrow_mut().insert(path.to_string(), measured);
        measured
    }));
    // Existence over the same two stores, in the same order, as `ui_text::engine`'s face loader,
    // memoised because a path's answer cannot change and an addon may call `SetFont` every frame.
    let seen: std::cell::RefCell<std::collections::HashMap<String, bool>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    script.set_font_probe(Box::new(move |path| {
        if let Some(&cached) = seen.borrow().get(path) {
            return cached;
        }
        let found = font_chain.lock_recover().contains(path)
            || font_root.as_deref().is_some_and(|r| {
                benilla_assets::loose_addon_file(r, &benilla_assets::normalize_path(path)).is_some()
            });
        seen.borrow_mut().insert(path.to_string(), found);
        found
    }));
}

/// Whether this run wants the player UI: always outside a capture, and in one only when opted in
/// ([`crate::run_mode::capture_ui_opted_in`]), since capture baselines test the world render.
fn ui_wanted(world: &World) -> bool {
    !world.contains_resource::<crate::run_mode::CaptureMode>()
        || crate::run_mode::capture_ui_opted_in()
}

/// The world-entry UI load, armed at `OnEnter(InWorld)` and run by [`run_pending_entry_load`]
/// once the loading cover is actually ON THE GLASS.
///
/// This is 0962's frame accounting, second offender: `OnEnter(InWorld)` fires on exactly the
/// frame whose render would first present the cover (the raise frame still renders the glue
/// above it), so a synchronous burst there holds the *previous* present — the frozen character
/// screen — for its whole duration. 0962 taught the menagerie to wait for
/// `WARM_COVER_PRESENT_FRAMES`; 1051 then moved the FrameXML load onto the same unprotected
/// frame, and by 1290+addons the burst had grown to ~0.5 s (measured live: raise 48.317 →
/// reference sourcing + 55 files + addons + saved vars + `PLAYER_LOGIN` → 48.83) — the
/// director's "frozen char for 1 sec" at every Enter World.
///
/// The wait itself is [`crate::loading_screen::EntryCover`]'s — one count of covered+in-world
/// frames for the whole client (upstream folded the warm pass's, this latch's and the world
/// camera's identical counters into it). So the latch here marks *is the entry UI still owed?*
/// and, since the slicing, *where the load is*; the timing question is answered in one place.
/// The loading screen folds this resource's existence into its clear condition, so a reveal can
/// never precede the UI it is supposed to reveal.
///
/// **Since the world-entry slicing (2026-08-31) the load is a resumable state machine, not one
/// burst.** Measured on the page: the whole edge — reference sourcing, ~55 FrameXML/Lua files
/// through the wasm Lua VM, positions, addons, saved variables — was ONE main-thread task of
/// 3.0–3.4 s warm, and on a cold WAN cache (every UI sprite the first draw resolves is a
/// synchronous XHR) **206 s**: Chrome's "page unresponsive" dialog, every Enter World. Slicing
/// spends the same work over frames — [`ENTRY_LOAD_FRAME_BUDGET`] of it per frame, at least one
/// step each — so the cover keeps presenting, its bar keeps moving ([`Self::fraction`]) and the
/// tab stays responsive. The latch's other contracts are unchanged: it exists from
/// `OnEnter(InWorld)` until the tree is built ([`ingame_ui_pending`]), and the loading screen
/// clears only after it is gone.
#[derive(Resource, Default)]
pub(crate) struct PendingEntryUiLoad {
    stage: EntryStage,
    /// Steps done / total, for the loading bar. `total` is known after the first step.
    done: usize,
    total: usize,
    started: Option<bevy::platform::time::Instant>,
    frames: u32,
    covering: bool,
    identity: Option<(String, String)>,
    /// The realm's character list, read once at `Armed` and handed to `load_third_party` two
    /// stages later: upstream's `EnableStore` resolves an addon this character never had an
    /// opinion about from what the realm's others said (decision 2311), so the sliced path has
    /// to carry the node set across the frames it is sliced over.
    roster: Vec<String>,
    version_check: bool,
    /// The chain core and its file list, and benilla's layer and its own, both resolved once at
    /// the first step: upstream's `load_core`/`load_layer` each walk their list in one burst, and
    /// this path walks the same lists a file at a time.
    core: Option<(super::addons::Addon, Vec<String>)>,
    layer: Option<(super::addons::Addon, Vec<String>)>,
    /// The core walk's accumulating record, closed under the toc banner at [`EntryStage::Layer`].
    toc: benilla_ui::status::Status,
}

impl PendingEntryUiLoad {
    /// 0..1 of the load's steps done — the loading screen folds it into its bar.
    pub(crate) fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }
}

/// Where the sliced entry load is — see [`PendingEntryUiLoad`]. The order is
/// [`load_ingame_ui`]'s, one step per stage transition: what used to be one call is now the
/// same calls with frame boundaries allowed between them.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum EntryStage {
    /// Waiting for the cover, or about to take the first step.
    #[default]
    Armed,
    /// The chain's `FrameXML.toc`, one file per step; the last step closes the banner and runs
    /// `Bindings.xml`.
    Core { next: usize },
    /// benilla's layer ([`super::manifest::LAYER_MANIFEST`]), one file per step, after the whole
    /// core — upstream's own order.
    Layer { next: usize },
    /// `UIParent_ManageFramePositions()`.
    Positions,
    /// Every third-party addon (one step: the walk is dependency-ordered and fires
    /// `ADDON_LOADED` per addon — kept atomic; the page has no addon folder, and a native
    /// player's addon burst is the reference's own behaviour).
    ThirdParty,
    /// Saved variables, `VARIABLES_LOADED`, the failure count, the identity record.
    Finish,
}

/// How much of one frame the entry load may spend before yielding to the next. At least one
/// step always runs, so a single slow file still makes progress; the budget only decides
/// whether a *second* step starts. Small on purpose: the frames it yields to are the loading
/// cover's — cheap to render — and every one of them is a frame the browser can answer.
const ENTRY_LOAD_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(8);

/// The boot VM, parked between the world-entry edge and the deferred load, out of every feed's
/// reach: each feed takes the VM as an `Option` and returns on `None`.
pub(crate) struct ParkedBootVm(UiScript);

/// `OnEnter(InWorld)`: arm the deferred entry load and park the boot VM.
pub(crate) fn arm_entry_ui_load(world: &mut World) {
    world.insert_resource(PendingEntryUiLoad::default());
    if let Some(vm) = world.remove_non_send_resource::<UiScript>() {
        world.insert_non_send_resource(ParkedBootVm(vm));
    }
}

/// Retire the glue VM and build the one the entry load runs on: the reference re-makes its Lua
/// state inside `UI_Init` (`0x48fe97`, in `0x48fbf0`). The glue VM's CVars fold first, since the
/// character screen can write one (the AddOns panel's out-of-date toggle) and this is the last
/// moment its table exists. Everything else is re-seeded onto the new VM or is a
/// [`super::VmMemo`], which is meant to reset here.
fn mint_entry_vm(world: &mut World) {
    crate::cvars::fold_dying_vm_cvars(world);
    if let Some(script) = new_vm(world) {
        world.insert_non_send_resource(script);
    }
}

/// Put the parked boot VM back into the world, if one is parked.
fn unpark_boot_vm(world: &mut World) {
    if let Some(ParkedBootVm(vm)) = world.remove_non_send_resource::<ParkedBootVm>() {
        world.insert_non_send_resource(vm);
    }
}

/// Run condition for every in-world feed: the in-game UI is up on the VM in the world, since a
/// feed that fires or drains into a VM with no frames loses what it sends. The reference likewise
/// processes no world state before its UI is up: world entry (`0x420d63`) runs after the same
/// iteration's inbound drain (`0x420d55`, its container `0x420c00` entered once per process),
/// socket threads only enqueue (`0x537b7c`; `SMSG_PONG` at `0x537b56` the one bypass), and the
/// login request waits for the UI (`0x46c272`).
///
/// Two terms, because the latch trails the wire by a frame: `apply_net_updates` drains `Connected`
/// and the login burst behind it in one pass, and the `InWorld` transition that arms the latch and
/// parks the VM runs at the next frame's `StateTransition`; the `InWorld` term covers that frame.
/// **`/console uiLua 0`**: the player interface, off.
///
/// Journal 69 put the frame at 21.7 ms with animation already out of the way, and the biggest
/// thing left is the bridge between the game and the Lua UI: **101 `feed_*` and 89 `drain_*`
/// systems**, every one of them every frame. One of them measured 0.00 ms on its own, so the cost
/// is not a slow system - it is a hundred and ninety of them, and bevy builds each one's params
/// before its body can decide to leave.
///
/// Every one of those systems is gated on [`ingame_ui_up`], so refusing here refuses the lot in a
/// single place, without touching 190 registrations.
static UI_LUA: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Set by the CVar.
/// Whether the player interface is switched on; see [`set_ui_lua`].
pub(crate) fn ui_lua_on() -> bool {
    UI_LUA.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn set_ui_lua(on: bool) {
    UI_LUA.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn ingame_ui_up(
    pending: Option<Res<PendingEntryUiLoad>>,
    state: Option<Res<State<crate::char_select::ClientState>>>,
) -> bool {
    UI_LUA.load(std::sync::atomic::Ordering::Relaxed)
        && pending.is_none()
        && state.is_some_and(|s| *s.get() == crate::char_select::ClientState::InWorld)
}

/// `PreUpdate`, after [`run_pending_reload`] so a reload never interleaves an armed entry load:
/// run the armed entry load once the cover has presented, at once when there is no cover (a
/// capture booting straight `InWorld`). Leaving the world first drops the latch unrun, and
/// [`end_ui_session`] then skips the shutdown writes, so a UI-less VM never overwrites the saved
/// files.
pub(crate) fn run_pending_entry_load(world: &mut World) {
    if world.get_resource::<PendingEntryUiLoad>().is_none() {
        return;
    }
    let in_world = *world
        .resource::<State<crate::char_select::ClientState>>()
        .get()
        == crate::char_select::ClientState::InWorld;
    if !in_world {
        // Left the world before the load ran: nothing to build a UI for.
        world.remove_resource::<PendingEntryUiLoad>();
        unpark_boot_vm(world);
        return;
    }
    let covering = world
        .get_resource::<crate::loading_screen::LoadingScreen>()
        .is_some_and(|s| s.covering());
    // The wait is [`crate::loading_screen::EntryCover`]'s, not a second count of the same frames
    // (upstream's fold; `presented()` is already true with no cover up, which is the "nothing
    // watching the previous present, load now" arm). Only the first step waits: once the sliced
    // load is under way the cover is on the glass by construction.
    if world.resource::<PendingEntryUiLoad>().stage == EntryStage::Armed
        && !world
            .get_resource::<crate::loading_screen::EntryCover>()
            .is_none_or(|c| c.presented())
    {
        return;
    }
    // One frame's slice: steps until the budget is spent (at least one), then yield. The VM comes
    // out of the park the arm put it in (1978) for the duration of the slice and goes straight
    // back at the end of it, so no feed ever sees the half-built interface — 1978's window is the
    // whole load, which for a sliced load is many frames rather than the old single burst.
    unpark_boot_vm(world);
    if !ui_wanted(world) {
        world.remove_resource::<PendingEntryUiLoad>();
        return;
    }
    if matches!(
        world.resource::<PendingEntryUiLoad>().stage,
        EntryStage::Armed
    ) {
        mint_entry_vm(world);
    }
    let Some(mut script) = world.remove_non_send_resource::<UiScript>() else {
        warn!("ui_script: entering the world with no VM — the in-game UI will not load");
        world.remove_resource::<PendingEntryUiLoad>();
        return;
    };
    let frame_start = bevy::platform::time::Instant::now();
    world.resource_mut::<PendingEntryUiLoad>().frames += 1;
    loop {
        let stage = world.resource::<PendingEntryUiLoad>().stage;
        match stage {
            EntryStage::Armed => {
                let (identity, roster, version_check) = entry_prepare(world, &mut script);
                // **The whole load edge is silent** — the TOC walk, every addon, the saved
                // variables and `PLAYER_LOGIN`, the span `0x48fbf0` brackets itself across
                // (`0x48fbfa` → `0x49016d`). Upstream writes it as `silenced_ui_load`, a closure
                // around the load, and the one-shot path below still does. This one cannot: the
                // load is SLICED across frames (the browser's frozen-tab carry), so no closure
                // can span it. Same two calls in the same order, opened here and closed in
                // `entry_finish` — which only this path calls.
                script.push_sound_suppression();
                let core = super::manifest::open_core(&script);
                let layer = super::manifest::layer_enabled().then(super::manifest::open_layer);
                let core_steps = core.as_ref().map_or(0, |(_, f)| f.len());
                let layer_steps = layer.as_ref().map_or(0, |(_, f)| f.len());
                let mut pending = world.resource_mut::<PendingEntryUiLoad>();
                pending.identity = identity;
                pending.roster = roster;
                pending.version_check = version_check;
                pending.covering = covering;
                pending.started = Some(frame_start);
                // Every core file, then every layer file, then positions + addons + finish.
                pending.total = core_steps + layer_steps + 3;
                pending.done = 0;
                pending.core = core;
                pending.layer = layer;
                pending.toc = Default::default();
                pending.stage = EntryStage::Core { next: 0 };
            }
            EntryStage::Core { next } => {
                let mut pending = world.resource_mut::<PendingEntryUiLoad>();
                let more = match &pending.core {
                    Some((_, files)) => next < files.len(),
                    None => false,
                };
                if more {
                    // Split so the record can be written while the core is borrowed to read from.
                    let PendingEntryUiLoad { core, toc, .. } = &mut *pending;
                    let (core, files) = core.as_ref().expect("checked just above");
                    // Errors are logged by the loader itself and retained for `/errors`; the
                    // one-shot path discards them too.
                    let _ = super::manifest::load_core_file(&script, core, &files[next], toc);
                    pending.done += 1;
                    pending.stage = EntryStage::Core { next: next + 1 };
                } else {
                    // The banner over the whole walk, then `Bindings.xml` — `load_core`'s own
                    // tail, taken once the last file is in. With no core at all there is no walk
                    // to put a banner over (`open_core` has already reported the miss), and
                    // `load_core`'s miss arm goes straight on to the bindings; so does this.
                    let had_core = pending.core.is_some();
                    let toc = std::mem::take(&mut pending.toc);
                    drop(pending);
                    let _ = if had_core {
                        super::manifest::close_core(&script, toc)
                    } else {
                        super::manifest::load_core_bindings(&script)
                    };
                    world.resource_mut::<PendingEntryUiLoad>().stage = EntryStage::Layer { next: 0 };
                    continue; // free transition
                }
            }
            EntryStage::Layer { next } => {
                let pending = world.resource::<PendingEntryUiLoad>();
                let file = pending
                    .layer
                    .as_ref()
                    .and_then(|(_, files)| files.get(next).cloned());
                match file {
                    Some(file) => {
                        let layer = pending.layer.as_ref().map(|(a, _)| a).expect("just read");
                        let _ = super::manifest::load_layer_file(&script, layer, &file);
                        let mut pending = world.resource_mut::<PendingEntryUiLoad>();
                        pending.done += 1;
                        pending.stage = EntryStage::Layer { next: next + 1 };
                    }
                    None => {
                        world.resource_mut::<PendingEntryUiLoad>().stage = EntryStage::Positions;
                        continue; // free transition
                    }
                }
            }
            EntryStage::Positions => {
                let _ = super::manifest::bootstrap_positions(&script);
                let mut pending = world.resource_mut::<PendingEntryUiLoad>();
                pending.done += 1;
                pending.stage = EntryStage::ThirdParty;
            }
            EntryStage::ThirdParty => {
                let (identity, roster, version_check) = {
                    let p = world.resource::<PendingEntryUiLoad>();
                    (p.identity.clone(), p.roster.clone(), p.version_check)
                };
                let _ =
                    addons::load_third_party(&mut script, identity.as_ref(), &roster, version_check);
                let mut pending = world.resource_mut::<PendingEntryUiLoad>();
                pending.done += 1;
                pending.stage = EntryStage::Finish;
            }
            EntryStage::Finish => {
                let pending = world
                    .remove_resource::<PendingEntryUiLoad>()
                    .unwrap_or_default();
                entry_finish(world, &mut script, pending.identity.clone());
                // Read before the move, not through the world afterwards: upstream takes it off
                // the world because its load is one burst, and ours has the VM in hand here.
                let session = script.session();
                world.insert_non_send_resource(script);
                // The standing instrument for this edge: the one number that says whether the
                // cover is still hiding it — over how many frames it was spread, and, since
                // upstream's 2226, WHICH VM it came up on. The session is the load-bearing half
                // of that record: every `VmMemo` keys on it, so a login whose session did not
                // move from the character screen's is one where the whole one-shot class is
                // already spent — and the next report of a missing login line can be answered
                // from the log the run already wrote.
                info!(
                    "ui_script: in-game UI up in {:.0} ms over {} frame(s) (behind the cover:                      {}, vm session: {session})",
                    pending
                        .started
                        .map_or(0.0, |s| s.elapsed().as_secs_f32() * 1000.0),
                    pending.frames,
                    pending.covering
                );
                return;
            }
        }
        if frame_start.elapsed() >= ENTRY_LOAD_FRAME_BUDGET {
            break; // resume next frame
        }
    }
    // Back to the park, not to the live slot: the load is not done, and 1978's window is open
    // until it is. `EntryStage::Finish` is the one arm that hands the VM back to the world.
    world.insert_non_send_resource(ParkedBootVm(script));
}

/// The entry load's opening: the identity the addons see, the realm and player the file scopes
/// read, the four seeds of decision 2241, the raster seam, and the instruction bound (the
/// reference FrameXML is a manifest entry now).
///
/// **It is the SLICED path's opening only.** It used to be shared with the one-shot
/// [`load_ingame_ui_on_world_entry`], which is why the *why* of most lines still lives down
/// there — upstream's 2226/2241 rewrite inlined its own copy, so the two now run the same steps
/// from two places. Anything added to one belongs in the other: a step left on upstream's path
/// alone simply does not happen in this fork, and the keybinding table is what proved it.
fn entry_prepare(
    world: &mut World,
    script: &mut UiScript,
) -> (Option<(String, String)>, Vec<String>, bool) {
    // The character whose AddOn enable state applies. Resolved before the load because the
    // enable file gates which addons run at all.
    let identity = world
        .get_resource::<crate::char_select::Roster>()
        .and_then(crate::ui_macro::identity);
    // The realm's character list is the enable store's node set: the reference keeps one
    // `ADDONSTATELIST` node per character, and an addon this one never set resolves from the rest.
    let roster: Vec<String> = world
        .get_resource::<crate::char_select::Roster>()
        .map(|r| r.chars.iter().map(|c| c.name.clone()).collect())
        .unwrap_or_default();
    // The addon array before any addon file runs: `SMSG_ADDON_INFO` lands during the handshake, so
    // the reference has it before `UI_Init`'s addon walk and `GetNumAddOns()` at file scope sees
    // it. A server that never answered leaves it empty for the session, as in the reference.
    if let Some(reply) = world
        .get_resource::<crate::net::AddonInfoReply>()
        .and_then(|r| r.0.clone())
    {
        script.note_addon_info_reply(&reply);
    }
    // The CVar table goes in before the UI loads: stock `UIOptionsFrame.xml`'s camera dropdowns
    // read `cameraSmoothStyle` and `cameraSmoothTrackingStyle` in their `OnLoad`, where a nil is a
    // concat error (`UIOptionsFrame.lua:379-384`, `:501-506`), and this VM was born inside this
    // call, before `cvars::sync_cvars`'s per-VM seed. Its order: the file's unclaimed entries
    // first, so an addon's `RegisterCVar` starts at the player's value, then the live table.
    match world.get_resource::<crate::cvars::Cvars>() {
        Some(cvars) => {
            script.set_cvar_saved_base(cvars.orphans());
            script.seed_cvars(cvars.vm_seed());
        }
        None => script.register_cvars(crate::cvars::registered_pairs()),
    }
    // The realm before the load, since addons read `GetRealmName()` at file scope; the roster
    // carries the realm-list entry this session connected to.
    let realm = world
        .get_resource::<crate::char_select::Roster>()
        .and_then(|r| r.realm.as_ref().map(|r| r.name.clone()))
        .unwrap_or_default();
    script.set_realm_name(&realm);
    // `CGGameUI::InitializeGame` flags these read-only before `UI_Init` loads anything
    // (`0x48f566`-`0x48f584`), so an in-game `SetCVar` of one raises, logout handlers included;
    // `ShutdownGame` clears them (`0x491240`), and here the flags die with this VM. The host's
    // own realm writes are engine writes, which the flag does not stop.
    for name in benilla_ui::script::IN_WORLD_READ_ONLY_CVARS {
        script.set_cvar_read_only(name, true);
    }
    // The player too, before the addon walk. The record first: the reference's
    // `UnitName`/`UnitRace`/`UnitClass`/`UnitSex` read only a copy of the char-enum row made at the
    // Enter World commit (`0x5abd9e`) and never cleared, and each rebuilt VM is told once. The
    // snapshot stands in for the descriptor until that streams in.
    if let Some(record) = world
        .get_resource::<crate::char_select::Roster>()
        .and_then(record_from_roster)
    {
        script.set_player_record(record);
    }
    if let Some(seat) = world
        .get_resource::<crate::char_select::Roster>()
        .and_then(seat_from_roster)
    {
        script.set_unit("player", Some(seat));
    }
    // The addon version gate, read from the persisted base the fold above just made current, so a
    // character-screen "Load out of date AddOns" click counts. A bare test world is check on.
    let version_check = world
        .get_resource::<crate::cvars::Cvars>()
        .is_none_or(crate::cvars::Cvars::addon_version_check);
    // **…and the rest of the same class** (decision 2241). Each of these was a per-VM claim in
    // `Update`, which answers *which* VM and not *when inside its life* — and every one of them
    // backs a Lua getter the load burst below reads:
    //
    // - the **zone-channel catalog**, whose empty state is not "no zone yet" to its three verbs
    //   but *"no such built-in channel"* — the leg that files `General` as a custom channel in the
    //   chat cache and puts a real `CMSG_JOIN_CHANNEL` on the wire;
    // - the **keybinding table**, without which stock `ActionButton_OnLoad` paints a blank hotkey
    //   corner and an addon's `SetBinding` on a stock command is a silent nil;
    // - the **default language**, which `ChatFrame_OnEvent`'s `PLAYER_ENTERING_WORLD` arm turns
    //   into the `[Language]` prefix gate for every chat line of the session.
    crate::ui_chat::seed_zone_channel_catalog(world, script);
    crate::bindings::seed_bindings_for_vm(world, script);
    crate::ui_unit::seed_default_language(world, script);
    crate::ui_unit::seed_language_table(world, script);
    // **The world map's catalog, before the first addon file runs** (decision 2240). The continent
    // and zone lists behind `GetMapContinents`/`GetMapZones` are static DBC data, and the corpus
    // reads them at file scope: Astrolabe — the positioning library under Questie and Cartographer
    // — builds its entire continent → zone table inside `AceLibrary:Register`'s synchronous
    // `activate`. Pushed from an `Update` system it landed ~210 ms after this whole call returned,
    // so that table was built from two empty lists and every icon placement afterwards indexed a
    // nil zone. Same shape as the four seeds above, and the reference has no timing here at all.
    crate::ui_world_map::seed_world_map_catalog(world, script);
    // **The raster seam — the screen size and the font engine — before the first `<OnLoad>` runs**
    // (decisions 2242 and 2028). The screen size is 2242's: a fresh VM starts at 1024×768 and the
    // feed that corrects it is an `Update` system, so every OnLoad that *computes* from
    // `GetScreenWidth()`/`GetScreenHeight()` baked the wrong number for the session — the stock
    // world map's full-screen blackout quad among them.
    //
    // The font engine is 2028's. Every file the
    // walk below loads may measure the text it just set — the era's own tab law is
    // `label:GetStringWidth() + 40` at OnLoad, and the addon corpus writes the same pair — and a
    // `GetStringWidth` with no measurer installed answers 0. Seated only from the per-frame pass
    // (`extract::tick_script`, an `Update` system), a VM that is BORN and LOADED inside one
    // exclusive `PreUpdate` slot never sees it: that is exactly `ReloadUI()`, which mints a fresh
    // boot VM in `end_ui_session` and calls straight into here, so every `/reload` measured 0
    // through its whole load edge and only converged a frame later off whatever poll the caller
    // had written to survive it.
    seat_raster_seam_for_load(world, script);
    script.set_instruction_budget(addons::LOAD_INSTRUCTION_BUDGET);
    (identity, roster, version_check)
}

/// The entry load's closing — everything [`load_ingame_ui_on_world_entry`] does after the
/// files: minimap zoom seed, saved variables + `VARIABLES_LOADED`, the failure count, the budget
/// disarm, the identity record. Same sharing arrangement as [`entry_prepare`].
fn entry_finish(world: &mut World, script: &mut UiScript, identity: Option<(String, String)>) {
    let zoom = {
        let z = world.resource::<crate::minimap::MinimapZoom>();
        (z.outdoor, z.inside)
    };
    script.set_minimap_zoom(zoom.0, zoom.1);
    // The same edge as the one-shot path's, with the same two seats (decisions 2125 and 2132):
    // the host settings between the saved-variables chunk and `VARIABLES_LOADED`, the chat-cache
    // restore between `VARIABLES_LOADED` and `PLAYER_LOGIN`. Its comment there has the why.
    // A plate setting `config.toml` still holds, carried into FrameXML's saved variables once.
    // Only by a load with the layer, which registers `FRIENDNAMEPLATES_ON` for save; a stock-UI
    // load leaves it for the next layered one.
    let legacy_plates = if super::manifest::layer_enabled() {
        world
            .get_resource_mut::<crate::cvars::Cvars>()
            .map(|mut cvars| crate::vplates::take_legacy_settings(&mut cvars))
            .unwrap_or_default()
    } else {
        Default::default()
    };
    finish_ui_load_with(
        script,
        |script| crate::vplates::seat_legacy_settings(script, &legacy_plates),
        |script| {
            crate::ui_chat::restore_chat_looks(world, script);
        },
    );
    let failed = script
        .diagnostics()
        .iter()
        .filter(|d| d.kind == benilla_ui::script::diagnostics::DiagnosticKind::Load)
        .count();
    if failed > 0 {
        if let Some(mut chat) = world.get_resource_mut::<crate::ui_chat::ChatLog>() {
            chat.push_event(crate::ui_chat::ChatEvent::text_only(
                crate::ui_chat::ChatEventKind::System,
                format!(
                    "{failed} addon load {} — type /errors to see {}.",
                    if failed == 1 { "failure" } else { "failures" },
                    if failed == 1 { "it" } else { "them" }
                ),
            ));
        }
    }
    script.clear_instruction_budget();
    // The far end of the bracket `entry_prepare` opened — see its comment.
    script.pop_sound_suppression();
    world.insert_resource(AddOnIdentity(identity));
}

/// **The raster seam, at the load edge**: the VM's screen size and its font engine, both under the
/// CURRENT window and `uiScale` — the load-edge half of [`super::extract::tick_script`]'s own
/// per-frame pair.
///
/// **The screen size is the half that had been missing, and it is a real screen bug** (decision
/// 2242). A fresh `Model` starts at **1024×768** and `set_screen_size` is called from
/// `tick_script`, an `Update` system — so between 2226 (the entry mints its own VM) and this, every
/// `<OnLoad>` in FrameXML and every addon's file scope read `GetScreenWidth()/GetScreenHeight()` as
/// 1024×768 no matter what window the player had. Verified live with a probe addon: `SCREENPROBE
/// file-scope … 1024x768`, on both logins of a round trip.
///
/// Almost everything survives that, because almost everything is *anchored* and the first `Update`
/// resize re-solves it. What does not survive is a number a file **computed once** from those
/// getters — and the stock world map is exactly that: `WorldMapFrame_OnLoad` sizes `BlackoutWorld`,
/// the full-screen quad that hides the world behind the map, with `GetScreenWidth()`/
/// `GetScreenHeight()` and never touches it again. Sized 1024×768 UI units on a wider window, it
/// leaves the world showing along the right and bottom edges — the director's report.
///
/// No atlas means no measure to be had (it bakes on the first `Update`, from the patch chain and
/// the window's real `scale_factor`); the per-frame pass seats one on the first frame it appears,
/// exactly as before. The screen size goes in either way, and only when the window has a real size
/// — a bare test world has none, and `0` would be worse than the default it already carries.
fn seat_raster_seam_for_load(world: &mut World, script: &mut UiScript) {
    let ui_scale = world
        .get_resource::<super::UiScaleCvar>()
        .map_or(1.0, |c| c.0);
    let (w, h) = {
        let mut q = world.query_filtered::<&Window, With<bevy::window::PrimaryWindow>>();
        q.single(world)
            .map_or((0.0, 0.0), |win| (win.width(), win.height()))
    };
    let s = super::seam_scale(h, ui_scale);
    if w > 0.0 && h > 0.0 {
        // `tick_script`'s own line, with its own units: the VM lives in 768-tall virtual space
        // (decision 0582), so the window's logical size divided by the seam scale IS the screen
        // the getters answer. No `UIParent_ManageFramePositions` beside it, unlike there: nothing
        // is laid out yet, and the first tick will see no resize to re-run it for.
        script.set_screen_size(w / s, h / s);
    }
    let Some(atlas) = world.get_resource::<crate::ui_text::UiFontAtlas>() else {
        return;
    };
    super::extract::seat_text_measurer(script, atlas, s);
}

/// Materialize the in-game UI for **this** session.
///
/// **Once per world entry, not once per process** (decision 1290). The reference builds the whole
/// in-game UI at `CGGameUI::Initialize 0x48fbf0` and destroys it again at `0x490bd0` on the way
/// out, so every login runs every addon's file scope afresh — and that file scope is where the
/// corpus reads the character it is looking at (`local currentPlayer = UnitName("player")`, the
/// idiom [`seat_from_roster`] already documents). While this was latched per process, a second
/// login kept the first login's captured name: the director's "the selected one is always
/// Onewarrior no matter what char I log into", and, worse, its saved variables went to the first
/// character's file. [`world_entry_tests`] holds both ends.
///
/// **The entry BUILDS its VM; it does not adopt the glue phase's** (decision 2226). That is the
/// reference's own shape — the next Lua state is born *inside* `UI_Init 0x48fbf0`, at `0x48fe97`,
/// before the bindings and the FrameXML walk — and it is what [`benilla_ui::script::UiScript::session`]'s contract has
/// claimed all along ("destroys its Lua state at logout and builds another at the next world
/// entry"). We were half-keeping it: [`install_boot_vm`] ran at `end_ui_session`, so a *second*
/// login was a genuine second load, but the boot VM that `Startup` (or that same logout) left
/// behind then carried its session **across the entry edge** — the glue phase and the world that
/// followed it were one VM with one session id.
///
/// That shared id is what made the login one-shot class (1348, B376) so hard to close. A
/// [`super::VmMemo`] keys on the session, so an edge spent against the FRAMELESS glue VM — any
/// feed that reached it in the window between the wire turning in-world and this load — was spent
/// for the whole login: the event went to a VM with no frames to hear it, and the memo said
/// "already told" forever after. Gating the feeds (1978's park, B376's `ingame_ui_up`) closes
/// that window one feed at a time and silently misses the next one written. A new session closes
/// it for **every** memo at once, without any feed having to know this problem exists: whatever
/// was spent against the glue VM is re-spent here, against a VM that has an interface.
///
/// Safe on the state edge only because 1038 moved the initial transition after `PostStartup` — a
/// capture boots straight into `InWorld`, so before that this would have run ahead of
/// [`benilla_assets::AssetSet::Open`] and loaded against no patch chain.
pub(crate) fn load_ingame_ui_on_world_entry(world: &mut World) {
    // The VM the entry edge parked (1978); the live slot is the fallback for a caller that did
    // not go through the arm. It goes back in the world because the fold below reads it there.
    unpark_boot_vm(world);
    if !ui_wanted(world) {
        return;
    }
    mint_entry_vm(world);
    let Some(mut script) = world.remove_non_send_resource::<UiScript>() else {
        warn!("ui_script: entering the world with no VM — the in-game UI will not load");
        return;
    };
    let (identity, roster, version_check) = entry_prepare(world, &mut script);
    // The minimap zoom pair and the plate pair are read out of the world FIRST: the bracket's
    // body borrows `world` for the chat-cache restore, and two `Copy` pairs cost nothing next to
    // fighting that borrow. The seat itself happens inside, beside the load it has to precede.
    let zoom = {
        let z = world.resource::<crate::minimap::MinimapZoom>();
        (z.outdoor, z.inside)
    };
    // The saved-variables chunk runs HERE — after the XML assigned its file-scope defaults, before
    // any consumer reads them — then `VARIABLES_LOADED`. That is the reference's own load order
    // (`AddOn_Load 0x51f240` steps 2 → 4 → 6, decision 1128); reversing it means the defaults
    // always win and nothing can ever be remembered.
    //
    // **And the chat cache restores inside it, between `VARIABLES_LOADED` and `PLAYER_LOGIN`** —
    // the reference's own slot for the reader's `UPDATE_CHAT_WINDOWS` + `UPDATE_CHAT_COLOR` burst
    // (`0x4900d6`, after `0x4900b2` and before `0x490959`; decisions 2119 and 2125). It is the
    // sole firer of `UPDATE_CHAT_WINDOWS`, which is the only thing that registers a chat frame for
    // any `CHAT_MSG_*` (ref `ChatFrame.lua` l.1261-1273) — as an `Update` system it landed after
    // the session's first chat had already been routed, and the login MOTD went to a window
    // registered for nothing.
    // The plate pair is read out of the world FIRST: the second closure borrows `world` for the
    // chat-cache restore, and a `Copy` of two bools costs nothing next to fighting that borrow.
    // Absent in a bare test world, where "both off" is also the resource's own default.
    // A plate setting `config.toml` still holds, carried into FrameXML's saved variables once.
    // Only by a load with the layer, which registers `FRIENDNAMEPLATES_ON` for save; a stock-UI
    // load leaves it for the next layered one.
    let legacy_plates = if super::manifest::layer_enabled() {
        world
            .get_resource_mut::<crate::cvars::Cvars>()
            .map(|mut cvars| crate::vplates::take_legacy_settings(&mut cvars))
            .unwrap_or_default()
    } else {
        Default::default()
    };
    // No sound during the load edge, as in the reference's `UI_Init` (`0x48fbfa` → `0x49016d`).
    silenced_ui_load(&mut script, |script| {
        let _ = load_ingame_ui(script, identity.as_ref(), &roster, version_check);
        crate::ui_chat::commands::register_dev_commands(script, crate::run_mode::dev_affordances());
        // The new Minimap's zoom indices from the persisted CVars, as the reference's minimap reset
        // copies each CVar into its live index; `Minimap:SetZoom` owns them from here.
        script.set_minimap_zoom(zoom.0, zoom.1);
        // The saved-variables chunk runs after the files set their defaults and before
        // `VARIABLES_LOADED`, the reference's order (`AddOn_Load` `0x51f240`); reversed, the
        // defaults always win. The chat cache restores after `VARIABLES_LOADED` and before any chat
        // is routed: its `UPDATE_CHAT_WINDOWS` registers each chat frame for its `CHAT_MSG_*`
        // events (`ChatFrame.lua:1261-1273`).
        finish_ui_load_with(
            script,
            // `NAMEPLATES_ON`/`FRIENDNAMEPLATES_ON` before `VARIABLES_LOADED`, whose
            // `UIParent_OnEvent` arm runs the first `UpdateNameplates()` (`UIParent.lua:231-234`).
            |script| crate::vplates::seat_legacy_settings(script, &legacy_plates),
            |script| {
                crate::ui_chat::restore_chat_looks(world, script);
            },
        );
    });
    // Deviation: failed addons are reported in chat, because otherwise an addon that does not
    // load says nothing on screen. Counted from the deduplicated diagnostics log as `/errors`
    // lists them, after the whole load edge (handlers included); the VM is fresh, so every `Load`
    // row is this load's.
    let failed = script
        .diagnostics()
        .iter()
        .filter(|d| d.kind == benilla_ui::script::diagnostics::DiagnosticKind::Load)
        .count();
    if failed > 0 {
        if let Some(mut chat) = world.get_resource_mut::<crate::ui_chat::ChatLog>() {
            // Queued: the chat feed drains `ChatLog` once the UI is up.
            chat.push_event(crate::ui_chat::ChatEvent::text_only(
                crate::ui_chat::ChatEventKind::System,
                format!(
                    "{failed} addon load {} — type /errors to see {}.",
                    if failed == 1 { "failure" } else { "failures" },
                    if failed == 1 { "it" } else { "them" }
                ),
            ));
        }
    }
    // `DAMAGE_TEXT_FONT` binds at the end of the load, after every addon's `ADDON_LOADED` (where
    // MikScrollingBattleText and pfUI assign it). The reference reads it eagerly, once per world
    // entry (`0x6c8470`, called at `0x401620` in `0x401570`, after the UI load at `0x401602`),
    // and never again: `[0xce8820]` has one writer, and a `/reloadui` does not re-run it, where
    // this edge re-reads it on every reload.
    world.insert_resource(crate::combat_text::read_damage_text_font(&script));
    // The load is over: disarm its instruction bound, so no handler is killed for being slow.
    script.clear_instruction_budget();
    world.insert_non_send_resource(script);
    world.insert_resource(AddOnIdentity(identity));
    // Arm the world latch, the reference's `0x490168 call 0x4908c0`: a reload creates no entity
    // to re-arm it, so without this a second `/reload` would not fire `PLAYER_LEAVING_WORLD`. The
    // reference skips this leg on a fresh login (`0x490166 je`, the GUID still 0) and arms from
    // the player's create instead; both reach the same armed byte.
    if let Some(mut armed) = world.get_resource_mut::<LeavingWorldArmed>() {
        armed.arm();
    }
}

/// The wire's gender byte (0 male, 1 female) on `UnitSex`'s 2/3 scale, as `ui_unit::snapshot`
/// maps the descriptor's; the record and the seat share it.
fn roster_sex(gender: u8) -> u8 {
    match gender {
        0 => 2,
        1 => 3,
        _ => 0,
    }
}

/// The local player record the UI loads under: our copy of the char-enum row the reference copies
/// at the Enter World commit (layout on [`benilla_ui::script::PlayerRecord`]). Race and class go
/// through the same lookups as [`seat_from_roster`]; the reference resolves them through
/// `ChrRaces`/`ChrClasses` at call time, the same answer. No level: the reference's record has
/// one at `+0x108`, but its accessor (`0x5abe00`) has no caller, so `UnitLevel("player")` reads
/// the descriptor.
pub(crate) fn record_from_roster(
    roster: &crate::char_select::Roster,
) -> Option<benilla_ui::script::PlayerRecord> {
    let row = roster.pending_row()?;
    let race = crate::ui_unit::race_names(row.race);
    let class = crate::ui_unit::class_names(row.class);
    Some(benilla_ui::script::PlayerRecord {
        name: row.name.clone(),
        race: race.map(|(n, f)| (n.to_string(), f.to_string())),
        class: class.map(|(n, f)| (n.to_string(), f.to_string())),
        sex: roster_sex(row.gender),
    })
}

/// The `"player"` snapshot the UI loads under, from the roster row of the pick in flight, until
/// the self descriptor streams in; `None` with no pick (a capture, a scenario, a test world).
/// [`record_from_roster`] answers name, race, class and sex; this carries what else addon file
/// scope reads, the faction group chief among it. Health and power stay zero, the descriptor's
/// to say.
pub(crate) fn seat_from_roster(
    roster: &crate::char_select::Roster,
) -> Option<benilla_ui::script::UnitState> {
    let row = roster.pending_row()?;
    let race = crate::ui_unit::race_names(row.race);
    let class = crate::ui_unit::class_names(row.class);
    let sex = roster_sex(row.gender);
    Some(benilla_ui::script::UnitState {
        // Before the player object exists the reference answers `UnitExists("player")`
        // (`0x515fb0`) nil: the resolver reads the GUID from the object (`0x515994`), and the
        // fallback `0x491900` bails on the zero GUID (`0x4e80aa`) to `0x516001 lua_pushnil`.
        // `UnitLevel` (`0x517fc0`) answers the number 0 (`0x51813e push 0`); the two misses differ.
        exists: false,
        name: Some(row.name.clone()),
        race: race.map(|(n, _)| n.to_string()),
        race_file: race.map(|(_, f)| f.to_string()),
        class: class.map(|(n, _)| n.to_string()),
        class_file: class.map(|(_, f)| f.to_string()),
        sex,
        is_player: true,
        player_controlled: true,
        // AceDB-2.0 concatenates `UnitFactionGroup("player")` at file scope, where nil is an error.
        faction_group: crate::ui_unit::race_faction_group(row.race).map(str::to_string),
        ..Default::default()
    })
}

/// The character the loaded AddOn state belongs to, kept so the shutdown writes go back to its
/// files after the roster's pick is gone.
#[derive(Resource, Default)]
pub(crate) struct AddOnIdentity(pub(crate) Option<(String, String)>);

/// The world latch, the reference's `[0xb4b424]`: set at `0x4908ce` in the world-enter cascade
/// `0x4908c0` and cleared at `0x490a8d` as `PLAYER_LEAVING_WORLD` fires (`0x490b4d`, its one
/// site), so the first departure in a world fires and the rest are no-ops until the next entry.
/// Ours has two producers, [`shutdown_ui_state`]'s tail and `ui_unit`'s worldport fire. A
/// cross-map worldport spends it without leaving `InWorld`, so a quit on its loading screen
/// fires nothing; a `/reload` fires, and its rebuild re-arms.
#[derive(Resource, Default)]
pub(crate) struct LeavingWorldArmed(bool);

impl LeavingWorldArmed {
    /// A world began. Idempotent, like the reference's `mov byte [0xb4b424],1`.
    pub(crate) fn arm(&mut self) {
        self.0 = true;
    }

    /// Take the one-shot: `true` at most once per world, `0x490a8d`'s clear folded in.
    pub(crate) fn spend(&mut self) -> bool {
        std::mem::take(&mut self.0)
    }

    /// Read without taking, for tests.
    #[cfg(test)]
    pub(crate) fn is_armed(&self) -> bool {
        self.0
    }
}

/// Arm the latch when the local player's entity appears: the reference's `0x5deb60` entry into
/// the world-enter cascade, which a fresh login and a worldport's new-world create both take
/// (`0x5deb49 call 0x468570` / `0x5deb50 jne 0x5deb6a`).
pub(crate) fn arm_leaving_world_on_self_create(
    created: Query<(), Added<crate::net::SelfPlayer>>,
    mut armed: ResMut<LeavingWorldArmed>,
) {
    if !created.is_empty() {
        armed.arm();
    }
}

/// The UI shutdown in the reference's order, `0x490bd0`'s tail: `PLAYER_LEAVING_WORLD` (273) →
/// `PLAYER_LOGOUT` (271, `0x490c2a`) → `layout-cache.txt` → the flat saved file (`0x490c7e`) →
/// the per-addon files (`0x490c83`) → `AddOns.txt` (`0x490c88`) → destroy the frame-script owner
/// (`0x490c97`) → nil all 216 C bindings out of `_G` (`0x490cba` → `0x490ce0`).
///
/// `PLAYER_LOGOUT` fires before any write, an addon's last chance to change a saved global. One
/// function on every root because the steps are ordered against each other; the layout save is
/// here because a `/reload` never leaves `InWorld`. No autosave, as the reference has none (the
/// layout cache's crash-only debounce in [`crate::ui_layout`] is ours).
pub(crate) fn shutdown_ui_state(
    script: &mut UiScript,
    identity: Option<&(String, String)>,
    leaving_world: bool,
) {
    // The one conditional step: the reference reaches `0x490c20 call 0x490a80` only past the
    // active-player guard (`0x490bee call 0x468550`, `0x490bf5 je 0x490c25`) and the world latch,
    // while `PLAYER_LOGOUT` (`0x490c2a`) fires on every root. The caller spends the latch.
    if leaving_world {
        script.fire_event("PLAYER_LEAVING_WORLD", vec![]);
    }
    script.fire_event("PLAYER_LOGOUT", vec![]);
    crate::ui_layout::save_now(script, identity);
    crate::ui_saved::save(script);
    addons::save_addon_variables(script, identity);
    addons::save_enable_state(script, identity);
}

/// `OnExit(InWorld)`, a `/logout` or a disconnect (two of the reference's five roots): run the
/// shutdown tail, then end this session's Lua state, so no frame, global or addon upvalue reaches
/// the next login. The reference's state outlives `0x490bd0` and is replaced twice through
/// `0x703b80`, in `ShutdownGame` (at `0x491231`) and then in the glue build (`0x46a7b0`), so the
/// character screen runs on a state of its own. Ours is a fresh boot VM, which also carries the
/// font registry the shared glyph atlas needs.
pub(crate) fn end_ui_session(world: &mut World) {
    // A VM still parked (the load never ran) goes back into the world first.
    unpark_boot_vm(world);
    // An entry load still armed means no in-game UI was built, and the tail would overwrite the
    // saved files with the boot VM's emptiness.
    let ui_never_loaded = world.remove_resource::<PendingEntryUiLoad>().is_some();
    let identity = world
        .get_resource::<AddOnIdentity>()
        .and_then(|id| id.0.clone());
    // Spent at the root: of this edge and the worldport's, the first to a departure owns it.
    let leaving_world = world
        .get_resource_mut::<LeavingWorldArmed>()
        .is_some_and(|mut l| l.spend());
    if !ui_never_loaded {
        if let Some(mut script) = world.get_non_send_resource_mut::<UiScript>() {
            shutdown_ui_state(&mut script, identity.as_ref(), leaving_world);
        }
    }
    // After the shutdown events (a `PLAYER_LOGOUT` handler may `SetCVar`, which in the reference
    // lands in an engine-side store that survives) and before the VM is replaced.
    crate::cvars::fold_dying_vm_cvars(world);
    // The chat cache folds here, not in its own `OnExit(InWorld)` system, which a `/reload` skips.
    crate::ui_chat::settings::fold_dying_vm_chat_cache(world);
    world.insert_resource(AddOnIdentity(None));

    // Host values taken from the dying frame tree go with it (a `VmMemo` resets on its own): the
    // hovered frame, the keyboard capture (`feed_ui_input` stops outside `InWorld`), the cursor
    // payload and the minimap's hole. Not [`UiClock`]: `GetTime` is the reference's OS clock, and
    // [`seed_vm_clock`] re-anchors it for the next VM.
    if let Some(mut hover) = world.get_resource_mut::<PlayerUiHover>() {
        hover.0 = None;
    }
    if let Some(mut keys) = world.get_resource_mut::<UiKeyboardCapture>() {
        keys.typing = false;
        keys.arrows_fall_through = false;
        keys.consumed.clear();
    }
    if let Some(mut held) = world.get_resource_mut::<CursorPayloadHeld>() {
        *held = CursorPayloadHeld::default();
    }
    if let Some(mut minimap) = world.get_resource_mut::<crate::minimap::MinimapWidget>() {
        minimap.0 = None;
    }

    install_boot_vm(world);
}

/// A `ReloadUI()` waiting to run: set when [`crate::ui_logout`] drains
/// [`benilla_ui::script::SessionRequest::ReloadUi`], run by [`run_pending_reload`] in the next
/// frame's `PreUpdate`. A flag, as in the reference (`[0xb4b3f4]`, written only by `0x491380` and
/// read only by the per-frame `0x495590`), so the VM that asked is never mid-call when destroyed.
#[derive(Resource, Default)]
pub(crate) struct ReloadUiPending(pub(crate) bool);

/// Run a pending `ReloadUI()`: the reference's teardown and rebuild (`0x495664 call 0x490bd0`,
/// `0x495669 call 0x48fbf0`), for us [`end_ui_session`] then [`load_ingame_ui_on_world_entry`]
/// without leaving the world, so a `DisableAddOn` staged in the dying VM reaches `AddOns.txt`
/// before the rebuild reads it. `PLAYER_ENTERING_WORLD` then refires from [`crate::ui_unit`]'s
/// feed on the new VM. In-world only, as the reference's gate (`0x494a50(0xa)`) is; elsewhere the
/// request is dropped.
pub(crate) fn run_pending_reload(world: &mut World) {
    if !std::mem::take(&mut world.resource_mut::<ReloadUiPending>().0) {
        return;
    }
    let in_world = *world
        .resource::<State<crate::char_select::ClientState>>()
        .get()
        == crate::char_select::ClientState::InWorld;
    if !in_world {
        info!("ui_script: ReloadUI outside the world — dropped");
        return;
    }
    info!("ui_script: ReloadUI — ending the UI session and building a new one");
    end_ui_session(world);
    load_ingame_ui_on_world_entry(world);
}

/// `AppExit`, the quit roots, read as a message because a quit from in-world never leaves
/// `InWorld`. A quit from the character screen, or on a worldport's loading screen, finds the
/// latch spent, so `PLAYER_LEAVING_WORLD` does not fire.
pub(crate) fn shutdown_on_exit(
    script: Option<NonSendMut<UiScript>>,
    id: Res<AddOnIdentity>,
    pending_entry: Option<Res<PendingEntryUiLoad>>,
    mut armed: ResMut<LeavingWorldArmed>,
    mut exits: MessageReader<AppExit>,
) {
    if exits.read().next().is_none() {
        return;
    }
    // Same guard as [`end_ui_session`]: a quit inside the entry-load window has no UI to save.
    if pending_entry.is_some() {
        return;
    }
    let leaving_world = armed.spend();
    if let Some(mut script) = script {
        shutdown_ui_state(&mut script, id.0.as_ref(), leaving_world);
    }
}

/// The load's ordered tail without the host seams, for tests; production runs
/// [`finish_ui_load_with`]. The reference's `UI_Init` (`0x48fbf0`) loads every non-LoadOnDemand
/// addon, each firing `ADDON_LOADED` at `0x51f5ad` (`0x4900a3` → `0x51f600`), reads the flat
/// saved file and fires `VARIABLES_LOADED` (`0x4900b2` → `0x4913b0`), and arms `PLAYER_LOGIN`
/// (`[0xb4e260]`, set at `0x49011d`). The world-enter cascade `0x4908c0` fires `PLAYER_LOGIN` if
/// armed (read at `0x49094b`, fired at `0x490959`, disarmed at `0x49095e`), then
/// `PLAYER_ENTERING_WORLD` (`0x49096a`).
///
/// On a `/reload`, `UI_Init` enters the cascade itself (`0x490168`). On a fresh login that call is
/// skipped (`0x490166 je`, the active-player GUID still 0) and the cascade runs from the local
/// player's own create (`0x5deb60 call 0x4908c0`). This fires `PLAYER_LOGIN` at the end of the
/// load on both, the `/reload` shape; `PLAYER_ENTERING_WORLD` fires from [`crate::ui_unit`] once
/// the self descriptor lands.
#[cfg(test)]
pub(crate) fn finish_ui_load(script: &mut UiScript) {
    finish_ui_load_with(script, |_| {}, |_| {});
}

/// [`finish_ui_load`] with its two host seams. `host_settings` runs between the saved-variables
/// chunk and `VARIABLES_LOADED`, pushing the settings benilla keeps in `config.toml`. `between`
/// is the chat-cache restore's `UPDATE_CHAT_WINDOWS` + `UPDATE_CHAT_COLOR` burst, after
/// `VARIABLES_LOADED` and before `PLAYER_LOGIN`, where the reference registers its reader
/// (`0x4900d6` → `0x498a20`). On a fresh login the reference's cache (`0x5afe50`) defers that
/// reader until the server's account data reconciles (`0x5afa42`/`0x5afd01`), after `UI_Init` has
/// returned. Either way a `VARIABLES_LOADED` handler sees the boot chat colours, not the file's.
pub(crate) fn finish_ui_load_with(
    script: &mut UiScript,
    host_settings: impl FnOnce(&mut UiScript),
    between: impl FnOnce(&mut UiScript),
) {
    // Still the load edge, so still bounded: re-armed so the saved variables and every
    // `PLAYER_LOGIN` handler get the full allowance; the entry edge disarms it after.
    script.set_instruction_budget(addons::LOAD_INSTRUCTION_BUDGET);
    crate::ui_saved::load_saved_variables(script, host_settings);
    between(script);
    script.fire_event("PLAYER_LOGIN", vec![]);
}

/// Run the patch chain's `Interface\FrameXML\GlobalStrings.lua`, the first file in the reference's
/// `FrameXML.toc` and the source of every localized string global. Failures are logged loudly:
/// without it every red error line is silently empty.
fn load_global_strings(world: &mut World, script: &UiScript) {
    let Some(assets) = world.get_resource::<benilla_assets::WorldAssets>() else {
        warn!("ui_script: no patch chain — GlobalStrings absent, error lines will be empty");
        return;
    };
    let bytes = {
        let mut chain = assets.chain.lock_recover();
        chain.read_file("Interface\\FrameXML\\GlobalStrings.lua")
    };
    let src = match bytes {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            error!("ui_script: GlobalStrings.lua read failed — error lines will be empty: {e:#}");
            return;
        }
    };
    if let Err(e) = script.run(&src) {
        error!("ui_script: GlobalStrings.lua failed to run: {e}");
        return;
    }
    // Sentinel: the cast-fail drain's own lookup, checked for presence only (any locale).
    let sentinel: Option<String> = script.lua().globals().get("SPELL_FAILED_NO_AMMO").ok();
    match sentinel {
        Some(s) if !s.is_empty() => info!("ui_script: GlobalStrings loaded"),
        other => {
            error!("ui_script: GlobalStrings sentinel missing ({other:?}) — error lines broken")
        }
    }
}

/// Run the emote token table from the patch chain's `ChatFrame.lua` (`EMOTE87_TOKEN = "SIT"`),
/// which maps each `GlobalStrings.lua` alias (`EMOTE87_CMD1 = "/sit"`) to its `EmotesText` name.
/// Only whole lines matching [`is_emote_token_line`] run, none of the file's code.
fn load_emote_tokens(world: &mut World, script: &UiScript) {
    let Some(assets) = world.get_resource::<benilla_assets::WorldAssets>() else {
        return; // already warned by `load_global_strings`
    };
    let bytes = {
        let mut chain = assets.chain.lock_recover();
        chain.read_file("Interface\\FrameXML\\ChatFrame.lua")
    };
    let src = match bytes {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            error!("ui_script: ChatFrame.lua read failed — emote commands will be dead: {e:#}");
            return;
        }
    };
    let table: Vec<&str> = src
        .lines()
        .map(str::trim)
        .filter(|l| is_emote_token_line(l))
        .collect();
    let count = table.len();
    if let Err(e) = script.run(&table.join("\n")) {
        error!("ui_script: emote token table failed to run: {e}");
        return;
    }
    // Sentinel: `EMOTE87` is `/sit`.
    let sentinel: Option<String> = script.lua().globals().get("EMOTE87_TOKEN").ok();
    match sentinel.as_deref() {
        Some("SIT") => info!("ui_script: {count} emote tokens loaded"),
        other => error!(
            "ui_script: emote token sentinel is {other:?}, not \"SIT\" ({count} lines) — \
             emote slash commands are broken"
        ),
    }
}

/// Whether a line is one of `ChatFrame.lua`'s `EMOTE<digits>_TOKEN = "<NAME>";` assignments, with
/// `NAME` in `[A-Z0-9_]`; that whole-line shape makes running it the same as reading data.
pub(crate) fn is_emote_token_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("EMOTE") else {
        return false;
    };
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let Some(rest) = rest[digits..].strip_prefix("_TOKEN = \"") else {
        return false;
    };
    let Some(name) = rest.strip_suffix("\";") else {
        return false;
    };
    digits > 0
        && !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_edge_parks_the_boot_vm_and_the_session_end_returns_it() {
        let mut world = World::new();
        let boot = UiScript::new().unwrap();
        let session = boot.session();
        world.insert_non_send_resource(boot);
        arm_entry_ui_load(&mut world);
        assert!(
            world.get_non_send_resource::<UiScript>().is_none(),
            "no VM in the deferral window"
        );
        assert!(world.get_resource::<PendingEntryUiLoad>().is_some());
        assert!(world.get_non_send_resource::<ParkedBootVm>().is_some());
        // Left the world before the load ran: the VM comes back, untouched.
        unpark_boot_vm(&mut world);
        let vm = world
            .get_non_send_resource::<UiScript>()
            .expect("the parked VM is back");
        assert_eq!(vm.session(), session, "the same VM, no session moved");
        assert!(world.get_non_send_resource::<ParkedBootVm>().is_none());
    }

    /// The reference re-makes its Lua state inside `UI_Init` (`0x48fe97`).
    #[test]
    fn the_world_entry_builds_its_own_vm_rather_than_adopting_the_glue_phases() {
        let mut world = World::new();
        let glue = UiScript::new().unwrap();
        let glue_session = glue.session();
        world.insert_non_send_resource(glue);

        mint_entry_vm(&mut world);

        let entry = world
            .get_non_send_resource::<UiScript>()
            .expect("the entry load has a VM to run on");
        assert_ne!(
            entry.session(),
            glue_session,
            "the entry VM is a new session — every VmMemo resets here, so a one-shot spent \
             against the frameless glue VM is spent again against the one that has an interface"
        );
    }

    /// A one-shot fired at the frameless glue VM reaches nobody (the reference's `0x703f50` drops
    /// an event with no listener), so the entry VM must be told again.
    #[test]
    fn a_one_shot_spent_against_the_character_screens_vm_is_spent_again_after_the_entry() {
        let mut world = World::new();
        world.insert_non_send_resource(UiScript::new().expect("a glue VM"));
        let mut told: super::super::VmMemo<bool> = Default::default();

        assert!(
            told.claim(world.non_send_resource::<UiScript>()),
            "the window: a feed fires its login one-shot at the frameless character-screen VM"
        );
        assert!(
            !told.claim(world.non_send_resource::<UiScript>()),
            "and the memo has it as told — which is correct for THAT VM"
        );

        mint_entry_vm(&mut world);

        assert!(
            told.claim(world.non_send_resource::<UiScript>()),
            "the VM the in-game UI loads onto has never been told, so the feed tells it again — \
             no run condition, no ordering edge, and nothing the feed had to know about"
        );
    }

    /// The reference's `GetTime` is `GetTickCount` × 0.001 (`0x515ea0`) and never restarts; stock
    /// `CooldownFrame_SetTimer` hides a cooldown whose start is not above 0.
    #[test]
    fn a_rebuilt_vm_inherits_the_running_gettime_clock() {
        use std::time::{Duration, Instant};

        let mut world = World::new();
        world.init_resource::<UiClock>();
        // The process has been up 90 s, on `Time<Real>` and on the dying VM alike.
        let startup = Instant::now() - Duration::from_secs(90);
        let mut time = Time::<bevy::time::Real>::new(startup);
        // `Time<Real>` measures `elapsed` from its first update, not from `startup`.
        time.update_with_instant(startup);
        time.update_with_instant(startup + Duration::from_secs(90));
        world.insert_resource(time);
        let mut dying = UiScript::new().unwrap();
        dying.set_now(90.0);
        world.insert_non_send_resource(dying);
        // No in-game UI was loaded, so the shutdown tail and its file writes are skipped.
        world.init_resource::<PendingEntryUiLoad>();

        end_ui_session(&mut world);

        let vm = world
            .get_non_send_resource::<UiScript>()
            .expect("the session end installs a fresh boot VM");
        assert_eq!(
            vm.now(),
            90.0,
            "the new VM's GetTime clock restarted at {} instead of the process's 90 s",
            vm.now()
        );
        // The conversion pair moved too: a 10-minute cooldown armed 30 s before the rebuild keeps
        // its real start.
        let clock = world.resource::<UiClock>();
        let armed = crate::spell::cooldowns::CooldownInfo {
            start: startup + Duration::from_secs(60),
            remaining_ms: 570_000,
            duration_ms: 600_000,
            enabled: true,
        };
        let triple = armed
            .ui_triple(clock.anchor, clock.ui_now)
            .expect("a running cooldown pushes a triple");
        assert_eq!(
            triple,
            (60_000, 600_000, true),
            "a cooldown armed before the rebuild derived start {} ms; stock Cooldown.lua hides \
             anything but `start > 0`",
            triple.0
        );
    }
}
