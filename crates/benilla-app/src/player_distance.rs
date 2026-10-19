//! **`playerDistance` — how far away another player's model is still drawn.** Ours; 1.12 has no
//! such CVar, and no 1.12 machine had to draw eight hundred of them in one street.
//!
//! Why it exists, as a measurement and as a setting:
//!
//! Twenty journals failed to price the crowd, because the crowd is not something a measurement can
//! hold still. The same spot gave 739 players one run and 947 the next, and the machine itself ran
//! up to 1.85x slower between runs — so a comparison ACROSS runs measured the afternoon, not the
//! code. What was missing was a way to change one thing inside ONE run, on one machine, a minute
//! apart. A distance wall is that: slide it to 0, the crowd stops being drawn, and the difference
//! is the cost of drawing it. Nothing else moves.
//!
//! And it is the honest shipping answer to "it should be fast on any machine". Other players are
//! the one part of a city a client cannot budget for: their count, their gear and their composited
//! skins all belong to other people. Everything else here already has a wall — `farclip` for the
//! world, `effectsDistance` for the particles. This is the third.
//!
//! The entities stay and the network keeps updating them; a body past the wall is not built, and
//! one that walks out fades and is torn down ([`BeyondPlayerDistance`]). The first build only hid
//! them, and a hidden body was a whole body still paid for. The player's own model is never
//! touched: a camera subject that vanishes is disorienting.
//!
//! wenilla carries on top: Smart Player Distance ([`SmartPlayerDistance`]) and Player Shards
//! ([`PlayerShards`]), with the shard selector's feed and the other shards' shadows.

use benilla_protocol::EntityKind;
use bevy::prelude::*;

/// The settable range, shared by the CVar apply and the options slider so the two cannot drift.
/// `0` is "draw none" and is a real setting, not a guard value. The top is [`farclip`'s ceiling],
/// i.e. "no wall of our own" — beyond it the world's own far plane decides.
///
/// [`farclip`'s ceiling]: benilla_world::view::FARCLIP_RANGE
pub(crate) const PLAYER_DISTANCE_RANGE: std::ops::RangeInclusive<f32> = 0.0..=777.0;

/// Yards. Default is the top of the range: every player drawn, which is what every build before
/// this one did.
#[derive(Resource, Clone, Copy, PartialEq, Debug)]
pub(crate) struct PlayerDistance(pub(crate) f32);

impl Default for PlayerDistance {
    fn default() -> Self {
        // wenilla carry: 250 yd in the browser, `playerDistance`'s default there (the owner's,
        // 2026-10-19); a CVar's default fires no change, so the two must agree.
        if cfg!(target_arch = "wasm32") {
            Self(250.0)
        } else {
            Self(*PLAYER_DISTANCE_RANGE.end())
        }
    }
}

/// wenilla carry: **a player past the wall is not built, not merely hidden.** Journal
/// 2026-10-18T112520 was taken at `playerDistance 0` and still paid for every arriving body:
/// texture uploads (`R:GpuImage` 14.8 ms), skin composites (`skin_us` 9.8), meshes
/// (`R:allocate_and_free_meshes` 6.4) - a hidden body is a whole body. This marker keeps
/// `entities::attach_entity_visuals` from building one and has `entities::live_display` tear down
/// one that walked out; the unit itself, its fields, name, target and chat, stay. Set past the
/// wall and cleared [`REBUILD_BAND`] yards inside it, so a body on the line does not churn.
#[derive(Component)]
pub(crate) struct BeyondPlayerDistance;

/// The hysteresis band (yd): a torn-down body is rebuilt only once it is this far inside the wall.
const REBUILD_BAND: f32 = 10.0;

/// wenilla carry: **Smart Player Distance** (`smartPlayerDistance`, `smartPlayerCount`): the wall
/// pulls in to the `count`-th nearest other player, so a square of hundreds shows its nearest
/// `count` while an empty street keeps the full `playerDistance`. The owner's idea (2026-10-18):
/// a fixed wall is either too short for a road or too long for a square.
#[derive(Resource)]
pub(crate) struct SmartPlayerDistance {
    pub(crate) on: bool,
    pub(crate) count: usize,
}

impl Default for SmartPlayerDistance {
    fn default() -> Self {
        // On by default (the owner, 2026-10-19), as `smartPlayerDistance`'s row says.
        Self {
            on: true,
            count: SMART_COUNT_DEFAULT,
        }
    }
}

/// `smartPlayerCount`'s default and range; keep in step with `cvars/table.rs` and the options row.
pub(crate) const SMART_COUNT_DEFAULT: usize = 60;
pub(crate) const SMART_COUNT_RANGE: std::ops::RangeInclusive<f32> = 10.0..=300.0;

/// How fast (yd/s) the smart wall widens again as a crowd thins; it narrows at once.
const SMART_WIDEN_PER_SEC: f32 = 10.0;

/// wenilla carry: **Player Shards** (`playerShards`, `playerShard`, `playerShardCapacity`,
/// `playerShardRadius`): a crowd split into shards that hold still.
///
/// The owner's design (2026-10-18). A shard is tied to a PLACE, not to us and not to a hash: where
/// a crowd of more than `capacity` players gathers, an **area** is cut - a sphere of `radius` yards
/// around the crowd's mean position, anchored there for as long as the crowd lasts - and every
/// player inside it is a member of one of its shards, `capacity` each. A membership is **stored**:
/// a player stays in his shard while he stays in the area, so nobody hops between shards as the
/// crowd moves. He leaves it only by walking out of the area, and that frees his place. We always
/// fill our own shard first: a player entering the area joins ours while it has room, and a place
/// that frees in ours is filled from the other shards, nearest to us first - the one move a
/// membership ever makes. No crowd, no area: one shard, everyone shown (to `playerDistance`).
///
/// Only our shard is built (`BeyondPlayerDistance` marks the rest); the group and raid, our target
/// and anyone targeting us are shown whatever their shard, and so is one who has stood within
/// [`SHARD_NEAR_YARDS`] of us for [`SHARD_NEAR_SECS`]. Not one merely passing: the owner's
/// screenshot (2026-10-18) showed a plain 8 yd exemption was the flicker itself - a full shard
/// cannot take them in, so each one showed while we passed and vanished as we walked on.
/// `/console playerShard N` picks which shard of each area we see.
#[derive(Resource)]
pub(crate) struct PlayerShards {
    pub(crate) on: bool,
    /// The shard asked for; taken modulo each area's shard count.
    pub(crate) shard: u32,
    /// How many players a shard holds (`playerShardCapacity`).
    pub(crate) capacity: usize,
    /// An area's radius, yards (`playerShardRadius`).
    pub(crate) radius: f32,
    /// `playerShardShow`: draw the areas and colour each sharded player's ring.
    pub(crate) show: bool,
    /// `playerShardGhosts`: a shadow disc under each player of another shard.
    pub(crate) ghosts: bool,
    /// `playerShardSwap`: one of another shard who stands by us `SHARD_NEAR_SECS` trades places
    /// with the member of ours farthest from us, rather than only being shown.
    pub(crate) swap: bool,
    areas: Vec<ShardArea>,
    /// guid -> (area id, shard): the stored memberships.
    members: std::collections::HashMap<u64, (u32, u16)>,
    /// Members out of the stream: guid -> (area id, shard, since when).
    absent: std::collections::HashMap<u64, (u32, u16, f32)>,
    /// Since when each player has stood within `SHARD_NEAR_YARDS` of us, unbroken.
    near_since: std::collections::HashMap<u64, f32>,
    next_area: u32,
    /// `Time::elapsed_secs` of the next area upkeep.
    next_scan: f32,
}

