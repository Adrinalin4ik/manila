//! Composited body skins, one 256² atlas per look. An arriving body's first composite runs off the
//! main thread: the reference's world composite is unforced, so its section loads poll and never
//! block the frame (`0x44b430`), and it draws nothing of a unit whose first composite has not
//! finished, its ShouldRender answering `0x477860(cc, 0)`'s result (`0x607e7c`). Only the forced
//! callers wait on their loads (`0x44ad50`): the glue model (`0x470c59`, `0x471308`, `0x4731b6`),
//! the dressing room (`0x504485`), `PlayerModel`'s `SetUnit` (`0x5059be`) and world entry
//! (`0x49091e`). Here the forced lane ([`SkinComposites::force`]) composites on the calling thread.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use benilla_assets::{repeat_texture_authored, LockRecover, SpatialCache};
use benilla_formats::{blp_bytes_to_mip_chain, BlpMipChain, Chain, CharSections, CompositePlan};
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};

/// The `CharSections` skin lookup and the chain its textures read from; without it a player's body
/// skin stays untextured.
#[derive(Resource)]
pub(super) struct SkinSections {
    pub(super) tables: CharSections,
    chain: Arc<Mutex<Chain>>,
}

impl SkinSections {
    pub(super) fn new(tables: CharSections, chain: Arc<Mutex<Chain>>) -> Self {
        Self { tables, chain }
    }
}

/// What decides a composited body skin: race and sex pick the `CharSections` rows, the dials pick
/// the variations, and `equip` holds the worn armour display ids by body slot − 2.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct SkinKey {
    pub(super) race: u8,
    pub(super) sex: u8,
    pub(super) skin: u8,
    pub(super) face: u8,
    pub(super) facial_hair: u8,
    pub(super) hair_style: u8,
    pub(super) hair_color: u8,
    pub(super) equip: [u32; 8],
    /// The guild emblem: two guilds' members wear one tabard display but must not share an atlas.
    pub(super) emblem: Option<benilla_formats::GuildEmblem>,
    /// The tabard designer's preview: the emblem paints over an empty tabard slot.
    pub(super) tabard_preview: bool,
}

/// Where a body's atlas stands.
#[derive(Debug, PartialEq)]
pub(super) enum BodyAtlas {
    /// Ready to bind; `None` for a look with no atlas (no base skin row, or art that never read).
    Ready(Option<Handle<Image>>),
    /// Its composite is running.
    Pending,
}

/// A composite's whole work, run where it lands: reads, decodes and blits.
// `+ Sync` is a fork carry: the wasm arm KEEPS one of these as a fallback inside the resource
// (see `Running::Worker`), and a bevy `Resource` must be `Sync`. Upstream only ever moves one
// into a task. What it captures - a plan and the shared chain - is `Sync` already.
type Work = Box<dyn FnOnce() -> Option<BlpMipChain> + Send + Sync>;

/// Composited body skins by look, so every body wearing a look shares one atlas, and the
/// composites still running.
#[derive(Resource, Default)]
pub(super) struct SkinComposites {
    /// Finished atlases, swept by distance ([`benilla_world::art_scope`]); `None` for a look with
    /// no atlas, so it is never retried.
    pub(super) done: SpatialCache<SkinKey, Option<Handle<Image>>>,
    /// Composites in flight, one per look however many bodies wait on it.
    running: HashMap<SkinKey, Running>,
    /// The Worker's request ids (wasm32 only; the pool needs none).
    next_id: u32,
}

/// **`/console skinComposite 0`** - composite at the request, on this thread, as the client did
/// before this lane existed.
///
/// A measuring lever, and the reason it exists is a measurement I got wrong: I compared the frame
/// across two sessions and reported a 3.8 ms win that a second run of the same build refuted
/// (22.0 and 22.8 ms before, 18.5 then 22.9 after). This harness's own README says why - a
/// comparison across runs measures the afternoon, not the code - and a build cannot be A/B'd
/// inside one session. A CVar can: `ab.mjs` runs both legs a minute apart on one crowd, which is
/// the only comparison that has ever held here.
///
/// Off, the work runs where the request is made, so its cost lands on the drawing thread exactly
/// as it used to. The plan, the blits and the atlas are otherwise identical, so the difference
/// between the legs is the lane and nothing else.
static INLINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set by the CVar; `on` = the off-thread lane, its default.
pub(crate) fn set_off_thread(on: bool) {
    INLINE.store(!on, std::sync::atomic::Ordering::Relaxed);
}

