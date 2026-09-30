//! The body-composite Web Worker's transport.
//!
//! **Why this survives upstream's own off-thread composite.** `entities::skin_composite` runs a
//! look's composite on `AsyncComputeTaskPool`, which is the right answer everywhere bevy has
//! threads. wasm32 is not such a place: bevy hard-disables the multi-threaded executor there
//! (`bevy_tasks/src/lib.rs:21`), so a task on that pool is still the main thread and "off the
//! main thread" can only mean a real Worker. `web/skin_worker.js` owns that Worker; this module
//! is the client half, and `crates/manila-skin` is what runs inside it.
//!
//! Nothing is shared and nothing needs `SharedArrayBuffer`: a `CompositePlan` goes in as JSON and
//! an encoded atlas comes back. The landing machinery this module used to carry is gone with
//! upstream's design - a body is not drawn until its atlas exists and the atlas arrives as a NEW
//! handle, so there is no longer an image to replace behind handles bodies already wear, and no
//! materials to touch afterwards.

use benilla_formats::CompositePlan;

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
pub(in crate::entities) fn post(id: u32, plan: &CompositePlan) -> Option<()> {
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
pub(in crate::entities) fn collect(id: u32) -> Option<Vec<u8>> {
    use wasm_bindgen::JsValue;
    let f = js_fn("__manila_skin_take")?;
    let v = f.call1(&JsValue::NULL, &JsValue::from_f64(id as f64)).ok()?;
    if v.is_null() || v.is_undefined() {
        return None;
    }
    Some(js_sys::Uint8Array::new(&v).to_vec())
}

#[cfg(not(target_arch = "wasm32"))]
pub(in crate::entities) fn post(_id: u32, _plan: &CompositePlan) -> Option<()> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
pub(in crate::entities) fn collect(_id: u32) -> Option<Vec<u8>> {
    None
}
