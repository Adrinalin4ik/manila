//! Probe: what does this server's Warden actually send, and is our key derivation right?
//! The oracle is the opcode — a correct decrypt lands in `server_op`'s 0x00..=0x05, a swapped key
//! pair lands in noise. Credentials via argv only.
//!   cargo run --release -p benilla-protocol --example warden_probe -- <host> <user> <pass>
use benilla_protocol::world::warden::ServerMessage;
use benilla_protocol::WorldSession;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (host, user, pass) = (a[0].clone(), a[1].clone(), a[2].clone());
    let l = benilla_protocol::logon(&host, &user, &pass)?;
    let realm = l.realms.first().expect("no realms");
    println!("realm {:?} @ {}", realm.name, realm.address);

    let mut s = WorldSession::connect(&realm.address, &user, l.session_key)?;
    println!("world session established");
    report(&mut s);

    match s.char_enum() {
        Ok(chars) => println!("roster: {} character(s)", chars.len()),
        Err(e) => println!("char_enum: {e:#}"),
    }
    report(&mut s);
    Ok(())
}

fn report(s: &mut WorldSession) {
    let msgs = s.take_warden_messages();
    if msgs.is_empty() {
        println!("  (no warden messages yet)");
        return;
    }
    for m in msgs {
        match m {
            ServerMessage::ModuleUse { id, key, size } => println!(
                "  MODULE_USE id={} key={} size={size}",
                hex(&id),
                hex(&key)
            ),
            ServerMessage::ModuleCache { chunk } => {
                println!("  MODULE_CACHE {} bytes", chunk.len())
            }
            ServerMessage::ModuleInitialize => println!("  MODULE_INITIALIZE"),
            ServerMessage::CheatChecksRequest { body } => {
                println!("  CHEAT_CHECKS_REQUEST {} bytes: {}", body.len(), hex(&body[..body.len().min(32)]))
            }
            ServerMessage::HashRequest { seed } => println!("  HASH_REQUEST seed={}", hex(&seed)),
            ServerMessage::Unknown { opcode, body } => println!(
                "  UNKNOWN opcode={opcode:#04x} ({} bytes) {} <-- outside 0x00..=0x05 means the keys are wrong",
                body.len(),
                hex(&body[..body.len().min(16)])
            ),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
}
