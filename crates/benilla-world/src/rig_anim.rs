//! **The rig machinery the renderer needs** — pose storage, the pose post-pass window, the rig
//! world composition, and the free-running global-sequence channels.
//!
//! Split out of `creature_anim` by 1160 stage two, along the line 1163 drew when it re-checked
//! `finalize_rig_worlds` and found it was not gameplay behaviour at all: "its entire input set is
//! rig/transform data, and its only `crate::` references are `billboard`, `rig_palette` and
//! `WorldCamera` — all three engine. **Mis-filing, not behaviour.**" The same is true of the pose
//! evaluator beside it (its only cross-module reference is `billboard`) and of the global-sequence
//! driver (which has none at all).
//!
//! The line between this and what stayed is **machinery versus policy**. Evaluating an
//! `AnimationPlayer` into per-bone locals, composing those into world matrices and palette rows,
//! and running a model's free channels are things a second program must do to put an animated
//! model in a world — so they are engine, and `benilla-worldview` gets them for free. *Which* clip
//! a unit plays, when a mount rider re-seats, when a rig is worth parking — that is a game's
//! business and stayed in `creature_anim`.
//!
//! [`AnimParked`] sits here for the same reason: `creature_anim::lod` decides *when* to park (a
//! policy, reading net entities and the target), but the marker gates the pose evaluator and the
//! billboard joint pass, both of which are here.

use bevy::prelude::*;

mod compose;
mod global_seq;
mod pose;

pub(crate) use compose::seed_rig_rows;
pub use compose::{finalize_rig_worlds, PosePost};
pub use global_seq::GlobalSeqDrive;
pub use pose::{RigAnchor, RigFrame, RigPose};

/// **Does the compare-before-write guard actually bite?** - the `rig_wr`/`rig_sk` journal pair.
///
/// `compose_rig_models` and `apply_global_sequences` stopped writing a `Transform` equal to the
/// one already there, because `Mut<Transform>` marks the row changed on `DerefMut` and a marked
/// row costs a transform-propagation subtree. That is only worth anything if the values really do
/// repeat, and there is a specific reason to doubt it: a global sequence samples a continuous
/// curve at `t % period` with `t` advancing every frame, so its result can differ in the last
/// float bits for ever while looking perfectly still.
///
/// A scene comparison cannot settle that - journal 34 had 22,470 entities against journal 33's
/// 37,330, because world population varies by spawn, so the two are not the same measurement. A
/// suppressed/written ratio is scene-independent and answers it directly. Counted unconditionally:
/// two relaxed increments against writes that were already walking a skeleton.
static ANCHOR_WROTE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ANCHOR_SKIPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Record one pose write that landed, or one the guard suppressed.
pub(crate) fn note_anchor_write(wrote: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    if wrote {
        ANCHOR_WROTE.fetch_add(1, Relaxed);
    } else {
        ANCHOR_SKIPPED.fetch_add(1, Relaxed);
    }
}

/// `(wrote, suppressed)` since the last call, and the reset.
pub fn take_anchor_writes() -> (u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (ANCHOR_WROTE.swap(0, Relaxed), ANCHOR_SKIPPED.swap(0, Relaxed))
}

/// This rig's per-bone pose evaluation is parked (decision 0448): the pose evaluator and the pose
/// post-passes skip it. The sequence clocks, the driver state machine, and the event scanner all
/// keep running — parking turns *sampling* off, nothing else.
#[derive(Component)]
pub struct AnimParked;

/// Wire the rig machinery in. Registered by `WorldPlugins`, so a program with no game still poses
/// and composes the rigs it spawns.
pub fn plugin(app: &mut App) {
    global_seq::plugin(app);
    pose::plugin(app);
    compose::plugin(app);
}
