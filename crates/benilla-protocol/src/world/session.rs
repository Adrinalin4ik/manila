use anyhow::{anyhow, bail, Context, Result};
use benilla_srp::vanilla_header::{HeaderCrypto, ProofSeed};
use benilla_srp::{NormalizedString, SESSION_KEY_LENGTH};

use crate::messages::{self, opcode, Character, MoveMode, ServerPacket};
use crate::transport::Conn;

use super::movement::{client_uptime_ms, movement_info, MOVEMENT_FLAG_FORWARD};
use super::reader::WorldReader;
use super::writer::WorldWriter;
use super::warden;
use super::{recv_packet, send_packet};

/// Read timeout through `player_login`, where each step awaits one reply.
const HANDSHAKE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Read timeout in the login queue, where updates can be minutes apart. Deviation: the reference
/// waits forever; a bound tells a server that died mid-queue from a long wait.
const QUEUE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// The server requires Warden. Deviation: benilla does not implement it; vmangos ships with it
/// off (`Warden.WinEnabled`, `Warden.OSXEnabled`). vmangos kicks a client that leaves a Warden
/// request unanswered for 30 s (`Warden.cpp`), so the connect refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WardenRequired;

/// The world server refused the session. Its code is from `messages`' `AUTH_*` block, not
/// [`crate::AuthReject`]'s, whose numbers overlap with unrelated meanings.
#[derive(Debug, Clone, Copy)]
pub struct WorldAuthReject {
    pub code: u8,
}

impl std::fmt::Display for WorldAuthReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "world server refused the session: result {:#04x}",
            self.code
        )
    }
}

impl std::error::Error for WorldAuthReject {}

impl std::fmt::Display for WardenRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Was "which benilla does not implement", which stopped being true once the module lane
        // landed and is now actively misleading: a live realm gets through the offer, the challenge
        // and the re-key, and refuses on ONE scan it cannot witness. The player-facing sentence has
        // to say which of those it was, or the login screen blames the wrong thing.
        write!(
            f,
            "this server's Warden asked for something this client cannot answer about itself"
        )
    }
}

impl std::error::Error for WardenRequired {}

/// An authenticated world-server session with 1.12 header obfuscation active.
pub struct WorldSession {
    conn: Conn,
    crypto: HeaderCrypto,
    /// Roster guid to race, so [`Self::player_login`] can pick the chat language.
    roster_races: std::collections::HashMap<u64, u8>,
    /// The language chat sends speak, the character's faction tongue: vmangos drops chat, even
    /// dot-commands, in a language the character does not know.
    chat_language: u32,
    /// The rested billing minutes the admitting `SMSG_AUTH_RESPONSE` carried. Deviation: `0` when
    /// the body is too short, where the reference leaves its global as it was; this field lives
    /// per connection, with nothing earlier to keep.
    billing_time_rested: u32,
    /// `SMSG_TUTORIAL_FLAGS` when it lands during the handshake rather than in the world stream.
    tutorial_flags: Option<Vec<u8>>,
    /// `SMSG_ADDON_INFO`'s per-record status bytes in order; `None` until the server answers.
    addon_info: Option<Vec<u8>>,
    /// Kept because Warden's ciphers are derived from it (`warden::WardenCrypto::from_session_key`)
    /// and the first `SMSG_WARDEN_DATA` can arrive before the handshake even finishes — there is no
    /// later point at which the caller could hand it back.
    session_key: [u8; SESSION_KEY_LENGTH],
    /// Built on the first Warden packet rather than at connect, so a server that never runs Warden
    /// pays nothing for it.
    warden: Option<warden::WardenCrypto>,
    /// The scan encoding this server uses and what we can witness, set by
    /// [`Self::set_warden_profile`]. `None` — the default — means Warden is refused: the encoding
    /// is either read from the offered module's `.cr` or stated by the server, and guessing one
    /// does not fail, it silently misreads every request.
    warden_profile: Option<warden::WardenProfile>,
    /// Every distinct Warden message this session chose not to answer, so each is reported once
    /// instead of once per round — a server re-asks on its own clock (~30 s), and the same
    /// sentence every half minute buries the log it is supposed to explain.
    ///
    /// A set and not a counter because the *reasons* are the diagnosis: "asked for scans before any
    /// encoding was known" and "asked something we cannot witness" are different problems with
    /// different fixes, and a tally would merge them.
    warden_unanswered: std::collections::HashSet<String>,
}

/// What one Warden message obliges the client to do.
struct WardenReply {
    /// The `CMSG_WARDEN_DATA` body to send, its client opcode already in front.
    body: Vec<u8>,
    /// The module key pair to re-key with once `body` is on the wire — set only by the challenge
    /// answer. See [`WorldSession::handle_warden`] for why it cannot be applied any earlier.
    rekey: Option<([u8; 16], [u8; 16])>,
}

