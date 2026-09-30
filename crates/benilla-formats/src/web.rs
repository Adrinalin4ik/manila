//! Web-target chain plumbing: `wasm32-unknown-unknown` has no filesystem, so [`crate::Chain`]
//! answers every read/existence/listing question with an HTTP call against the Data URL scheme a
//! companion web host (`wenilla-host`, Lane H) serves — `GET {origin}/data/{encoded name}`,
//! `HEAD` for existence, `GET /data/__index` for the name list. This module is that HTTP shim.
//!
//! [`encode_name`] is plain string math with no browser dependency, so it is exercised natively
//! (`tests/web_names.rs`); [`data_base`], [`fetch_sync`], and [`exists_sync`] need `web-sys` and
//! only make sense — and only compile their bodies — on `wasm32`.

/// Percent-encode `name` exactly like JavaScript's `encodeURIComponent`: the unreserved set is
/// `A-Za-z0-9-_.!~*'()`; every other byte, including `\` (chain names are internally
/// backslash-separated), becomes `%XX` uppercase-hex. This is the client half of the Data URL
/// scheme's "the full name percent-encoded as one component" rule — the web host's decode must be
/// this function's exact inverse, so it is kept pure and unit-tested rather than left to whatever
/// a URL-building crate happens to escape.
pub fn encode_name(name: &str) -> String {
    const UNRESERVED: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'()";
    let mut out = String::with_capacity(name.len());
    for &byte in name.as_bytes() {
        if UNRESERVED.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `name` with its `Interface\AddOns\` prefix stripped, or `None` when it carries none.
///
/// An addon's own art, fonts and audio are NOT in the patch chain — `read_chain_or_loose`'s doc
/// says so, and the desktop reference reads them off the install tree. That tree is a filesystem,
/// which wasm does not have, so in the browser these belong to the host's `/addons` route and
/// asking `/data` for them can only ever 404.
///
/// Case-insensitive, and both separators: the reference is a Windows client, and a sprite path
/// reaches the asset reader lowercased with backslashes while a `.toc` may write either.
pub fn addons_rel(name: &str) -> Option<&str> {
    const PREFIXES: [&str; 2] = ["interface\\addons\\", "interface/addons/"];
    PREFIXES.iter().find_map(|prefix| {
        let head = name.get(..prefix.len())?;
        head.eq_ignore_ascii_case(prefix)
            .then(|| name.get(prefix.len()..))
            .flatten()
    })
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use wasm_bindgen::JsValue;
    use web_sys::XmlHttpRequest;

    /// The root every chain fetch is served from: `/data` under this page's own origin. The web
    /// host answers both the client bundle and the `/data/*` routes from one process (Lane H), so
    /// there is never a cross-origin question to configure — this is the whole answer.
    pub fn data_base() -> String {
        let origin = web_sys::window()
            .expect("benilla_formats::web only runs inside a browser tab")
            .location()
            .origin()
            .expect("window.location.origin");
        format!("{origin}/data")
    }

    /// **The mounted install's fingerprint** (`GET /data/__chain`), fetched once and appended to
    /// every `/data/*` URL as `?v=`.
    ///
    /// It is a cache key, not a parameter: the host ignores the query, and the browser keys its
    /// cache on the whole URL. Every file there is served `immutable, max-age=31536000` while the
    /// path alone says nothing about WHICH install is mounted, so pointing the host at a different
    /// `--data` otherwise leaves a year of the previous one answering under the same addresses.
    ///
    /// **One source of truth on purpose.** Three places build these URLs - the chain's sync reads,
    /// bevy's async asset reader, and the page's boot prefetch (`web/boot.js`) - and they must
    /// agree byte for byte or the prefetch warms an address the reads never ask for. That would
    /// silently undo the one thing the prefetch exists for.
    ///
    /// Empty when the host has no such route: the URLs are then exactly what they were before the
    /// pin, which is the only acceptable failure for something whose job is to make a cache
    /// correct.
    pub fn cache_pin() -> &'static str {
        static PIN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        PIN.get_or_init(|| {
            fetch_sync(&format!("{}/__chain", data_base()))
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_hexdigit()))
                .unwrap_or_default()
        })
    }

    /// `{data_base()}/{encoded name}` with the cache pin appended - the one URL builder.
    pub fn data_url(name: &str) -> String {
        let pin = cache_pin();
        if pin.is_empty() {
            return format!("{}/{}", data_base(), super::encode_name(name));
        }
        format!("{}/{}?v={pin}", data_base(), super::encode_name(name))
    }

    /// `{origin}/addons/<rel>` — [`data_url`]'s sibling for a path
    /// [`super::addons_rel`] claimed, each component percent-encoded so an addon folder with a
    /// space in its name (`Attack bar`) addresses correctly.
    ///
    /// **No `?v=` pin, deliberately.** That pin is the mounted ARCHIVE's fingerprint, and an addon
    /// folder is not in the archive: pinning its files to it would key a year-long cache on
    /// something that does not describe them. The route answers `private, max-age=3600` on its
    /// own terms, which is the operator editing his own files and wanting to see it.
    pub fn addons_url(rel: &str) -> String {
        let base = data_base();
        let root = base.strip_suffix("/data").unwrap_or(&base);
        let path = rel
            .replace('\\', "/")
            .split('/')
            .map(super::encode_name)
            .collect::<Vec<_>>()
            .join("/");
        format!("{root}/addons/{path}")
    }

    /// Append `name` to the page's boot-read trace, if the page armed one — the input to
    /// `web/boot-manifest.json` (see `web/boot.js`). A page that wants the trace defines
    /// `window.__wenilla_boottrace = []` before `init()` (the `?boottrace=1` switch does);
    /// every chain read then pushes its raw backslash name in true first-need order, and a
    /// person copies the deduplicated array out of the console into the manifest. Without the
    /// array this is one snapshotted `Reflect::get` for the whole session and per-call nothing —
    /// tracing must cost the boot it measures as close to zero as possible.
    /// One console line naming how many distinct names have gone past the index to the host.
    ///
    /// The index exists because per-name `HEAD`s were 2,145 asks and ~125 s of frozen tab in one
    /// world entry; making a miss ask the host again pays a round trip per distinct name, and
    /// this is how that price stays a measurement instead of a hope. Called only at powers of
    /// ten, so the instrument cannot become the cost.
    pub fn log_index_misses(distinct: usize) {
        web_sys::console::log_1(
            &format!(
                "chain: {distinct} distinct name(s) not in the index have been verified against \
                 the host (an incomplete (listfile) is normal on a server's own content)"
            )
            .into(),
        );
    }

    pub fn trace(name: &str) {
        use wasm_bindgen::JsCast;
        thread_local! {
            static TRACE: std::cell::OnceCell<Option<js_sys::Array>> =
                const { std::cell::OnceCell::new() };
        }
        TRACE.with(|cell| {
            let arr = cell.get_or_init(|| {
                let window = web_sys::window()?;
                js_sys::Reflect::get(&window, &"__wenilla_boottrace".into())
                    .ok()?
                    .dyn_into::<js_sys::Array>()
                    .ok()
            });
            if let Some(arr) = arr {
                arr.push(&name.into());
            }
        });
    }

    /// A blocking `XMLHttpRequest` GET, returning the raw response bytes.
    ///
    /// Synchronous on purpose: [`crate::Chain::read`] is called from Bevy systems on the main
    /// thread today — `WorldAssets` and ~60 other call sites read the chain synchronously — and
    /// making the whole call chain async to reach it would ripple through every one of them. A
    /// *synchronous* `XMLHttpRequest` is the one browser primitive that can return bytes from a
    /// call that must return before the function does; the Bevy `AssetReader` path
    /// (`benilla-assets`) is already `async` end to end, so it uses `fetch` instead (no such
    /// constraint there).
    ///
    /// A sync XHR's `responseType` is stuck at the default `""` (text), so an `arraybuffer`
    /// response type — the normal way to get binary out of an XHR — isn't available here. The
    /// standard workaround, used below, is `override_mime_type("text/plain; charset=x-user-defined")`:
    /// it forces the browser to decode the response body as one code unit (0x00-0xFF) per byte
    /// instead of guessing UTF-8 and mangling anything non-ASCII into U+FFFD, so `response_text()`
    /// round-trips arbitrary binary losslessly through `& 0xFF`.
    pub fn fetch_sync(url: &str) -> std::io::Result<Vec<u8>> {
        let xhr = XmlHttpRequest::new().map_err(js_err)?;
        xhr.open_with_async("GET", url, false).map_err(js_err)?;
        xhr.override_mime_type("text/plain; charset=x-user-defined")
            .map_err(js_err)?;
        xhr.send().map_err(js_err)?;
        match xhr.status().map_err(js_err)? {
            200 => {
                let text = xhr.response_text().map_err(js_err)?.unwrap_or_default();
                Ok(text.chars().map(|c| (c as u32 & 0xff) as u8).collect())
            }
            404 => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                url.to_string(),
            )),
            status => Err(std::io::Error::other(format!("{url}: HTTP {status}"))),
        }
    }

    /// A blocking `HEAD` request: does the web host have this name? No body to decode, so no mime
    /// override is needed — just the status line.
    ///
    /// **`None` is a third answer and it is load-bearing: "could not ask".** This used to return
    /// a plain `bool`, which made a browser that refused to send the request indistinguishable
    /// from a host that answered 404 — and [`crate::Chain::contains`] writes that answer into a
    /// cache it never revisits. Observed live: Chrome began failing sends with
    /// `ERR_NO_BUFFER_SPACE` under this client's request rate, and every name asked about during
    /// the outage would have been remembered as ABSENT for the rest of the session, long after
    /// the sockets came back. A transport failure is not a fact about the file.
    pub fn exists_sync(url: &str) -> Option<bool> {
        let xhr = XmlHttpRequest::new().ok()?;
        xhr.open_with_async("HEAD", url, false).ok()?;
        xhr.send().ok()?;
        Some(xhr.status().ok()? == 200)
    }

    fn js_err(e: JsValue) -> std::io::Error {
        std::io::Error::other(format!("{e:?}"))
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::{
    addons_url, cache_pin, data_base, data_url, exists_sync, fetch_sync, log_index_misses, trace,
};

#[cfg(test)]
mod tests {
    use super::addons_rel;

    /// The prefix is matched however the path spells it, and nothing else is claimed.
    #[test]
    fn addon_paths_are_claimed_whatever_their_case_or_separator() {
        assert_eq!(addons_rel("interface\\addons\\shagudps\\img\\announce.tga"), Some("shagudps\\img\\announce.tga"));
        assert_eq!(
            addons_rel("Interface/AddOns/pfQuest/img/init/simple.tga"),
            Some("pfQuest/img/init/simple.tga")
        );
        // A chain path that merely starts with the interface folder stays the chain's.
        assert_eq!(addons_rel("Interface\\Icons\\INV_Misc_Bag_08.blp"), None);
        assert_eq!(addons_rel("interface"), None);
    }
}
