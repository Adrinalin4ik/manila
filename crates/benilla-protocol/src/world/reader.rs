use anyhow::{anyhow, Result};
use benilla_srp::vanilla_header::DecrypterHalf;

use crate::messages::{self, ServerPacket};
use crate::transport::ConnReader;

use super::{read_frame, recv_packet};

/// The read half of a split [`WorldSession`](super::WorldSession): cloned socket and decrypter,
/// and what the connection learned before the split, for
/// [`WorldSession::rejoin`](super::WorldSession::rejoin).
pub struct WorldReader {
    pub(super) reader: ConnReader,
    pub(super) decrypter: DecrypterHalf,
    pub(super) link: super::session::LinkFacts,
    /// The connection's Warden state, held for the same rejoin.
    pub(super) warden: super::session::WardenCarry,
    pub(super) traffic: std::sync::Arc<super::LinkTraffic>,
}

impl WorldReader {
    /// Read + decrypt one server packet (blocking) — the native twin of [`Self::recv_async`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn recv(&mut self) -> Result<ServerPacket> {
        futures_lite::future::block_on(self.recv_async())
    }

    /// Read + decrypt one server packet.
    pub async fn recv_async(&mut self) -> Result<ServerPacket> {
        recv_packet(&mut self.reader, Some(&mut self.decrypter), &self.traffic).await
    }

    /// Read one packet and decode it into a [`crate::Poll`] (blocking) — the native twin of
    /// [`Self::poll_async`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn poll(&mut self) -> Result<crate::Poll> {
        futures_lite::future::block_on(self.poll_async())
    }

    /// Read one packet and decode it into a [`crate::Poll`]: the typed [`crate::SessionEvent`]s it
    /// produced, or [`crate::Poll::Skipped`] for an unparseable one. Errors only on a real socket
    /// failure (disconnect). The running world model lives in the ECS — the reader is stateless,
    /// turning bytes into events and nothing more.
    ///
    /// `read_frame` reads the whole body into a buffer before parsing, so a parse error leaves the
    /// stream aligned — we skip that packet rather than tear down the session.
    pub async fn poll_async(&mut self) -> Result<crate::Poll> {
        let (opcode, body) = read_frame(&mut self.reader, Some(&mut self.decrypter), &self.traffic)
            .await
            .map_err(|e| anyhow!("world stream closed: {e}"))?;
        let body_len = body.len();
        let t0 = parse_clock::now();
        let parsed = messages::parse_server_with_tail(opcode, &body);
        match parsed {
            Ok((packet, tail)) => {
                let events = crate::decode(packet);
                parse_clock::note(t0, body_len);
                Ok(crate::Poll::Events {
                    opcode,
                    events,
                    tail,
                    body,
                })
            }
            Err(e) => Ok(crate::Poll::Skipped {
                opcode,
                reason: format!(
                    "opcode {opcode:#06x} ({}): {e} [{}B: {}]",
                    messages::opcode_name(opcode).unwrap_or("?"),
                    body.len(),
                    hex_preview(&body, 64)
                ),
                body,
            }),
        }
    }
}

/// wenilla carry: **what decoding the inbound stream costs the thread that runs it** - the
/// parse and the event decode of each packet (an `SMSG_COMPRESSED_UPDATE_OBJECT`'s inflate
/// included), not the wait for bytes. On the web that thread is the page's own (the sequencer is
/// a `spawn_local` task), so this is frame time; the app's fps journal reads it as `# net`.
pub mod parse_clock {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    #[cfg(target_arch = "wasm32")]
    use web_time::Instant;
    #[cfg(not(target_arch = "wasm32"))]
    use std::time::Instant;

    static MICROS: AtomicU64 = AtomicU64::new(0);
    static BYTES: AtomicU64 = AtomicU64::new(0);
    static PACKETS: AtomicU64 = AtomicU64::new(0);

    pub(super) fn now() -> Instant {
        Instant::now()
    }

    pub(super) fn note(t0: Instant, bytes: usize) {
        MICROS.fetch_add(t0.elapsed().as_micros() as u64, Relaxed);
        BYTES.fetch_add(bytes as u64, Relaxed);
        PACKETS.fetch_add(1, Relaxed);
    }

    /// `(micros, bytes, packets)` decoded since the last call, and a reset.
    pub fn take() -> (u64, u64, u64) {
        (
            MICROS.swap(0, Relaxed),
            BYTES.swap(0, Relaxed),
            PACKETS.swap(0, Relaxed),
        )
    }
}

/// Hex of the first `max` bytes of `body`, `…` when truncated, for decoding a packet by hand.
fn hex_preview(body: &[u8], max: usize) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for b in body.iter().take(max) {
        let _ = write!(s, "{b:02x} ");
    }
    if body.len() > max {
        s.push('…');
    }
    s.trim_end().to_string()
}