/// Decide the answer to one decrypted Warden message; `Ok(None)` means the message takes no reply.
///
/// Split out of [`WorldSession::handle_warden`] because it needs `&mut` on the profile (a module
/// offer makes it adopt an encoding) while the send needs `&mut self`. It touches nothing else —
/// no socket, no cipher — so the send-then-re-key ordering lives in one place at the call site
/// instead of being re-derived in each branch here.
fn warden_reply(
    message: &Option<warden::ServerMessage>,
    xor: u8,
    profile: Option<&mut warden::WardenProfile>,
) -> Result<Option<WardenReply>> {
    let refuse = |what: String| -> anyhow::Error {
        anyhow::Error::new(WardenRequired).context(format!("warden sent {what}"))
    };
    let (message, profile) = match (message, profile) {
        (Some(m), Some(p)) => (m, p),
        (other, _) => {
            let what = match other {
                Some(msg) => format!("{msg:?}"),
                None => "an empty body".to_string(),
            };
            return Err(refuse(what));
        }
    };

    match message {
        // Answering MODULE_OK skips the transfer entirely: `Warden::HandlePacket`'s
        // WARDEN_CMSG_MODULE_OK branch reads nothing past the opcode and goes straight to
        // `RequestChallenge`. MODULE_MISSING would instead stream us the module in 500-byte chunks
        // we have no use for, and a second MODULE_MISSING after that is a kick.
        //
        // The offer is only acceptable because the `.cr` gives us the module's challenge table and
        // opcode map without running it. Adopting it here, before the reply goes out, means the
        // scan encoding is already right when the first request lands.
        warden::ServerMessage::ModuleUse { id, .. } => {
            profile.adopt_offered_module(id).map_err(|e| {
                anyhow::Error::new(WardenRequired)
                    .context(format!("warden offered a module we cannot adopt: {e}"))
            })?;
            Ok(Some(WardenReply {
                body: vec![warden::client_op::MODULE_OK],
                rekey: None,
            }))
        }

        // The challenge is a table lookup, not a computation. The server checks the body is
        // *exactly* `1 + sizeof(reply)` = 21 bytes and memcmps from offset 1
        // (`HandleChallengeResponse`), so neither a short nor a padded body passes.
        warden::ServerMessage::HashRequest { seed } => {
            let module = profile.module().ok_or_else(|| {
                anyhow::Error::new(WardenRequired)
                    .context("warden sent a challenge before offering a module we adopted")
            })?;
            let entry = module.answer_challenge(seed).ok_or_else(|| {
                anyhow::Error::new(WardenRequired).context(format!(
                    "warden's challenge seed is not in module {}'s table — \
                     the adopted module is not the one it offered",
                    warden::module_id_hex(&module.id)
                ))
            })?;
            let mut body = Vec::with_capacity(1 + entry.reply.len());
            body.push(warden::client_op::HASH_RESULT);
            body.extend_from_slice(&entry.reply);
            Ok(Some(WardenReply {
                body,
                rekey: Some((entry.client_key, entry.server_key)),
            }))
        }

        // Silence is the protocol here, not a gap. `WardenWin::InitializeClient` concatenates three
        // MODULE_INITIALIZE blocks into one packet, sets `_initialized` and reads nothing back —
        // there is no `WARDEN_CMSG_*` opcode that could answer it, and `HandlePacket` has no case
        // that would accept one. They tell the module where the client's Lua, file and timing
        // functions are; we answer those scans from our own state instead, so there is nothing in
        // the blocks for us to act on and nothing for us to send.
        warden::ServerMessage::ModuleInitialize => Ok(None),

        warden::ServerMessage::CheatChecksRequest { body } => {
            // Reborrowed shared once, so the closure below and `terminator_on_wire` are plainly two
            // reads of the same `&` rather than two captures of a `&mut`.
            let profile: &warden::WardenProfile = profile;
            // No encoding yet means the server asked for scans before offering the module that
            // would have named them. Refusing is the only honest answer: there is no table to read
            // the request under, and a placeholder one would misread rather than fail.
            let terminator = profile.terminator_on_wire(xor).ok_or_else(|| {
                anyhow::Error::new(WardenRequired)
                    .context("warden asked for scans before any encoding was known")
            })?;
            let requests =
                warden::parse_checks_request(body, &|b| profile.decode(b, xor), terminator)
                    .map_err(|e| anyhow!("unreadable Warden scan request: {e:?}"))?;
            let outcomes: Vec<_> = requests
                .iter()
                .map(|r| warden::answer(r, profile.witness.as_ref()))
                .collect();
            // An unanswerable scan stops the whole reply rather than being padded: results are
            // matched to scans positionally, so filler is not a gap, it is a false answer to one
            // specific question. The profile asked something this client cannot witness, and
            // saying so is the honest end.
            let body = warden::build_checks_result(&outcomes).map_err(|reason| {
                anyhow::Error::new(WardenRequired)
                    .context(format!("warden asked something we cannot witness: {reason}"))
            })?;
            Ok(Some(WardenReply { body, rekey: None }))
        }

        // What is left is MODULE_CACHE, which can only follow a MODULE_MISSING we never send, and
        // `Unknown` — which is where MEM_CHECKS_REQUEST lands, and also where a mis-keyed cipher
        // lands, since noise decodes to an opcode outside `server_op`. Refused by name so the
        // difference is readable in the log rather than guessed at.
        other => Err(refuse(format!("{other:?}"))),
    }
}

