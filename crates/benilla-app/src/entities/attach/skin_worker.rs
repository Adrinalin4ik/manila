//! **The character-skin composite, handed to a Worker.**
//!
//! Journal 43 priced the thing this exists to move: a body composite cost **248 ms** at the median
//! and **410 ms at p90**, 437 of them inside a 357-second capture — 110 seconds, 31% of wall time,
//! on the thread that draws. It is the hitch the owner sees whenever a character walks into view,
//! and it is why frame p90 fell 81.84 → 52.03 ms the moment the network was switched off: no
//! arrivals, no composites.
//!
//! `crates/manila-skin` is the same blit stack compiled as its own wasm module, instantiated
//! inside a Web Worker with its OWN linear memory — so no atomics, no `SharedArrayBuffer`, no
//! cross-origin isolation, and no dependence on threads that bevy hard-disables on wasm32 anyway
//! (`bevy_tasks/src/lib.rs:21`). `web/skin_worker.js` owns that Worker; this module is the client
//! half.
//!
//! **A reserved handle, not a second material.** The dressing path is re-entered only on an
//! equipment change (`redress_player_looks` is gated on `*live == applied.0`), so a placeholder
//! material would stay a placeholder for ever on every body that never re-dresses — every NPC.
//! Instead the miss arm renders the plan's BASE ONLY (one decode, no blits), puts it behind a
//! handle, caches THAT handle under the real key, and [`drain_skin_worker`] later replaces the
//! image *behind the same handle*. `AssetEvent::Modified` re-uploads it; nothing re-dresses, and
//! `AppliedEquipment` semantics are untouched. The visible cost is a character wearing its bare
//! skin for the few frames the worker is busy.
//!
//! **Poll, not callback.** A Bevy system cannot await and must not be re-entered from JS, so a
//! request returns immediately and the answer is collected on a later frame by id.
//!
//! Every failure route ends in the old synchronous composite, never in a wrong atlas: no worker
//! (native, or a browser that would not start one) composites inline at the call site; a worker
//! that starts and then gives up is finished here on the main thread from the plan we kept.

use benilla_formats::BodyPlan;
use bevy::prelude::*;

use super::super::SkinSections;
use benilla_assets::{repeat_texture_authored, LockRecover, WorldAssets};

/// One composite in flight: the id the page answers under, the handle whose image it will become,
/// and the plan that produced it — kept because the main thread has to be able to finish the job
/// itself if the worker gives up.
struct PendingSkin {
    id: u32,
    handle: Handle<Image>,
    plan: BodyPlan,
}

/// The composites currently out at the Worker.
#[derive(Resource, Default)]
pub(in crate::entities) struct PendingSkins {
    next: u32,
    live: Vec<PendingSkin>,
}

/// The page's two entry points, read by name rather than declared as `wasm_bindgen` imports.
///
/// A missing import is a load-time failure of the WHOLE module; a missing property is a `None`
/// here. `web/skin_worker.js` is imported by `web/boot.js` and so reaches both pages, but the
/// client must still boot on a page that does not have it — that is the difference between an
/// optional feature and a black canvas.
#[cfg(target_arch = "wasm32")]
fn js_fn(name: &str) -> Option<js_sys::Function> {
    use wasm_bindgen::JsCast;
    js_sys::Reflect::get(&js_sys::global(), &wasm_bindgen::JsValue::from_str(name))
        .ok()?
        .dyn_into::<js_sys::Function>()
        .ok()
}

/// Post `plan` to the Worker. `None` when there is no worker to take it — the caller then does the
/// work itself, exactly as it did before this module existed.
#[cfg(target_arch = "wasm32")]
fn post(id: u32, plan: &BodyPlan) -> Option<()> {
    use wasm_bindgen::JsValue;
    let f = js_fn("__manila_skin_request")?;
    let json = benilla_formats::plan_to_json(plan)?;
    // The SAME addresses the client's own sync reads use, version pin included; the worker
    // appends the encoded name between them. Three URL shapes for one file would be three browser
    // cache entries and a boot prefetch that warms none of them.
    let pin = benilla_formats::web::cache_pin();
    let prefix = format!("{}/", benilla_formats::web::data_base());
    let suffix = if pin.is_empty() {
        String::new()
    } else {
        format!("?v={pin}")
    };
    let ok = f
        .apply(
            &JsValue::NULL,
            &js_sys::Array::of4(
                &JsValue::from_f64(id as f64),
                &JsValue::from_str(&json),
                &JsValue::from_str(&prefix),
                &JsValue::from_str(&suffix),
            ),
        )
        .ok()?;
    ok.as_bool().unwrap_or(false).then_some(())
}

/// `None` while the Worker is still at it; `Some(bytes)` once, and an EMPTY `bytes` when it gave
/// up. Handed over exactly once, so two polls cannot both act on one answer.
#[cfg(target_arch = "wasm32")]
fn collect(id: u32) -> Option<Vec<u8>> {
    use wasm_bindgen::JsValue;
    let f = js_fn("__manila_skin_take")?;
    let v = f.call1(&JsValue::NULL, &JsValue::from_f64(id as f64)).ok()?;
    if v.is_null() || v.is_undefined() {
        return None;
    }
    Some(js_sys::Uint8Array::new(&v).to_vec())
}