/// One crowd's area: anchored where the crowd stood when it formed.
struct ShardArea {
    id: u32,
    center: Vec3,
    /// Members per shard; the shard count is its length.
    counts: Vec<u32>,
    /// Since when the area has held too few players to need shards.
    thin_since: Option<f32>,
    /// When its centre was last moved to the crowd's peak (`SHARD_RECENTER_SECS`).
    centred_at: f32,
    /// The shards we have shown in this area. A free place in ours is filled only from shards
    /// never shown, so nobody we have seen in one shard turns up in another.
    viewed: std::collections::HashSet<u16>,
}

impl Default for PlayerShards {
    fn default() -> Self {
        // On, with shadows, by default (the owner, 2026-10-19), as the CVar rows say.
        Self {
            on: true,
            shard: 0,
            capacity: SHARD_CAPACITY_DEFAULT,
            radius: SHARD_RADIUS_DEFAULT,
            show: false,
            ghosts: true,
            swap: false,
            areas: Vec::new(),
            members: Default::default(),
            absent: Default::default(),
            near_since: Default::default(),
            next_area: 0,
            next_scan: 0.0,
        }
    }
}

/// `playerShardCapacity`'s default and range; keep in step with `cvars/table.rs` and the row.
pub(crate) const SHARD_CAPACITY_DEFAULT: usize = 40;
pub(crate) const SHARD_CAPACITY_RANGE: std::ops::RangeInclusive<f32> = 10.0..=300.0;

/// `playerShardRadius`'s default and range (yd); keep in step with `cvars/table.rs` and the row.
pub(crate) const SHARD_RADIUS_DEFAULT: f32 = 150.0;
pub(crate) const SHARD_RADIUS_RANGE: std::ops::RangeInclusive<f32> = 50.0..=400.0;

/// How often the areas are re-cut (seconds); memberships are kept every frame.
const SHARD_SCAN_SECS: f32 = 0.5;

/// How long an area may hold no more than 85% of a shard before it dissolves (seconds).
///
/// A minute, not seconds: the server streams players only ~100 yd out and an area is wider, so
/// walking at its rim the client sees a part of it and its count dips; a 5 s dissolve then cut
/// a new area with a new id and re-dealt every membership - a whole-crowd reshuffle.
const SHARD_DISSOLVE_SECS: f32 = 60.0;

/// At most this many players move into our shard in one frame, so a freed shard fills smoothly.
const SHARD_MOVES_PER_FRAME: usize = 8;

/// How many points are tried as an area's seed, at most: a seed's neighbours are counted, and in a
/// crowd of a thousand every one of them is a neighbour.
const SHARD_SEED_SAMPLES: usize = 200;

/// One of another shard who stands this close to us (yd) ...
const SHARD_NEAR_YARDS: f32 = 8.0;
/// ... for this long, unbroken (seconds), is shown: the owner's rule (2026-10-18).
const SHARD_NEAR_SECS: f32 = 10.0;

/// How often an area's centre follows its crowd (seconds): the owner's rule (2026-10-18), once a
/// minute - players move, and a centre left where the crowd first stood drifts to its rim.
const SHARD_RECENTER_SECS: f32 = 60.0;

/// How long a member out of the stream keeps his shard (seconds).
const SHARD_MEMORY_SECS: f32 = 120.0;

/// The core kernel the density peak is found in, as a fraction of the area's radius.
const SHARD_CORE_FRACTION: f32 = 0.2;

/// A member keeps his area until this far out, as a fraction of its radius.
const SHARD_LEAVE_FACTOR: f32 = 1.2;

impl PlayerShards {
    /// Forget every area and membership (shards switched off).
    fn clear(&mut self) {
        if !self.areas.is_empty() {
            info!("player shards: {} areas dissolved", self.areas.len());
        }
        self.areas.clear();
        self.members.clear();
        self.absent.clear();
        self.near_since.clear();
    }

    /// Deal every area's crowd anew, the areas kept where they stand: a changed `capacity` must
    /// change the shard count, which otherwise only grows while an area lives (the owner,
    /// 2026-10-19: "I change the shard size, the count does not change"). Every player then joins
    /// as a newcomer, nearest first into ours.
    fn redeal(&mut self) {
        self.members.clear();
        self.absent.clear();
        for a in &mut self.areas {
            a.counts.clear();
            a.viewed.clear();
        }
    }

    /// Our shard in an area of `k` shards.
    fn ours(&self, k: usize) -> usize {
        self.shard as usize % k.max(1)
    }

    /// Whether `guid` has stood within `SHARD_NEAR_YARDS` of us for `SHARD_NEAR_SECS`.
    fn dwelt(&self, guid: u64, now: f32) -> bool {
        self.near_since
            .get(&guid)
            .is_some_and(|&since| now - since >= SHARD_NEAR_SECS)
    }

