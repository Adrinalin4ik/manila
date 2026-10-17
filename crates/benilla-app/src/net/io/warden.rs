//! The app's side of the Warden lane: the witness that answers a scan, the source of a stock
//! server's module `.cr` files, and the switches that arm them. The lane itself is
//! `benilla_protocol::world::warden`; this file only supplies what a protocol crate cannot reach,
//! the install's files and the client's clocks.
//!
//! wenilla carry, in a file of its own. It was `NetConfig` in `io.rs` (6dcb53e22) and fell out at
//! the 11bc6b85c merge, which left `run.md`'s switches documented and read by nobody: every
//! Warden message went unanswered.

use std::sync::{Arc, OnceLock};

use benilla_protocol::world::warden::{self, ClientWitness, WardenProfile};

/// Whether to answer a stock server's module offer by loading its `.cr` from
/// `<data>/warden_modules/<ID>.cr`: `WOW_WARDEN_MODULES` (`?warden_modules=` on the page), on
/// unless it is `0`. A server that runs no Warden never offers a module, so the switch costs
/// nothing there; one that does costs a read per module it offers, a 404 where the install has
/// none.
fn modules_enabled() -> bool {
    crate::webenv::var("WOW_WARDEN_MODULES").as_deref() != Some("0")
}

/// The scan encoding a server has agreed to use with this client, from `WOW_WARDEN_PROFILE`.
///
/// Configuration rather than a constant because it is the SERVER's choice: on a stock server the
/// type bytes come from the module's own table, which the module lane reads; a server that fixed
/// an encoding instead states it here. Never guessed: a guessed encoding does not fail, it
/// misreads every request.
fn stated_encoding() -> Option<([u8; 9], u8)> {
    crate::webenv::var("WOW_WARDEN_PROFILE")
        .as_deref()
        .and_then(parse_warden_encoding)
}

/// Parse `WOW_WARDEN_PROFILE`: nine comma-separated scan-type bytes then the terminator, all
/// decimal or `0x`-prefixed — e.g. `0,1,2,3,4,5,6,7,8;255`. The order is `WindowsScanType`'s:
/// READ_MEMORY, FIND_MODULE_BY_NAME, FIND_MEM_IMAGE_CODE_BY_HASH, FIND_CODE_BY_HASH,
/// HASH_CLIENT_FILE, GET_LUA_VARIABLE, API_CHECK, FIND_DRIVER_BY_NAME, CHECK_TIMING_VALUES.
///
/// `None` on anything malformed rather than a partial table: a half-read encoding would mis-decode
/// requests instead of refusing them, and a refusal is the failure we can see.
fn parse_warden_encoding(spec: &str) -> Option<([u8; 9], u8)> {
    let (types, terminator) = spec.split_once(';')?;
    let byte = |t: &str| -> Option<u8> {
        let t = t.trim();
        match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            Some(hex) => u8::from_str_radix(hex, 16).ok(),
            None => t.parse().ok(),
        }
    };
    let parsed: Vec<u8> = types.split(',').filter_map(byte).collect();
    let table: [u8; 9] = parsed.try_into().ok()?;
    Some((table, byte(terminator)?))
}

/// Read one module's `.cr` by id, from where the rest of the client reads data: the install's
/// `Data` directory natively, the web host's `/data` route on wasm (`wenilla-host` serves
/// `/data/warden_modules/<ID>.cr`). Loose files rather than `Chain::read`: they are the server's
/// own files, copied in beside the archives, named by the id as 32 uppercase hex digits as
/// tortoise-wow's `WardenModuleMgr` pairs a module's `.bin`, `.key` and `.cr`.
fn read_warden_cr(id: &[u8; 16]) -> Option<Vec<u8>> {
    let name = format!("warden_modules/{}.cr", warden::module_id_hex(id));
    #[cfg(target_arch = "wasm32")]
    {
        let url = format!("{}/{name}", benilla_formats::web::data_base());
        benilla_formats::web::fetch_sync(&url).ok()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::fs::read(benilla_formats::wow_data()?.join(name)).ok()
    }
}