#[cfg(not(target_arch = "wasm32"))]
fn post(_id: u32, _plan: &BodyPlan) -> Option<()> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn collect(_id: u32) -> Option<Vec<u8>> {
    None
}

impl PendingSkins {
    /// Ask the Worker for `plan`'s atlas, to arrive behind `handle`. `false` when there is no
    /// Worker, and the caller must composite it itself.
    pub(in crate::entities) fn request(&mut self, handle: &Handle<Image>, plan: BodyPlan) -> bool {
        let id = self.next;
        if post(id, &plan).is_none() {
            return false;
        }
        self.next = self.next.wrapping_add(1);
        self.live.push(PendingSkin {
            id,
            handle: handle.clone(),
            plan,
        });
        true
    }
}

/// Collect finished atlases and write them behind the handles the bodies are already wearing.
///
/// The cost this carries on the main thread is the decode of a flat buffer and one upload — the
/// blits, the fetches and the BLP decodes all happened in the Worker. It is metered into the same
/// `skin_us` column as the synchronous composite so that column keeps meaning exactly what it
/// meant before: microseconds of the DRAWING thread spent on skins.
///
/// **Replacing the image is not enough, and this is the last hop.** `bevy_render`'s
/// `render_asset.rs:279` re-extracts a Modified *Image* and builds a NEW `GpuImage` with a new
/// `Texture`; `bevy_pbr`'s `material.rs` declares no render-asset dependency on the images a
/// material samples, so a material whose bind group was already prepared goes on pointing at the
/// old texture — for ever. That is the naked-body defect: a character rendered before its atlas
/// landed kept the base-only skin, while one whose atlas arrived before it was ever drawn came out
/// dressed. Mixed, and deterministic by timing, which is exactly what it looked like.
///
/// So every material sampling a replaced atlas is touched through `get_mut`, which emits
/// `AssetEvent::Modified` for the MATERIAL and re-prepares its bind group against the new texture.
/// One pass over ~2,240 materials on an arrival frame only; frames with nothing to collect leave
/// at the first line.
pub(in crate::entities) fn drain_skin_worker(
    mut pending: ResMut<PendingSkins>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<benilla_assets::materials::WowModelMaterial>>,
    world_assets: Option<Res<WorldAssets>>,
    sections: Option<Res<SkinSections>>,
) {
    if pending.live.is_empty() {
        return;
    }
    let mut finished = Vec::new();
    let mut replaced: Vec<AssetId<Image>> = Vec::new();
    for (i, p) in pending.live.iter().enumerate() {
        let Some(bytes) = collect(p.id) else { continue };
        // Timed per atlas, and only when one actually arrived: a sample taken on every frame that
        // merely HAS work outstanding would be a near-zero, and `skins_new` counts samples. That
        // is how a meter starts agreeing with whoever is reading it.
        let started = bevy::platform::time::Instant::now();
        // Empty: the Worker could not do it. Finish it here from the plan we kept — the old cost,
        // and only on the path where the alternative is a body that never gets its face.
        let atlas = benilla_formats::decode_atlas(&bytes).or_else(|| {
            let (world, sections) = (world_assets.as_deref()?, sections.as_deref()?);
            let chain = &mut world.chain.lock_recover();
            sections.0.render_plan(chain, &p.plan).ok()?
        });
        if let Some(atlas) = atlas {
            // The return is checked, not discarded: a failed insert leaves the base-only skin
            // behind the handle, which is the naked body this module's header describes — and
            // `replaced` must not then claim the atlas landed, or the material touch below would
            // re-prepare a bind group against a texture that never changed.
            match images.insert(
                p.handle.id(),
                repeat_texture_authored(benilla_assets::for_upload(atlas), (true, true)),
            ) {
                Ok(()) => replaced.push(p.handle.id()),
                Err(e) => warn!("skin worker: atlas insert refused, body stays bare: {e}"),
            }
        }
        crate::perf::journal::note_skin_composite(started.elapsed().as_micros() as u64);
        finished.push(i);
    }
    // Descending, so each `swap_remove` can only pull in an index already dealt with.
    for i in finished.into_iter().rev() {
        pending.live.swap_remove(i);
    }
    if !replaced.is_empty() {
        // Gathered first, because `get_mut` needs the mutable borrow the scan is holding.
        let stale: Vec<_> = materials
            .iter()
            .filter(|(_, m)| {
                m.base
                    .base_color_texture
                    .as_ref()
                    .is_some_and(|tex| replaced.contains(&tex.id()))
            })
            .map(|(id, _)| id)
            .collect();
        for id in stale {
            // The touch IS the fix; the value needs no edit. See this function's header.
            let _ = materials.get_mut(id);
        }
    }
}
