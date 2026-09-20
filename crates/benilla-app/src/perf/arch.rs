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
}
