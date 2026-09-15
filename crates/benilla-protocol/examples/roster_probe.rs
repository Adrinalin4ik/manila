//! Probe: what does this realm's `SMSG_CHAR_ENUM` actually decode to?
//!
//! Exists because a roster COUNT proves nothing. `parse.rs` reads `u8 count` then that many
//! records and stops — it never checks whether anything was left over, so a record whose width is
//! wrong for this server still yields the right number of characters, each filled with garbage
//! from the neighbouring field. That is the most repeated defect class in this codebase and every
//! instance of it was silent.
//!
//! So this prints the fields and lets a human judge them. What to look at, in order — the first
//! one that is wrong is where the layout diverges, because every field after a bad read is
//! shifted:
//!
//!   * `name` — a cstring. Garbage here means the record start is wrong.
//!   * `race`/`class`/`gender` — 1..=8ish, 1..=11, 0|1 on a stock 1.12 server. A custom realm may
//!     legitimately have races outside that range; that is data, not misalignment.
//!   * `level` — 1..=60ish. A four-digit level means the reads before it are off.
//!   * `map`/`zone` — small ids. A huge one is the tell.
//!   * `position` — plausible world coordinates, not 1e30 or 0,0,0.
//!   * `equipment` — display ids, mostly small or zero. All-zero on a levelled character, or
//!     enormous values, means the tail is misread.
//!
//! Credentials via argv only, never a file.
//!   cargo run --release -p benilla-protocol --example roster_probe -- <host> <user> <pass> [dir]
use benilla_protocol::world::warden::{module_id_hex, ScanWitness, WardenProfile};
use benilla_protocol::WorldSession;

/// Answers no scan. The roster lands long before the first scan round, so nothing here needs a
/// witness — and a probe that guessed at answers would be reporting its own guesses.
struct NoWitness;
impl ScanWitness for NoWitness {}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 3 {
        eprintln!("usage: roster_probe <host> <user> <pass> [module-dir]");
        std::process::exit(2);
    }
    let (host, user, pass) = (a[0].clone(), a[1].clone(), a[2].clone());
    let dir = a.get(3).cloned().unwrap_or_else(|| {
        format!(
            "{}/warden_modules",
            std::env::var("WOW_DATA").unwrap_or_default()
        )
    });

    let profile = WardenProfile::awaiting_module(
        Box::new(NoWitness),
        Box::new(move |id| std::fs::read(format!("{dir}/{}.cr", module_id_hex(id))).ok()),
    );

    let l = benilla_protocol::logon(&host, &user, &pass)?;
    let realm = l.realms.first().expect("no realms");
    println!("realm {:?} @ {}", realm.name, realm.address);

    let mut session =
        WorldSession::connect_with_warden(&realm.address, &user, l.session_key, profile)?;
    let roster = session.char_enum()?;
    println!("roster: {} character(s)\n", roster.len());
    for c in &roster {
        println!("{c:#?}\n");
    }
    Ok(())
}