impl WorldSession {
    /// Connect to the world server and complete the auth handshake (blocking) — the native twin of
    /// [`Self::connect_async`], for the CLIs/probes and the native net thread.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn connect(
        addr: &str,
        username: &str,
        session_key: [u8; SESSION_KEY_LENGTH],
    ) -> Result<Self> {
        Self::connect_queued(addr, username, session_key, &mut |_| true)
    }

    /// [`Self::connect`] as a future — the browser build's lane (no thread to block).
    pub async fn connect_async(
        addr: &str,
        username: &str,
        session_key: [u8; SESSION_KEY_LENGTH],
    ) -> Result<Self> {
        Self::connect_queued_async(addr, username, session_key, &mut |_| true).await
    }

    /// [`Self::connect`], reporting our place in the **login queue** as it moves.
    ///
    /// `on_queue` fires once per `AUTH_WAIT_QUEUE` the server sends — `None` when the packet
    /// carried no readable position — and the call returns only once we are admitted (`AUTH_OK`),
    /// refused, or the socket fails. The queue is a wait, not an outcome, so it is a callback
    /// rather than a return value: the screen has to be able to show the position *while* the
    /// handshake is still parked here.
    ///
    /// **Returning `false` abandons the queue** and fails the connect. That is the only way out of
    /// a wait that can last minutes: every other stage of the handshake is bounded tightly enough
    /// that a cancel is honoured at its next boundary, and a queue is not. Abandoning is checked
    /// once per packet rather than per frame — the reference tears the socket down on the next
    /// tick, which is finer-grained, but it costs a player at most one server update.
    pub fn connect_queued(
        addr: &str,
        username: &str,
        session_key: [u8; SESSION_KEY_LENGTH],
        on_queue: &mut dyn FnMut(Option<u32>) -> bool,
    ) -> Result<Self> {
        futures_lite::future::block_on(Self::connect_queued_async(
            addr,
            username,
            session_key,
            on_queue,
        ))
    }

    /// [`Self::connect_queued`] as a future. `addr` is `host[:port]` (the realm list's own
    /// address form; a bare host gets [`WORLD_PORT`](crate::WORLD_PORT)).
    pub async fn connect_queued_async(
        addr: &str,
        username: &str,
        session_key: [u8; SESSION_KEY_LENGTH],
        on_queue: &mut dyn FnMut(Option<u32>) -> bool,
    ) -> Result<Self> {
        Self::connect_inner(addr, username, session_key, on_queue, None).await
    }

    /// [`Self::connect`] with a Warden profile in place BEFORE the handshake runs.
    ///
    /// It has to be a connect-time argument rather than a setter: the server arms Warden as soon as
    /// the session authenticates, so its first `SMSG_WARDEN_DATA` routinely arrives while
    /// `SMSG_AUTH_RESPONSE` is still being waited for. A profile installed after `connect` returns
    /// would be installed after the message it was needed for — which is exactly the hole that
    /// writing the wiring test exposed.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn connect_with_warden(
        addr: &str,
        username: &str,
        session_key: [u8; SESSION_KEY_LENGTH],
        profile: warden::WardenProfile,
    ) -> Result<Self> {
        futures_lite::future::block_on(Self::connect_inner(
            addr,
            username,
            session_key,
            &mut |_| true,
            Some(profile),
        ))
    }

    /// [`Self::connect_queued_async`] with a Warden profile in place before the handshake runs —
    /// the async twin of [`Self::connect_with_warden`], and the one the app's net lane uses because
    /// that lane is a task on the browser's event loop as well as a thread natively.
    pub async fn connect_queued_with_warden_async(
        addr: &str,
        username: &str,
        session_key: [u8; SESSION_KEY_LENGTH],
        on_queue: &mut dyn FnMut(Option<u32>) -> bool,
        profile: Option<warden::WardenProfile>,
    ) -> Result<Self> {
        Self::connect_inner(addr, username, session_key, on_queue, profile).await
    }

    /// The body both entry points share.
    async fn connect_inner(
        addr: &str,
        username: &str,
        session_key: [u8; SESSION_KEY_LENGTH],
        on_queue: &mut dyn FnMut(Option<u32>) -> bool,
        warden_profile: Option<warden::WardenProfile>,
    ) -> Result<Self> {
        let (host, port) = crate::host_port(addr, super::WORLD_PORT);
        let mut queued = false;
        let mut stream = Conn::connect(host, port)
            .await
            .context("connecting to world server")?;
        // **Nagle off** (decision 0617). VERIFIED in the reference client: `0x5bca60` calls
        // `setsockopt(s, 6 /* IPPROTO_TCP */, 1 /* TCP_NODELAY */, &1, 4)` unconditionally on its game
        // socket (WSOCK32 ordinal 21, IAT slot `0x7ff6f8`; a sibling helper at `0x43dda0` toggles the
        // same option from a bool). Rust leaves Nagle *on*, which is exactly wrong for this protocol:
        // every packet we send is a sub-MSS write on a latency-critical stream, so the kernel holds
        // each one until the server ACKs the last — coalescing our movement stream into delayed-ACK-
        // sized clumps. The de-jitter chain on the observing client then sizes its buffer to that
        // self-inflicted lateness (decision 0615). Fatal on failure: the option cannot fail on a
        // healthy connected socket, so an error here means the next handshake write is doomed anyway —
        // better one named failure than a session that silently plays at delayed-ACK cadence.
        stream
            .set_nodelay(true)
            .context("disabling Nagle (TCP_NODELAY) on the world socket")?;
        // `into_split` clears this for the streaming phase, where a quiet world is legal.
        stream
            .set_read_timeout(Some(HANDSHAKE_READ_TIMEOUT))
            .context("setting handshake read timeout")?;

        // 1. SMSG_AUTH_CHALLENGE (unencrypted) carries the server seed.
        let server_seed = match recv_packet(&mut stream, None).await? {
            ServerPacket::AuthChallenge { server_seed } => server_seed,
            other => bail!("expected SMSG_AUTH_CHALLENGE, got {}", other.name()),
        };

        let username_n =
            NormalizedString::new(username).map_err(|e| anyhow!("invalid username: {e}"))?;
        let seed = ProofSeed::new();
        let client_seed = seed.seed();
        let (client_proof, crypto) =
            seed.into_client_header_crypto(&username_n, session_key, server_seed);

        // Sent plain; header encryption starts right after. The addon block is required (cmangos
        // kicks a zero-size one); `STOCK_SECURE_ADDONS` is what a stock 1.12.1 install reports.
        let body = messages::auth_session(
            u32::from(crate::CLIENT_BUILD),
            &username.to_uppercase(),
            client_seed,
            &client_proof,
            &messages::STOCK_SECURE_ADDONS,
        );
        send_packet(&mut stream, None, opcode::CMSG_AUTH_SESSION, &body)
            .context("sending CMSG_AUTH_SESSION")?;

        let mut session = WorldSession {
            conn: stream,
            crypto,
            roster_races: Default::default(),
            chat_language: messages::LANGUAGE_COMMON,
            billing_time_rested: 0,
            tutorial_flags: None,
            addon_info: None,
            session_key,
            warden: None,
            warden_profile,
            warden_unanswered: Default::default(),
        };

        // AUTH_RESPONSE is not always first, so others are skipped; Warden data ends the connect.
        loop {
            match session.recv_async().await? {
                ServerPacket::AuthResponse {
                    result,
                    billing_time_rested,
                    ..
                } if result == messages::AUTH_OK => {
                    // The reference keeps the last AUTH_RESPONSE's billing group.
                    session.billing_time_rested = billing_time_rested.unwrap_or(0);
                    break;
                }
                // Queued, not refused: the server re-sends as we move up and ends with `AUTH_OK`.
                ServerPacket::AuthResponse {
                    result,
                    queue_position,
                    ..
                } if result == messages::AUTH_WAIT_QUEUE => {
                    if !queued {
                        queued = true;
                        session
                            .set_read_timeout(Some(QUEUE_READ_TIMEOUT))
                            .context("relaxing the read timeout for the login queue")?;
                    }
                    if !on_queue(queue_position) {
                        bail!("login queue abandoned");
                    }
                }
                ServerPacket::AuthResponse { result, .. } => {
                    return Err(WorldAuthReject { code: result }.into())
                }
                _ => continue,
            }
        }
        // Admitted: the rest of the handshake is prompt again.
        if queued {
            session
                .set_read_timeout(Some(HANDSHAKE_READ_TIMEOUT))
                .context("restoring the handshake read timeout after the queue")?;
        }

        Ok(session)
    }

    /// Rested billing minutes (`GetBillingTimeRested`); always 0 from vmangos (`World.cpp:331`).
    pub fn billing_time_rested(&self) -> u32 {
        self.billing_time_rested
    }

    /// The tutorial bank captured during the login handshake, if any.
    pub fn take_tutorial_flags(&mut self) -> Option<Vec<u8>> {
        self.tutorial_flags.take()
    }

    /// Set the socket's read timeout; `None` makes reads block.
    pub fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> Result<()> {
        self.conn
            .set_read_timeout(timeout)
            .context("setting world socket read timeout")
    }

    /// Read + decrypt + parse one server packet (blocking) — the native twin of
    /// [`Self::recv_async`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn recv(&mut self) -> Result<ServerPacket> {
        futures_lite::future::block_on(self.recv_async())
    }

    /// The `SMSG_ADDON_INFO` statuses, taken once; `None` is real, as vmangos stays silent when it
    /// rejects the addon block (`WorldSocket.cpp:447`).
    pub fn take_addon_info(&mut self) -> Option<Vec<u8>> {
        self.addon_info.take()
    }

    /// Read + decrypt + parse one server packet, answering Warden on the way through.
    ///
    /// **Warden is handled HERE, not in a loop arm**, for the same reason `SMSG_ADDON_INFO` is
    /// captured here: which read loop happens to see a packet is the server's timing, not our
    /// contract, and `recv_async` is the one place all of them go through — the native `recv`
    /// included (decision 2175).
    ///
    /// It was a loop arm, in the handshake and the roster step only, and that was a real hole: the
    /// world phase reads through `recv_async` and nothing else, so once a session could get PAST
    /// Warden it stopped answering it. A live probe against a real realm caught it — the scan round
    /// arrived ~40 s after login, two `SMSG_WARDEN_DATA` came back to the caller unhandled, and the
    /// server was answered by nobody. Unreachable before the module lane existed, because no
    /// session ever reached the world with Warden armed.
    ///
    /// A handled Warden packet is not returned: it is the transport talking to itself, and no
    /// caller has business seeing it. Refusals still surface, as the `Err` from this call.
    pub async fn recv_async(&mut self) -> Result<ServerPacket> {
        loop {
            let packet = recv_packet(&mut self.conn, Some(self.crypto.decrypter())).await?;
            if let ServerPacket::AddonInfo { statuses } = &packet {
                self.addon_info = Some(statuses.clone());
            }
            if let ServerPacket::WardenData { body } = packet {
                self.handle_warden(body)?;
                continue;
            }
            return Ok(packet);
        }
    }

    /// Send a client packet (encrypted header + plaintext body). Synchronous on every target — a
    /// write is a buffered hand-off on both bodies of the transport seam.
    fn send(&mut self, opcode: u16, body: &[u8]) -> Result<()> {
        send_packet(&mut self.conn, Some(self.crypto.encrypter()), opcode, body)
    }

    /// Declare which Warden encoding this server uses, and what this client can witness.
    ///
    /// Without it every Warden message is refused. With it, a `CHEAT_CHECKS_REQUEST` whose types the
    /// profile defines is answered; and when the profile carries a module source, a module offer is
    /// accepted and its challenge completed.
    pub fn set_warden_profile(&mut self, profile: warden::WardenProfile) {
        self.warden_profile = Some(profile);
    }

    /// Handle one `SMSG_WARDEN_DATA`.
    ///
    /// The body is decrypted first in every case, so even a refusal can NAME what arrived rather
    /// than reporting a bare "Warden" — and an opcode outside `server_op`'s range in that name is
    /// the tell for a mis-keyed cipher rather than for an exotic server.
    ///
    /// **The re-key happens after the reply is on the wire, and that ordering is load-bearing.**
    /// `Warden::HandlePacket` decrypts the whole incoming packet with its OLD `_inputCrypto` before
    /// `HandleChallengeResponse` compares the reply and re-inits both ciphers
    /// (`Warden.cpp:364-367`, `:141-142`). So our `HASH_RESULT` must go out under the old key and
    /// only then may we re-key. Doing it the other way round does not error — the server reads
    /// noise where the opcode should be, finds it is not `HASH_RESULT` while a challenge is
    /// pending, and kicks.
    fn handle_warden(&mut self, mut body: Vec<u8>) -> Result<()> {
        let crypto = self
            .warden
            .get_or_insert_with(|| warden::WardenCrypto::from_session_key(&self.session_key));
        crypto.decrypt(&mut body);
        // The scan encoding is masked with a byte derived from the session key, not sent by the
        // server, so it is read off our own cipher rather than carried in the profile.
        let xor = crypto.scan_xor();
        let message = warden::parse_server_message(&body);

        // The profile is taken out for the duration because answering a module offer MUTATES it
        // (it adopts the module's encoding) while `send_warden` needs `&mut self` too. Put back on
        // every path, including the error one, so a refused message does not also lose the profile.
        let mut profile = self.warden_profile.take();
        let decided = warden_reply(&message, xor, profile.as_mut());
        self.warden_profile = profile;

        // **We never end the session over Warden. The server does.**
        //
        // Every refusal [`warden_reply`] can produce lands here, and none of them propagates: a
        // message we cannot answer honestly is answered with silence, and whether that silence is
        // fatal is the server's call to make. The earlier policy made it ours, and that was the
        // wrong half to decide from — a Turtle-derived server was observed sending scans it never
        // enforced, so a client that bailed lost a session the server was content to keep.
        //
        // The trade is real and it is the reverse one: on a server that DOES enforce, the player
        // now gets a silent drop on the server's clock instead of one honest sentence at the login
        // screen. That is what this `warn!` is for — the log still names what was asked and what we
        // could not say about it, so the silence has an explanation even though the screen does not.
        //
        // What does NOT change is that we never fabricate: results are matched to scans
        // positionally, so a filler byte is a false answer to one specific question rather than a
        // gap. Silence is the only honest alternative to an answer.
        let reply = match decided {
            Ok(Some(reply)) => reply,
            Ok(None) => return Ok(()),
            Err(e) => {
                let reason = format!("{e:#}");
                if self.warden_unanswered.insert(reason.clone()) {
                    tracing::warn!(
                        "warden: {reason} — sending nothing and leaving the disconnect to the \
                         server, which may drop us on its own response clock (~30 s)"
                    );
                }
                return Ok(());
            }
        };
        self.send_warden(&reply.body)?;
        if let Some((client_key, server_key)) = reply.rekey {
            self.warden
                .as_mut()
                .expect("the cipher was inserted at the top of this function")
                .rekey(&client_key, &server_key);
        }
        Ok(())
    }

    /// Send one `CMSG_WARDEN_DATA`: the body is RC4'd under our outgoing key, then travels as an
    /// ordinary packet (the world header crypto is a separate layer and still applies).
    fn send_warden(&mut self, body: &[u8]) -> Result<()> {
        let mut encrypted = body.to_vec();
        self.warden
            .as_mut()
            .ok_or_else(|| anyhow!("no Warden cipher to send under"))?
            .encrypt(&mut encrypted);
        self.send(opcode::CMSG_WARDEN_DATA, &encrypted)
    }

    /// Request the character list (blocking) — the native twin of [`Self::char_enum_async`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn char_enum(&mut self) -> Result<Vec<Character>> {
        futures_lite::future::block_on(self.char_enum_async())
    }

    /// Request the character list and return it (remembering each character's race, so
    /// [`Self::player_login`] can pick the right chat tongue).
    pub async fn char_enum_async(&mut self) -> Result<Vec<Character>> {
        self.send(opcode::CMSG_CHAR_ENUM, &[])?;
        loop {
            match self.recv_async().await? {
                ServerPacket::CharEnum { characters } => {
                    self.roster_races = characters.iter().map(|c| (c.guid, c.race)).collect();
                    return Ok(characters);
                }
                // The tutorial bank, if the server sends it this early (1976): kept for the world
                // entry — skipped here it would be lost to the roster loop.
                ServerPacket::TutorialFlags(flags) => {
                    self.tutorial_flags = Some(flags.bytes);
                    continue;
                }
                // The server interleaves account-data / cache packets here; skip them.
                _ => continue,
            }
        }
    }

    /// Create a character (the create screen's request, or the create-if-empty starter that unblocks
    /// `PLAYER_LOGIN` on a fresh account). Returns the `SMSG_CHAR_CREATE` result byte (`WorldResult`)
    /// for the caller to inspect ([`messages::CHAR_CREATE_SUCCESS`], a `CHAR_NAME_*` code, …).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn create_character(&mut self, req: &messages::CharCreateReq) -> Result<u8> {
        futures_lite::future::block_on(self.create_character_async(req))
    }

    /// Create a character — the awaited twin of [`Self::create_character`].
    pub async fn create_character_async(&mut self, req: &messages::CharCreateReq) -> Result<u8> {
        self.send(opcode::CMSG_CHAR_CREATE, &messages::char_create(req))?;
        loop {
            match self.recv_async().await? {
                ServerPacket::CharCreate { result } => return Ok(result),
                _ => continue,
            }
        }
    }

    /// Delete a character (`CMSG_CHAR_DELETE`, full u64 guid — only valid at character select,
    /// never while in-world). Returns the `SMSG_CHAR_DELETE` result byte
    /// ([`messages::CHAR_DELETE_SUCCESS`] on success) for the caller to inspect.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn delete_character(&mut self, guid: u64) -> Result<u8> {
        futures_lite::future::block_on(self.delete_character_async(guid))
    }

    /// Delete a character — the awaited twin of [`Self::delete_character`].
    pub async fn delete_character_async(&mut self, guid: u64) -> Result<u8> {
        self.send(opcode::CMSG_CHAR_DELETE, &messages::full_guid(guid))?;
        loop {
            match self.recv_async().await? {
                ServerPacket::CharDelete { result } => return Ok(result),
                _ => continue,
            }
        }
    }

    /// Enter the world as `guid`, adopting its faction tongue for chat (Common if not enumerated).
    pub fn player_login(&mut self, guid: u64) -> Result<()> {
        self.chat_language = self
            .roster_races
            .get(&guid)
            .map_or(messages::LANGUAGE_COMMON, |&race| {
                messages::faction_language(race)
            });
        self.send(opcode::CMSG_PLAYER_LOGIN, &messages::full_guid(guid))
    }

    /// Declare the unit we move, as the 1.12 client does at login; vmangos drops moves until then.
    pub fn set_active_mover(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_SET_ACTIVE_MOVER, &messages::full_guid(guid))
    }

    /// [`Self::player_login`], awaited. It only writes — the awaited twin exists so the sequencer's
    /// handshake reads as one uninterrupted `.await` chain rather than switching idiom mid-stride.
    pub async fn player_login_async(&mut self, guid: u64) -> Result<()> {
        self.player_login(guid)
    }

    /// [`Self::set_active_mover`], awaited — same reason as [`Self::player_login_async`].
    pub async fn set_active_mover_async(&mut self, guid: u64) -> Result<()> {
        self.set_active_mover(guid)
    }

    /// Acknowledge a triggered cinematic as finished (`CMSG_COMPLETE_CINEMATIC`, empty body) — the
    /// unsplit twin of [`WorldWriter::complete_cinematic`], for the CLI/probe path.
    pub fn complete_cinematic(&mut self) -> Result<()> {
        self.send(opcode::CMSG_COMPLETE_CINEMATIC, &[])
    }

    /// Start walking forward (`MSG_MOVE_START_FORWARD`).
    pub fn start_forward(&mut self, pos: [f32; 3], orientation: f32) -> Result<()> {
        self.send(
            opcode::MSG_MOVE_START_FORWARD,
            &messages::movement(&movement_info(pos, orientation, MOVEMENT_FLAG_FORWARD)),
        )
    }

    /// Continue moving (`MSG_MOVE_HEARTBEAT`).
    pub fn heartbeat(&mut self, pos: [f32; 3], orientation: f32) -> Result<()> {
        self.send(
            opcode::MSG_MOVE_HEARTBEAT,
            &messages::movement(&movement_info(pos, orientation, MOVEMENT_FLAG_FORWARD)),
        )
    }

    /// Stop (`MSG_MOVE_STOP`, no movement flags).
    pub fn stop(&mut self, pos: [f32; 3], orientation: f32) -> Result<()> {
        self.send(
            opcode::MSG_MOVE_STOP,
            &messages::movement(&movement_info(pos, orientation, 0)),
        )
    }

    /// Ack a `SMSG_FORCE_*_SPEED_CHANGE` at rest with the full guid, counter and exact speed.
    pub fn force_speed_ack(
        &mut self,
        kind: messages::SpeedKind,
        guid: u64,
        counter: u32,
        speed: f32,
        pos: [f32; 3],
        orientation: f32,
    ) -> Result<()> {
        self.send(
            kind.ack_opcode(),
            &messages::force_speed_ack(guid, counter, &movement_info(pos, orientation, 0), speed),
        )
    }

    /// Ack a finished self `SMSG_MONSTER_MOVE` at its endpoint (`CMSG_MOVE_SPLINE_DONE`).
    pub fn move_spline_done(
        &mut self,
        pos: [f32; 3],
        orientation: f32,
        spline_id: u32,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_MOVE_SPLINE_DONE,
            &messages::move_spline_done(&movement_info(pos, orientation, 0), spline_id),
        )
    }

    /// Ask a player's name (`CMSG_NAME_QUERY`), answered by `SMSG_NAME_QUERY_RESPONSE`.
    pub fn name_query(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_NAME_QUERY, &messages::full_guid(guid))
    }

    /// Ask a creature template's name (`CMSG_CREATURE_QUERY`).
    pub fn creature_query(&mut self, entry: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_CREATURE_QUERY,
            &messages::creature_query(entry, guid),
        )
    }

    /// Cast a spell (`CMSG_CAST_SPELL`), `None` targeting self; answered by `SMSG_CAST_RESULT`.
    pub fn cast_spell(&mut self, spell_id: u32, target: Option<u64>) -> Result<()> {
        self.send(
            opcode::CMSG_CAST_SPELL,
            &messages::cast_spell(spell_id, target),
        )
    }

    /// Cast a spell at a ground point in world coords (`TARGET_FLAG_DEST_LOCATION`).
    pub fn cast_spell_at_dest(&mut self, spell_id: u32, dest: [f32; 3]) -> Result<()> {
        self.send(
            opcode::CMSG_CAST_SPELL,
            &messages::cast_spell_at_dest(spell_id, dest),
        )
    }

    /// Cast an OPEN_LOCK spell (e.g. 3365 Opening, 2575 Mining) at a GameObject.
    pub fn cast_spell_gameobject(&mut self, spell_id: u32, go_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_CAST_SPELL,
            &messages::cast_spell_gameobject(spell_id, go_guid),
        )
    }

    /// Ask an item template (`CMSG_ITEM_QUERY_SINGLE`).
    pub fn item_query(&mut self, entry: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_ITEM_QUERY_SINGLE,
            &messages::item_query(entry, guid),
        )
    }

    /// Use an item by bag position (`CMSG_USE_ITEM`).
    pub fn use_item(&mut self, bag_index: u8, slot: u8, spell_slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_USE_ITEM,
            &messages::use_item(
                bag_index,
                slot,
                spell_slot,
                messages::UseItemTarget::default(),
            ),
        )
    }

    /// Open an item by bag position (`CMSG_OPEN_ITEM`).
    pub fn open_item(&mut self, bag_index: u8, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_OPEN_ITEM,
            &messages::open_item(bag_index, slot),
        )
    }

    /// Equip a bag item (`CMSG_AUTOEQUIP_ITEM`).
    pub fn auto_equip_item(&mut self, bag_index: u8, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOEQUIP_ITEM,
            &messages::auto_equip_item(bag_index, slot),
        )
    }

    /// Swap two player-array slots (`CMSG_SWAP_INV_ITEM`).
    pub fn swap_inv_item(&mut self, src_slot: u8, dst_slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_SWAP_INV_ITEM,
            &messages::swap_inv_item(src_slot, dst_slot),
        )
    }

    /// Ask a vendor's stock (`CMSG_LIST_INVENTORY`), answered by `SMSG_LIST_INVENTORY`.
    pub fn list_inventory(&mut self, vendor_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_LIST_INVENTORY,
            &messages::list_inventory(vendor_guid),
        )
    }

    /// Buy from a vendor (`CMSG_BUY_ITEM`); `entry` is the item template, not the row's `muid`.
    pub fn buy_item(&mut self, vendor_guid: u64, entry: u32, count: u8) -> Result<()> {
        self.send(
            opcode::CMSG_BUY_ITEM,
            &messages::buy_item(vendor_guid, entry, count),
        )
    }

    /// Buy into a named container slot (`CMSG_BUY_ITEM_IN_SLOT`), the merchant cursor's drop.
    pub fn buy_item_in_slot(
        &mut self,
        vendor_guid: u64,
        entry: u32,
        bag_guid: u64,
        bag_slot: u8,
        count: u8,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_BUY_ITEM_IN_SLOT,
            &messages::buy_item_in_slot(vendor_guid, entry, bag_guid, bag_slot, count),
        )
    }

    /// Sell an item to a vendor (`CMSG_SELL_ITEM`); `count` 0 sells the whole stack.
    pub fn sell_item(&mut self, vendor_guid: u64, item_guid: u64, count: u8) -> Result<()> {
        self.send(
            opcode::CMSG_SELL_ITEM,
            &messages::sell_item(vendor_guid, item_guid, count),
        )
    }

    /// Buy a sold item back (`CMSG_BUYBACK_ITEM`); `slot` is the absolute buyback slot 69-80.
    pub fn buyback_item(&mut self, vendor_guid: u64, slot: u32) -> Result<()> {
        self.send(
            opcode::CMSG_BUYBACK_ITEM,
            &messages::buyback_item(vendor_guid, slot),
        )
    }

    /// Repair at a vendor (`CMSG_REPAIR_ITEM`); `item_guid` 0 repairs everything.
    pub fn repair_item(&mut self, vendor_guid: u64, item_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_REPAIR_ITEM,
            &messages::repair_item(vendor_guid, item_guid),
        )
    }

    /// Open the bank at a banker (`CMSG_BANKER_ACTIVATE`), answered by `SMSG_SHOW_BANK`.
    pub fn banker_activate(&mut self, banker_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_BANKER_ACTIVATE,
            &messages::banker_activate(banker_guid),
        )
    }

    /// Buy the next bank bag slot; success is silent, failure answers `SMSG_BUY_BANK_SLOT_RESULT`.
    pub fn buy_bank_slot(&mut self, banker_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_BUY_BANK_SLOT,
            &messages::buy_bank_slot(banker_guid),
        )
    }

    /// Deposit the item at `(bag, slot)` into the first free bank slot (`CMSG_AUTOBANK_ITEM`).
    pub fn autobank_item(&mut self, bag: u8, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOBANK_ITEM,
            &messages::autobank_item(bag, slot),
        )
    }

    /// Withdraw a bank position, or deposit any other (`CMSG_AUTOSTORE_BANK_ITEM`).
    pub fn autostore_bank_item(&mut self, bag: u8, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOSTORE_BANK_ITEM,
            &messages::autostore_bank_item(bag, slot),
        )
    }

    /// Ask an NPC's overhead `!`/`?` status (`CMSG_QUESTGIVER_STATUS_QUERY`).
    pub fn questgiver_status_query(&mut self, npc: u64) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_STATUS_QUERY,
            &messages::questgiver_status_query(npc),
        )
    }

    /// Open a questgiver dialog (`CMSG_QUESTGIVER_HELLO`), the server's gossip-hello path.
    pub fn questgiver_hello(&mut self, npc: u64) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_HELLO,
            &messages::questgiver_hello(npc),
        )
    }

    /// Ask a quest's detail panel (`CMSG_QUESTGIVER_QUERY_QUEST`).
    pub fn questgiver_query_quest(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_QUERY_QUEST,
            &messages::questgiver_query_quest(npc, quest),
        )
    }

    /// Accept a quest (`CMSG_QUESTGIVER_ACCEPT_QUEST`); the server closes the gossip window.
    pub fn questgiver_accept_quest(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_ACCEPT_QUEST,
            &messages::questgiver_accept_quest(npc, quest),
        )
    }

    /// Ask a quest's turn-in panel: `REQUEST_ITEMS`, or `OFFER_REWARD` when nothing is required.
    pub fn questgiver_complete_quest(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_COMPLETE_QUEST,
            &messages::questgiver_complete_quest(npc, quest),
        )
    }

    /// Advance to the reward panel (`CMSG_QUESTGIVER_REQUEST_REWARD`).
    pub fn questgiver_request_reward(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_REQUEST_REWARD,
            &messages::questgiver_request_reward(npc, quest),
        )
    }

    /// Choose reward index `reward` and finish the quest (`CMSG_QUESTGIVER_CHOOSE_REWARD`).
    pub fn questgiver_choose_reward(&mut self, npc: u64, quest: u32, reward: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_CHOOSE_REWARD,
            &messages::questgiver_choose_reward(npc, quest, reward),
        )
    }

    /// Ask a quest's full template by id alone (`CMSG_QUEST_QUERY`).
    pub fn quest_query(&mut self, quest_id: u32) -> Result<()> {
        self.send(opcode::CMSG_QUEST_QUERY, &messages::quest_query(quest_id))
    }

    /// Ask the server's unix-seconds clock, the epoch of timed-quest deadlines (`CMSG_QUERY_TIME`).
    pub fn query_time(&mut self) -> Result<()> {
        self.send(opcode::CMSG_QUERY_TIME, &messages::query_time())
    }

    /// Abandon a quest-log slot; no reply, the server clears the `PLAYER_QUEST_LOG` fields.
    pub fn questlog_remove_quest(&mut self, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTLOG_REMOVE_QUEST,
            &messages::questlog_remove_quest(slot),
        )
    }

    /// Send a `/say` line, GM dot-commands included, in the character's own tongue.
    pub fn send_chat(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(messages::CHAT_TYPE_SAY, self.chat_language, message),
        )
    }

    /// Send a chat line on any lane; `target` is the whisper target or channel name.
    pub fn send_chat_kind(
        &mut self,
        chat_type: u32,
        target: Option<&str>,
        message: &str,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat_kind(chat_type, self.chat_language, target, message),
        )
    }

    /// Join a channel; `password` is empty for a channel that has none.
    pub fn join_channel(&mut self, name: &str, password: &str) -> Result<()> {
        self.send(
            opcode::CMSG_JOIN_CHANNEL,
            &messages::join_channel(name, password),
        )
    }

    /// Leave a channel.
    pub fn leave_channel(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_LEAVE_CHANNEL, &messages::leave_channel(name))
    }

    /// Invite a player to our group by name (`CMSG_GROUP_INVITE`).
    pub fn group_invite(&mut self, member_name: &str) -> Result<()> {
        self.send(
            opcode::CMSG_GROUP_INVITE,
            &messages::group_invite(member_name),
        )
    }

    /// Accept the group invite we were just offered (`CMSG_GROUP_ACCEPT`, empty body).
    pub fn group_accept(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GROUP_ACCEPT, &messages::group_accept())
    }

    /// Leave or disband our group (`CMSG_GROUP_DISBAND`, empty body).
    pub fn group_disband(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GROUP_DISBAND, &messages::group_disband())
    }

    /// Send an addon message: [`messages::LANGUAGE_ADDON`] as the language, `target` the channel
    /// name on [`messages::CHAT_TYPE_CHANNEL`]. vmangos drops it unless `AddonChannel` is on and
    /// the lane allows it. Probe-only: benilla runs no third-party addons.
    pub fn send_addon_message(
        &mut self,
        chat_type: u32,
        target: Option<&str>,
        text: &str,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat_kind(chat_type, messages::LANGUAGE_ADDON, target, text),
        )
    }

    /// Echo a same-map teleport ack; without it the server freezes our movement.
    pub fn teleport_ack(&mut self, guid: u64, counter: u32) -> Result<()> {
        self.send(
            opcode::MSG_MOVE_TELEPORT_ACK,
            &messages::teleport_ack(guid, counter, client_uptime_ms()),
        )
    }

    /// Ack a cross-map worldport (empty body); without it nothing on the new map is streamed.
    pub fn worldport_ack(&mut self) -> Result<()> {
        self.send(opcode::MSG_MOVE_WORLDPORT_ACK, &[])
    }

    /// Ack a granted mover mode with the counter and current `pose`; nothing applies until then.
    pub fn move_mode_ack(
        &mut self,
        guid: u64,
        counter: u32,
        mode: MoveMode,
        apply: bool,
        flags: u32,
        pose: ([f32; 3], f32),
    ) -> Result<()> {
        let info = movement_info(pose.0, pose.1, flags);
        let trailing = mode.ack_carries_apply().then_some(apply);
        self.send(
            mode.ack_opcode(apply),
            &messages::move_flag_ack(guid, counter, &info, trailing),
        )
    }

    /// Release the spirit (`CMSG_REPOP_REQUEST`, empty body).
    pub fn repop_request(&mut self) -> Result<()> {
        self.send(opcode::CMSG_REPOP_REQUEST, &[])
    }

    /// Ask where our corpse is (`MSG_CORPSE_QUERY`, empty body), answered on the same opcode.
    pub fn corpse_query(&mut self) -> Result<()> {
        self.send(opcode::MSG_CORPSE_QUERY, &[])
    }

    /// Self-resurrect; the server casts `PLAYER_SELF_RES_SPELL` and zeroes it.
    pub fn self_res(&mut self) -> Result<()> {
        self.send(opcode::CMSG_SELF_RES, &[])
    }

    /// Take the Spirit Healer's resurrection: 50% health and a 25% durability loss.
    pub fn spirit_healer_activate(&mut self, npc: u64) -> Result<()> {
        self.send(
            opcode::CMSG_SPIRIT_HEALER_ACTIVATE,
            &messages::spirit_healer_activate(npc),
        )
    }

    /// Start melee auto-attack (`CMSG_ATTACKSWING`), echoed as `SMSG_ATTACKSTART`.
    pub fn attack_swing(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_ATTACKSWING, &messages::attack_swing(guid))
    }

    /// Set our target (`CMSG_SET_SELECTION`, full guid); the client selects before it casts.
    pub fn set_selection(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_SET_SELECTION, &messages::full_guid(guid))
    }

    /// Open a loot window (`CMSG_LOOT`), answered by `SMSG_LOOT_RESPONSE`.
    pub fn loot(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_LOOT, &messages::loot(guid))
    }

    /// Use a world GameObject; the server answers by type, or not at all.
    pub fn gameobj_use(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_GAMEOBJ_USE, &messages::gameobj_use(guid))
    }

    /// Ask a GameObject template (`CMSG_GAMEOBJECT_QUERY`).
    pub fn gameobject_query(&mut self, entry: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_GAMEOBJECT_QUERY,
            &messages::gameobject_query(entry, guid),
        )
    }

    /// Take one loot row; `loot_slot` is the 0-based row of the `SMSG_LOOT_RESPONSE`.
    pub fn autostore_loot_item(&mut self, loot_slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOSTORE_LOOT_ITEM,
            &messages::autostore_loot_item(loot_slot),
        )
    }

    /// Take the loot's coin (`CMSG_LOOT_MONEY`, empty body).
    pub fn loot_money(&mut self) -> Result<()> {
        self.send(opcode::CMSG_LOOT_MONEY, &messages::loot_money())
    }

    /// Close the loot window; the server ignores `guid` and releases its own loot target.
    pub fn loot_release(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_LOOT_RELEASE, &messages::loot_release(guid))
    }

    /// Split into a reader and a writer on cloned sockets, after [`Self::player_login`].
    pub fn into_split(self) -> Result<(WorldReader, WorldWriter)> {
        // The handshake is over — clear its read timeout (decision 0065): the streaming phase reads
        // block indefinitely (a quiet world is legal; connection liveness is the caller's loop).
        self.conn
            .set_read_timeout(None)
            .context("clearing handshake read timeout")?;
        let (reader, writer) = self.conn.split().context("splitting the world connection")?;
        let (encrypter, decrypter) = self.crypto.split();
        Ok((
            WorldReader { reader, decrypter },
            WorldWriter {
                writer,
                encrypter,
                chat_language: self.chat_language,
                sent: None,
            },
        ))
    }

    /// Cleanly leave the world back to character-select: send `CMSG_LOGOUT_REQUEST` and wait for
    /// `SMSG_LOGOUT_COMPLETE` (instant for GM / rested). The server persists the character on logout,
    /// so a follow-up [`Self::char_enum`] reflects the saved position. Requires a read timeout (so the
    /// poll loop can give up); returns once logged out or `timeout` elapses.
    ///
    /// Native only: the deadline is a `std::time::Instant`, which a browser has no clock for, and
    /// the in-app logout is the writer's `CMSG_LOGOUT_REQUEST` + the streamed `SMSG_LOGOUT_COMPLETE`
    /// rather than this synchronous wait. The CLIs and probes are its only callers.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn logout(&mut self, timeout: std::time::Duration) -> Result<()> {
        self.send(opcode::CMSG_LOGOUT_REQUEST, &[])?;
        let deadline = std::time::Instant::now() + timeout;
        let mut denied = false;
        while std::time::Instant::now() < deadline {
            match self.recv() {
                Ok(ServerPacket::LogoutComplete) => return Ok(()),
                Ok(ServerPacket::LogoutResponse { reason, .. }) => {
                    // A non-zero reason means the server refused (combat, falling, GM-frozen).
                    denied = reason != messages::LOGOUT_SUCCESS;
                }
                Ok(_) => {}
                Err(_) => {} // a read-timeout tick; poll until the deadline
            }
        }
        if denied {
            bail!("logout refused by server (in combat / not allowed)")
        }
        bail!("timed out waiting for SMSG_LOGOUT_COMPLETE")
    }
}
