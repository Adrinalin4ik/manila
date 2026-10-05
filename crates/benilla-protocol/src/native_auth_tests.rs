//! Real TCP/SRP exchanges: a stock strict-version realm and Turtle's proof-stage rejection.

use super::*;
use futures_lite::future::block_on;
use num_bigint::BigUint;
use sha1::{Digest, Sha1};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

const PRIME: [u8; 32] = benilla_srp::LARGE_SAFE_PRIME_LITTLE_ENDIAN;
const SALT: [u8; 32] = [0x42; 32];
const CRC_SALT: [u8; 16] = [
    0xba, 0xa3, 0x1e, 0x99, 0xa0, 0x0b, 0x21, 0x57, 0xfc, 0x37, 0x3f, 0xb3, 0x69, 0xcd, 0xd2, 0xf1,
];
const WINDOWS_HASH: [u8; 20] = [
    0x95, 0xed, 0xb2, 0x7c, 0x78, 0x23, 0xb3, 0x63, 0xcb, 0xdd, 0xab, 0x56, 0xa3, 0x92, 0xe7, 0xcb,
    0x73, 0xfc, 0xca, 0x20,
];

enum Reply {
    ChallengeReject(u8),
    ProofReject(u8),
    ChallengeShapedProofReject(u8),
    PatchOffer,
    Success { flags: u8 },
    BadServerProof,
    Disconnect,
}

fn hash(parts: &[&[u8]]) -> [u8; 20] {
    let mut hash = Sha1::new();
    for part in parts {
        hash.update(part);
    }
    hash.finalize().into()
}

fn padded(value: &BigUint) -> [u8; 32] {
    let bytes = value.to_bytes_le();
    let mut result = [0; 32];
    result[..bytes.len()].copy_from_slice(&bytes);
    result
}

// Independent server-side SRP6: validate the client's proof and derive M2 from its A.
fn srp_exchange(stream: &mut TcpStream, build: u16) -> [u8; 20] {
    let prime = BigUint::from_bytes_le(&PRIME);
    let generator = BigUint::from(7u8);
    let x = BigUint::from_bytes_le(&hash(&[&SALT, &hash(&[b"PROBE:PASSWORD"])]));
    let verifier = generator.modpow(&x, &prime);
    let mut private = BigUint::from(11u8);
    let public = loop {
        let public = padded(&((3u8 * &verifier + generator.modpow(&private, &prime)) % &prime));
        if public[31] != 0 {
            break public;
        }
        private += 1u8;
    };
    let mut challenge = vec![0, 0, 0];
    challenge.extend_from_slice(&public);
    challenge.extend_from_slice(&[1, 7, 32]);
    challenge.extend_from_slice(&PRIME);
    challenge.extend_from_slice(&SALT);
    challenge.extend_from_slice(&CRC_SALT);
    challenge.push(0);
    stream.write_all(&challenge).unwrap();

    let mut proof = [0; 75];
    stream.read_exact(&mut proof).unwrap();
    assert_eq!(proof[0], 1);
    let client_public = &proof[1..33];
    let u = BigUint::from_bytes_le(&hash(&[client_public, &public]));
    let shared = padded(
        &((BigUint::from_bytes_le(client_public) * verifier.modpow(&u, &prime)) % &prime)
            .modpow(&private, &prime),
    );
    let even: Vec<_> = shared.iter().step_by(2).copied().collect();
    let odd: Vec<_> = shared.iter().skip(1).step_by(2).copied().collect();
    let even = hash(&[&even]);
    let odd = hash(&[&odd]);
    let mut key = [0; 40];
    for i in 0..20 {
        key[i * 2] = even[i];
        key[i * 2 + 1] = odd[i];
    }
    let xor: Vec<_> = hash(&[&PRIME])
        .iter()
        .zip(hash(&[&[7]]))
        .map(|(a, b)| a ^ b)
        .collect();
    let m1 = hash(&[
        &xor,
        &hash(&[b"PROBE"]),
        &SALT,
        client_public,
        &public,
        &key,
    ]);
    assert_eq!(&proof[33..53], &m1, "client SRP proof");
    if build == 5875 {
        // Stock StrictVersionCheck=1 must still receive the published Windows integrity proof.
        assert_eq!(&proof[53..73], &hash(&[client_public, &WINDOWS_HASH]));
    }
    hash(&[client_public, &m1, &key])
}

