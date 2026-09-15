//! Conformance for the Warden scan lane: every scan type this client can witness is answered, at
//! the width the server reads, from something the client actually observed.
//!
//! This file began as a red list — one failing test per attestation still to write. They are green
//! now that [`ClientWitness`] implements all six, so it has become the suite that keeps them that
//! way. Two properties it exists to protect:
//!
//! - **the widths.** Results are matched to scans positionally and the scan block carries no
//!   per-scan length, so a reply of the wrong size does not fail — it shifts every scan after it in
//!   the same packet, silently.
//! - **the `0x4A` marker.** Three scans compare the reply against `0x4A`, not `1`. Answering `1`
//!   for "found" reads to the server as "not found": plausible, wrong, and invisible.
//!
//! DELIBERATELY ABSENT: `READ_MEMORY`, `FIND_CODE_BY_HASH` and `FIND_MEM_IMAGE_CODE_BY_HASH`. They
//! ask for the contents of a `WoW.exe` process image, which this client does not have and cannot
//! acquire by implementing anything — the only way to produce a value is to copy one out of that
//! binary, and a copied answer describes it rather than us. They are permanently
//! `ScanOutcome::Unanswerable`, and `the_image_scans_stay_unanswerable` pins that as a property,
//! not a gap. A scan profile meant for this client should not contain them.

use benilla_protocol::world::warden::{
    answer, CheckType, ClientWitness, ScanOutcome, ScanParams, ScanRequest, ScanWitness,
    FOUND_0X4A,
};
use sha1::{Digest, Sha1};

const TOC: &str = r"Interface\FrameXML\FrameXML.toc";
const TOC_BYTES: &[u8] = b"## Interface: 11200\n";

/// The witness the client ships, handed the three things only a client can supply. The clocks agree,
/// which is the state an unhooked client reports.
fn witness() -> ClientWitness {
    ClientWitness::new(
        Box::new(|path| (path == TOC).then(|| TOC_BYTES.to_vec())),
        Box::new(|name| (name == "UIParent").then(|| "table".to_string())),
        Box::new(|| (1_000, 1_000)),
    )
}

fn outcome(check: CheckType, params: ScanParams, w: &dyn ScanWitness) -> ScanOutcome {
    answer(&ScanRequest { check, params }, w)
}

fn answered(check: CheckType, params: ScanParams, w: &dyn ScanWitness) -> Vec<u8> {
    match outcome(check, params, w) {
        ScanOutcome::Answered(bytes) => bytes,
        ScanOutcome::Unanswerable(reason) => panic!("{check:?} unanswered: {reason}"),
    }
}

#[test]
fn a_held_file_is_hashed_from_its_real_bytes() {
    let bytes = answered(
        CheckType::HashClientFile,
        ScanParams::FileHash { path: TOC.into() },
        &witness(),
    );
    assert_eq!(bytes.len(), 21, "found byte + SHA-1");
    assert_eq!(bytes[0], 0, "zero means found for this scan");
    let expected: [u8; 20] = Sha1::digest(TOC_BYTES).into();
    assert_eq!(
        &bytes[1..],
        &expected,
        "the digest must be of the bytes the client holds, not a constant"
    );
}

#[test]
fn a_file_the_client_does_not_hold_is_reported_absent_at_full_width() {
    let bytes = answered(
        CheckType::HashClientFile,
        ScanParams::FileHash {
            path: r"Interface\NotHere.blp".into(),
        },
        &witness(),
    );
    assert_eq!(bytes.len(), 21, "width is fixed even when not found");
    assert_ne!(bytes[0], 0, "non-zero means not found");
}

#[test]
fn a_lua_global_is_read_from_the_clients_own_vm() {
    let bytes = answered(
        CheckType::GetLuaVariable,
        ScanParams::LuaVariable {
            name: "UIParent".into(),
        },
        &witness(),
    );
    assert_eq!(bytes[0], 0, "found");
    assert_eq!(bytes[1] as usize, bytes.len() - 2, "declared length matches");
    assert_eq!(&bytes[2..], b"table");

    let missing = answered(
        CheckType::GetLuaVariable,
        ScanParams::LuaVariable {
            name: "NoSuchGlobal".into(),
        },
        &witness(),
    );
    assert_ne!(missing[0], 0, "an unset global is absent, not an error");
}

/// The three lookups a browser cannot have. Their answer is fixed, which is honest — and worth
/// keeping visible, because a check whose answer never depends on the client detects nothing.
#[test]
fn the_process_shaped_lookups_answer_absent_with_the_right_marker() {
    for (check, params) in [
        (
            CheckType::FindModuleByName,
            ScanParams::ModulePresence {
                seed: 1,
                name_digest: [0xAB; 20],
            },
        ),
        (
            CheckType::FindDriverByName,
            ScanParams::DriverPresence {
                seed: 1,
                path_digest: [0xAB; 20],
                name: "npf.sys".into(),
            },
        ),
        (
            CheckType::ApiCheck,
            ScanParams::ApiCheck {
                module: "Kernel32.dll".into(),
                proc: "GetTickCount".into(),
                hash: [0xCD; 20],
                offset: 0,
                length: 8,
            },
        ),
    ] {
        let bytes = answered(check, params, &witness());
        assert_eq!(bytes.len(), 1, "{check:?} replies with one byte");
        assert_ne!(
            bytes[0], FOUND_0X4A,
            "{check:?} must not claim to have found anything"
        );
    }
}