/// A composite in flight, and where.
enum Running {
    /// The async compute pool - the right answer wherever bevy has threads.
    Pool(Task<Option<BlpMipChain>>),
    /// **The browser's arm, and the reason it exists.** bevy hard-disables its multi-threaded
    /// executor on wasm32 (`bevy_tasks/src/lib.rs:21`), so a task on the compute pool there is
    /// still the main thread: it spreads the work over frames, which already removes the one-frame
    /// stall, but it does not take a microsecond off the drawing thread. A real Worker does, and
    /// this fork has one (`entities::attach::skin_worker`, `crates/manila-skin`).
    ///
    /// `fallback` is the same work as the pool arm would have run, kept because a Worker that
    /// starts and then gives up must not leave a body with no face - it answers with an empty
    /// buffer and this finishes the job here, which is what happened before the Worker existed.
    #[cfg(target_arch = "wasm32")]
    Worker { id: u32, fallback: Work },
}

impl SkinComposites {
    /// A world body's atlas: finished, or running, its composite started here on a miss. `plan` is
    /// read only then; `None` is a look with no atlas.
    pub(super) fn request(
        &mut self,
        key: SkinKey,
        sections: &SkinSections,
        plan: impl FnOnce() -> Option<CompositePlan>,
    ) -> BodyAtlas {
        if let Some(done) = self.done.fetch(&key) {
            return BodyAtlas::Ready(done);
        }
        if self.running.contains_key(&key) {
            return BodyAtlas::Pending;
        }
        let Some(plan) = plan() else {
            self.done.insert(key, None);
            return BodyAtlas::Ready(None);
        };
        let running = self.start(plan, sections.chain.clone());
        self.running.insert(key, running);
        BodyAtlas::Pending
    }

    /// Where a composite goes: the Worker in a browser, the compute pool everywhere else - and the
    /// pool in a browser too when there is no Worker to take it (an older page, or one that could
    /// not start one). See [`Running::Worker`].
    #[cfg_attr(not(target_arch = "wasm32"), expect(unused_mut, reason = "the wasm arm bumps it"))]
    fn start(&mut self, plan: CompositePlan, chain: Arc<Mutex<Chain>>) -> Running {
        // The lever: composite here and hand the finished atlas to an already-resolved task, so
        // `land` installs it unchanged and only the COST moves. See `INLINE`.
        if INLINE.load(std::sync::atomic::Ordering::Relaxed) {
            let atlas = work(plan, chain)();
            return Running::Pool(AsyncComputeTaskPool::get().spawn(async move { atlas }));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let id = self.next_id;
            if crate::entities::attach::skin_worker::post(id, &plan).is_some() {
                self.next_id = self.next_id.wrapping_add(1);
                return Running::Worker {
                    id,
                    fallback: work(plan, chain),
                };
            }
        }
        Running::Pool(AsyncComputeTaskPool::get().spawn({
            let work = work(plan, chain);
            async move { work() }
        }))
    }

    /// [`Self::request`] over any work, so the lane is testable without the install.
    fn request_with(&mut self, key: SkinKey, work: impl FnOnce() -> Option<Work>) -> BodyAtlas {
        if let Some(done) = self.done.fetch(&key) {
            return BodyAtlas::Ready(done);
        }
        if self.running.contains_key(&key) {
            return BodyAtlas::Pending;
        }
        let Some(work) = work() else {
            self.done.insert(key, None);
            return BodyAtlas::Ready(None);
        };
        self.running.insert(
            key,
            Running::Pool(AsyncComputeTaskPool::get().spawn(async move { work() })),
        );
        BodyAtlas::Pending
    }

    /// An atlas composited on this thread on a miss, or waited for if it is already running, as
    /// the reference's forced composite waits on its section loads (`0x44ad50`) and passes no
    /// admission test: the glue model's (`0x477860(cc, 1)` at `0x470c59`, `0x471308`, `0x4731b6`)
    /// and the dressing room's (`0x504485`, in `0x504470`). A re-dress of a standing body and a
    /// rig-heal rebuild come here too, so neither drops out for a frame.
    pub(super) fn force(
        &mut self,
        key: SkinKey,
        sections: &SkinSections,
        plan: impl FnOnce() -> Option<CompositePlan>,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        self.force_with(
            key,
            || plan().map(|p| work(p, sections.chain.clone())),
            images,
        )
    }

    /// [`Self::force`] over any work, so the lane is testable without the install.
    fn force_with(
        &mut self,
        key: SkinKey,
        work: impl FnOnce() -> Option<Work>,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        if let Some(done) = self.done.fetch(&key) {
            return done;
        }
        // The forced lane pays on this thread by design, so it is the same meter - see `land`.
        let started = bevy::platform::time::Instant::now();
        let atlas = match self.running.remove(&key) {
            Some(Running::Pool(task)) => block_on(task),
            // Forced means "be right now", and the Worker's answer is not here: do it on this
            // thread, which is what every forced caller in the reference does anyway.
            #[cfg(target_arch = "wasm32")]
            Some(Running::Worker { fallback, .. }) => fallback(),
            None => work().and_then(|w| w()),
        };
        crate::perf::journal::note_skin_composite(started.elapsed().as_micros() as u64);
        self.install(key, atlas, images)
    }

