//! **The character-skin compositor, off the main thread.**
//!
//! Journal 43 priced what this exists to move: a body composite cost **248 ms** at the median and
//! **410 ms at p90**, 437 of them in a 357-second capture - 110 seconds of wall time, 31% of it,
//! all on the thread that draws. It is what the owner sees as a hitch whenever a character comes
//! into view, and it is why p90 fell from 82 ms to 52 ms the moment the network was switched off:
//! no arrivals, no composites.
//!
//! Two halves, measured separately: cost tracks decode count at r = +0.735, and the seconds with
//! FEW decodes still cost **202.8 ms** each. So caching the decodes takes about a third - the
//! owner's own `skinCacheMb` A/B moved the hit rate 15% -> 30% and the per-composite cost only
//! 239 -> 233 ms - and the blit floor underneath has to move somewhere else entirely.
//!
//! **No threads, no atomics.** A separate wasm instance in a Worker has its own linear memory, so
//! nothing is shared and nothing needs `SharedArrayBuffer`: a `CompositePlan` goes in as JSON, the
//! finished atlas comes back as a transferable buffer. That matters because the alternatives are
//! closed - bevy hard-disables its multi-threaded executor on wasm32 in its own `cfg`, so threads
//! would not buy the ECS anything even if we had them.
//!
//! The layering itself is NOT reimplemented here. `benilla_formats::CompositePlan::run`
//! is the single implementation, written against a reader; this module supplies one that fetches
//! and decodes, exactly as the main thread supplies one that reads its cache. Two copies of a
//! texel-exact order is how a character ends up with the wrong face.

use std::collections::HashMap;
use std::sync::Arc;

use benilla_formats::CompositePlan;
use benilla_formats::BlpMipChain;
use wasm_bindgen::prelude::*;

/// Fetch one file from the host's Data URL scheme. Async, and off the main thread - which is the
/// whole point: the same read on the main thread is a synchronous `XMLHttpRequest` that blocks the
/// frame until the bytes arrive.
async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
    let scope: web_sys::WorkerGlobalScope = js_sys::global().dyn_into().ok()?;
    let resp: web_sys::Response = wasm_bindgen_futures::JsFuture::from(scope.fetch_with_str(url))
        .await
        .ok()?
        .dyn_into()
        .ok()?;
    if !resp.ok() {
        return None;
    }
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer().ok()?)
        .await
        .ok()?;
    Some(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Render one body atlas from a plan.
///
/// `plan_json` is a serialized [`CompositePlan`]; `data_url_for` is the page's own URL builder, passed
/// in rather than rebuilt here so the worker asks for the SAME addresses the client does - version
/// pin included. Three URL shapes for one file would be three cache entries and a prefetch that
/// warms none of them.
///
/// Returns the atlas in `benilla_formats::encode_atlas`'s shape - a flat buffer, because it
/// crosses as a transfer rather than a copy, and because the client decodes it with that module's
/// own `decode_atlas`. One owner for both ends.
#[wasm_bindgen]
pub async fn render_body(plan_json: String, url_prefix: String, url_suffix: String) -> Option<Vec<u8>> {
    let plan: CompositePlan = serde_json::from_str(&plan_json).ok()?;
    // One decode per distinct path per plan, and the miss is remembered too - re-fetching a path
    // that 404'd once per step would turn a wardrobe into a round trip storm.
    //
    // **Candidates stop at the first hit, exactly as `CompositePlan::run` will consume them.** The
    // first cut fetched every candidate of every step up front, because the renderer is sync and
    // `fetch` is not. That is correct and wasteful: `equip_region_candidates` orders the list
    // `['U', <sex>]` and the universal texture almost always exists, so the gendered one was
    // fetched, 404'd and thrown away for nearly every dressed region. The owner saw the litter in
    // his network tab - `..._Glove_AL_F.blp` 404 while the archive holds `..._Glove_AL_U.blp` -
    // and the picture was right the whole time because `find_map` never asked for the second one.
    //
    // Walking the plan in the renderer's own order costs nothing extra and asks for nothing the
    // renderer will not consult.
    let mut seen: HashMap<String, Option<Arc<BlpMipChain>>> = HashMap::new();
    let mut fetch_once = async |path: &str,
                                seen: &mut HashMap<String, Option<Arc<BlpMipChain>>>|
     -> bool {
        if let Some(hit) = seen.get(path) {
            return hit.is_some();
        }
        let url = format!(
            "{url_prefix}{}{url_suffix}",
            benilla_formats::web::encode_name(&path.replace('/', "\\"))
        );
        let decoded = match fetch_bytes(&url).await {
            Some(bytes) => benilla_formats::blp_bytes_to_mip_chain(&bytes)
                .ok()
                .map(Arc::new),
            None => None,
        };
        let ok = decoded.is_some();
        seen.insert(path.to_string(), decoded);
        ok
    };
    fetch_once(&plan.base, &mut seen).await;
    for (candidates, _tile) in &plan.layers {
        for path in candidates {
            if fetch_once(path, &mut seen).await {
                break;
            }
        }
    }
    // The blit stack is upstream's `CompositePlan::run`, the same one the main thread uses. Two
    // copies of a texel-exact layering order is how a character ends up with the wrong face.
    let atlas = plan
        .run(|path: &str| seen.get(path).cloned().flatten())
        .ok()?;
    Some(benilla_formats::encode_atlas(&atlas))
}
