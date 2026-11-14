# Vendored noVNC bundle

**Upstream:** https://github.com/novnc/noVNC — tag **v1.5.0**
**Tarball:** https://github.com/novnc/noVNC/archive/refs/tags/v1.5.0.tar.gz
**Tarball sha256:** `6a73e41f98388a5348b7902f54b02d177cb73b7e5eb0a7a0dcf688cc2c79b42a`
**Vendored:** 2026-11-14 (lsbx issue #55)
**License:** MPL-2.0 (see `LICENSE.txt`) — the only MPL component in this repo;
it is served as-is, unmodified, and is not linked into the Rust binaries.

## What is vendored, and what is not

Included (the runtime subset the console page loads):

```
app/  core/  vendor/  vnc.html  vnc_lite.html  package.json  LICENSE.txt  AUTHORS
```

Excluded on purpose: upstream `tests/`, `docs/`, `po/`, `snap/`, `.github/`,
and lint/test config. They are never served and would triple the tree size.

## Why vendored instead of fetched at runtime

- The deployment artifact is one binary (`/etc/systemd/system/lsbx-serve.service`
  runs `.../lsbx serve`); there is no asset directory shipped with it.
- The noVNC client and this gateway's `/stream/{sandbox_id}/{guest_path}`
  WebSocket route are one protocol contract. Vendoring pins them to the same
  release by construction; a Caddy-served directory or a CDN copy could drift.

## How to update

1. Download the new tag's tarball and record its sha256.
2. Replace the directory contents with the same subset listed above.
3. Update this file (tag, URL, checksum, date).
4. `cargo test -p lsbx-stream` — `src/assets.rs` asserts that the files the
   console page loads (`core/rfb.js`, `core/websock.js`, `app/ui.js`, …) exist,
   so a botched re-vendor fails loudly instead of serving a broken console.
5. Bump `NOVNC_BUNDLE_VERSION` in `src/console.rs` (it appears in the served
   page's `generator` meta tag, so a running deployment can be identified).