    /// Whether `guid` is a member of a shard of his area other than ours.
    fn off_shard(&self, guid: u64) -> bool {
        let Some(&(area, shard)) = self.members.get(&guid) else {
            return false;
        };
        self.areas
            .iter()
            .find(|a| a.id == area)
            .is_some_and(|a| usize::from(shard) != self.ours(a.counts.len()))
    }

    /// One frame of upkeep over `crowd`, every player that may be sharded, from `eye`.
    fn update(&mut self, crowd: &[(u64, Vec3)], eye: Vec3, now: f32) {
        let cap = self.capacity.max(1);
        let r2 = self.radius * self.radius;
        if now >= self.next_scan {
            self.next_scan = now + SHARD_SCAN_SECS;
            self.recut_areas(crowd, now);
        }
        // Memberships: kept while the player stays in his area; a newcomer joins ours first.
        let area_of = |areas: &[ShardArea], p: Vec3| {
            areas
                .iter()
                .enumerate()
                .filter(|(_, a)| p.distance_squared(a.center) <= r2)
                .min_by(|x, y| {
                    p.distance_squared(x.1.center)
                        .total_cmp(&p.distance_squared(y.1.center))
                })
                .map(|(i, _)| i)
        };
        let old = std::mem::take(&mut self.members);
        for a in &mut self.areas {
            a.counts.iter_mut().for_each(|c| *c = 0);
        }
        let pos: std::collections::HashMap<u64, Vec3> = crowd.iter().copied().collect();
        // The dwell clock: started on stepping inside `SHARD_NEAR_YARDS`, dropped on leaving it
        // (or the stream).
        let near2 = SHARD_NEAR_YARDS * SHARD_NEAR_YARDS;
        self.near_since
            .retain(|g, _| pos.get(g).is_some_and(|p| p.distance_squared(eye) <= near2));
        for &(g, p) in crowd {
            if p.distance_squared(eye) <= near2 {
                self.near_since.entry(g).or_insert(now);
            }
        }
        // **A member out of the stream keeps his place** for `SHARD_MEMORY_SECS`. The server
        // streams players only some 100 yd out, an area is wider, so as we walk its rim drops out
        // and comes back; coming back as a newcomer re-dealt him into some shard.
        for (&g, &(id, shard)) in &old {
            if !pos.contains_key(&g) {
                self.absent.entry(g).or_insert((id, shard, now));
            }
        }
        self.absent
            .retain(|_, &mut (_, _, since)| now - since < SHARD_MEMORY_SECS);
        let mut fresh: Vec<(u64, usize, f32)> = Vec::new();
        // A member leaves his area only past `SHARD_LEAVE_FACTOR` of its radius, so one walking
        // along the edge does not flicker out and back in.
        let keep2 = r2 * SHARD_LEAVE_FACTOR * SHARD_LEAVE_FACTOR;
        for &(guid, p) in crowd {
            let prior = old
                .get(&guid)
                .copied()
                .or_else(|| self.absent.remove(&guid).map(|(id, shard, _)| (id, shard)));
            let kept = prior.and_then(|(id, shard)| {
                let ai = self.areas.iter().position(|a| a.id == id)?;
                (p.distance_squared(self.areas[ai].center) <= keep2).then_some((ai, shard))
            });
            if let Some((ai, shard)) = kept {
                let area = &mut self.areas[ai];
                let s = usize::from(shard);
                if area.counts.len() <= s {
                    area.counts.resize(s + 1, 0);
                }
                area.counts[s] += 1;
                self.members.insert(guid, (area.id, shard));
            } else if let Some(ai) = area_of(&self.areas, p) {
                fresh.push((guid, ai, p.distance_squared(eye)));
            }
        }
        // **A member out of the stream still holds his place.** Counting only the present ones
        // freed his slot the moment the server stopped sending him: it was filled from another
        // shard, and on his return he was restored into ours on top of it - walking through an
        // area, ours grew past `capacity` (the owner's screenshot, 2026-10-19: far more than 40
        // shown at capacity 40). What is left in `absent` here is truly away (a returner was taken
        // out above), so each is counted once.
        for &(id, shard, _) in self.absent.values() {
            if let Some(area) = self.areas.iter_mut().find(|a| a.id == id) {
                let s = usize::from(shard);
                if area.counts.len() <= s {
                    area.counts.resize(s + 1, 0);
                }
                area.counts[s] += 1;
            }
        }
        // The nearest newcomers first, so ours fills from up close.
        fresh.sort_by(|a, b| a.2.total_cmp(&b.2));
        for (guid, ai, _) in fresh {
            let ours = self.ours(self.areas[ai].counts.len());
            let area = &mut self.areas[ai];
            let s = if area.counts.get(ours).is_some_and(|&c| (c as usize) < cap)
                || area.counts.is_empty()
            {
                ours
            } else {
                match (0..area.counts.len())
                    .filter(|&i| (area.counts[i] as usize) < cap)
                    .min_by_key(|&i| area.counts[i])
                {
                    Some(i) => i,
                    None => area.counts.len(),
                }
            };
            if area.counts.len() <= s {
                area.counts.resize(s + 1, 0);
            }
            area.counts[s] += 1;
            self.members.insert(guid, (area.id, s as u16));
        }
        for ai in 0..self.areas.len() {
            // The shard count only grows while the area lives: trimming it moved `ours`
            // (`playerShard % count`) and swapped the whole visible crowd.
            let k = self.areas[ai].counts.len();
            if k < 2 {
                continue;
            }
            let ours = self.ours(k);
            let id = self.areas[ai].id;
            // **Never from a shard we have shown** (the owner, 2026-10-18: "one and the same
            // character turns up in different shards"). Switching `playerShard` left the new
            // shard short, and filling it nearest-first moved the very people we had just seen
            // into it.
            self.areas[ai].viewed.insert(ours as u16);
            let viewed = &self.areas[ai].viewed;
            let mut theirs: Vec<(u64, u16, f32)> = Vec::new();
            for (&g, &(a, s)) in &self.members {
                if a != id {
                    continue;
                }
                let Some(p) = pos.get(&g) else { continue };
                let d2 = p.distance_squared(eye);
                if !viewed.contains(&s) {
                    theirs.push((g, s, d2));
                }
            }
            // **Ours only takes free places, it never trades** (the owner, 2026-10-18): nobody is
            // pushed out of ours and it never holds more than `capacity`. A free place goes to the
            // NEAREST of the others: the owner's rule (2026-10-18), the best candidate for a place
            // is the one who stands closest.
            theirs.sort_by(|a, b| a.2.total_cmp(&b.2));
            let room = cap.saturating_sub(self.areas[ai].counts[ours] as usize);
            for &(g, from, _) in theirs.iter().take(room.min(SHARD_MOVES_PER_FRAME)) {
                self.areas[ai].counts[usize::from(from)] -= 1;
                self.areas[ai].counts[ours] += 1;
                self.members.insert(g, (id, ours as u16));
            }
            // **`playerShardSwap`** (the owner's option, 2026-10-19): one of another shard - any,
            // shown or not - who has stood within `SHARD_NEAR_YARDS` of us for `SHARD_NEAR_SECS`
            // joins ours for good, and the member of ours farthest from us takes his place in his
            // shard. Both counts hold; the one traded out is the one least seen. Without the
            // option he is only shown while he stays (`dwelt`).
            if self.swap {
                let near = &self.near_since;
                let dwellers: Vec<(u64, u16)> = self
                    .members
                    .iter()
                    .filter(|(g, &(a, s))| {
                        a == id
                            && usize::from(s) != ours
                            && near.get(*g).is_some_and(|&t| now - t >= SHARD_NEAR_SECS)
                    })
                    .map(|(&g, &(_, s))| (g, s))
                    .collect();
                if !dwellers.is_empty() {
                    // Ours, farthest first; never one standing by us himself.
                    let mut mine: Vec<(u64, f32)> = self
                        .members
                        .iter()
                        .filter(|(_, &(a, s))| a == id && usize::from(s) == ours)
                        .filter_map(|(&g, _)| pos.get(&g).map(|p| (g, p.distance_squared(eye))))
                        .filter(|&(_, d2)| d2 > near2)
                        .collect();
                    mine.sort_by(|a, b| b.1.total_cmp(&a.1));
                    for ((g, from), (out, _)) in dwellers
                        .into_iter()
                        .zip(mine)
                        .take(SHARD_MOVES_PER_FRAME)
                    {
                        self.members.insert(g, (id, ours as u16));
                        self.members.insert(out, (id, from));
                    }
                }
            }
        }
    }

