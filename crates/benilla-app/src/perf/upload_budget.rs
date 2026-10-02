//! `uploadBudgetMb`: bevy's per-frame GPU upload cap, reachable from a browser.
//!
//! A crowd arriving together lands every new body's meshes and images in one frame's
//! `PrepareAssets`, and the browser then takes the whole upload at submit and present: journal 94
//! has a second at 764 ms per frame whose `rapp` is 654 ms, of which the named render tiles
//! (`r_assets` + `r_prepare` + `r_render`) are about 30 ms, while `mats` rose by 800 and `emat` by
//! 3,600 in six seconds and `pipes` stayed flat (so not pipeline compilation). That the rest is
//! upload is the hypothesis this lever tests, not a measured fact.
//!
//! bevy already has the cure: `RenderAssetBytesPerFrame` (bevy_render-0.18.1, render_asset.rs)
//! makes `prepare_assets` defer a sized asset to the next frame once the frame's bytes are spent,
//! always writing at least one. It is read into the render world on every extract
//! (`extract_render_asset_bytes_per_frame`), so changing it at runtime takes effect next frame.
//! It existed here only as `WOW_UPLOAD_BUDGET` (`benilla-world/src/world_plugins.rs`), an env var,
//! which `std::env::var` can never see on wasm32 - so it had never run in the browser.
//!
//! The cost is latency, not frames: a deferred mesh or image is simply not drawn yet, and a
//! material whose texture is not prepared retries (`PrepareAssetError::RetryNextUpdate`). Arriving
//! bodies already fade in over two seconds, which covers a few frames of it.
//!
//! Browser only: the native build keeps its env-var lever and its unlimited default. `0` removes
//! the cap.

use std::sync::atomic::{AtomicUsize, Ordering};

use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetBytesPerFrame;

/// The cvar's default, in MiB per frame; keep in step with `cvars/table.rs`. At 60 fps that is
/// still ~480 MiB/s, so only a burst ever waits.
pub(crate) const DEFAULT_MB: usize = 8;

static MB: AtomicUsize = AtomicUsize::new(DEFAULT_MB);

/// The `uploadBudgetMb` observer's write; `0` is unlimited.
pub(crate) fn set_mb(mb: usize) {
    MB.store(mb, Ordering::Relaxed);
}

/// Mirror the cvar into bevy's resource whenever it moves, the first frame included.
fn apply(mut commands: Commands, mut applied: Local<Option<usize>>) {
    let mb = MB.load(Ordering::Relaxed);
    if *applied == Some(mb) {
        return;
    }
    *applied = Some(mb);
    commands.insert_resource(match mb {
        0 => RenderAssetBytesPerFrame::default(),
        mb => RenderAssetBytesPerFrame::new(mb * 1024 * 1024),
    });
    info!("GPU upload budget: {}", if mb == 0 { "unlimited".to_string() } else { format!("{mb} MiB/frame") });
}

pub(crate) fn plugin(app: &mut App) {
    #[cfg(target_arch = "wasm32")]
    app.add_systems(First, apply);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (app, apply);
}
