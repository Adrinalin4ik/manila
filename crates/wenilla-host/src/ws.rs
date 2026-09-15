//! `GET /ws/{port}` — the other half of the shared WebSocket scheme (Lane T ↔ Lane H): a browser
//! tab cannot open a raw TCP socket, so `benilla-protocol::transport::web::Conn` opens this
//! instead, and this proxy relays its binary frames to and from the real login (3724) / world
//! (8085) TCP ports. `port` is checked against an explicit allowlist, not just "any u16" — this
//! host runs on a Tailscale-reachable bind address, so an unchecked proxy would be an open relay
//! onto whatever else is listening on the box.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone)]
struct WsState {
    /// Port → host the proxy dials for it — plain hostname/IP, not a URL. The key set *is* the
    /// allowlist: a port with no entry is refused. One host for every port is the single-box
    /// deployment (`main.rs` defaults to loopback); a host per port is the containerised one,
    /// where realmd (3724) and mangosd (8085) are different services.
    upstreams: Arc<HashMap<u16, Arc<str>>>,
    /// Dial the `?host=` the client asked for, falling back to [`Self::upstreams`] when it is
    /// absent or unusable. **Off by default and off in every existing caller**, because turning it
    /// on makes this an outbound TCP relay to the allowlisted ports on any address the page names.
    ///
    /// This is why it is a field rather than a change to [`upgrade`]: `wenilla-realm` mounts this
    /// very router (`wenilla-realm/src/lib.rs`) and is internet-facing, so following the client
    /// there would be a server-side request forgery reachable by any visitor. Only
    /// `wenilla-host`'s `--follow-client-realmlist` sets it.
    follow_client: bool,
}

/// Build the `/ws/{port}` router with one upstream host for every allowed port. `allowed` is the
/// exact port set the proxy will dial — production passes `{3724, 8085}` (the plan's shared
/// scheme: "only 3724 and 8085 are allowed"); tests pass their own echo-server port so the
/// allowlist check doesn't have to be bypassed to exercise it.
pub fn router(upstream: impl Into<Arc<str>>, allowed: impl IntoIterator<Item = u16>) -> Router {
    let upstream: Arc<str> = upstream.into();
    router_map(allowed.into_iter().map(|p| (p, Arc::clone(&upstream))))
}

/// Build the `/ws/{port}` router from an explicit port → host map; the keys are the allowlist.
///
/// The client-supplied `?host=` is **ignored**: every session for a port dials that port's
/// configured host. This is the door `wenilla-realm` uses and its behaviour must not change.
pub fn router_map(upstreams: impl IntoIterator<Item = (u16, Arc<str>)>) -> Router {
    build(upstreams, false)
}

/// [`router_map`], but each session dials the `?host=` the client asked for — the address the
/// player typed into the login screen's realmlist box, which reaches us because
/// `benilla-protocol::transport::web`'s `connect_url` already puts it on the query string. The
/// map is then only the **default** for a session that names no host.
///
/// **This is an open outbound relay to the allowlisted ports, and only those.** The port is still
/// checked against the map's keys before anything is dialed, so this widens *where* a session may
/// go and never *what* it may reach there. Fit for a local development host whose operator chose
/// it; not fit for a service with visitors, which is why `wenilla-realm` calls [`router_map`].
pub fn router_map_following(upstreams: impl IntoIterator<Item = (u16, Arc<str>)>) -> Router {
    build(upstreams, true)
}

fn build(upstreams: impl IntoIterator<Item = (u16, Arc<str>)>, follow_client: bool) -> Router {
    Router::new()
        .route("/ws/{port}", get(upgrade))
        .with_state(WsState {
            upstreams: Arc::new(upstreams.into_iter().collect()),
            follow_client,
        })
}

/// The `?host=` value, if it is something we are willing to hand to a resolver.
///
/// Not a security check — [`WsState::follow_client`] is the decision that matters, and the port is
/// bounded before this is consulted. It rejects the shapes that would otherwise reach the resolver
/// as a puzzling failure: an empty box, a pasted `http://…` URL, a `user@host`, or a trailing
/// `:3724` the player copied from a setup page. The client splits the port off itself
/// (`benilla_protocol::host_port`) and it travels as the path segment, so a host arriving with one
/// attached is a value we could not honour anyway — better refused by name in the log than turned
/// into a DNS lookup for `"realm.example:3724"`.
///
/// A bracketed IPv6 literal keeps its brackets stripped, which is the spelling
/// `TcpStream::connect((host, port))` wants.
fn client_host(raw: &str) -> Option<&str> {
    let host = raw.trim();
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return (!inner.is_empty()).then_some(inner);
    }
    let bad = host.is_empty()
        || host.len() > 253
        || host.contains(':')
        || host.contains('/')
        || host.contains('@')
        || host.contains('?')
        || host.chars().any(char::is_whitespace);
    (!bad).then_some(host)
}

