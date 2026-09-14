//! The world handshake against a fake server, exercising [`WorldSession::connect`] over a real
//! socket with real header obfuscation.
//!
//! What these pin down is the packet *ordering* tolerance the handshake needs. A server does not
//! promise `SMSG_AUTH_RESPONSE` is the first encrypted packet — it interleaves its own traffic —
//! and one of those interleaved packets, `SMSG_WARDEN_DATA`, means the server runs an anticheat we
//! cannot answer and must be refused at login rather than entered and kicked ~30 s later.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use benilla_protocol::messages::opcode;
use benilla_protocol::world::warden::{
    self, Attestation, CheckType, FileHash, ScanWitness, WardenProfile,
};
use benilla_protocol::{messages, WardenRequired, WorldSession};
use benilla_srp::vanilla_header::HeaderCrypto;
use benilla_srp::SESSION_KEY_LENGTH;

const SESSION_KEY: [u8; SESSION_KEY_LENGTH] = [7u8; SESSION_KEY_LENGTH];
const SERVER_SEED: u32 = 0xDEAD_BEEF;

/// Write one server packet: 2-byte BE size (counts the opcode, not itself) + 2-byte LE opcode,
/// encrypted once `crypto` is in play, then the plaintext body.
fn send(stream: &mut TcpStream, crypto: Option<&mut HeaderCrypto>, opcode: u16, body: &[u8]) {
    let size = (body.len() + 2) as u16;
    let s = size.to_be_bytes();
    let o = opcode.to_le_bytes();
    let mut header = [s[0], s[1], o[0], o[1]];
    if let Some(c) = crypto {
        c.encrypter().encrypt(&mut header);
    }
    stream.write_all(&header).unwrap();
    stream.write_all(body).unwrap();
}

/// Read the client's unencrypted `CMSG_AUTH_SESSION` (6-byte header: BE size + LE u32 opcode).
fn read_auth_session(stream: &mut TcpStream) {
    let mut header = [0u8; 6];
    stream.read_exact(&mut header).unwrap();
    let size = u16::from_be_bytes([header[0], header[1]]) as usize;
    let mut body = vec![0u8; size - 4];
    stream.read_exact(&mut body).unwrap();
}

/// Stand up a fake world server that sends `pre` (opcode, body) pairs — encrypted, in order —
/// before a successful `SMSG_AUTH_RESPONSE`. Returns the address to point `connect` at.
fn fake_server(pre: Vec<(u16, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        send(
            &mut stream,
            None,
            opcode::SMSG_AUTH_CHALLENGE,
            &SERVER_SEED.to_le_bytes(),
        );
        read_auth_session(&mut stream);
        let mut crypto = HeaderCrypto::from_session_key(SESSION_KEY);
        for (op, body) in pre {
            send(&mut stream, Some(&mut crypto), op, &body);
        }
        send(
            &mut stream,
            Some(&mut crypto),
            opcode::SMSG_AUTH_RESPONSE,
            &[messages::AUTH_OK],
        );
        // Hold the socket open so the client's reads never see a premature EOF.
        thread::sleep(std::time::Duration::from_secs(2));
    });
    addr
}

/// The plain case: `SMSG_AUTH_RESPONSE` leads, the handshake completes.
#[test]
fn auth_response_alone_completes_the_handshake() {
    let addr = fake_server(vec![]);
    assert!(WorldSession::connect(&addr, "one", SESSION_KEY).is_ok());
}

/// A server interleaving its own traffic ahead of the auth response must not fail the handshake —
/// the real one does this, and demanding AUTH_RESPONSE lead once broke login outright.
#[test]
fn packets_ahead_of_the_auth_response_are_skipped() {
    let addr = fake_server(vec![
        (opcode::SMSG_LOGIN_VERIFY_WORLD, vec![0u8; 20]),
        (opcode::SMSG_SET_FACTION_STANDING, vec![0u8; 12]),
    ]);
    assert!(WorldSession::connect(&addr, "one", SESSION_KEY).is_ok());
}

