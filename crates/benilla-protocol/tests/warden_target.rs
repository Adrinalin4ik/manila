//! The Warden scan lane's TARGET state — one failing test per attestation still to be written.
//!
//! These are not regression tests. They assert the end state: a client that can answer every scan
//! type it is possible for this client to witness. Each one fails until the matching
//! `ScanWitness` method is implemented against the real client, and turns green when it is, so the
//! list doubles as the remaining work.
//!
//! They carry `#[ignore]` so `cargo test` stays a regression signal — a permanently red default
//! suite hides the failures that actually mean something. Run the list with:
//!
//!     cargo test -p benilla-protocol --test warden_target -- --ignored
//!
//! Drop the `#[ignore]` lines if you would rather they fail by default; nothing else changes.
//!
//! DELIBERATELY ABSENT: `READ_MEMORY`, `FIND_CODE_BY_HASH` and `FIND_MEM_IMAGE_CODE_BY_HASH`. They
//! ask for the contents of a `WoW.exe` process image, which this client does not have and cannot
//! acquire by implementing anything — the only way to produce a value is to copy one from that
//! binary, and a copied answer describes it rather than us. They are permanently
//! `ScanOutcome::Unanswerable`, and `the_three_image_scans_stay_unanswerable` below pins that as a
//! property, not a gap. A scan profile meant for this client should not contain them.

use benilla_protocol::world::warden::{
    answer, Attestation, CheckType, ScanOutcome, ScanRequest, ScanWitness,
};

/// Swap this for the real client's witness as the methods land.
struct ClientWitness;
impl ScanWitness for ClientWitness {}

fn outcome(check: CheckType, param: &str) -> ScanOutcome {
    answer(
        &ScanRequest {
            check,
            param: param.to_string(),
        },
        &ClientWitness,
    )
}

/// Assert a scan type is answered at all — the reply's shape is the server's business, ours is that
/// something the client witnessed comes back.
fn assert_answered(check: CheckType, param: &str) {
    match outcome(check, param) {
        ScanOutcome::Answered(bytes) => {
            assert!(!bytes.is_empty(), "{check:?} answered with no bytes")
        }
        ScanOutcome::Unanswerable(reason) => panic!("{check:?} still unanswered: {reason}"),
    }
}

#[test]
#[ignore = "target: implement ScanWitness::hash_client_file over the files the client downloaded"]
fn hash_client_file_is_witnessed() {
    assert_answered(
        CheckType::HashClientFile,
        r"Interface\FrameXML\FrameXML.toc",
    );
}

#[test]
#[ignore = "target: implement ScanWitness::lua_variable against the real Lua VM"]
fn lua_variable_is_witnessed() {
    assert_answered(CheckType::GetLuaVariable, "UIParent");
}

#[test]
#[ignore = "target: implement ScanWitness::module_loaded (a browser has none, so: always Absent)"]
fn module_lookup_is_witnessed() {
    assert_answered(CheckType::FindModuleByName, "Cheat Engine");
}

#[test]
#[ignore = "target: implement ScanWitness::driver_loaded (a browser has none, so: always Absent)"]
fn driver_lookup_is_witnessed() {
    assert_answered(CheckType::FindDriverByName, "npf.sys");
}

#[test]
#[ignore = "target: implement ScanWitness::api_detoured (a browser has no detourable API)"]
fn api_check_is_witnessed() {
    assert_answered(CheckType::ApiCheck, "Kernel32.dll");
}

#[test]
#[ignore = "target: implement ScanWitness::timing_values"]
fn timing_values_are_witnessed() {
    assert_answered(CheckType::CheckTimingValues, "");
}

/// NOT a target. This one must stay green: the image scans are unanswerable by construction, and a
/// change that makes one of them return `Answered` has not implemented a check — it has started
/// reporting a value this client did not witness.
#[test]
fn the_three_image_scans_stay_unanswerable() {
    for check in [
        CheckType::ReadMemory,
        CheckType::FindCodeByHash,
        CheckType::FindMemImageCodeByHash,
    ] {
        match outcome(check, "") {
            ScanOutcome::Unanswerable(reason) => {
                assert!(reason.contains("WoW.exe"), "{check:?}: {reason}")
            }
            ScanOutcome::Answered(bytes) => panic!(
                "{check:?} answered {bytes:?} — this client has no process image to witness"
            ),
        }
    }
}

/// The gap report: what is left, as one line per unimplemented attestation. Run it with
/// `--ignored --nocapture` to see the list.
#[test]
#[ignore = "reporting only: prints the remaining attestations"]
fn remaining_attestations() {
    let checks = [
        (CheckType::HashClientFile, r"Interface\FrameXML\FrameXML.toc"),
        (CheckType::GetLuaVariable, "UIParent"),
        (CheckType::FindModuleByName, "Cheat Engine"),
        (CheckType::FindDriverByName, "npf.sys"),
        (CheckType::ApiCheck, "Kernel32.dll"),
        (CheckType::CheckTimingValues, ""),
    ];
    let mut left = 0;
    for (check, param) in checks {
        match outcome(check, param) {
            ScanOutcome::Answered(b) => println!("  done  {check:?} -> {} byte(s)", b.len()),
            ScanOutcome::Unanswerable(reason) => {
                left += 1;
                println!("  TODO  {check:?} -> {reason}");
            }
        }
    }
    println!("{left} attestation(s) remaining");
    let _ = Attestation::<()>::NotImplemented;
}