#[test]
fn the_clocks_are_reported_and_a_disagreement_shows_up() {
    let agreeing = answered(CheckType::CheckTimingValues, ScanParams::Timing, &witness());
    assert_eq!(agreeing.len(), 5, "u8 + u32");
    assert_eq!(agreeing[0], 0, "clocks agree");
    assert_eq!(
        u32::from_le_bytes([agreeing[1], agreeing[2], agreeing[3], agreeing[4]]),
        1_000
    );

    let skewed = ClientWitness::new(
        Box::new(|_| None),
        Box::new(|_| None),
        Box::new(|| (1_000, 1_050)),
    );
    let bytes = answered(CheckType::CheckTimingValues, ScanParams::Timing, &skewed);
    assert_eq!(bytes[0], 1, "a disagreement must be reported, not smoothed over");
    assert_eq!(
        u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]),
        1_000,
        "the game clock is the one sent"
    );
}

/// NOT implementable, and that is a property rather than a gap: a change that makes one of these
/// return `Answered` has not implemented a check — it has started reporting a value this client did
/// not witness.
#[test]
fn the_image_scans_stay_unanswerable() {
    for check in [
        CheckType::ReadMemory,
        CheckType::FindCodeByHash,
        CheckType::FindMemImageCodeByHash,
    ] {
        match outcome(check, ScanParams::Image, &witness()) {
            ScanOutcome::Unanswerable(reason) => {
                assert!(reason.contains("WoW.exe"), "{check:?}: {reason}")
            }
            ScanOutcome::Answered(bytes) => panic!(
                "{check:?} answered {bytes:?} — this client has no process image to witness"
            ),
        }
    }
}

/// A witness that implements nothing must still NAME its gaps rather than passing. This guards the
/// trait's default bodies, which is what any partial implementation falls back to.
#[test]
fn an_empty_witness_still_names_its_gaps() {
    struct Empty;
    impl ScanWitness for Empty {}

    match outcome(
        CheckType::HashClientFile,
        ScanParams::FileHash { path: TOC.into() },
        &Empty,
    ) {
        ScanOutcome::Unanswerable(reason) => assert!(reason.contains("not implemented")),
        ScanOutcome::Answered(bytes) => panic!("an empty witness answered {bytes:?}"),
    }
}

/// The scan set runs every round, so the five door models must be read from the archive ONCE per
/// session, not once per round. On the web each read is a synchronous XHR on the browser's own
/// thread, so the difference is a hitch the player feels.
///
/// Counting the reads is the only way to check this: the answers are identical either way, so an
/// assertion on the reply would pass against a witness with no cache at all.
#[test]
fn each_file_is_read_from_the_archive_once_per_session() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&reads);
    let witness = ClientWitness::new(
        Box::new(move |path| {
            counter.fetch_add(1, Ordering::SeqCst);
            (path == TOC).then(|| TOC_BYTES.to_vec())
        }),
        Box::new(|_| None),
        Box::new(|| (0, 0)),
    );

    let ask = || {
        answered(
            CheckType::HashClientFile,
            ScanParams::FileHash { path: TOC.into() },
            &witness,
        )
    };
    let first = ask();
    for _ in 0..4 {
        assert_eq!(ask(), first, "the cached answer must not drift");
    }
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "five rounds must cost one archive read"
    );

    // A file the client does not hold is cached too, or a missing path is re-read every round.
    let missing = ScanParams::FileHash {
        path: r"Interface\NotHere.blp".into(),
    };
    answered(CheckType::HashClientFile, missing, &witness);
    answered(
        CheckType::HashClientFile,
        ScanParams::FileHash {
            path: r"Interface\NotHere.blp".into(),
        },
        &witness,
    );
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "a missing file costs one read, not one per round"
    );
}

/// The paths the scan table actually names. Pinned so a careless edit cannot quietly drop one — a
/// scan whose file the app never warmed still answers, it just pays an archive read mid-round.
#[test]
fn the_door_models_are_the_ones_the_scan_table_names() {
    use benilla_protocol::world::warden::DOOR_INTEGRITY_FILES;
    assert_eq!(DOOR_INTEGRITY_FILES.len(), 5);
    for path in DOOR_INTEGRITY_FILES {
        assert!(path.ends_with(".m2"), "{path} should be a model");
        assert!(path.starts_with(r"World\"), "{path} should be an MPQ path");
    }
    assert!(DOOR_INTEGRITY_FILES
        .iter()
        .any(|p| p.contains("OnyxiasGate01")));
}