fn realms(flags: u8) -> Vec<u8> {
    let mut body = vec![0, 0, 0, 0, 1]; // padding + one realm
    body.extend_from_slice(&0u32.to_le_bytes()); // realm type
    body.push(flags);
    body.extend_from_slice(b"Test realm\0");
    body.extend_from_slice(b"127.0.0.1:8085\0");
    body.extend_from_slice(&0.1f32.to_le_bytes());
    body.extend_from_slice(&[1, 0, 1, 0, 0]); // characters, category, id, footer
    let mut packet = vec![0x10];
    packet.extend_from_slice(&(body.len() as u16).to_le_bytes());
    packet.extend_from_slice(&body);
    packet
}

fn server(steps: Vec<(u16, Reply)>) -> (String, thread::JoinHandle<Vec<u16>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = thread::spawn(move || {
        let mut builds = Vec::new();
        for (expected, reply) in steps {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "client never tried build {expected}"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut header = [0; 4];
            stream.read_exact(&mut header).unwrap();
            assert_eq!(&header[..2], &[0, 3]);
            let mut body = vec![0; u16::from_le_bytes([header[2], header[3]]) as usize];
            stream.read_exact(&mut body).unwrap();
            let build = u16::from_le_bytes([body[7], body[8]]);
            assert_eq!(build, expected);
            assert_eq!(&body[30..], b"PROBE");
            builds.push(build);
            match reply {
                Reply::ChallengeReject(code) => stream.write_all(&[0, 0, code]).unwrap(),
                Reply::Disconnect => {}
                reply => {
                    let m2 = srp_exchange(&mut stream, build);
                    match reply {
                        Reply::ProofReject(code) => stream.write_all(&[1, code]).unwrap(),
                        Reply::ChallengeShapedProofReject(code) => {
                            stream.write_all(&[0, 0, code]).unwrap();
                        }
                        Reply::PatchOffer => {
                            // WOW_FAIL_VERSION_UPDATE followed by CMD_XFER_INITIATE. The old
                            // socket, including its unread transfer offer, must be discarded.
                            let mut offer = vec![1, 10, 0x30, 5];
                            offer.extend_from_slice(b"Patch");
                            offer.extend_from_slice(&1024u64.to_le_bytes());
                            offer.extend_from_slice(&[0; 16]);
                            stream.write_all(&offer).unwrap();
                        }
                        Reply::Success { .. } | Reply::BadServerProof => {
                            let mut proof = vec![1, 0];
                            proof.extend_from_slice(if matches!(reply, Reply::BadServerProof) {
                                &[0; 20]
                            } else {
                                &m2
                            });
                            proof.extend_from_slice(&0u32.to_le_bytes());
                            stream.write_all(&proof).unwrap();
                            if let Reply::Success { flags } = reply {
                                let mut request = [0; 5];
                                stream.read_exact(&mut request).unwrap();
                                assert_eq!(request, [0x10, 0, 0, 0, 0]);
                                stream.write_all(&realms(flags)).unwrap();
                            }
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
        builds
    });
    (addr, handle)
}

fn run(steps: Vec<(u16, Reply)>, pinned: Option<&str>) -> Result<crate::Logon> {
    let (addr, handle) = server(steps);
    let result = block_on(logon_with_override(&addr, "probe", "password", pinned));
    handle.join().unwrap();
    result
}

#[test]
fn stock_classic_logs_in_with_5875_and_a_valid_integrity_proof() {
    for override_value in [None, Some("")] {
        let logon = run(vec![(5875, Reply::Success { flags: 0 })], override_value).unwrap();
        assert_eq!(logon.realms[0].flags, 0);
    }
    assert_eq!(crate::CLIENT_BUILD, 5875);
}

#[test]
fn turtle_rejects_stock_at_proof_then_logs_in_with_its_exact_realm_build() {
    let logon = run(
        vec![
            (5875, Reply::ChallengeShapedProofReject(9)),
            (7272, Reply::Success { flags: 0 }),
        ],
        None,
    )
    .unwrap();
    assert_eq!(logon.realms[0].flags, 0);
}

#[test]
fn version_rejection_at_challenge_also_retries() {
    run(
        vec![
            (5875, Reply::ChallengeReject(9)),
            (7272, Reply::Success { flags: 0 }),
        ],
        None,
    )
    .unwrap();
}

#[test]
fn turtle_patch_offer_retries_on_a_fresh_socket_without_downloading() {
    run(
        vec![
            (5875, Reply::PatchOffer),
            (7272, Reply::Success { flags: 0 }),
        ],
        None,
    )
    .unwrap();
}

#[test]
fn custom_realms_can_still_use_the_upstream_compatibility_build() {
    run(
        vec![
            (5875, Reply::ProofReject(9)),
            (7272, Reply::ProofReject(9)),
            (12340, Reply::Success { flags: 0 }),
        ],
        None,
    )
    .unwrap();
}

#[test]
fn all_versions_rejected_returns_the_original_auth_error() {
    let error = run(
        vec![
            (5875, Reply::ChallengeReject(9)),
            (7272, Reply::ChallengeReject(9)),
            (12340, Reply::ChallengeReject(9)),
        ],
        None,
    )
    .err()
    .unwrap();
    assert_eq!(error.downcast_ref::<crate::AuthReject>().unwrap().code, 9);
}

#[test]
fn non_version_auth_errors_never_retry() {
    for code in [3, 4, 5, 6, 7, 8, 12, 13, 15, 16] {
        let error = run(vec![(5875, Reply::ChallengeReject(code))], None)
            .err()
            .unwrap();
        assert_eq!(
            error.downcast_ref::<crate::AuthReject>().unwrap().code,
            code
        );
    }
    let error = run(vec![(5875, Reply::ProofReject(4))], None)
        .err()
        .unwrap();
    assert_eq!(error.downcast_ref::<crate::AuthReject>().unwrap().code, 4);
}

#[test]
fn transport_and_server_proof_errors_never_retry() {
    let error = run(vec![(5875, Reply::Disconnect)], None).err().unwrap();
    assert!(error.downcast_ref::<std::io::Error>().is_some());
    let error = run(vec![(5875, Reply::BadServerProof)], None)
        .err()
        .unwrap();
    assert!(error.to_string().contains("server proof mismatch"));
}

#[test]
fn genuinely_offline_realms_keep_the_servers_flags() {
    let logon = run(vec![(5875, Reply::Success { flags: 2 })], None).unwrap();
    assert_eq!(logon.realms[0].flags, 2);
}

#[test]
fn pinned_build_skips_negotiation_even_on_version_rejection() {
    run(vec![(7272, Reply::Success { flags: 0 })], Some("7272")).unwrap();
    let error = run(vec![(5875, Reply::ProofReject(9))], Some("5875"))
        .err()
        .unwrap();
    assert_eq!(error.downcast_ref::<crate::AuthReject>().unwrap().code, 9);
}

#[test]
fn invalid_override_fails_before_connecting() {
    for value in ["0", "-1", "65536", "turtle"] {
        let error = block_on(logon_with_override(
            "127.0.0.1:0",
            "probe",
            "password",
            Some(value),
        ))
        .err()
        .unwrap();
        assert!(error.to_string().contains("WOW_REALMD_BUILD"), "{value:?}");
    }
}
