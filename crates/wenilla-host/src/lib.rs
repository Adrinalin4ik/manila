//! Library half of `wenilla-host` — split out purely so `tests/*` can build the exact routers
//! `main.rs` serves. An integration test can only reach a crate's *library* target (a bin-only
//! package has no seam a `tests/` file can `use`), so `main.rs` stays a thin CLI wrapper over
//! these modules.

pub mod addons;
pub mod data;
pub mod static_site;
pub mod ws;

/// The ports the `/ws/{port}` proxy has a configured upstream for — mangos realmd (login) and
/// worldd (world). Fixed here, not a CLI flag: this host can bind `0.0.0.0` on a Tailscale box, so
/// the allowlist is a security boundary, not a convenience default. A pinned router
/// (`ws::router_map`) dials these and nothing else; the following one also dials a session's own
/// host on any port from `ws::MIN_FOLLOWED_PORT` up, because a realm list chooses its world port.
/// Public so a wrapper binary that mounts these routers behind its own auth (wenilla-realm) uses
/// the same set.
pub const ALLOWED_PORTS: [u16; 2] = [3724, 8085];

/// The world (worldd) port out of [`ALLOWED_PORTS`] — named because it is the one a deploy may have
/// to point at a different host than the login server (`main.rs`'s `--world-upstream`).
pub const WORLD_PORT: u16 = 8085;