    /// Dissolve the areas whose crowd has gone, and cut one around each new crowd.
    fn recut_areas(&mut self, crowd: &[(u64, Vec3)], now: f32) {
        let cap = self.capacity.max(1);
        let r2 = self.radius * self.radius;
        let absent = &self.absent;
        self.areas.retain_mut(|a| {
            // Its members out of the stream still count: they are there, only not sent to us.
            let pop = crowd
                .iter()
                .filter(|(_, p)| p.distance_squared(a.center) <= r2)
                .count()
                + absent.values().filter(|&&(id, ..)| id == a.id).count();
            if pop * 100 > cap * 85 {
                a.thin_since = None;
                return true;
            }
            let since = *a.thin_since.get_or_insert(now);
            let keep = now - since < SHARD_DISSOLVE_SECS;
            if !keep {
                info!("player shards: area {} dissolved, {pop} players left in it", a.id);
            }
            keep
        });
        // **Once a minute each centre follows its crowd**: a mean shift in the core kernel from
        // where it stands, so it climbs to the peak of ITS crowd, never jumps to another. The id
        // and every membership stay; one now past the leave line leaves as anyone does. A move
        // that would bring two areas within two radii is not made: areas never overlap.
        let core2 = (self.radius * SHARD_CORE_FRACTION).powi(2);
        for i in 0..self.areas.len() {
            if now - self.areas[i].centred_at < SHARD_RECENTER_SECS {
                continue;
            }
            self.areas[i].centred_at = now;
            let mut c = self.areas[i].center;
            for _ in 0..12 {
                let (n, sum) = crowd
                    .iter()
                    .filter(|(_, p)| p.distance_squared(c) <= core2)
                    .fold((0usize, Vec3::ZERO), |(n, s), &(_, p)| (n + 1, s + p));
                if n == 0 {
                    break;
                }
                let next = sum / n as f32;
                let moved = next.distance_squared(c);
                c = next;
                if moved < 0.25 {
                    break;
                }
            }
            let clear = self
                .areas
                .iter()
                .enumerate()
                .all(|(j, b)| j == i || b.center.distance_squared(c) >= 4.0 * r2);
            if clear {
                let from = self.areas[i].center;
                info!(
                    "player shards: area {} recentred, moved {:.0} yd",
                    self.areas[i].id,
                    from.distance(c)
                );
                self.areas[i].center = c;
            }
        }
        // The players no area holds yet.
        let mut free: Vec<Vec3> = crowd
            .iter()
            .map(|&(_, p)| p)
            .filter(|p| !self.areas.iter().any(|a| p.distance_squared(a.center) <= r2))
            .collect();
        // **Centred on the crowd's densest point, not its mean** (the owner, 2026-10-18): the mean
        // of a wide sphere drifts off the core toward whatever stands at its rim, and the first
        // build cut areas whose edge ran through the middle of the crowd. The peak is found in a
        // small core kernel: the free player with the most neighbours inside it, then a mean shift
        // in the same kernel, which climbs to the local density maximum and stops there.
        let core = self.radius * SHARD_CORE_FRACTION;
        let core2 = core * core;
        let around = |free: &[Vec3], c: Vec3, d2: f32| -> (usize, Vec3) {
            free.iter()
                .filter(|p| p.distance_squared(c) <= d2)
                .fold((0, Vec3::ZERO), |(n, s), &p| (n + 1, s + p))
        };
        // At most a few areas a pass; the next pass takes the rest.
        for _ in 0..4 {
            if free.len() <= cap {
                break;
            }
            // The densest seed among a sample of the free players.
            let step = (free.len() / SHARD_SEED_SAMPLES).max(1);
            let Some(seed) = (0..free.len())
                .step_by(step)
                .max_by_key(|&i| around(&free, free[i], core2).0)
            else {
                break;
            };
            let mut center = free[seed];
            for _ in 0..12 {
                let (n, sum) = around(&free, center, core2);
                if n == 0 {
                    break;
                }
                let next = sum / n as f32;
                let moved = next.distance_squared(center);
                center = next;
                if moved < 0.25 {
                    break;
                }
            }
            let count = around(&free, center, r2).0;
            if count <= cap {
                break;
            }
            // **One crowd, one area.** Areas never overlap: a crowd's rim, outside an area, is
            // not cut into a second area beside it - that stacked areas side by side, and walking
            // into a crowd crossed from one area's shards into another's. A peak too close to an
            // area is left unsharded and taken off this pass's candidates.
            if self
                .areas
                .iter()
                .any(|a| a.center.distance_squared(center) < 4.0 * r2)
            {
                free.retain(|p| p.distance_squared(center) > r2);
                continue;
            }
            let id = self.next_area;
            self.next_area = self.next_area.wrapping_add(1);
            info!(
                "player shards: area {id} formed around {count} players, radius {:.0} yd",
                self.radius
            );
            self.areas.push(ShardArea {
                id,
                center,
                counts: Vec::new(),
                thin_since: None,
                centred_at: now,
                viewed: Default::default(),
            });
            free.retain(|p| p.distance_squared(center) > r2);
        }
    }
}

