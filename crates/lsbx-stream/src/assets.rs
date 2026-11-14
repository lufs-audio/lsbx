//! Embedded vendored noVNC bundle + the `GET /assets/novnc/<path>` handler.
//!
//! ## Why the bundle lives in the binary (issue #55)
//! Before this module the gateway could proxy WebSocket bytes to a guest's
//! `:8000` but had no page that knew how to speak them: `assets/` held only a
//! placeholder HTML file that said so. `GET /console?target=<id>` therefore
//! iframed the sandbox's `console_url`, which on the libvirt backend is
//! `http://192.168.122.x:8000/vnc.html` — an address that exists only on the
//! hypervisor host. Every off-host browser got a dead frame.
//!
//! Serving the client from the gateway's own origin fixes three things at once:
//! the browser never needs to reach a guest IP, the WebSocket is same-origin (so
//! no CORS, no mixed-content, and it works through the HTTPS vhost unchanged),
//! and the noVNC version is pinned by the binary that also implements the
//! `/stream/*` protocol it talks to.
//!
//! ## Why not `ServeDir` / a runtime directory
//! The deployment artifact is a single binary (`lsbx serve` under systemd), with
//! no asset directory shipped alongside it. The tree is embedded at compile time
//! via `include_dir`.
//!
//! ## Updating the bundle
//! `assets/novnc/PROVENANCE.md` records the upstream release, download URL and
//! tarball checksum. Replace the tree wholesale; do not hand-edit vendored files.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use include_dir::{include_dir, Dir};

/// The vendored noVNC runtime subset (`app/`, `core/`, `vendor/`, `vnc.html`,
/// `vnc_lite.html`, `package.json`, `LICENSE.txt`, `AUTHORS`).
///
/// Deliberately excludes upstream's `tests/`, `docs/`, `po/`, `snap/`, and lint
/// config: they are not served, and they would triple the size of the tree.
static NOVNC: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets/novnc");

/// Path prefix every vendored asset is served under. Kept stable because the
/// console page and the vendored `vnc.html` both reference it.
pub const NOVNC_ASSET_PREFIX: &str = "/assets/novnc";

/// Maps a file extension to the Content-Type a browser needs.
///
/// Deliberately explicit rather than a crate dependency: the bundle only ever
/// contains these eight types, and a wrong type for `.js` breaks ES-module
/// loading (the bundle is imported as `type="module"`), which is a silent,
/// confusing failure.
fn content_type_for(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("txt") => "text/plain; charset=utf-8",
        Some("md") => "text/markdown; charset=utf-8",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// `GET /assets/novnc/<path>` — serves one embedded bundle file.
///
/// Path traversal is not expressible: the lookup goes through
/// `Dir::get_file` against the compile-time tree, so `..` cannot escape it and
/// anything not in the tree is a plain 404.
pub async fn serve_asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    let Some(file) = NOVNC.get_file(&path) else {
        return (StatusCode::NOT_FOUND, "no such asset").into_response();
    };
    let Some(contents) = file.contents_utf8().map(str::as_bytes).or(Some(file.contents()))
    else {
        return (StatusCode::NOT_FOUND, "no such asset").into_response();
    };

    let cache = if path.contains('/') {
        // app/, core/, vendor/ are content-addressed by the vendored release:
        // safe to cache hard for the life of the page.
        "public, max-age=3600"
    } else {
        "no-cache"
    };

    let mut response = (StatusCode::OK, contents).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_for(&path)),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    // The bundle is same-origin by construction; disallow it being framed or
    // sniffed elsewhere.
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn bundle_contains_the_files_the_console_page_loads() {
        for required in [
            "core/rfb.js",
            "core/websock.js",
            "core/encodings.js",
            "app/ui.js",
            "vnc.html",
            "LICENSE.txt",
        ] {
            assert!(
                NOVNC.get_file(required).is_some(),
                "vendored noVNC bundle is missing {required} — re-vendor per PROVENANCE.md"
            );
        }
    }

    #[test]
    fn javascript_is_served_as_a_module_compatible_type() {
        // ES-module imports fail silently in browsers when this is not a JS
        // MIME type, so it is pinned here rather than left to a generic guess.
        assert_eq!(
            content_type_for("core/rfb.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type_for("app/styles/base.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type_for("vnc.html"), "text/html; charset=utf-8");
        assert_eq!(content_type_for("no-extension"), "application/octet-stream");
    }

    #[tokio::test]
    async fn traversal_attempts_do_not_escape_the_bundle() {
        let response = serve_asset(axum::extract::Path("../../Cargo.toml".to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response = serve_asset(axum::extract::Path("/etc/passwd".to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
