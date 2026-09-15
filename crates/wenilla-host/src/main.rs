//! `wenilla-host` — the piece of the browser build that can't be static hosting: it serves the
//! wasm bundle, answers game-data reads out of the real MPQ chain, and proxies the two TCP ports
//! the client needs (login 3724, world 8085) over WebSocket, since a browser tab cannot open a
//! raw socket. See `web/README.md` for
//! the exact URL/encoding rules the client lanes code against.
//!
//! This host is a **local development tool**. `/data` hands out the operator's game files to
//! anyone who can reach the socket, with no login, so it binds to loopback by default and must
//! never be exposed on the open internet. Multi-user hosting is `wenilla-realm`, which mounts the
//! same routers behind a session cookie.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use benilla_formats::Chain;
use clap::Parser;

#[derive(Parser)]
#[command(
    about = "Serve the benilla browser build, its game data, and its net proxy (local testing only)",
    after_help = "wenilla-host is for local testing. /data serves the game files under --data to \
                  anyone who can reach the socket, without any login. Keep it on loopback or a \
                  private network; never expose it on the open internet. For hosting players, \
                  use wenilla-realm."
)]
struct Cli {
    /// Address to listen on. Loopback by default; anything else exposes `/data` to that
    /// network unauthenticated (see `--help`).
    #[arg(long, default_value = "127.0.0.1:8090")]
    bind: String,
    /// Directory holding the wasm-bindgen output (`index.html`, `wenilla.js`, `*.wasm`) —
    /// `scripts/web-build.sh`'s `web/dist/`.
    #[arg(long)]
    www: PathBuf,
    /// The vanilla `Data` directory (or a single `.MPQ`) the chain opens — the same one `benilla`
    /// itself reads on the desktop.
    #[arg(long)]
    data: PathBuf,
    /// Host the `/ws/{port}` proxy dials for the allowed ports — the mangos boxes this host
    /// itself runs against, so it defaults to loopback.
    #[arg(long, default_value = "127.0.0.1")]
    upstream: String,
    /// Host dialed for the WORLD port (8085) when it is not the same box as the login server.
    /// Defaults to `--upstream`, which is right for a local mangos pair and for any deploy that
    /// runs both on one address.
    ///
    /// **Only needed under `--pin-upstream`.** By default the proxy follows the world address the
    /// client read out of the realm list, so a realm that advertises a different host for the
    /// world than the one it answers logins on is reached without saying anything here.
    ///
    /// Pinned, that realm is unreachable without this flag — the world socket opens against the
    /// login host instead, and since a DDoS front accepts a connection on every port, the failure
    /// looks like a world server that connected and then said nothing (`up=0 down=0` in the proxy
    /// log) rather than like a wrong address.
    #[arg(long)]
    world_upstream: Option<String>,
    /// Never dial the realmlist the client asked for — pin every session to `--upstream` and
    /// `--world-upstream`.
    ///
    /// **The default is to follow the client**, because the login screen's realmlist box travels
    /// to this proxy as `?host=` and a control that silently changes nothing is worse than no
    /// control. A session that names no host still falls back to the configured upstream, so the
    /// flags keep working exactly as they did for anyone who never touches the box.
    ///
    /// Following makes this host an outbound TCP relay to ports 3724 and 8085 on any address a
    /// page names. That is a proportionate default for a **local development tool** — on the
    /// default loopback bind it reaches nothing the operator could not reach anyway, and on a
    /// shared bind this host is already serving the whole game install with no login, which the
    /// startup warning says. The port allowlist holds either way: this widens where a session may
    /// go, never what it may reach there.
    ///
    /// `wenilla-realm` is unaffected and has no such default: it mounts `ws::router_map`, the door
    /// that never follows, because there the same behaviour would be an SSRF any visitor could aim.
    #[arg(long)]
    pin_upstream: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();

    let chain = Arc::new(
        Chain::open(&cli.data).with_context(|| format!("opening {}", cli.data.display()))?,
    );
    tracing::info!(data = %cli.data.display(), "patch chain open");

    // One upstream per port rather than one for the whole allowlist, so the world can live on a
    // different host than the login server. The keys stay exactly `ALLOWED_PORTS`.
    let world_upstream = cli.world_upstream.clone().unwrap_or_else(|| cli.upstream.clone());
    let upstreams = wenilla_host::ALLOWED_PORTS.map(|port| {
        let host: std::sync::Arc<str> = if port == wenilla_host::WORLD_PORT {
            world_upstream.as_str().into()
        } else {
            cli.upstream.as_str().into()
        };
        (port, host)
    });

    // The Warden `.cr` files live beside the archives, not inside them, so they get their own
    // route. Derived from `--data` rather than given its own flag: they are part of the same
    // install, and an operator who has none simply has an empty directory and 404s.
    let modules = if cli.data.is_dir() {
        cli.data.join("warden_modules")
    } else {
        // `--data` may name a single `.MPQ`; the install is then its parent.
        cli.data
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("warden_modules")
    };
    tracing::info!(dir = %modules.display(), exists = modules.is_dir(), "warden module directory");

    // The player's own `Interface\AddOns`, beside the `Data` directory rather than inside it —
    // so it is derived from `--data`'s parent, the install root. A browser tab has no filesystem,
    // so without this route `discover_folder` finds nothing and only the twelve `Blizzard_*`
    // addons inside the archive ever load.
    let install_root = if cli.data.is_dir() {
        cli.data.parent().map(std::path::Path::to_path_buf)
    } else {
        cli.data.parent().and_then(|d| d.parent().map(std::path::Path::to_path_buf))
    };
    let addons = install_root
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("Interface")
        .join("AddOns");
    tracing::info!(dir = %addons.display(), exists = addons.is_dir(), "addon directory");

    let app = wenilla_host::data::router_with_modules(chain, Some(modules))
        .merge(wenilla_host::addons::router(addons))
        .merge(if cli.pin_upstream {
            wenilla_host::ws::router_map(upstreams)
        } else {
            wenilla_host::ws::router_map_following(upstreams)
        })
        .merge(wenilla_host::static_site::router(&cli.www));

    let listener = tokio::net::TcpListener::bind(&cli.bind)
        .await
        .with_context(|| format!("binding {}", cli.bind))?;
    if !binds_loopback(&listener) {
        tracing::warn!(
            bind = %cli.bind,
            "wenilla-host is a local testing tool: /data serves your game files to anyone who \
             can reach this address, with no login. Do not expose it on the open internet — use \
             wenilla-realm to host players."
        );
        // Only on a shared bind, and only as a second line under the one above: following the
        // client is the default, so warning about it on every loopback run would be noise on the
        // runs where it reaches nothing the operator could not reach directly.
        if !cli.pin_upstream {
            tracing::warn!(
                "…and /ws/{{port}} dials the realmlist the page asks for, so this address is also \
                 a TCP relay to ports 3724 and 8085 anywhere. --pin-upstream turns that off."
            );
        }
    }
    tracing::info!(
        bind = %cli.bind,
        www = %cli.www.display(),
        upstream = %cli.upstream,
        world_upstream = %world_upstream,
        follows_client_realmlist = !cli.pin_upstream,
        "wenilla-host listening"
    );
    axum::serve(listener, app).await.context("serving")
}

/// Is the listener bound to a loopback address (the only place `/data` is safe unauthenticated)?
fn binds_loopback(listener: &tokio::net::TcpListener) -> bool {
    listener
        .local_addr()
        .map(|a| a.ip().is_loopback())
        .unwrap_or(false)
}