    /// Move every finished composite into [`Self::done`]; how many landed.
    ///
    /// **Metered into `skins_new`/`skin_us`.** That pair means one thing and has to keep meaning
    /// it: microseconds of the DRAWING thread spent on skins. Before this lane it was the whole
    /// composite; now it is the decode of a finished buffer and its upload, which is the only part
    /// that still happens here - and a fall in the column IS the change, so it must not go
    /// unmeasured. A frame that lands nothing takes no sample: a zero among real numbers drags a
    /// median that `skins_new` is supposed to be the denominator of.
    pub(super) fn land(&mut self, images: &mut Assets<Image>) -> usize {
        let started = bevy::platform::time::Instant::now();
        let mut finished = Vec::new();
        self.running.retain(|key, running| match running {
            Running::Pool(task) => match block_on(future::poll_once(task)) {
                Some(atlas) => {
                    finished.push((*key, atlas));
                    false
                }
                None => true,
            },
            #[cfg(target_arch = "wasm32")]
            Running::Worker { id, fallback } => {
                match crate::entities::attach::skin_worker::collect(*id) {
                    None => true,
                    // Empty: the Worker gave up. Finish it here rather than leave a faceless body.
                    Some(bytes) if bytes.is_empty() => {
                        let run = std::mem::replace(fallback, Box::new(|| None));
                        finished.push((*key, run()));
                        false
                    }
                    Some(bytes) => {
                        finished.push((*key, benilla_formats::decode_atlas(&bytes)));
                        false
                    }
                }
            }
        });
        let landed = finished.len();
        if landed == 0 {
            return 0;
        }
        for (key, atlas) in finished {
            self.install(key, atlas, images);
        }
        crate::perf::journal::note_skin_composite(started.elapsed().as_micros() as u64);
        landed
    }

    /// Upload one atlas and cache it by look.
    fn install(
        &mut self,
        key: SkinKey,
        atlas: Option<BlpMipChain>,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        // Through the upload gate like every texture: a no-op on this RGBA8 composite, but it
        // keeps the format and the bytes in agreement.
        let handle = atlas.map(|a| {
            images.add(repeat_texture_authored(
                benilla_assets::for_upload(a),
                (true, true),
            ))
        });
        self.done.insert(key, handle.clone());
        handle
    }

    /// How many composites are running.
    #[cfg(test)]
    pub(super) fn running(&self) -> usize {
        self.running.len()
    }

    /// Drop every atlas and every running composite: the map-change teardown.
    pub(super) fn clear(&mut self) {
        self.done.clear();
        self.running.clear();
    }
}

/// A plan's work over the shared chain, which each read locks for its archive lookup alone.
fn work(plan: CompositePlan, chain: Arc<Mutex<Chain>>) -> Work {
    Box::new(move || {
        plan.run(|path| read_texture(&chain, path).ok().map(Arc::new))
            .inspect_err(|e| warn!("body skin composite failed: {e:#}"))
            .ok()
    })
}

/// Read and decode one BLP off `chain`, locked only to find its archive.
#[cfg(not(target_arch = "wasm32"))]
fn read_texture(chain: &Mutex<Chain>, path: &str) -> anyhow::Result<BlpMipChain> {
    let name = path.replace('/', "\\");
    let archive = chain.lock_recover().archive_for(&name)?;
    let bytes = archive
        .read_file(&name)
        .with_context(|| format!("reading texture '{name}'"))?;
    blp_bytes_to_mip_chain(&bytes).with_context(|| format!("decoding texture '{name}'"))
}

/// The browser has no archive to open: the chain is the host's `/data` route and a read is one
/// request. Only the FORCED and fallback paths come here - an arriving body's composite goes to
/// the Worker, which fetches and blits without touching this thread at all.
#[cfg(target_arch = "wasm32")]
fn read_texture(chain: &Mutex<Chain>, path: &str) -> anyhow::Result<BlpMipChain> {
    let name = path.replace('/', "\\");
    let bytes = chain
        .lock_recover()
        .read(&name)
        .with_context(|| format!("reading texture '{name}'"))?;
    blp_bytes_to_mip_chain(&bytes).with_context(|| format!("decoding texture '{name}'"))
}

