//! `GET /addons/__index` and `GET /addons/{*path}` — the player's `Interface\AddOns\` folder,
//! served the way `/data` serves the patch chain.
//!
//! **Why it needs a route at all.** The desktop client finds addons with `std::fs::read_dir` over
//! `<install>/Interface/AddOns`. A browser tab has no filesystem, so `discover_folder` finds
//! nothing there and only the twelve `Blizzard_*` addons inside the archive ever load — every
//! addon the player actually installed is structurally invisible on the web, whatever the Options
//! window says. This is the other half of the same seam `data` already is: the host reads the
//! disk, the page reads the host.
//!
//! The index is what replaces `read_dir`. Discovery needs two things — the folder names, and each
//! folder's `<Name>.toc` matched case-insensitively — and both fall out of one flat list of
//! relative paths, so there is no second listing route. Measured on a real install: 61 folders,
//! 3078 files, ~126 KB of paths. One fetch, parsed once.
//!
//! **One path per line, not JSON.** `/data/__index` is JSON because `benilla-formats` already
//! carries a parser; `benilla-app` does not, and a list of names is not worth a dependency on one
//! at both ends. A name containing a newline would break the format, so the walk drops it — no
//! addon has ever had one, and silently omitting it beats corrupting the whole list.
//!
//! This host is a local development tool and this route inherits that: it hands the operator's
//! addon folder to anyone who can reach the socket, exactly as `/data` hands over the game files.
//! `wenilla-realm` does not mount it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tokio::sync::OnceCell;

#[derive(Clone)]
struct AddonsState {
    root: Arc<PathBuf>,
    index: Arc<OnceCell<Bytes>>,
}

/// Build the `/addons/*` router over one `Interface/AddOns` directory.
///
/// A root that does not exist is not an error: the index answers empty and every file answers 404,
/// which is exactly what an install with no addons should look like.
pub fn router(root: PathBuf) -> Router {
    Router::new()
        .route("/addons/__index", get(index))
        .route("/addons/{*path}", get(file))
        .with_state(AddonsState {
            root: Arc::new(root),
            index: Arc::default(),
        })
}

/// Reject anything that is not a plain relative path inside the addons folder.
///
/// **This is the whole path-safety story**, and it is a function so it can be tested without a
/// server. A name that passes has no `..`, no empty or dot segment, no backslash (which is a
/// separator on the host filesystem and would smuggle one past a `/`-only check), and no `:`
/// (a Windows drive or alternate stream). What is left cannot leave `root` when joined to it.
fn safe_relative(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut segments = 0usize;
    for seg in path.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return None;
        }
        if seg.contains('\\') || seg.contains(':') || seg.contains('\0') {
            return None;
        }
        out.push(seg);
        segments += 1;
    }
    (segments > 0).then_some(out)
}

/// `GET /addons/{*path}` — one file out of the addon folder.
async fn file(AxumPath(path): AxumPath<String>, State(state): State<AddonsState>) -> Response {
    let decoded = percent_encoding::percent_decode_str(&path).decode_utf8_lossy();
    let Some(rel) = safe_relative(&decoded) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let full = state.root.join(rel);
    match tokio::task::spawn_blocking(move || std::fs::read(full)).await {
        Ok(Ok(bytes)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                // The operator's own files, cacheable by their browser and never by a shared one —
                // the same terms `/data` serves the game files on.
                (header::CACHE_CONTROL, "private, max-age=3600"),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(_)) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "addon read task panicked");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `GET /addons/__index` — every file under the addons root, as `/`-separated relative paths.
///
/// Built once per mounted root. Ordering is the walk's, not sorted: the client sorts folder names
/// itself under the reference's own directory-order law, and sorting here would only imply a
/// guarantee this route does not make.
async fn index(State(state): State<AddonsState>) -> Response {
    let root = Arc::clone(&state.root);
    let built = state
        .index
        .get_or_try_init(|| async move {
            let bytes = tokio::task::spawn_blocking(move || {
                let mut names = Vec::new();
                walk(&root, &root, &mut names);
                Bytes::from(names.join("
"))
            })
            .await
            .map_err(|e| anyhow::anyhow!("addon index walk: {e}"))?;
            Ok::<Bytes, anyhow::Error>(bytes)
        })
        .await;
    match built {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            bytes.clone(),
        )
            .into_response(),
        Err(e) => {
            // Not cached: `get_or_try_init` leaves the cell empty on failure, so a transient
            // problem is retried rather than frozen into an empty listing for the session.
            tracing::error!(error = %e, "addon index failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Depth-first walk, collecting `/`-separated paths relative to `base`. Symlinks are followed by
/// `read_dir`'s own semantics and a cycle would not terminate — the same exposure `/data`'s
/// `Chain::list` has over the install, on a host that is already handing that install out whole.
fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // an unreadable folder is simply not listed
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(base, &path, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            // See the module note: the format is one path per line.
            let mut s = String::new();
            for (i, part) in rel.components().enumerate() {
                if i > 0 {
                    s.push('/');
                }
                s.push_str(&part.as_os_str().to_string_lossy());
            }
            if !s.contains('\n') {
                out.push(s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::safe_relative;

    /// The only thing between a request and the filesystem.
    #[test]
    fn only_a_plain_relative_path_reaches_the_disk() {
        assert!(safe_relative("AtlasLoot/AtlasLoot.toc").is_some());
        assert!(safe_relative("Bagnon/Core/Bagnon.lua").is_some());
        for bad in [
            "",
            "..",
            "../WTF/Account/config.wtf",
            "AtlasLoot/../../Data/patch.MPQ",
            "AtlasLoot//AtlasLoot.toc",
            "AtlasLoot/./AtlasLoot.toc",
            "C:/Windows/System32/drivers/etc/hosts",
            "AtlasLoot\\..\\..\\secret",
            "AtlasLoot/AtlasLoot.toc:Zone.Identifier",
        ] {
            assert!(safe_relative(bad).is_none(), "{bad:?} must not become a path");
        }
    }
}