/// The shard CVars into [`PlayerShards`].
fn on_shard_cvar(ev: On<crate::cvars::CvarChanged>, mut shards: ResMut<PlayerShards>) {
    if ev.is("playerShards") {
        shards.on = ev.flag();
        info!("player shards: {}", if shards.on { "on" } else { "off" });
    }
    if ev.is("playerShardCapacity") {
        shards.capacity = ev
            .num()
            .clamp(*SHARD_CAPACITY_RANGE.start(), *SHARD_CAPACITY_RANGE.end()) as usize;
        shards.redeal();
        info!("player shards: {} players per shard, areas dealt anew", shards.capacity);
    }
    if ev.is("playerShardRadius") {
        shards.radius = ev
            .num()
            .clamp(*SHARD_RADIUS_RANGE.start(), *SHARD_RADIUS_RANGE.end());
        // Areas are cut at a radius; a new one re-cuts them all.
        shards.clear();
        info!("player shards: areas of {:.0} yd", shards.radius);
    }
    if ev.is("playerShardShow") {
        shards.show = ev.flag();
    }
    if ev.is("playerShardGhosts") {
        shards.ghosts = ev.flag();
    }
    if ev.is("playerShardSwap") {
        shards.swap = ev.flag();
    }
    if ev.is("playerShard") {
        shards.shard = ev.num().max(0.0) as u32;
        info!("player shards: showing shard {} of each area", shards.shard);
    }
}

/// The two CVars into [`SmartPlayerDistance`].
fn on_smart_cvar(ev: On<crate::cvars::CvarChanged>, mut smart: ResMut<SmartPlayerDistance>) {
    if ev.is("smartPlayerDistance") {
        smart.on = ev.flag();
    }
    if ev.is("smartPlayerCount") {
        smart.count = ev.num().clamp(*SMART_COUNT_RANGE.start(), *SMART_COUNT_RANGE.end()) as usize;
    }
}

