//! **What the entities ARE** — a one-shot archetype census that works in the browser.
//!
//! `perf::census` has had an archetype census since 1354, and it has never once answered a
//! question about the browser, for two reasons that both have to hold:
//!
//! 1. it is armed by `WOW_ARCH_CENSUS`, and `std::env::var` returns `Err` on `wasm32` — the
//!    whole env-gated instrument bank is native-only by construction;
//! 2. it reports through `eprintln!`, and this page's `fd_write` returns `EBADF`
//!    (`web/wasi_stubs.js:21`), so even an armed one would print into a closed pipe;
//!
//! and one more that settles it: `perf::census` lives under `#[cfg(feature = "dev")]`, which
//! `scripts/web-build.sh` does not pass. It is not merely silent in the player build, it is
//! absent from it.
//!
//! The browser is where the frame costs 40 ms, so that is where the census has to run. This is
//! the shipping copy: a CVar instead of an env var, `info!` instead of `eprintln!` (bevy's
//! `LogPlugin` routes it to the console on wasm), and no registration at all until it is asked
//! for — see [`arm`].
//!
//! The question it exists to answer: journal 33 counted **37,266 entities against 759 network
//! entities**, so ~36,500 rows belong to something else, and every per-entity sweep in the frame
//! is priced on that number. Which lane owns them is not guessable from the code — the lanes are
//! per-batch, per-anchor, per-emitter and per-cell, and their populations differ by pin.

use bevy::prelude::*;

/// Dump every non-empty archetype: how many entities, and the component set that defines it,
/// largest first. Exclusive so it sees the live archetypes in one stop.
///
/// **Not registered in any schedule.** It is run once through `run_system_cached`, so a player
/// who never types the CVar pays nothing at all — not a resource read, and in particular not the
/// `ApplyDeferred` sync that an exclusive system standing in `Last` would force on every frame.
pub(crate) fn arch_census(world: &mut World) {
    // Component paths trimmed to their last two segments: the census reads as lanes, not imports.
    // **Never return an empty name.** The first run of this census printed 254 archetypes as
    // nothing but `+` separators, and the reason was this helper, not the data: without bevy's
    // `debug` feature every component name is the literal string "<Enable the debug feature to
    // see the name>", and splitting on `<` and taking what comes before it yields "". The
    // instrument hid the one fact it most needed to report. The fallback makes that condition
    // say its own name instead of vanishing.
    let short = |full: &str| -> String {
        let base = full.split('<').next().unwrap_or(full);
        if base.is_empty() {
            return full.to_string();
        }
        let segs: Vec<&str> = base.split("::").collect();
        segs[segs.len().saturating_sub(2)..].join("::")
    };
    let mut rows: Vec<(u32, String)> = world
        .archetypes()
        .iter()
        .filter(|a| !a.is_empty())
        .map(|a| {
            let mut names: Vec<String> = a
                .components()
                .iter()
                .filter_map(|&c| world.components().get_info(c))
                .map(|i| short(&i.name().to_string()))
                .collect();
            names.sort();
            (a.len(), names.join("+"))
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    let total: u32 = rows.iter().map(|r| r.0).sum();
    info!("[census] {} entities across {} archetypes", total, rows.len());
    // Sixty rows covered the population wherever this has been run; the tail is a long list of
    // singletons. The count above is of ALL of them, so a truncated list cannot misreport the
    // total - which is the number the frame is priced on.
    let mut listed = 0u32;
    for (n, sig) in rows.iter().take(60) {
        listed += n;
        info!("[census] {n:>7}  {sig}");
    }
    info!(
        "[census] {listed} of {total} entities listed above ({} archetypes not shown)",
        rows.len().saturating_sub(60)
    );
    character_draw_census(world);
}

/// **Arming it twice in one session needs the two writes in different FRAMES.** A CVar write
/// that does not change the value fires no observer (`Cvars::write` answers `Unchanged`), and
/// `archCensus` disarms itself by mirroring its row back to `0`, so a second `archCensus 1` is a
/// change only once that mirror has landed. Sent 1.5 s apart through the chat box the pair
/// coalesced and nothing fired — four attempts read as a broken instrument. Forty-five seconds
/// apart it works every time. Space them, or send `0` and then `1` in separate breaths.
///
/// First numbers, a live city on 2026-09-29: 41,515 entities, and of the draws using the world
/// material, **4,930 draws over 1,840 distinct meshes and 1,834 distinct materials, forming 3,122
/// batch groups**.
/// **What batching would have to overcome on the crowd: distinct meshes against distinct
/// materials, among character parts alone.**
///
/// bevy batches two draws only when `(MaterialBindGroupIndex, AssetId<Mesh>, Lightmap)` match
/// (`bevy_pbr`'s `GetBatchData::CompareData`), so a shared texture array - one material for every
/// character - buys nothing unless the MESH is shared too. The world-wide `mats` and `meshes`
/// columns cannot answer that: they count the terrain and the doodads with everything else, and
/// say meshes outnumber materials four to one. This counts the character parts on their own.
///
/// Read it as a ceiling on batching, and the first reading settled a design question: the meshes
/// and the materials are in a dead heat (1,840 against 1,834), so a shared texture array — one
/// material for the whole crowd, which was the plan — would leave 1,840 groups where there are
/// now 3,122. Forty-one per cent of the batches, off a crowd whose whole drawing cost is 5.5 ms:
/// about 2 ms of a 40 ms frame, for layer allocation, an LRU over 256 layers and a shader change.
/// The assumption it was built on — that characters vary by material and share meshes — is simply
/// not true here, and one line of census said so before anything was built.
///
/// Filtered on `SkinnedMesh` rather than the dressing crate's own marker, which is private: every
/// part that skins to a rig is a character or creature part, which is the population in question.
fn character_draw_census(world: &mut World) {
    use bevy::prelude::*;
    use std::collections::HashSet;
    // Every draw that uses the world material, unfiltered. `SkinnedMesh` was the first filter
    // and it counted ZERO: this client skins through its own `rig_palette`, not bevy's component,
    // so that marker names nothing here. Unfiltered is the honest question anyway - "how much
    // batching headroom is there at all" - and the pair count answers it directly.
    let mut q =
        world.query::<(&Mesh3d, &MeshMaterial3d<benilla_assets::materials::WowModelMaterial>)>();
    let (mut meshes, mut mats, mut pairs) = (HashSet::new(), HashSet::new(), HashSet::new());
    let mut parts = 0u32;
    for (mesh, mat) in q.iter(world) {
        parts += 1;
        meshes.insert(mesh.0.id());
        mats.insert(mat.0.id());
        pairs.insert((mesh.0.id(), mat.0.id()));
    }
    info!(
        "[census] world-material draws {parts}: {} distinct meshes, {} distinct materials,          {} distinct (mesh, material) pairs - the batch groups bevy can actually form",
        meshes.len(),
        mats.len(),
        pairs.len()
    );
}