/// `host` arrives as `?host=` — the address the client wanted. By default it is **logging only**
/// and the proxy dials this port's configured upstream, so a page cannot redirect the socket
/// somewhere else on the network; under [`router_map_following`] it is what gets dialed, with the
/// configured upstream as the fallback. Either way the port comes from the path and is checked
/// against the allowlist first.
async fn upgrade(
    State(state): State<WsState>,
    Path(port): Path<u16>,
    Query(params): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(configured) = state.upstreams.get(&port).cloned() else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let host_label = params.get("host").cloned().unwrap_or_default();
    // Resolved before the upgrade so the log line names the address that was actually dialed —
    // "asked for X, dialed Y" is the whole diagnosis when a realmlist edit appears to do nothing.
    let asked = state
        .follow_client
        .then(|| client_host(&host_label))
        .flatten();
    let upstream: Arc<str> = match asked {
        Some(h) => Arc::from(h),
        None => configured,
    };
    ws.on_upgrade(move |socket| async move {
        // Opening and closing are both logged at INFO because the two failures a player hits are
        // indistinguishable from the client: the upgrade is accepted before the upstream is dialed,
        // so an unreachable realmd reaches the login screen as a socket that opened and then died —
        // the same generic `LOGIN_FAILED` ("Unable to connect") a refusal produces. The byte counts
        // are what separate them: 0 down means nothing answered, a few bytes down is realmd
        // refusing, and `first_down` names the refusal outright.
        tracing::info!(host = %host_label, dialed = %upstream, port, "ws proxy session opening");
        match relay(socket, &upstream, port).await {
            Ok(stats) => tracing::info!(
                host = %host_label,
                dialed = %upstream,
                port,
                up = stats.up,
                down = stats.down,
                first_down = %stats.first_down,
                "ws proxy session closed",
            ),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    host = %host_label,
                    dialed = %upstream,
                    port,
                    "ws proxy session ended",
                )
            }
        }
    })
}

/// Relay one session: dial the upstream TCP port, then pump bytes both directions until both
/// sides have closed. "One TCP read chunk = one binary frame, any frame = one `write_all`"
/// (shared scheme) — no re-framing or buffering beyond the OS's own read/write granularity.
///
/// Each direction propagates its own end-of-stream to the *other* transport, rather than the two
/// futures racing in a `select!`: a `select!` here would drop whichever direction lost the race
/// mid-flight, which for the ws->tcp direction means the client's Close frame never gets echoed
/// back (tungstenite then reports a `ResetWithoutClosingHandshake` protocol error instead of a
/// clean close — caught by `tests/ws_proxy.rs`'s close-handshake test hanging, then failing,
/// before this shape). `join!` runs both to their own natural end instead:
/// receiving a WS Close (or a read error) shuts down our TCP write half, which the upstream reads
/// as EOF and — for a well-behaved peer — closes its own side, which our TCP read then sees as
/// EOF and answers with our own Close frame back to the client.
/// What one relayed session moved, for the log line in [`upgrade`]. `first_down` is the head of the
/// upstream's first chunk in hex — for realmd that is the whole diagnosis (`00 00 04` is a logon
/// challenge refused as an unknown account), and it is bounded so a world stream cannot flood the log.
#[derive(Default)]
struct RelayStats {
    up: u64,
    down: u64,
    first_down: String,
}

async fn relay(ws: WebSocket, upstream: &str, port: u16) -> anyhow::Result<RelayStats> {
    let tcp = TcpStream::connect((upstream, port)).await?;
    tcp.set_nodelay(true)?;
    let (mut tcp_r, mut tcp_w) = tcp.into_split();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let tcp_to_ws = async {
        let mut buf = [0u8; 65536];
        let mut down: u64 = 0;
        let mut first_down = String::new();
        loop {
            match tcp_r.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    // If the client already sent its own Close, tungstenite auto-queues a reply
                    // the moment `ws_rx` reads it (the behaviour `Message::Close`'s docs mention:
                    // "axum will automatically respond with a close frame if necessary") — but
                    // only *queues* it; nothing has driven a write-side poll since, so it's still
                    // sitting unflushed. `send` here fails with a "send after closing" protocol
                    // error in that case (there's already a Close in flight); flushing instead is
                    // what actually puts those bytes on the wire. Without this the socket would
                    // just drop with the reply never sent — tungstenite calls that a
                    // `ResetWithoutClosingHandshake`, and it's exactly the shape
                    // `tests/ws_proxy.rs`'s close test hit before this fallback existed.
                    if ws_tx.send(Message::Close(None)).await.is_err() {
                        let _ = ws_tx.flush().await;
                    }
                    break;
                }
                Ok(n) => {
                    down += n as u64;
                    if first_down.is_empty() {
                        first_down = buf[..n.min(8)]
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<Vec<_>>()
                            .join(" ");
                    }
                    if ws_tx
                        .send(Message::binary(buf[..n].to_vec()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        (down, first_down)
    };

    let ws_to_tcp = async {
        let mut up: u64 = 0;
        loop {
            match ws_rx.next().await {
                Some(Ok(Message::Binary(data))) => {
                    up += data.len() as u64;
                    if tcp_w.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    up += text.len() as u64;
                    if tcp_w.write_all(text.as_bytes()).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            }
        }
        // Half-close our write side so the upstream sees EOF, not just a dangling socket.
        let _ = tcp_w.shutdown().await;
        up
    };

    let ((down, first_down), up) = tokio::join!(tcp_to_ws, ws_to_tcp);
    Ok(RelayStats {
        up,
        down,
        first_down,
    })
}
