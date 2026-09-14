//! Probe: what does this server's Warden send, and is our key derivation right?
//! The refusal names the decrypted message, so a correct decrypt reads as a real Warden opcode and a
//! mis-keyed one reads as `Unknown`. Credentials via argv only.
//!   cargo run --release -p benilla-protocol --example warden_probe -- <host> <user> <pass>
use benilla_protocol::WorldSession;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (host, user, pass) = (a[0].clone(), a[1].clone(), a[2].clone());
    let l = benilla_protocol::logon(&host, &user, &pass)?;
    let realm = l.realms.first().expect("no realms");
    println!("realm {:?} @ {}", realm.name, realm.address);
    match WorldSession::connect(&realm.address, &user, l.session_key) {
        Ok(mut s) => println!("no warden — roster: {:?}", s.char_enum().map(|c| c.len())),
        Err(e) => println!("refused: {e:#}"),
    }
    Ok(())
}
