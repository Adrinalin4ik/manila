//! The rig machinery the renderer needs: pose storage, the pose post-pass window, the rig world
//! composition and the free-running global-sequence channels. Which clip a unit plays and when a
//! rig parks are the game's policy (`creature_anim`); [`AnimParked`] lives here because it gates
//! the pose evaluator and the billboard joint pass.

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

/// Registers the rig machinery; `WorldPlugins` adds it, so a program with no game still poses its
/// rigs.
pub fn plugin(app: &mut App) {
    global_seq::plugin(app);
    pose::plugin(app);
    compose::plugin(app);
}