/// The patch chain the witness hashes `HASH_CLIENT_FILE`'s files from, opened once per process:
/// `Chain::read` takes `&self` and is `Send + Sync`, so it crosses to the net lane as it stands.
/// `None` with no install (capture runs, tests), and then Warden has no witness.
fn chain() -> Option<Arc<benilla_formats::Chain>> {
    static CHAIN: OnceLock<Option<Arc<benilla_formats::Chain>>> = OnceLock::new();
    CHAIN
        .get_or_init(|| {
            benilla_formats::wow_data()
                .and_then(|dir| benilla_formats::Chain::open(&dir).ok())
                .map(Arc::new)
        })
        .clone()
}

/// A fresh Warden profile for one connection attempt, or `None` when the lane is off or there is
/// no install to witness from.
///
/// Fresh per attempt on purpose: [`ClientWitness`] memoises the digests it computed, and that
/// cache belongs to one session — a reconnect is a new client to the server's scan round.
pub(super) fn profile() -> Option<WardenProfile> {
    let encoding = stated_encoding();
    let modules = modules_enabled();
    // Either half is enough to take part: a stated encoding for a server that fixed one, or a
    // module source for a stock server that names its own.
    if encoding.is_none() && !modules {
        return None;
    }
    let chain = chain()?;
    let for_files = Arc::clone(&chain);
    let witness = ClientWitness::new(
        Box::new(move |path| for_files.read(path).ok()),
        // No Lua reader: the VM is a `!Send` `NonSend` resource on the Bevy main thread, so this
        // lane cannot read it, and the scan set has no GET_LUA_VARIABLE rows. A server that adds
        // one gets a named gap, not a wrong answer.
        Box::new(|_| None),
        Box::new(client_clocks),
    );
    // Warm the files the scan table names, so a round does not pay their reads inside its
    // response window; on the web each is a synchronous XHR on the browser's thread.
    for path in warden::DOOR_INTEGRITY_FILES {
        if !witness.warm(path) {
            bevy::log::warn!("warden: this install has no {path} to hash");
        }
    }
    let witness = Box::new(witness);
    let Some((opcodes, terminator)) = encoding else {
        // Modules on, no stated encoding: the offered module supplies the table, and until it
        // does the profile decodes nothing rather than guess.
        return Some(WardenProfile::awaiting_module(
            witness,
            Box::new(read_warden_cr),
        ));
    };
    let profile = WardenProfile::new(opcodes, terminator, witness);
    Some(if modules {
        profile.with_modules(Box::new(read_warden_cr))
    } else {
        profile
    })
}

/// Two independent clocks, for `CHECK_TIMING_VALUES`.
///
/// The scan exists because the reference client's own timer and `GetTickCount` should agree, and
/// a hooked one makes them diverge. The honest equivalent is a MONOTONIC clock against a WALL
/// clock: two mechanisms on both targets (`Instant` vs `SystemTime` natively, `performance.now()`
/// vs `Date.now()` in a page, which is what `web-time` maps them to). Both are reduced to
/// milliseconds and truncated to `u32`, the width the reply carries.
fn client_clocks() -> (u32, u32) {
    use web_time::{Instant, SystemTime, UNIX_EPOCH};

    // The monotonic side needs an origin to measure from; the first call fixes one per process.
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let monotonic = ORIGIN.get_or_init(Instant::now).elapsed().as_millis() as u32;
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(monotonic);
    (monotonic, wall)
}

#[cfg(test)]
mod tests {
    use super::parse_warden_encoding;

    #[test]
    fn a_stated_encoding_parses_whole_or_not_at_all() {
        assert_eq!(
            parse_warden_encoding("0,1,2,3,4,5,6,7,0x08;255"),
            Some(([0, 1, 2, 3, 4, 5, 6, 7, 8], 255))
        );
        assert_eq!(parse_warden_encoding("0,1,2;255"), None);
    }
}
