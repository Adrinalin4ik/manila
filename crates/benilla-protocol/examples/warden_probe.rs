//! Probe: what does this server's Warden send, and can we answer it?
//!
//! Three things it pins that no fixture can. The key derivation: the refusal names the decrypted
//! message, so a correct decrypt reads as a real Warden opcode and a mis-keyed one reads as
//! `Unknown`. The module lane: given the server's own `.cr` files it accepts the offer, answers the
//! challenge from that module's table, re-keys, and reads the scan request in the module's own
//! encoding. And the scan set: what this server actually asks a session for.
//!
//! The witness deliberately answers NOTHING — every scan comes back `NotImplemented`, by name. A
//! probe should report what the server asked, not pretend to answer it, and the real witness needs
//! the MPQ chain, which lives a crate away. So it records each question and prints the list at the
//! end: that list is what a scan profile has to be built against.
//!
//! Credentials via argv only, never a file.
//!   cargo run --release -p benilla-protocol --example warden_probe -- <host> <user> <pass> [dir]
//!
//! `dir` defaults to `$WOW_DATA/warden_modules`. Only `.cr` files are read — the `.bin` and `.key`
//! are the server's business, not ours.
use std::sync::Mutex;
use std::time::{Duration, Instant};

use benilla_protocol::world::warden::{
    module_id_hex, ApiIntegrity, Attestation, DriverPresence, FileHash, LuaValue, ModulePresence,
    ScanWitness, TimingValues, WardenProfile,
};
use benilla_protocol::WorldSession;

/// How long to sit in the character list waiting for scans. The server arms its scan clock right
/// after the challenge (`Warden::BeginScanClock`, `Warden.ScanFrequency`), so a round arrives on
/// its own; nothing here has to provoke it.
const LINGER: Duration = Duration::from_secs(90);

/// Answers nothing and says so, but records every question. See the module note.
#[derive(Default)]
struct NamingWitness {
    asked: Mutex<Vec<String>>,
}

impl NamingWitness {
    fn note(&self, what: String) {
        println!("    scan asked: {what}");
        self.asked.lock().unwrap().push(what);
    }
}

impl ScanWitness for NamingWitness {
    fn hash_client_file(&self, path: &str) -> Attestation<FileHash> {
        self.note(format!("HASH_CLIENT_FILE {path}"));
        Attestation::NotImplemented
    }
    fn lua_variable(&self, name: &str) -> Attestation<LuaValue> {
        self.note(format!("GET_LUA_VARIABLE {name}"));
        Attestation::NotImplemented
    }
    fn module_loaded(&self, seed: u32, _d: &[u8; 20]) -> Attestation<ModulePresence> {
        self.note(format!("FIND_MODULE_BY_NAME (hmac, seed {seed:#010x})"));
        Attestation::NotImplemented
    }
    fn driver_loaded(&self, _s: u32, _d: &[u8; 20], name: &str) -> Attestation<DriverPresence> {
        self.note(format!("FIND_DRIVER_BY_NAME {name}"));
        Attestation::NotImplemented
    }
    fn api_detoured(
        &self,
        module: &str,
        proc: &str,
        _h: &[u8; 20],
        offset: u32,
        length: u8,
    ) -> Attestation<ApiIntegrity> {
        self.note(format!("API_CHECK {module}!{proc} +{offset:#x} len {length}"));
        Attestation::NotImplemented
    }
    fn timing_values(&self) -> Attestation<TimingValues> {
        self.note("CHECK_TIMING_VALUES".to_string());
        Attestation::NotImplemented
    }
}

/// A read that returned nothing in time is the quiet case, not a failure — the scan clock simply
/// has not fired yet.
///
/// Matched on the message, not by downcasting: `world::recv_packet` formats its io error into an
/// `anyhow!` string (`world/mod.rs:54`), so there is no `io::Error` left in the chain to downcast
/// to. A first cut of this probe did downcast, silently never matched, and cut the wait short.
fn is_timeout(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    s.contains("temporarily unavailable")
        || s.contains("timed out")
        || s.contains("would block")
        || s.contains("os error 11")
        || s.contains("os error 10060")
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 3 {
        eprintln!("usage: warden_probe <host> <user> <pass> [module-dir]");
        std::process::exit(2);
    }
    let (host, user, pass) = (a[0].clone(), a[1].clone(), a[2].clone());
    let dir = a.get(3).cloned().unwrap_or_else(|| {
        format!(
            "{}/warden_modules",
            std::env::var("WOW_DATA").unwrap_or_default()
        )
    });
    println!("module dir: {dir}");

    let profile = WardenProfile::awaiting_module(
        Box::new(NamingWitness::default()),
        Box::new(move |id| {
            let path = format!("{dir}/{}.cr", module_id_hex(id));
            match std::fs::read(&path) {
                Ok(bytes) => {
                    println!("  module {} -> {} bytes", module_id_hex(id), bytes.len());
                    Some(bytes)
                }
                Err(e) => {
                    println!("  module {} -> NOT READ: {e}", module_id_hex(id));
                    None
                }
            }
        }),
    );

    let l = benilla_protocol::logon(&host, &user, &pass)?;
    let realm = l.realms.first().expect("no realms");
    println!("realm {:?} @ {}", realm.name, realm.address);

    let mut session =
        match WorldSession::connect_with_warden(&realm.address, &user, l.session_key, profile) {
            Ok(s) => {
                println!("module handshake: OK");
                s
            }
            Err(e) => {
                println!("refused: {e:#}");
                return Ok(());
            }
        };

    println!("roster: {:?}", session.char_enum().map(|c| c.len()));

    // Sit still and let the scan clock fire. Short per-read timeout so the loop stays responsive;
    // the deadline is what bounds it.
    session.set_read_timeout(Some(Duration::from_secs(2)))?;
    println!("waiting {}s for a scan round…", LINGER.as_secs());
    let deadline = Instant::now() + LINGER;
    while Instant::now() < deadline {
        match session.recv() {
            Ok(p) => println!("  <- {}", p.name()),
            Err(e) if is_timeout(&e) => continue,
            Err(e) => {
                println!("stopped: {e:#}");
                break;
            }
        }
    }
    println!("done.");
    Ok(())
}
