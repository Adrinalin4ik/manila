//! The world handshake against a fake server, exercising [`WorldSession::connect`] over a real
//! socket with real header obfuscation.
//!
//! What these pin down is the packet *ordering* tolerance the handshake needs. A server does not
//! promise `SMSG_AUTH_RESPONSE` is the first encrypted packet — it interleaves its own traffic —
//! and one of those interleaved packets, `SMSG_WARDEN_DATA`, means the server runs an anticheat.
//! What this client can answer of it, it answers; what it cannot, it leaves unanswered and reads
//! on, because whether that is fatal belongs to the server and not to us.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use benilla_protocol::messages::opcode;
use benilla_protocol::world::warden::{
    self, Attestation, CheckType, FileHash, ScanWitness, WardenProfile,
};
use benilla_protocol::{messages, WorldSession};
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

/// **A Warden message we cannot answer does not end the session — the server decides that.**
///
/// This body is 16 bytes of nothing, so it decodes to no message this client knows: the case that
/// also catches a mis-keyed cipher. It used to be a refusal carried out to the login screen, on the
/// premise that every Warden server arms an unconditional response clock. A Turtle-derived server
/// was then observed sending Warden and never enforcing it, which made the premise false and the
/// refusal a session thrown away for nothing.
///
/// So the client now falls silent and reads on. What it must never do — and this test does not
/// cover, because `build_checks_result` owns it — is answer with filler: results are matched to
/// scans positionally, so a padded reply is a wrong answer rather than a missing one.
#[test]
fn a_warden_message_we_cannot_answer_leaves_the_session_alive() {
    let addr = fake_server(vec![(opcode::SMSG_WARDEN_DATA, vec![0u8; 16])]);
    assert!(
        WorldSession::connect(&addr, "one", SESSION_KEY).is_ok(),
        "the unanswerable Warden message must not cost us the session"
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
    WardenProfile::new(opcodes, WIRE_TERMINATOR, Box::new(FileWitness))
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

/// The same request with no profile: unanswered, and the session lives.
///
/// The default profile is still `None` on purpose — an encoding has to be read from the offered
/// module's `.cr` or stated by the server, and a guessed one does not fail, it silently misreads
/// every request. What changed is only the consequence: an unreadable request is silence now, not
/// a refusal.
#[test]
fn without_a_profile_the_same_request_goes_unanswered() {
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

    assert!(
        WorldSession::connect(&addr, "one", SESSION_KEY).is_ok(),
        "no profile means no honest answer, but it does not mean no session"
    );
}

// --- the module offer and its challenge -----------------------------------------------------------

/// The module the fake server offers, and the row of its challenge table it picks.
const MODULE_ID: [u8; 16] = [0xAB; 16];
const CHALLENGE_SEED: [u8; 16] = [0x11; 16];
const CHALLENGE_REPLY: [u8; 20] = [0x22; 20];
const MODULE_CLIENT_KEY: [u8; 16] = [0x33; 16];
const MODULE_SERVER_KEY: [u8; 16] = [0x44; 16];

/// This module's opcode table, copied from the real header of
/// `020F5AF2B0D646B81D7C15542B1339D1` — indexed by `WindowsScanType`, and containing no zero, so
/// its derived `scanTerminator` is 0.
const MODULE_OPCODES: [u8; 9] = [116, 14, 168, 167, 65, 220, 219, 118, 16];

/// A `.cr` at the shipped shape: the real 17-byte header above, then two challenge rows.
///
/// The encoding is DELIBERATELY not the `WIRE_BASE` table `test_profile` states, and the server
/// below builds its request with `MODULE_OPCODES`. A client that ignored the module and kept the
/// profile's own table would decode nothing — which is what makes this prove the adoption.
fn module_cr() -> Vec<u8> {
    let mut cr = vec![
        0xB8, 0x47, 0x00, 0x00, // memoryRead
        0x79, 0x6C, 0x00, 0x00, // pageScanCheck
    ];
    cr.extend_from_slice(&MODULE_OPCODES);
    // A row the server never picks, so answering has to match on the seed rather than take row 0.
    cr.extend_from_slice(&[0x99; 16]);
    cr.extend_from_slice(&[0x00; 20]);
    cr.extend_from_slice(&[0x00; 16]);
    cr.extend_from_slice(&[0x00; 16]);
    // The row it does pick.
    cr.extend_from_slice(&CHALLENGE_SEED);
    cr.extend_from_slice(&CHALLENGE_REPLY);
    cr.extend_from_slice(&MODULE_CLIENT_KEY);
    cr.extend_from_slice(&MODULE_SERVER_KEY);
    cr
}

/// Read one packet the client sent: 6-byte header (BE size + LE u32 opcode), encrypted.
fn read_client_packet(stream: &mut TcpStream, crypto: &mut HeaderCrypto) -> Option<(u16, Vec<u8>)> {
    let mut header = [0u8; 6];
    stream.read_exact(&mut header).ok()?;
    crypto.decrypter().decrypt(&mut header);
    let size = u16::from_be_bytes([header[0], header[1]]) as usize;
    let op = u32::from_le_bytes([header[2], header[3], header[4], header[5]]) as u16;
    let mut body = vec![0u8; size.saturating_sub(4)];
    stream.read_exact(&mut body).ok()?;
    Some((op, body))
}

/// **The module handshake, end to end.** A server that offers a module we hold the `.cr` for must
/// get `MODULE_OK`, then `HASH_RESULT` carrying that module's own reply row — and from there both
/// sides must be on the module's keys and the module's scan encoding.
///
/// The last leg is what the socket is for. `Warden::HandlePacket` decrypts an incoming packet with
/// the OLD input cipher and re-keys only afterwards, so the client must send `HASH_RESULT` under
/// the old key and re-key after it is on the wire. Sending the scan request under the NEW keys and
/// demanding a correct answer is what pins that instant: a client that re-keyed a packet early or
/// late reads noise here and answers nothing at all.
#[test]
fn a_module_offer_is_answered_and_its_challenge_completes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, rx) = std::sync::mpsc::channel::<(u16, Vec<u8>)>();

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

        // 1. MODULE_USE: id, rc4 key, compressed size — `Warden::Warden`'s opening packet.
        let mut offer = vec![warden::server_op::MODULE_USE];
        offer.extend_from_slice(&MODULE_ID);
        offer.extend_from_slice(&[0x77; 16]);
        offer.extend_from_slice(&1024u32.to_le_bytes());
        // RC4 is symmetric, so applying the client's INCOMING cipher here is what makes ciphertext
        // it can read back.
        warden_cipher.decrypt(&mut offer);
        send(&mut stream, Some(&mut crypto), opcode::SMSG_WARDEN_DATA, &offer);
        tx.send(read_client_packet(&mut stream, &mut crypto).expect("MODULE_OK"))
            .unwrap();

        // 2. HASH_REQUEST, still under the session keys.
        let mut challenge = vec![warden::server_op::HASH_REQUEST];
        challenge.extend_from_slice(&CHALLENGE_SEED);
        warden_cipher.decrypt(&mut challenge);
        send(
            &mut stream,
            Some(&mut crypto),
            opcode::SMSG_WARDEN_DATA,
            &challenge,
        );
        tx.send(read_client_packet(&mut stream, &mut crypto).expect("HASH_RESULT"))
            .unwrap();

        // 3. Both sides re-key, as `HandleChallengeResponse` does after the memcmp.
        warden_cipher.rekey(&MODULE_CLIENT_KEY, &MODULE_SERVER_KEY);

        // 4. A scan request in the MODULE's encoding, under its keys and its new mask.
        let xor = warden_cipher.scan_xor();
        let mut request = vec![warden::server_op::CHEAT_CHECKS_REQUEST];
        request.push("realmlist.wtf".len() as u8);
        request.extend_from_slice(b"realmlist.wtf");
        request.push(0); // end of string table
        request.push(MODULE_OPCODES[CheckType::HashClientFile as usize] ^ xor);
        request.push(1); // 1-based index into that table
        request.push(0x00 ^ xor); // this module's derived scanTerminator is 0
        warden_cipher.decrypt(&mut request);
        send(
            &mut stream,
            Some(&mut crypto),
            opcode::SMSG_WARDEN_DATA,
            &request,
        );
        tx.send(read_client_packet(&mut stream, &mut crypto).expect("CHEAT_CHECKS_RESULT"))
            .unwrap();

        // AUTH_RESPONSE comes last on purpose. The whole exchange has to finish inside the
        // handshake loop, because that is the only phase this test keeps the session alive for —
        // and it is also where a real server puts it: Warden arms as soon as the session
        // authenticates, so its first packets routinely precede the auth response.
        send(
            &mut stream,
            Some(&mut crypto),
            opcode::SMSG_AUTH_RESPONSE,
            &[messages::AUTH_OK],
        );
        thread::sleep(std::time::Duration::from_secs(2));
    });

    let cr = module_cr();
    let profile =
        test_profile().with_modules(Box::new(move |id| (*id == MODULE_ID).then(|| cr.clone())));
    let session = WorldSession::connect_with_warden(&addr, "one", SESSION_KEY, profile)
        .expect("a module we hold the .cr for must not refuse the session");
    drop(session);

    let deadline = std::time::Duration::from_secs(5);
    // Mirrors the client's own cipher, so `encrypt` here recovers what it sent.
    let mut cipher = warden::WardenCrypto::from_session_key(&SESSION_KEY);

    let (op, mut plain) = rx.recv_timeout(deadline).expect("no MODULE_OK");
    assert_eq!(op, opcode::CMSG_WARDEN_DATA);
    cipher.encrypt(&mut plain);
    assert_eq!(
        plain,
        vec![warden::client_op::MODULE_OK],
        "the offer is accepted with a bare opcode, skipping the transfer entirely"
    );

    let (op, mut plain) = rx.recv_timeout(deadline).expect("no HASH_RESULT");
    assert_eq!(op, opcode::CMSG_WARDEN_DATA);
    cipher.encrypt(&mut plain);
    assert_eq!(
        plain.len(),
        21,
        "the server checks this body is exactly 1 + sizeof(reply) and kicks otherwise"
    );
    assert_eq!(plain[0], warden::client_op::HASH_RESULT);
    assert_eq!(
        &plain[1..],
        &CHALLENGE_REPLY,
        "the reply must come from the row the seed named, not the first row"
    );

    // Everything past here is under the module's keys — which is the assertion.
    cipher.rekey(&MODULE_CLIENT_KEY, &MODULE_SERVER_KEY);
    let (op, mut plain) = rx
        .recv_timeout(deadline)
        .expect("no scan answer after the re-key");
    assert_eq!(op, opcode::CMSG_WARDEN_DATA);
    cipher.encrypt(&mut plain);
    assert_eq!(plain[0], warden::client_op::CHEAT_CHECKS_RESULT);
    let payload = &plain[7..];
    assert_eq!(
        u32::from_le_bytes([plain[3], plain[4], plain[5], plain[6]]),
        warden::checks_result_checksum(payload)
    );
    assert_eq!(payload.len(), 21, "found byte + SHA-1");
    assert_eq!(payload[0], 0, "zero means found for a file hash");
    assert_eq!(
        &payload[1..],
        &[0x5A; 20],
        "answered in the MODULE's encoding, which replaced the profile's own"
    );
}
