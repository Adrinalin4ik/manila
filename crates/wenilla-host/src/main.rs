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
    /// It needs its own flag because the proxy dials `--upstream` and never the address the client
    /// asked for (`ws::upgrade`'s note: a page must not be able to aim the socket somewhere else on
    /// the network). A realm whose realmd advertises a *different* host for the world than the one
    /// the login server answers on is therefore unreachable without saying so here — the world
    /// socket opens against the login host instead, and since a DDoS front accepts a connection on
    /// every port, that failure looks like a world server that connected and then said nothing
    /// (`up=0 down=0` in the proxy log) rather than like a wrong address.
    #[arg(long)]
    world_upstream: Option<String>,
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

    let app = wenilla_host::data::router(chain)
        .merge(wenilla_host::ws::router_map(upstreams))
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
    }
    tracing::info!(
        bind = %cli.bind,
        www = %cli.www.display(),
        upstream = %cli.upstream,
        world_upstream = %world_upstream,
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