/// Land every finished composite before this frame's bodies ask for theirs.
pub(super) fn land_skin_composites(
    mut composites: ResMut<SkinComposites>,
    mut images: ResMut<Assets<Image>>,
) {
    composites.land(&mut images);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn key(race: u8) -> SkinKey {
        SkinKey {
            race,
            sex: 0,
            skin: 0,
            face: 0,
            facial_hair: 0,
            hair_style: 0,
            hair_color: 0,
            equip: [0; 8],
            emblem: None,
            tabard_preview: false,
        }
    }

    /// A 1×1 atlas.
    fn atlas() -> BlpMipChain {
        BlpMipChain {
            width: 1,
            height: 1,
            texels: benilla_formats::BlpTexels::Rgba8Unorm,
            mips: vec![vec![255; 4]],
        }
    }

    /// Work that finishes when the test says so, and counts how many times it ran. It gives up
    /// after two seconds, so work run inline on the test's thread fails the test, never hangs it.
    fn gated(ran: &Arc<std::sync::atomic::AtomicUsize>) -> (mpsc::Sender<()>, Work) {
        let (tx, rx) = mpsc::channel::<()>();
        let ran = ran.clone();
        let work: Work = Box::new(move || {
            ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            rx.recv_timeout(std::time::Duration::from_secs(2)).ok()?;
            Some(atlas())
        });
        (tx, work)
    }

    /// Land until `key` is done, or give up after two seconds.
    fn land_until(c: &mut SkinComposites, images: &mut Assets<Image>, key: SkinKey) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while c.running.contains_key(&key) && std::time::Instant::now() < deadline {
            c.land(images);
            std::thread::yield_now();
        }
    }

    /// A look's composite runs once off the main thread however many bodies ask, and every body
    /// gets the one atlas once it lands.
    #[test]
    fn a_look_composites_once_off_the_main_thread() {
        AsyncComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new);
        let mut images = Assets::<Image>::default();
        let mut c = SkinComposites::default();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (tx, w) = gated(&ran);
        let mut w = Some(w);

        assert_eq!(c.request_with(key(1), || w.take()), BodyAtlas::Pending);
        // A second body of the same look waits on the same composite; its work is never built.
        assert_eq!(
            c.request_with(key(1), || panic!(
                "a running look starts no second composite"
            )),
            BodyAtlas::Pending
        );
        assert_eq!(c.running(), 1);
        assert_eq!(
            c.land(&mut images),
            0,
            "nothing lands before the work finishes"
        );

        tx.send(()).unwrap();
        land_until(&mut c, &mut images, key(1));
        let BodyAtlas::Ready(Some(first)) = c.request_with(key(1), || None) else {
            panic!("the landed atlas is ready");
        };
        assert_eq!(
            c.request_with(key(1), || None),
            BodyAtlas::Ready(Some(first))
        );
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(c.running(), 0);
    }

    /// A forced composite of a look already running waits for that composite rather than starting
    /// a second, and the atlas is the one every later request gets.
    #[test]
    fn a_forced_composite_takes_over_the_running_one() {
        AsyncComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new);
        let mut images = Assets::<Image>::default();
        let mut c = SkinComposites::default();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (tx, w) = gated(&ran);
        let mut w = Some(w);
        assert_eq!(c.request_with(key(3), || w.take()), BodyAtlas::Pending);
        tx.send(()).unwrap();
        let forced = c.force_with(
            key(3),
            || panic!("a running look starts no second composite"),
            &mut images,
        );
        assert!(forced.is_some(), "the running composite's atlas");
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(c.running(), 0);
        assert_eq!(c.request_with(key(3), || None), BodyAtlas::Ready(forced));
    }

    /// A look with no atlas is ready at once, as `None`, and never retried.
    #[test]
    fn a_look_without_an_atlas_never_waits() {
        let mut c = SkinComposites::default();
        assert_eq!(c.request_with(key(9), || None), BodyAtlas::Ready(None));
        assert_eq!(
            c.request_with(key(9), || panic!("a look with no atlas is never retried")),
            BodyAtlas::Ready(None)
        );
        assert_eq!(c.running(), 0);
    }

    /// The map-change teardown drops running composites with the cache: a body still waiting
    /// starts its composite again.
    #[test]
    fn a_teardown_drops_the_running_composites() {
        AsyncComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new);
        let mut c = SkinComposites::default();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (_tx, w) = gated(&ran);
        let mut w = Some(w);
        assert_eq!(c.request_with(key(2), || w.take()), BodyAtlas::Pending);
        c.clear();
        assert_eq!(c.running(), 0);
        let (_tx2, w2) = gated(&ran);
        let mut w2 = Some(w2);
        assert_eq!(c.request_with(key(2), || w2.take()), BodyAtlas::Pending);
        assert!(
            w2.is_none(),
            "a body still waiting starts its composite again"
        );
    }
}
