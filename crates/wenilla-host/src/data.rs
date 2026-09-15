//! `GET`/`HEAD /data/{*name}` and `GET /data/__index` — the Data URL scheme Lane A's wasm
//! `Chain` fetches against verbatim (see the plan's "Shared interfaces"): the browser build has
//! no filesystem, so every asset load the client makes becomes one of these requests, answered
//! straight from the same [`Chain`] the native client reads off disk.
//!
//! The name in the URL is percent-decoded here, not left to axum's own path-segment decoding —
//! `Chain` names use `\` as their separator (`Interface\Glues\...`), and the wildcard capture
//! would otherwise hand us a single decoded segment with no way to tell an encoded `/` (a literal
//! path separator in some other scheme) apart from an encoded `\`. Decoding the raw tail
//! ourselves and mapping `/` -> `\` afterward matches exactly what the client's `encode_name`
//! produces on the way out.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use benilla_formats::Chain;
use tokio::sync::OnceCell;
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;

/// Router state: the opened patch chain. `Chain::read`/`list` are `&self` and lock-free (see the
/// chain module doc). The serialized index is shared across requests for this mounted chain.
#[derive(Clone)]
pub struct DataState {
    pub chain: Arc<Chain>,
    index: Arc<IndexCache>,
    /// Directory holding the server's Warden `.cr` files, when the operator has one. `None` — the
    /// default, and what `wenilla-realm` passes — leaves the route unmounted entirely.
    modules: Option<Arc<PathBuf>>,
}

/// Single-flight initialization; transient list errors are retried on the next request.
#[derive(Default)]
struct IndexCache(Arc<OnceCell<Bytes>>);

impl IndexCache {
    async fn get(
        &self,
        build: impl FnOnce() -> anyhow::Result<Vec<u8>> + Send + 'static,
    ) -> anyhow::Result<Bytes> {
        if let Some(bytes) = self.0.get() {
            return Ok(bytes.clone());
        }
        let cell = Arc::clone(&self.0);
        // The initializer owns the cell independently of this request. Dropping a request
        // cannot cancel spawn_blocking, so keep its result and single-flight guard alive too.
        tokio::spawn(async move {
            cell.get_or_try_init(|| async move {
                tokio::task::spawn_blocking(build).await?.map(Bytes::from)
            })
            .await
            .cloned()
        })
        .await?
    }
}

/// Build the `/data/*` router. Kept separate from `static_site`'s and `ws`'s so `main.rs` can
/// merge them with `Router::merge` and each test file can stand its half up alone.
pub fn router(chain: Arc<Chain>) -> Router {
    router_with_modules(chain, None)
}

/// [`router`] plus `GET /data/warden_modules/{id}.cr`, reading the loose `.cr` files a Warden
/// module lane needs out of `modules`.
///
/// **They cannot come from the chain**, which is what the first cut of this got wrong: `.cr` files
/// are the operator's server files sitting beside the archives, not members of them, so
/// `/data/{*name}` answers 404 for every one of them. The web client fetched exactly that URL and
/// the module lane refused with "no .cr for module …" — a 404 wearing the costume of a missing
/// module.
///
/// Off unless asked for. `wenilla-realm` mounts [`router`] and therefore serves none of this, which
/// is its behaviour today; turning it on there is a deliberate change, not something a signature
/// should hand over by default.
pub fn router_with_modules(chain: Arc<Chain>, modules: Option<PathBuf>) -> Router {
    let mut router = Router::new()
        .route("/data/__index", get(index))
        // axum's matchit picks the more specific literal route above over this wildcard on its
        // own — registration order here doesn't matter.
        .route("/data/{*name}", get(file).head(file));
    if modules.is_some() {
        // Registered ahead of the wildcard for the same matchit reason as `__index`; the test
        // below is what proves the specific route actually wins, rather than trusting it.
        router = router.route("/data/warden_modules/{id}", get(warden_module));
    }
    router
        // On the fly, not precompressed like `static_site`'s: that route serves the handful of
        // files `web-build.sh` writes and can pay brotli once at build time, while this one
        // serves an arbitrary slice of a 5 GB install nobody can enumerate ahead of time.
        // What keeps the cost sane is the predicate, not the level — see [`content_type`].
        .layer(CompressionLayer::new().compress_when(compressible()))
        // COEP `require-corp` on the document (see `static_site`) makes every subresource prove
        // it consents to being embedded. Same-origin responses pass that check without a header,
        // and today `/data` is always same-origin with the page — this is the belt to that
        // braces, so an operator who ever fronts the two from different origins gets a
        // recognisable failure instead of a world that loads with holes in it.
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .with_state(DataState {
            chain,
            index: Arc::default(),
            modules: modules.map(Arc::new),
        })
}