/// Warden among them is refused, with the error the login screen shows — never a session that
/// would be kicked once the server's response clock expires.
#[test]
fn a_warden_server_is_refused_at_the_handshake() {
    let addr = fake_server(vec![(opcode::SMSG_WARDEN_DATA, vec![0u8; 16])]);
    let Err(err) = WorldSession::connect(&addr, "one", SESSION_KEY) else {
        panic!("a Warden server must not yield a session");
    };
    assert!(
        err.downcast_ref::<WardenRequired>().is_some(),
        "expected WardenRequired, got: {err:#}"
    );
}


// --- the Warden scan lane, wired ------------------------------------------------------------------

/// The fixed encoding a profile supplies in place of the module's `opcodes[]` table. Deliberately
/// offset from the type's own discriminant, so a wiring that ignored the profile and read the raw
/// value would decode nothing at all.
const WIRE_BASE: u8 = 0x10;
const WIRE_TERMINATOR: u8 = 0xFF;

/// Witnesses exactly one file, with a digest distinctive enough to recognise in the reply.
struct FileWitness;

impl ScanWitness for FileWitness {
    fn hash_client_file(&self, path: &str) -> Attestation<FileHash> {
        let found = path == "realmlist.wtf";
        Attestation::Present(FileHash {
            found,
            sha1: if found { [0x5A; 20] } else { [0u8; 20] },
        })
    }
}

fn test_profile() -> WardenProfile {
    let mut opcodes = [0u8; 9];
    for (i, slot) in opcodes.iter_mut().enumerate() {
        *slot = WIRE_BASE + i as u8;
    }
    WardenProfile {
        opcodes,
        terminator: WIRE_TERMINATOR,
        witness: Box::new(FileWitness),
    }
}

/// A `CHEAT_CHECKS_REQUEST` body as `Warden::RequestScans` builds one — string table, scan block,
/// terminator — then put under the keystream the client will decrypt with. RC4 is symmetric, so
/// applying the client's *incoming* cipher here is what produces ciphertext it can read back.
///
/// The type byte and the terminator are written as `value ^ xor`, exactly as the server's builders
/// do, with `xor` taken from the session's own key stream. Building them WITHOUT the mask would
/// make this test pass against a client that ignored the mask too — and then the first real server
/// would send something neither of them could read.
fn warden_checks_request(cipher: &mut warden::WardenCrypto) -> Vec<u8> {
    let xor = cipher.scan_xor();
    let mut body = vec![warden::server_op::CHEAT_CHECKS_REQUEST];
    body.push("realmlist.wtf".len() as u8);
    body.extend_from_slice(b"realmlist.wtf");
    body.push(0); // end of string table
    body.push((WIRE_BASE + CheckType::HashClientFile as u8) ^ xor);
    body.push(1); // 1-based index into that table
    body.push(WIRE_TERMINATOR ^ xor);
    cipher.decrypt(&mut body);
    body
}

/// The mask must not be zero, or the test above would prove nothing about applying it.
#[test]
fn the_scan_mask_is_derived_and_non_trivial() {
    let xor = warden::WardenCrypto::from_session_key(&SESSION_KEY).scan_xor();
    assert_ne!(
        xor, 0,
        "a zero mask would make the masked and unmasked encodings identical"
    );
}