/// Hide the players past the wall, show the ones inside it.
///
/// Runs every frame and costs one squared-distance compare per streamed unit — a few hundred
/// compares against a frame that is drawing a few hundred characters. A `Visibility` is written
/// only when it actually changes, so a settled crowd writes nothing and dirties no change
/// detection downstream.
pub(crate) fn apply(
    wall: Res<PlayerDistance>,
    self_guid: Res<crate::net::SelfGuid>,
    // **The wall measures from the PLAYER, not the camera**, and the first build measured from the
    // camera and died on it: `camera.single()` returns `Err` for zero matches as well as two, the
    // function returned there, and every line below - including the one meant to explain a slider
    // that does nothing - was unreachable. A diagnostic behind an early return is not a
    // diagnostic. The player's own position is the honest origin anyway: a wall the camera carries
    // slides the crowd in and out as the view swings, which is not what a draw-distance setting
    // means anywhere else in this client.
    player: Option<Res<crate::player::Player>>,
    mut units: Query<(
        Entity,
        &crate::net::NetEntity,
        &crate::net::Guid,
        &GlobalTransform,
        Option<&mut Visibility>,
        Has<BeyondPlayerDistance>,
        // A built body past the wall fades out and is torn down (`entities::live_display`), so
        // it is not hidden here: the fade must be seen.
        Has<crate::entities::VisualAttached>,
        // Whom this unit targets: one targeting us is shown whatever his shard.
        Option<&crate::net::ObjectStore>,
    )>,
    mut reported: Local<Option<f32>>,
    mut commands: Commands,
    time: Res<Time>,
    smart: Res<SmartPlayerDistance>,
    // The smart wall as last applied, so it widens gradually.
    mut smart_wall: Local<Option<f32>>,
    mut shards: ResMut<PlayerShards>,
    // The shard exemptions: the group and raid, and our target.
    group: Option<Res<crate::ui_party::GroupState>>,
    selection: Option<Res<crate::target::Selection>>,
    // A battleground or arena is never sharded (`Map.dbc` `InstanceType` 3 or 4).
    maps: Option<Res<benilla_assets::MapCatalogRes>>,
    current_map: Option<Res<benilla_world::world_map::CurrentMap>>,
) {
    let eye = player.as_deref().filter(|p| p.active).map(|p| p.pos);
    let me = self_guid.0;
    // **The group and raid answer to `playerDistance` alone** (the owner, 2026-10-18): neither
    // the smart wall nor a shard takes them, and they do not count toward either.
    let in_group =
        |g: u64| group.as_deref().is_some_and(|gr| gr.members.iter().any(|m| m.guid == g));
    // The wall this frame: the setting, pulled in by Smart Player Distance to the `count`-th
    // nearest other player (and their pets), widening back at `SMART_WIDEN_PER_SEC`.
    // With shards on, the shard bounds the count, so the wall steps aside: a shard spans every
    // player the server streams, as the owner asked ("the shard's area may be far wider than
    // Player Distance") - a wall inside it made bodies of the shown shard vanish up close.
    // **No shards on a battleground** (the owner, 2026-10-18): every player there is in the
    // fight, so each one is shown, as the group always is, and the plain walls apply.
    let pvp_map = current_map
        .as_ref()
        .and_then(|m| maps.as_ref()?.0.instance_type(m.0))
        .is_some_and(|t| t == 3 || t == 4);
    let sharding = shards.on && !pvp_map;
    let mut effective = if sharding {
        *PLAYER_DISTANCE_RANGE.end()
    } else {
        wall.0
    };
    // **NPCs count too** (the owner, 2026-10-18): the smart wall is the `count`-th nearest
    // character, player or creature, and creatures past it are released like players.
    //
    // **With shards, the smart wall works on our shard** (the owner, 2026-10-19: a shard of 200
    // would still stutter): it counts only who the shard leaves shown - our shard's members and
    // their summons, players out of every area, creatures - never another shard's, and as the
    // shard has already stood the plain `playerDistance` aside, `playerDistance` does not cap it
    // either. Membership is last frame's, which is the upkeep's own cadence. Off shards, it counts
    // everyone as before.
    let mut npc_effective = *PLAYER_DISTANCE_RANGE.end();
    if let (true, Some(eye)) = (smart.on, eye) {
        let cap = if sharding {
            *PLAYER_DISTANCE_RANGE.end()
        } else {
            wall.0
        };
        let mut d2: Vec<f32> = units
            .iter()
            .filter(|(_, net, guid, .., store)| {
                matches!(net.kind, EntityKind::Player | EntityKind::Unit)
                    && Some(guid.0) != me
                    && !in_group(guid.0)
                    && !(sharding && shards.off_shard(owner_of(*store).unwrap_or(guid.0)))
            })
            .map(|(_, _, _, at, ..)| at.translation().distance_squared(eye))
            .collect();
        let target = if d2.len() > smart.count {
            d2.select_nth_unstable_by(smart.count, f32::total_cmp)
                .1
                .sqrt()
                .min(cap)
        } else {
            cap
        };
        let prev = smart_wall.unwrap_or(target);
        let widened = prev + SMART_WIDEN_PER_SEC * time.delta_secs();
        let smart_at = target.min(widened);
        *smart_wall = Some(smart_at);
        npc_effective = smart_at;
        effective = smart_at;
    } else {
        *smart_wall = None;
    }
    // Compared squared, so the per-unit test is a subtract and a dot rather than a square root.
    let limit = effective * effective;
    let npc_limit = npc_effective * npc_effective;
    // The shard upkeep: areas, memberships and our shard's fill (`PlayerShards`).
    match (sharding, eye) {
        (true, Some(eye)) => {
            let crowd: Vec<(u64, Vec3)> = units
                .iter()
                .filter(|(_, net, guid, .., store)| {
                    (net.kind == EntityKind::Player || benilla_protocol::guid::is_pet(guid.0))
                        && Some(guid.0) != me
                        && !in_group(guid.0)
                        // A summon is no member of its own: it follows its owner's shard.
                        && owner_of(*store).is_none()
                })
                .map(|(_, _, guid, at, ..)| (guid.0, at.translation()))
                .collect();
            shards.update(&crowd, eye, time.elapsed_secs());
        }
        _ => shards.clear(),
    }
    let target = selection.as_deref().and_then(|s| s.guid);
    // Whose summon a unit is: `SUMMONEDBY`, else `CREATEDBY` (a companion, a totem).
    fn owner_of(store: Option<&crate::net::ObjectStore>) -> Option<u64> {
        store.and_then(|s| s.0.unit_summoned_by().or_else(|| s.0.unit_created_by()))
    }
    let (mut players, mut hidden, mut inserted) = (0u32, 0u32, 0u32);
    // The players and pets hidden as of last frame's verdict, for the objects they made.
    let hidden_makers: std::collections::HashSet<u64> = units
        .iter()
        .filter(|(_, net, guid, _, _, beyond, ..)| {
            *beyond && (net.kind == EntityKind::Player || benilla_protocol::guid::is_pet(guid.0))
        })
        .map(|(_, _, guid, ..)| guid.0)
        .collect();
    if let Some(eye) = eye {
        // The rebuild line, `REBUILD_BAND` inside the wall; never negative.
        // Proportional, so a smart wall pulled in to 15 yd does not leave a 10 yd band of unbuilt
        // bodies inside it.
        let band = |wall: f32| {
            let r = (wall - REBUILD_BAND.min(wall * 0.15)).max(0.0);
            r * r
        };
        let (rebuild_sq, npc_rebuild_sq, group_rebuild_sq) =
            (band(effective), band(npc_effective), band(wall.0));
        for (entity, net, guid, at, vis, beyond, built, store) in &mut units {
            // **A player's pet walks out with its owner.** The setting is a crowd wall, and a
            // crowd's hunters, warlocks and mages bring a body each; leaving those standing while
            // their owners vanish is what the owner saw with the slider at zero - an empty square
            // full of pets. A pet is `EntityKind::Unit`, so the kind test alone never reached it,
            // but its guid says so on its own: `HIGHGUID_PET` (`guid::is_pet`), no owner field
            // and no second query needed.
            //
            // The honest limit: a minion the server spawns as a plain creature - a totem, an
            // unowned guardian - carries `HIGHGUID_UNIT` and is indistinguishable here from a
            // world mob, so it stays. Catching those needs the unit's summoned-by field, which
            // this pass does not read.
            // **An object a hidden player made goes with him** (the owner, 2026-10-19: another
            // shard's abilities were still seen) - a portal, a bobber, a trap, a ground AoE. It
            // takes his marker, so the attach never builds it and a built one fades and is torn
            // down like a body; it comes back with him.
            if matches!(net.kind, EntityKind::GameObject | EntityKind::DynamicObject) {
                let maker = store.and_then(|s| match net.kind {
                    EntityKind::GameObject => s.0.gameobject_created_by(),
                    _ => s.0.dynamicobject_caster(),
                });
                let want = maker.is_some_and(|m| hidden_makers.contains(&m));
                if want && !beyond {
                    commands.entity(entity).try_insert(BeyondPlayerDistance);
                } else if !want && beyond {
                    commands.entity(entity).try_remove::<BeyondPlayerDistance>();
                    commands
                        .entity(entity)
                        .try_insert(crate::entities::Arrival(time.elapsed_secs()));
                }
                if want && !built {
                    if let Some(mut vis) = vis {
                        if *vis != Visibility::Hidden {
                            *vis = Visibility::Hidden;
                        }
                    }
                }
                continue;
            }
            let controlled = net.kind == EntityKind::Player || benilla_protocol::guid::is_pet(guid.0);
            // A creature is walled only by the smart wall, and never sharded.
            let npc = !controlled && net.kind == EntityKind::Unit;
            if !(controlled || npc) || Some(guid.0) == me {
                continue;
            }
            if controlled {
                players += 1;
            }
            // Our target and anyone targeting us stay, whatever the wall: whom we fight is seen.
            let engaged = Some(guid.0) == target
                || store.is_some_and(|s| me.is_some() && s.0.unit_target() == me);
            let grouped = controlled && in_group(guid.0);
            // This unit's wall: the smart one for a creature, the plain setting for the group.
            let (own_wall, limit, rebuild_sq) = if npc {
                (npc_effective, npc_limit, npc_rebuild_sq)
            } else if grouped {
                (wall.0, wall.0 * wall.0, group_rebuild_sq)
            } else {
                (effective, limit, rebuild_sq)
            };
            // **This pass only ever HIDES.** `benilla_world::exterior_cull` writes `Visibility` on
            // every body each frame - it is the window and frustum cull - so showing is its job and
            // its alone. Two systems writing one component with no order between them is what the
            // first build did, and the cull won: 84 bodies were marked hidden every frame and not
            // one of them left the screen. Writing only one direction composes instead of racing,
            // and a wall slid back out restores itself because the cull restates the bodies it can
            // see on the very next frame.
            let d2 = at.translation().distance_squared(eye);
            // The build marker, with its band: set past the wall, cleared well inside it. At the
            // range's top there is no wall of ours, so nothing is ever marked.
            let walled = !engaged && own_wall < *PLAYER_DISTANCE_RANGE.end();
            // Outside the shown shard, and not exempt from sharding.
            // **A summon follows its owner's shard** (the owner, 2026-10-18: a paladin of another
            // shard summoned and its appear effect played here). A pet, guardian, totem or
            // companion was dealt a shard of its own - or, as a plain creature, none - so it, and
            // every effect played on it, showed beside an owner we never saw.
            let owner = owner_of(store).filter(|&o| Some(o) != me);
            let off_shard = match owner {
                Some(o) => {
                    sharding
                        && !engaged
                        && !in_group(o)
                        && !shards.dwelt(o, time.elapsed_secs())
                        && shards.off_shard(o)
                }
                None => {
                    sharding
                        && !npc
                        && !engaged
                        && !grouped
                        && !shards.dwelt(guid.0, time.elapsed_secs())
                        && shards.off_shard(guid.0)
                }
            };
            // The marker as it stands after this frame's verdict.
            let mut marked = beyond;
            if ((walled && d2 > limit) || off_shard) && !beyond {
                commands.entity(entity).try_insert(BeyondPlayerDistance);
                marked = true;
            } else if beyond && !off_shard && (!walled || d2 <= rebuild_sq) {
                commands.entity(entity).try_remove::<BeyondPlayerDistance>();
                marked = false;
                // Rebuilt from here: the appear ramp runs from now, not from the first arrival,
                // so the body fades in rather than popping.
                commands
                    .entity(entity)
                    .try_insert(crate::entities::Arrival(time.elapsed_secs()));
            }
            // Hidden while marked and unbuilt, wherever he stands: one inside the wall but still in
            // the rebuild band has no body yet, and his name must not float over the ground. A
            // built body fades out first (`entities::live_display`), so it is left drawn.
            if !marked || built {
                continue;
            }
            hidden += 1;
            let want = Visibility::Hidden;
            // **Write only; never insert.** Streamed bodies carry a `Visibility` already - the
            // counter below has read `0 given a Visibility` in every capture since it shipped -
            // so the insert branch was dead code that could still move an archetype mid-frame,
            // and an archetype move on an entity the render world has already collected as
            // visible is the shape of the crash seen in `bevy_pbr`'s material specialization
            // (material.rs:1061, an `unwrap` on a tick table the sweep had just emptied). Dead
            // code is not worth a hazard, however small the odds.
            match vis {
                Some(mut vis) => {
                    if *vis != want {
                        *vis = want;
                    }
                }
                None => inserted += 1,
            }
        }
    }
    // One line per setting change, never per frame, and **outside every early return**: "the
    // slider does nothing" is a question about what the code reached, and the code has to be able
    // to answer it even when it reached nothing.
    if *reported != Some(wall.0) {
        *reported = Some(wall.0);
        info!(
            "playerDistance {:.0} yd - player {}, {players} other players seen, {hidden} past the              wall, {inserted} without a Visibility at all",
            wall.0,
            if eye.is_some() { "found" } else { "ABSENT" }
        );
    }
}


