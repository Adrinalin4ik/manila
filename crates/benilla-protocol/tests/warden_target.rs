//! The Warden scan lane's TARGET state — one failing test per attestation still to be written.
//!
//! These are not regression tests. They assert the end state: a client that can answer every scan
//! type it is possible for this client to witness, with every FIELD of each reply produced from
//! something the client actually observed. Each fails until the matching `ScanWitness` method is
//! implemented against the real client, and turns green when it is, so the list doubles as the
//! remaining work.
//!
//! They carry `#[ignore]` so `cargo test` stays a regression signal — a permanently red default
//! suite hides the failures that actually mean something. Run the list with:
//!
//!     cargo test -p benilla-protocol --test warden_target -- --ignored --nocapture
//!
//! Delete the `#[ignore]` lines if you would rather they fail by default; nothing else changes.
//!
//! DELIBERATELY ABSENT: `READ_MEMORY`, `FIND_CODE_BY_HASH` and `FIND_MEM_IMAGE_CODE_BY_HASH`. They
//! ask for the contents of a `WoW.exe` process image, which this client does not have and cannot
//! acquire by implementing anything — the only way to produce a value is to copy one out of that
//! binary, and a copied answer describes it rather than us. They are permanently
//! `ScanOutcome::Unanswerable`, and `the_image_scans_stay_unanswerable` below pins that as a
//! property, not a gap. A scan profile meant for this client should not contain them.

use benilla_protocol::world::warden::{
    answer, CheckType, ScanOutcome, ScanParams, ScanRequest, ScanWitness, FOUND_0X4A,
};

/// Swap this for the real client's witness as the methods land.
struct ClientWitness;
impl ScanWitness for ClientWitness {}

fn outcome(check: CheckType, params: ScanParams) -> ScanOutcome {
    answer(&ScanRequest { check, params }, &ClientWitness)
}

/// Assert a scan is answered, and at the exact width the server reads for it. A reply of the wrong
/// length is not a partial answer: results are positional, so it shifts every scan after it.
fn assert_answered_width(check: CheckType, params: ScanParams, width: usize) {
    match outcome(check, params) {
        ScanOutcome::Answered(bytes) => assert_eq!(
            bytes.len(),
            width,
            "{check:?} must reply in exactly {width} byte(s), got {}",
            bytes.len()
        ),
        ScanOutcome::Unanswerable(reason) => panic!("{check:?} still unanswered: {reason}"),
    }
}

/// The `0x4A` scans: one byte, and it must be the found marker only when the client really found
/// the thing. Answering `1` for "found" is the mistake this guards — the server compares against
/// `0x4A`, so `1` reads as "not found": plausible, wrong, and silent.
fn assert_presence_marker(check: CheckType, params: ScanParams) {
    match outcome(check, params) {
        ScanOutcome::Answered(bytes) => {
            assert_eq!(bytes.len(), 1, "{check:?} replies with one byte");
            assert!(
                bytes[0] == FOUND_0X4A || bytes[0] != 1,
                "{check:?} answered 1, which the server reads as 'not found'"
            );
        }
        ScanOutcome::Unanswerable(reason) => panic!("{check:?} still unanswered: {reason}"),
    }
}

#[test]
#[ignore = "target: implement ScanWitness::hash_client_file over the files the client downloaded"]
fn hash_client_file_is_witnessed() {
    assert_answered_width(
        CheckType::HashClientFile,
        ScanParams::FileHash {
            path: r"Interface\FrameXML\FrameXML.toc".into(),
        },
        21, // found byte + SHA-1
    );
}

#[test]
#[ignore = "target: implement ScanWitness::lua_variable against the real Lua VM"]
fn lua_variable_is_witnessed() {
    match outcome(
        CheckType::GetLuaVariable,
        ScanParams::LuaVariable {
            name: "UIParent".into(),
        },
    ) {
        ScanOutcome::Answered(bytes) => {
            assert!(!bytes.is_empty(), "lua reply cannot be empty");
            if bytes[0] == 0 {
                assert!(bytes.len() >= 2, "a found variable carries a length byte");
                assert_eq!(
                    bytes.len(),
                    2 + bytes[1] as usize,
                    "the declared value length must match what follows"
                );
            }
        }
        ScanOutcome::Unanswerable(reason) => panic!("lua variable still unanswered: {reason}"),
    }
}