/// **The wiring test.** A server that asks for a scan this client can witness must get an ANSWER
/// back, not a refusal — a real `CMSG_WARDEN_DATA` carrying the digest the witness produced, under
/// the right cipher and with the checksum the server recomputes.
///
/// Without it every piece of the lane can be correct and never run: the parser, `answer` and the
/// reply builder all sat behind a session that refused before reaching any of them, and no unit
/// test could tell the difference.
#[test]
fn a_profiled_session_answers_a_scan_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        send(
            &mut stream,
            None,
            opcode::SMSG_AUTH_CHALLENGE,
            &SERVER_SEED.to_le_bytes(),
        );
        read_auth_session(&mut stream);
        let mut crypto = HeaderCrypto::from_session_key(SESSION_KEY);
        let mut warden_cipher = warden::WardenCrypto::from_session_key(&SESSION_KEY);
        let request = warden_checks_request(&mut warden_cipher);
        send(
            &mut stream,
            Some(&mut crypto),
            opcode::SMSG_WARDEN_DATA,
            &request,
        );
        send(
            &mut stream,
            Some(&mut crypto),
            opcode::SMSG_AUTH_RESPONSE,
            &[messages::AUTH_OK],
        );
        // The client's reply: 6-byte header (BE size + LE u32 opcode), encrypted.
        let mut header = [0u8; 6];
        if stream.read_exact(&mut header).is_ok() {
            crypto.decrypter().decrypt(&mut header);
            let size = u16::from_be_bytes([header[0], header[1]]) as usize;
            let op = u32::from_le_bytes([header[2], header[3], header[4], header[5]]) as u16;
            let mut body = vec![0u8; size.saturating_sub(4)];
            if stream.read_exact(&mut body).is_ok() {
                let mut framed = op.to_le_bytes().to_vec();
                framed.extend_from_slice(&body);
                let _ = tx.send(framed);
            }
        }
        thread::sleep(std::time::Duration::from_secs(2));
    });

    let session = WorldSession::connect_with_warden(&addr, "one", SESSION_KEY, test_profile())
        .expect("a scan this client can witness must not refuse the session");
    drop(session);

    let framed = rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .expect("the client never answered the scan request");

    let op = u16::from_le_bytes([framed[0], framed[1]]);
    assert_eq!(op, opcode::CMSG_WARDEN_DATA, "the answer must be a Warden packet");

    let mut plain = framed[2..].to_vec();
    // Recover the plaintext with the same keystream the client encrypted under.
    warden::WardenCrypto::from_session_key(&SESSION_KEY).encrypt(&mut plain);

    assert_eq!(plain[0], warden::client_op::CHEAT_CHECKS_RESULT);
    let len = u16::from_le_bytes([plain[1], plain[2]]) as usize;
    let sum = u32::from_le_bytes([plain[3], plain[4], plain[5], plain[6]]);
    let payload = &plain[7..];
    assert_eq!(payload.len(), len, "declared length must match the payload");
    assert_eq!(
        sum,
        warden::checks_result_checksum(payload),
        "the server recomputes this and kicks on a mismatch"
    );
    assert_eq!(payload.len(), 21, "found byte + SHA-1");
    assert_eq!(payload[0], 0, "zero means found for a file hash");
    assert_eq!(&payload[1..], &[0x5A; 20], "the witness's digest, unaltered");
}

/// The same server, with no profile: still refused. The default has to stay the safe one, because a
/// session that cannot answer Warden is a session that gets kicked 30 s in.
#[test]
fn without_a_profile_the_same_request_is_still_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        send(
            &mut stream,
            None,
            opcode::SMSG_AUTH_CHALLENGE,
            &SERVER_SEED.to_le_bytes(),
        );
        read_auth_session(&mut stream);
        let mut crypto = HeaderCrypto::from_session_key(SESSION_KEY);
        let mut warden_cipher = warden::WardenCrypto::from_session_key(&SESSION_KEY);
        let request = warden_checks_request(&mut warden_cipher);
        send(
            &mut stream,
            Some(&mut crypto),
            opcode::SMSG_WARDEN_DATA,
            &request,
        );
        send(
            &mut stream,
            Some(&mut crypto),
            opcode::SMSG_AUTH_RESPONSE,
            &[messages::AUTH_OK],
        );
        thread::sleep(std::time::Duration::from_secs(2));
    });

    let Err(err) = WorldSession::connect(&addr, "one", SESSION_KEY) else {
        panic!("a profileless session must not answer Warden");
    };
    assert!(
        err.downcast_ref::<WardenRequired>().is_some(),
        "expected WardenRequired, got: {err:#}"
    );
}