/// Registers the wall.
pub(crate) struct PlayerDistancePlugin;

/// `/console playerShardShow 1`: each area as two rings at its centre's height - its radius
/// (white) and the leave line `SHARD_LEAVE_FACTOR` out (grey) - with a post at the centre, and a
/// ring under every sharded player: green in our shard, red in another (not built unless group,
/// target or targeting us), orange in another but shown for standing by us (`SHARD_NEAR_SECS`),
/// yellow out of every area. Twice a second a line per area in the log.
/// Costs nothing while off.
fn draw_shards(
    mut gizmos: Gizmos,
    shards: Res<PlayerShards>,
    time: Res<Time>,
    units: Query<(&crate::net::Guid, &crate::net::NetEntity, &GlobalTransform)>,
    self_guid: Res<crate::net::SelfGuid>,
    mut next_log: Local<f32>,
) {
    if !shards.show || !shards.on {
        return;
    }
    let iso = |c: Vec3| {
        Isometry3d::new(c + Vec3::Y * 0.3, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2))
    };
    for a in &shards.areas {
        gizmos
            .circle(iso(a.center), shards.radius, Color::WHITE)
            .resolution(128);
        gizmos
            .circle(iso(a.center), shards.radius * SHARD_LEAVE_FACTOR, Color::srgb(0.5, 0.5, 0.5))
            .resolution(128);
        gizmos.line(a.center, a.center + Vec3::Y * 40.0, Color::WHITE);
    }
    for (guid, net, at) in &units {
        if Some(guid.0) == self_guid.0
            || !(net.kind == EntityKind::Player || benilla_protocol::guid::is_pet(guid.0))
        {
            continue;
        }
        let color = match shards.members.get(&guid.0) {
            None => Color::srgb(1.0, 0.9, 0.1),
            Some(_) if shards.off_shard(guid.0) && shards.dwelt(guid.0, time.elapsed_secs()) => {
                Color::srgb(1.0, 0.55, 0.0)
            }
            Some(_) if shards.off_shard(guid.0) => Color::srgb(1.0, 0.15, 0.1),
            Some(_) => Color::srgb(0.1, 1.0, 0.2),
        };
        gizmos.circle(iso(at.translation()), 0.8, color);
    }
    let now = time.elapsed_secs();
    if now >= *next_log {
        *next_log = now + 0.5;
        for a in &shards.areas {
            let ours = shards.ours(a.counts.len());
            let absent = shards.absent.values().filter(|&&(id, ..)| id == a.id).count();
            info!(
                "player shards: area {} shards {:?} ours #{ours} ({} of {}), {absent} out of stream",
                a.id,
                a.counts,
                a.counts.get(ours).copied().unwrap_or(0),
                shards.capacity,
            );
        }
    }
}