/// The on-disk filename for a requested module id, or `None` when the id is not one.
///
/// This is the whole path-safety story, which is why it is a function and not three lines inside
/// the handler: a name that passes is exactly 32 hex digits plus `.cr`, so it contains no
/// separator, no `..` and no dot beyond the extension, and the join cannot leave the directory.
/// Uppercased on the way out because that is how the files are named and how `module_id_hex`
/// spells an id — on a case-sensitive filesystem the two have to agree.
fn module_filename(id: &str) -> Option<String> {
    let stem = id.strip_suffix(".cr")?;
    (stem.len() == 32 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("{}.cr", stem.to_ascii_uppercase()))
}

/// `GET /data/warden_modules/{id}.cr` — one module's challenge/response file.
///
/// `id` is checked by [`module_filename`] before it reaches the filesystem. Anything it rejects is
/// a 404 rather than a 400: a probe should not learn from the status code whether it guessed the
/// shape right.
async fn warden_module(
    AxumPath(id): AxumPath<String>,
    State(state): State<DataState>,
) -> Response {
    let Some(dir) = state.modules.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(name) = module_filename(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = dir.join(name);
    match tokio::task::spawn_blocking(move || std::fs::read(path)).await {
        Ok(Ok(bytes)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable",
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(_)) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "warden module read task panicked");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Which bodies are worth compressing, as a value rather than inline in [`router`] so the tests
/// can put the real predicate behind a stub handler — otherwise it could only be exercised
/// against a chain, and every assertion about it would skip on a box with no game install.
///
/// `DefaultPredicate` already contributes the size floor and skips `image/*`; naming
/// `NotForContentType::IMAGES` again keeps the BLP half of [`content_type`]'s contract visible
/// where the decision is made, and `audio/mpeg` is the one family it does not cover.
fn compressible() -> impl Predicate {
    DefaultPredicate::new()
        .and(NotForContentType::IMAGES)
        .and(NotForContentType::const_new("audio/mpeg"))
}

/// The `Content-Type` for a chain name — chosen for what it tells the compression predicate,
/// not for the browser, which never looks (the wasm `Chain` reads bytes and parses them by
/// signature).
///
/// The chain hands us *inflated* bytes: `Chain::read` has already undone the MPQ's own zlib, so
/// a DBC or an ADT arrives here as the raw structured form and compresses like one. Two families
/// do not, and they are most of the volume:
///
/// - **BLP** — the texels are DXT blocks (or a palette + indices). Already compressed; brotli
///   spends CPU to add bytes. `image/x-blp` puts them in the family `DefaultPredicate` skips.
/// - **MP3** — likewise, and audio is *not* in that default skip set, so it needs naming.
///
/// Anything else falls through to `application/octet-stream` and gets compressed. That is the
/// right default: the unlisted formats are DBC, ADT, WDT, WDL, M2, WMO and the FrameXML `.lua`
/// / `.xml` / `.toc` text, and every one of them is structured and redundant. A `.wav` lands
/// here too, deliberately — vanilla's are uncompressed PCM.
fn content_type(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or_default();
    if ext.eq_ignore_ascii_case("blp") {
        "image/x-blp"
    } else if ext.eq_ignore_ascii_case("mp3") {
        "audio/mpeg"
    } else {
        "application/octet-stream"
    }
}

/// A chain-read failure that means "this path doesn't exist in the composite" — the two shapes
/// `Chain::read` produces for a missing file (never mounted at all, or tombstoned by a patch) —
/// as opposed to a real I/O fault reading a corrupt archive, which is a 500, not a 404.
fn is_missing(err: &anyhow::Error) -> bool {
    let msg = err.to_string();
    msg.contains("not in patch chain") || msg.contains("deleted from patch chain")
}

async fn file(method: Method, uri: Uri, State(state): State<DataState>) -> Response {
    let raw = uri.path().strip_prefix("/data/").unwrap_or("");
    let decoded = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
    let name = decoded.replace('/', "\\");
    // Before the move into `spawn_blocking` — the classification is pure string math on the
    // name and yields a `&'static str`, so it costs nothing to take it out of the task's way.
    let content_type = content_type(&name);

    let chain = Arc::clone(&state.chain);
    // `Chain::read` does synchronous file I/O (through benilla-mpq's blocking reads); running it
    // on the async runtime thread would stall every other in-flight request behind one disk seek.
    let read = tokio::task::spawn_blocking(move || chain.read(&name)).await;
    match read {
        Ok(Ok(bytes)) => {
            let body = if method == Method::HEAD {
                Vec::new()
            } else {
                bytes
            };
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, content_type),
                    // `private`: the bytes are the operator's game files — cacheable by the player's
                    // browser (they never change under one name), never by a shared cache.
                    (
                        header::CACHE_CONTROL,
                        "private, max-age=31536000, immutable",
                    ),
                ],
                body,
            )
                .into_response()
        }
        Ok(Err(e)) if is_missing(&e) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "chain read failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(join_err) => {
            tracing::error!(error = %join_err, "chain read task panicked");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `GET /data/__index` — every name [`Chain::list`] can enumerate, for the wasm `Chain::list`
/// (Lane A) to mirror the native directory walk it can't do in a browser.
async fn index(State(state): State<DataState>) -> Response {
    let chain = Arc::clone(&state.chain);
    let listed = state
        .index
        .get(move || {
            let entries = chain.list()?;
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            Ok(serde_json::to_vec(&names)?)
        })
        .await;
    match listed {
        Ok(bytes) => (
            [
                (header::CACHE_CONTROL, "private, max-age=86400"),
                (header::CONTENT_TYPE, "application/json"),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "chain index initialization failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{compressible, content_type};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn index_cache_coalesces_requests_and_is_scoped_to_mount() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let cache = Arc::new(super::IndexCache::default());
        let builds = Arc::new(AtomicUsize::new(0));
        let mut requests = Vec::new();
        for _ in 0..32 {
            let cache = cache.clone();
            let builds = builds.clone();
            requests.push(tokio::spawn(async move {
                cache
                    .get(move || {
                        builds.fetch_add(1, Ordering::SeqCst);
                        Ok(br#"["Interface\\FrameXML\\UI.lua"]"#.to_vec())
                    })
                    .await
                    .unwrap()
            }));
        }
        let mut first: Option<super::Bytes> = None;
        for request in requests {
            let bytes = request.await.unwrap();
            if let Some(ref prior) = first {
                assert_eq!(&bytes, prior);
                assert_eq!(bytes.as_ptr(), prior.as_ptr());
            } else {
                first = Some(bytes);
            }
        }
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        let new_mount = super::IndexCache::default();
        let bytes = new_mount.get(|| Ok(b"[]".to_vec())).await.unwrap();
        assert_eq!(&bytes[..], b"[]");
    }

    #[tokio::test]
    async fn index_cache_retains_initialization_when_first_request_is_canceled() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let cache = Arc::new(super::IndexCache::default());
        let builds = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_cache = cache.clone();
        let first_builds = builds.clone();
        let first = tokio::spawn(async move {
            first_cache
                .get(move || {
                    first_builds.fetch_add(1, Ordering::SeqCst);
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(br#"["first"]"#.to_vec())
                })
                .await
        });
        // Cancel only after the blocking work has definitely started and cannot be canceled.
        started_rx.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        let later_builds = builds.clone();
        let later_cache = cache.clone();
        let later = tokio::spawn(async move {
            later_cache
                .get(move || {
                    later_builds.fetch_add(1, Ordering::SeqCst);
                    Ok(br#"["duplicate"]"#.to_vec())
                })
                .await
                .unwrap()
        });
        release_tx.send(()).unwrap();
        let bytes = later.await.unwrap();
        assert_eq!(&bytes[..], br#"["first"]"#);
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        let warm = cache
            .get(|| panic!("warm cache must not rebuild"))
            .await
            .unwrap();
        assert_eq!(bytes.as_ptr(), warm.as_ptr());
    }

    /// The module route's two claims, neither of which the compiler checks.
    ///
    /// **What this covers and what it does not.** There is no `Chain` here — `Chain::open` needs a
    /// real vanilla install and refuses an empty directory — so this is not an end-to-end route
    /// test: it does not prove the handler reads a file or that `main.rs` passes the right
    /// directory. It proves the two things that were actually in doubt: that a specific literal
    /// route wins over the `/data/{*name}` wildcard (the whole reason the first cut 404'd), and
    /// that nothing but a real module id reaches the filesystem.
    #[tokio::test]
    async fn the_module_route_outranks_the_wildcard_and_only_accepts_real_ids() {
        // Registered in the same order and shape as `router_with_modules` does it.
        let app = axum::Router::new()
            .route("/data/{*name}", axum::routing::get(|| async { "wildcard" }))
            .route(
                "/data/warden_modules/{id}",
                axum::routing::get(|| async { "module" }),
            );
        let body = app
            .oneshot(
                Request::builder()
                    .uri("/data/warden_modules/BA877D8E62E30E3373505709FCE6DDCB.cr")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("router response")
            .into_body();
        let bytes = axum::body::to_bytes(body, 64).await.expect("body");
        assert_eq!(
            &bytes[..],
            b"module",
            "the wildcard must not swallow the module route — that is what 404'd the first time"
        );

        // A real id, as `module_id_hex` spells one, and the same id lowercased.
        assert_eq!(
            super::module_filename("BA877D8E62E30E3373505709FCE6DDCB.cr").as_deref(),
            Some("BA877D8E62E30E3373505709FCE6DDCB.cr")
        );
        assert_eq!(
            super::module_filename("ba877d8e62e30e3373505709fce6ddcb.cr").as_deref(),
            Some("BA877D8E62E30E3373505709FCE6DDCB.cr"),
            "case is normalised, because the files on disk are uppercase"
        );

        // Nothing else may reach the filesystem.
        for bad in [
            "../../../../etc/passwd",
            "../BA877D8E62E30E3373505709FCE6DDCB.cr",
            "BA877D8E62E30E3373505709FCE6DDCB",     // no extension
            "BA877D8E62E30E3373505709FCE6DDC.cr",   // 31 digits
            "BA877D8E62E30E3373505709FCE6DDCBA.cr", // 33
            "BA877D8E62E30E3373505709FCE6DDCG.cr",  // not hex
            "BA877D8E62E30E33/505709FCE6DDCB.cr",
            ".cr",
            "",
        ] {
            assert!(
                super::module_filename(bad).is_none(),
                "{bad:?} must not become a path"
            );
        }
    }

    #[tokio::test]
    async fn index_cache_retries_failed_initialization() {
        let cache = super::IndexCache::default();
        assert!(cache
            .get(|| anyhow::bail!("temporary read failure"))
            .await
            .is_err());
        assert_eq!(&cache.get(|| Ok(b"[]".to_vec())).await.unwrap()[..], b"[]");
    }

    /// Run one GET through the real predicate behind a handler that answers with `content_type`
    /// and `len` bytes, and report what `Content-Encoding` came back. `aaaa...` is maximally
    /// compressible on purpose: the question under test is whether the layer *tried*, not how
    /// well brotli did.
    async fn encoding_for(content_type: &'static str, len: usize) -> Option<String> {
        let app = axum::Router::new()
            .route(
                "/x",
                axum::routing::get(move || async move {
                    (
                        [(super::header::CONTENT_TYPE, content_type)],
                        vec![b'a'; len],
                    )
                }),
            )
            .layer(super::CompressionLayer::new().compress_when(compressible()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/x")
                    .header("accept-encoding", "br")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("router response");
        response
            .headers()
            .get(super::header::CONTENT_ENCODING)
            .map(|v| v.to_str().expect("ascii encoding").to_owned())
    }

    /// The pairing that makes the whole scheme work: a DBC/ADT/Lua body compresses, and the two
    /// families [`content_type`] diverts do not. Asserted through the real predicate, so a change
    /// to either half has to keep this true.
    #[tokio::test]
    async fn the_predicate_compresses_only_what_is_worth_compressing() {
        assert_eq!(
            encoding_for("application/octet-stream", 4096)
                .await
                .as_deref(),
            Some("br")
        );
        assert_eq!(encoding_for("image/x-blp", 4096).await, None);
        assert_eq!(encoding_for("audio/mpeg", 4096).await, None);
    }

    /// `DefaultPredicate`'s size floor still applies. Worth pinning: it is why a `HEAD`, whose
    /// body this route deliberately empties, never comes back claiming an encoding.
    #[tokio::test]
    async fn tiny_bodies_are_left_alone() {
        assert_eq!(encoding_for("application/octet-stream", 0).await, None);
    }

    /// The two families that must *not* be compressed, and the fall-through that must be. These
    /// are assertions about the compression predicate as much as about the strings: `image/x-blp`
    /// is only correct because `DefaultPredicate` skips `image/*`, and `audio/mpeg` is only
    /// correct because the layer in [`super::router`] names it.
    #[test]
    fn already_compressed_families_get_a_skipped_content_type() {
        assert_eq!(
            content_type("Interface\\Glues\\Common\\Glue-Panel-Button-Up.blp"),
            "image/x-blp"
        );
        assert_eq!(
            content_type("Sound\\Music\\CityMusic\\Stormwind\\1.mp3"),
            "audio/mpeg"
        );
    }

    /// Chain names arrive in whatever case the archive stored them in — the client's own reads
    /// mix `Interface\...` and `interface\...` in one session (see `web/world-manifest.json`),
    /// so a case-sensitive match would silently compress half the textures in the world.
    #[test]
    fn the_extension_match_is_case_insensitive() {
        assert_eq!(content_type("World\\Textures\\FOO.BLP"), "image/x-blp");
        assert_eq!(content_type("Sound\\ambience\\Forest.Mp3"), "audio/mpeg");
    }

    /// Everything else compresses, including the two edge shapes a `rsplit('.')` classifier can
    /// trip on: a name with no extension at all, and one whose only dot is in a directory.
    #[test]
    fn everything_else_is_compressible() {
        assert_eq!(
            content_type("DBFilesClient\\AreaTable.dbc"),
            "application/octet-stream"
        );
        assert_eq!(
            content_type("Interface\\FrameXML\\ContainerFrame.lua"),
            "application/octet-stream"
        );
        assert_eq!(
            content_type("World\\Maps\\Azeroth\\Azeroth_32_48.adt"),
            "application/octet-stream"
        );
        assert_eq!(content_type("Readme"), "application/octet-stream");
        assert_eq!(content_type("some.dir\\file"), "application/octet-stream");
    }
}
