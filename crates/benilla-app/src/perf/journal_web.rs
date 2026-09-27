//! Browser journals stay in a bounded session ring, outside quota-limited player settings.
//! Enabling `/console fpsJournal 1` exposes a download button and a console export hook.
//! Turning recording off retains the collected rows; re-enabling continues the same journal.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
const capacity = 3600; // The latest hour at one sample per second.
const rows = new Array(capacity);
let head = '', next = 0, count = 0;
export function journalText() {
    let csv = head;
    for (let i = 0; i < count; i++) csv += rows[(next - count + i + capacity) % capacity];
    return csv;
}
export function begin(header) {
    if (!head) head = header;
    window.__wenilla_fps_journal = { text: journalText, download };
    if (document.getElementById('wenilla-journal-download')) return;
    const button = document.createElement('button');
    button.id = 'wenilla-journal-download';
    button.textContent = 'download FPS journal';
    button.title = 'Download the latest hour of recorded performance samples';
    button.style.cssText = 'position:fixed;bottom:.5rem;right:.5rem;z-index:30';
    button.addEventListener('click', download);
    document.body.appendChild(button);
}
export function append(row) {
    rows[next] = row;
    next = (next + 1) % capacity;
    count = Math.min(count + 1, capacity);
}
export function fps(text) {
    let el = document.getElementById('manila-fps');
    if (!el) {
        el = document.createElement('div');
        el.id = 'manila-fps';
        el.style.cssText = 'position:fixed;top:.4rem;left:.4rem;z-index:31;font:600 13px/1.3 monospace;'
            + 'color:#ffd100;background:rgba(0,0,0,.55);padding:2px 6px;border-radius:3px;'
            + 'pointer-events:none;white-space:pre';
        document.body.appendChild(el);
    }
    el.textContent = text;
}
function download() {
    const url = URL.createObjectURL(new Blob([journalText()], {type: 'text/csv;charset=utf-8'}));
    const link = document.createElement('a');
    link.href = url;
    link.download = 'fps-journal.csv';
    document.body.appendChild(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
}
"#)]
extern "C" {
    pub(super) fn begin(header: &str);
    /// **The frame counter, in the PAGE rather than in Lua.**
    ///
    /// The in-game readout is drawn by the interface itself, so it goes dark exactly when the
    /// interface is switched off - which is the one measurement `/console uiLua 0` exists to take.
    /// A DOM node outside the canvas survives that, and costs one `textContent` write a second.
    pub(super) fn fps(text: &str);
    pub(super) fn append(row: &str);
}

/// **A way back from `/console uiLua 0`, which the console itself cannot give.**
///
/// The console is a frame of the player interface, so switching that off takes the console with
/// it: the command that turns the UI off is the last one it can accept. This installs
/// `window.manilaUiLua(true|false)` on the page, callable from the browser's own console, which
/// the client cannot switch off.
///
/// Installed once at startup and deliberately leaked (`forget`): it must outlive this call, and
/// there is exactly one for the life of the tab.
pub(crate) fn install_ui_lua_hook() {
    use wasm_bindgen::JsCast;
    let f = Closure::<dyn Fn(bool)>::new(|on: bool| {
        crate::ui_script::set_ui_lua(on);
        bevy::log::info!("player UI (Lua) {} — via window.manilaUiLua", if on { "ON" } else { "OFF" });
    });
    let _ = js_sys::Reflect::set(
        &js_sys::global(),
        &JsValue::from_str("manilaUiLua"),
        f.as_ref().unchecked_ref(),
    );
    f.forget();
}