#[test]
#[ignore = "target: implement ScanWitness::module_loaded (HMAC the seed over your module names)"]
fn module_lookup_is_witnessed() {
    assert_presence_marker(
        CheckType::FindModuleByName,
        ScanParams::ModulePresence {
            seed: 0x1234_5678,
            name_digest: [0xAB; 20],
        },
    );
}

#[test]
#[ignore = "target: implement ScanWitness::driver_loaded"]
fn driver_lookup_is_witnessed() {
    assert_presence_marker(
        CheckType::FindDriverByName,
        ScanParams::DriverPresence {
            seed: 0x1234_5678,
            path_digest: [0xAB; 20],
            name: "npf.sys".into(),
        },
    );
}

#[test]
#[ignore = "target: implement ScanWitness::api_detoured"]
fn api_check_is_witnessed() {
    assert_presence_marker(
        CheckType::ApiCheck,
        ScanParams::ApiCheck {
            module: "Kernel32.dll".into(),
            proc: "GetTickCount".into(),
            hash: [0xCD; 20],
            offset: 0,
            length: 8,
        },
    );
}

#[test]
#[ignore = "target: implement ScanWitness::timing_values"]
fn timing_values_are_witnessed() {
    assert_answered_width(CheckType::CheckTimingValues, ScanParams::Timing, 5); // u8 + u32
}

/// NOT a target. This one must stay green: the image scans are unanswerable by construction, and a
/// change that makes one of them return `Answered` has not implemented a check — it has started
/// reporting a value this client did not witness.
#[test]
fn the_image_scans_stay_unanswerable() {
    for check in [
        CheckType::ReadMemory,
        CheckType::FindCodeByHash,
        CheckType::FindMemImageCodeByHash,
    ] {
        match outcome(check, ScanParams::Image) {
            ScanOutcome::Unanswerable(reason) => {
                assert!(reason.contains("WoW.exe"), "{check:?}: {reason}")
            }
            ScanOutcome::Answered(bytes) => panic!(
                "{check:?} answered {bytes:?} — this client has no process image to witness"
            ),
        }
    }
}

/// The gap report: one line per attestation, done or remaining.
#[test]
#[ignore = "reporting only: prints the remaining attestations"]
fn remaining_attestations() {
    let cases = [
        (
            CheckType::HashClientFile,
            ScanParams::FileHash {
                path: r"Interface\FrameXML\FrameXML.toc".into(),
            },
        ),
        (
            CheckType::GetLuaVariable,
            ScanParams::LuaVariable {
                name: "UIParent".into(),
            },
        ),
        (
            CheckType::FindModuleByName,
            ScanParams::ModulePresence {
                seed: 1,
                name_digest: [0; 20],
            },
        ),
        (
            CheckType::FindDriverByName,
            ScanParams::DriverPresence {
                seed: 1,
                path_digest: [0; 20],
                name: "npf.sys".into(),
            },
        ),
        (
            CheckType::ApiCheck,
            ScanParams::ApiCheck {
                module: "Kernel32.dll".into(),
                proc: "GetTickCount".into(),
                hash: [0; 20],
                offset: 0,
                length: 8,
            },
        ),
        (CheckType::CheckTimingValues, ScanParams::Timing),
    ];
    let mut left = 0;
    for (check, params) in cases {
        match outcome(check, params) {
            ScanOutcome::Answered(b) => println!("  done  {check:?} -> {} byte(s)", b.len()),
            ScanOutcome::Unanswerable(reason) => {
                left += 1;
                println!("  TODO  {check:?} -> {reason}");
            }
        }
    }
    println!("{left} attestation(s) remaining");
}