/// A shadow disc standing in for a player of another shard (`playerShardGhosts`): its own
/// entity, not a child of the unit, whose `Visibility` is Hidden.
#[derive(Component)]
struct ShardGhost;

/// The half-size (yd) of a ghost's shadow quad, about a body's own blob shadow.
const GHOST_RADIUS: f32 = 0.8;

/// wenilla carry: **`playerShardGhosts`** - the owner's option (2026-10-18): where a player of
/// another shard stands, a soft shadow disc, so a crowd's layers are felt without being drawn.
/// One shared mesh and material, one entity per such player; the disc follows the unit's
/// position, written only when it moved. Off (or shards off): every disc is despawned and the
/// walk stops.
#[allow(clippy::type_complexity)]
fn sync_ghosts(
    mut commands: Commands,
    shards: Res<PlayerShards>,
    units: Query<(Entity, &crate::net::Guid, &GlobalTransform), With<BeyondPlayerDistance>>,
    mut ghosts: Query<&mut Transform, With<ShardGhost>>,
    mut live: Local<bevy::ecs::entity::EntityHashMap<Entity>>,
    mut assets: Local<Option<(Handle<Mesh>, Handle<StandardMaterial>)>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    asset_server: Res<AssetServer>,
) {
    if !(shards.on && shards.ghosts) {
        for (_, ghost) in live.drain() {
            commands.entity(ghost).try_despawn();
        }
        return;
    }
    // The body's own blob shadow, as the owner asked (2026-10-18): `ShadowBlob.blp` multiplied
    // onto the ground, as `blob_shadow` draws it (its module doc), on a flat quad rather than
    // its surface-decal projection.
    let (mesh, material) = assets
        .get_or_insert_with(|| {
            (
                meshes.add(Rectangle::new(GHOST_RADIUS * 2.0, GHOST_RADIUS * 2.0)),
                materials.add(StandardMaterial {
                    base_color: Color::WHITE,
                    base_color_texture: Some(
                        asset_server.load::<Image>("mpq://textures/shadowblob.blp"),
                    ),
                    unlit: true,
                    alpha_mode: AlphaMode::Multiply,
                    cull_mode: None,
                    fog_enabled: false,
                    ..default()
                }),
            )
        })
        .clone();
    // The quad lies flat (a `Rectangle` faces +Z) a hair above the feet, so the ground does not
    // z-fight it.
    let flat = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    let mut seen = bevy::ecs::entity::EntityHashSet::default();
    for (unit, guid, at) in &units {
        // A ghost for a shard's player alone; one past the plain wall gets none.
        if !shards.off_shard(guid.0) {
            continue;
        }
        seen.insert(unit);
        let want = Transform::from_translation(at.translation() + Vec3::Y * 0.05).with_rotation(flat);
        match live.get(&unit).copied() {
            Some(ghost) => {
                if let Ok(mut tf) = ghosts.get_mut(ghost) {
                    if tf.translation.distance_squared(want.translation) > 0.0001 {
                        *tf = want;
                    }
                }
            }
            None => {
                let ghost = commands
                    .spawn((
                        ShardGhost,
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(material.clone()),
                        want,
                        Visibility::default(),
                        bevy::light::NotShadowCaster,
                    ))
                    .id();
                live.insert(unit, ghost);
            }
        }
    }
    live.retain(|unit, ghost| {
        let keep = seen.contains(unit);
        if !keep {
            commands.entity(*ghost).try_despawn();
        }
        keep
    });
}

/// Push the shard selector's state (`GetPlayerShardInfo`): on, and the area we stand in - inside
/// its radius, the white ring of `playerShardShow`, so the selector shows on entering an area and
/// hides on leaving it (the owner, 2026-10-19; the leave line kept it up 20% past the ring) -
/// with the shard we see of it and its count.
fn feed_shard_info(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    shards: Res<PlayerShards>,
    player: Option<Res<crate::player::Player>>,
) {
    let Some(mut script) = script else { return };
    let eye = player.as_deref().filter(|p| p.active).map(|p| p.pos);
    let keep2 = shards.radius * shards.radius;
    let area = eye.and_then(|eye| {
        shards
            .areas
            .iter()
            .filter(|a| a.center.distance_squared(eye) <= keep2)
            .min_by(|x, y| {
                x.center
                    .distance_squared(eye)
                    .total_cmp(&y.center.distance_squared(eye))
            })
    });
    let area = area.map(|a| {
        let k = a.counts.len().max(1);
        (shards.ours(k) as u32, k as u32)
    });
    script.set_player_shard(shards.on, area);
}

impl Plugin for PlayerDistancePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerDistance>()
            .init_resource::<SmartPlayerDistance>()
            .init_resource::<PlayerShards>()
            .add_observer(on_smart_cvar)
            .add_observer(on_shard_cvar)
            .add_systems(
            PostUpdate,
            // After the frame's transforms are final and before visibility is propagated, so a
            // unit that moved across the wall this frame is drawn correctly on this frame rather
            // than the next.
            apply
                // After the cull, so the wall has the last word on the bodies it hides, and
                // before propagation, so that word reaches the children this same frame.
                .after(benilla_world::exterior_cull::ExteriorCullSet)
                .before(bevy::camera::visibility::VisibilitySystems::VisibilityPropagate),
        )
        .add_systems(
            PostUpdate,
            draw_shards.in_set(benilla_world::billboard::BillboardPlace),
        )
        .add_systems(Update, sync_ghosts)
        .add_systems(Update, feed_shard_info.in_set(crate::ui_script::UiFeed));
    }
}
