//! Loads `images.json` and `images.carnyx.json` and asserts the schema
//! parses correctly and the real `agent-base` base-name mismatch
//! (SPEC.md Deviation 2) is preserved, not silently normalized.
//!
//! ## Fixture provenance (Gap 2, final integration wiring pass)
//!
//! This test now loads the REAL, byte-exact `images.json`/`images.carnyx.json`
//! at the shared, top-level `tests/fixtures/` location (landed by Unit 20),
//! rather than the local, hand-reconstructed stand-in files that previously
//! lived under `crates/lsbx-golden/tests/fixtures/` (that directory's own
//! `README.md` explicitly flagged those as "reconstructed from confirmed
//! schema facts, not a byte-exact copy," pending exactly this repointing).
//!
//! ### What reconciling against the real byte-exact files actually found
//!
//! The stand-in fixtures' core claim — the `agent-base` golden's `base`
//! field differs between the two manifest files
//! (`"lsbx-default-v1"` in `images.json` vs. `"lsbx-agent-v1"` in
//! `images.carnyx.json`) — **is confirmed true of the real, byte-exact
//! files** (verified directly against the JSON content at
//! `../../tests/fixtures/images.json` and `images.carnyx.json` before
//! writing this test). SPEC.md Deviation 2 is not an invented detail; the
//! real files genuinely diverge exactly the way the stand-ins claimed.
//!
//! Everything else about the stand-in files' *shape*, however, was a
//! fabrication that does not match the real files, and this test has been
//! rewritten to assert what is REALLY true rather than force the old,
//! narrower shape to keep passing:
//!
//! - The stand-ins had exactly one `images[]` entry and one `goldens[]`
//!   entry each. The real files have **two** images (`win11`,
//!   `ubuntu-2404`) and **three** goldens (`agent-base`, `ci-runner`,
//!   `agent-web`) in both files.
//! - The stand-ins asserted `registry.images[0].key == "lsbx-default-v1"`
//!   — i.e. that the golden's `base` value was itself an `images[]` entry.
//!   **This is false of the real files**: neither `images.json` nor
//!   `images.carnyx.json` contains any `images[]` entry whose `key` is
//!   `"lsbx-default-v1"` or `"lsbx-agent-v1"` at all — the real image keys
//!   are `"win11"` and `"ubuntu-2404"` in both files. A golden's `base`
//!   field referencing a name absent from the same file's `images[]` array
//!   is not a schema violation (`ImageRegistry::load`/`validate_base`
//!   perform no cross-reference check between a golden's `base` and the
//!   `images[]` list — confirmed by direct re-read of
//!   `crates/lsbx-golden/src/registry.rs` immediately before writing this
//!   test), and is a real, honest characteristic of the byte-exact fixture
//!   files themselves: `base` names a previously-built golden or
//!   content-hash-named image produced by `golden build`, not necessarily
//!   a raw ISO entry in `images[]`. This test does not force a false
//!   assertion to keep the old (fabricated) shape passing — it asserts
//!   what the real files actually contain.
//! - The stand-ins' `agent-base.description` differed per file
//!   ("images.json variant" / "images.carnyx.json variant" — an invented
//!   detail with no real counterpart). The real files' `agent-base`
//!   descriptions differ too, but for a real, substantive reason: one says
//!   "Lean agent golden with an explicit DuckDuckGo HTML web-search CLI"
//!   and the carnyx variant appends "(carnyx)" — this test does not assert
//!   on description text at all, since it carries no schema-fidelity
//!   signal either fabricated or real.
//!
//! This test is written to FAIL if someone "fixes" the `agent-base`
//! mismatch — that is the point (per Unit 08's own Verification section).

// This is a test-only integration binary (tests/*.rs): every fn here is a
// #[test], so a failed unwrap()/expect() only ever panics inside `cargo test`,
// never in a shipped code path. clippy::unwrap_used / expect_used are
// restriction-group lints that don't understand "this whole file is test
// code" the way #[cfg(test)] does, so they fire here even though this unit's
// own acceptance criteria (and every other unit's test files) rely on
// idiomatic unwrap()-based assertions. Allow both, scoped to this file only —
// crates/lsbx-golden/src/**/*.rs (the real production code path) is
// unwrap/expect/panic-free under the same workspace lints with no allow
// needed. Pattern established in Unit 01's crates/lsbx-kernel/tests/test_kernel.rs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_golden::ImageRegistry;
use std::path::PathBuf;

/// Points at the shared, top-level `tests/fixtures/` directory (Unit 20's
/// real, byte-exact files) rather than a local copy under this crate's own
/// `tests/` — `crates/lsbx-golden` -> `crates` -> repo root is two `../`
/// hops from `CARGO_MANIFEST_DIR`.
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures")).join(name)
}

#[test]
fn images_json_parses_with_full_schema_fidelity() {
    let registry = ImageRegistry::load(&fixture_path("images.json")).expect("images.json should load");

    // Real shape: two images, three goldens, five profiles — see this
    // file's module doc comment for why this differs from the old
    // stand-in fixtures' fabricated one-of-each shape.
    assert_eq!(registry.images.len(), 2);
    let image_keys: Vec<&str> = registry.images.iter().map(|i| i.key.as_str()).collect();
    assert!(image_keys.contains(&"win11"));
    assert!(image_keys.contains(&"ubuntu-2404"));

    assert_eq!(registry.goldens.len(), 3);
    let golden = registry
        .find_golden("agent-base")
        .expect("images.json must contain an agent-base golden");
    assert_eq!(golden.key, "agent-base");
    assert_eq!(golden.base, "lsbx-default-v1");

    assert!(registry.profiles.contains_key("default"));
    assert!(registry.profiles.contains_key("ci"));
}

#[test]
fn images_carnyx_json_parses_with_full_schema_fidelity() {
    let registry =
        ImageRegistry::load(&fixture_path("images.carnyx.json")).expect("images.carnyx.json should load");

    assert_eq!(registry.images.len(), 2);
    let image_keys: Vec<&str> = registry.images.iter().map(|i| i.key.as_str()).collect();
    assert!(image_keys.contains(&"win11"));
    assert!(image_keys.contains(&"ubuntu-2404"));

    assert_eq!(registry.goldens.len(), 3);
    let golden = registry
        .find_golden("agent-base")
        .expect("images.carnyx.json must contain an agent-base golden");
    assert_eq!(golden.key, "agent-base");
    assert_eq!(golden.base, "lsbx-agent-v1");
}

/// The whole point of this test: `agent-base.base` MUST differ between the
/// two manifest files. If this test starts failing because both files
/// agree, that means someone "fixed"/harmonized the mismatch — which is
/// exactly the silent compatibility break SPEC.md Deviation 2 says this
/// rewrite must not introduce. Harmonizing the two files is a legitimate
/// follow-up, but only as an explicit, separately-flagged change — never
/// as a side effect of "cleaning up" this test or its fixtures.
///
/// Confirmed against the REAL, byte-exact fixture files (Gap 2) — not the
/// old hand-reconstructed stand-ins — and the mismatch holds exactly as
/// claimed. See this file's module doc comment for the full reconciliation
/// writeup, including what did NOT hold (the stand-ins' fabricated
/// one-image/one-golden shape).
#[test]
fn test_registry_schema_preserves_mismatch() {
    let images = ImageRegistry::load(&fixture_path("images.json")).expect("images.json should load");
    let carnyx = ImageRegistry::load(&fixture_path("images.carnyx.json")).expect("images.carnyx.json should load");

    let images_base = &images
        .find_golden("agent-base")
        .expect("images.json must contain an agent-base golden")
        .base;
    let carnyx_base = &carnyx
        .find_golden("agent-base")
        .expect("images.carnyx.json must contain an agent-base golden")
        .base;

    assert_eq!(images_base, "lsbx-default-v1");
    assert_eq!(carnyx_base, "lsbx-agent-v1");
    assert_ne!(
        images_base, carnyx_base,
        "agent-base's base golden must differ between images.json and images.carnyx.json \
         (SPEC.md Deviation 2) — if this assertion fails because the two now agree, someone \
         silently harmonized a real, load-bearing inconsistency instead of preserving it"
    );
}

#[test]
fn allowed_goldens_reflects_loaded_manifest() {
    let registry = ImageRegistry::load(&fixture_path("images.json")).expect("images.json should load");
    let allowed = registry.allowed_goldens();
    // Real shape: three distinct base values across the three real goldens
    // (agent-base -> lsbx-default-v1, ci-runner -> lsbx-ci-v1, agent-web ->
    // lsbx-web-v1) — not the stand-in's single-entry set.
    assert_eq!(allowed.len(), 3);
    assert!(allowed.contains("lsbx-default-v1"));
    assert!(allowed.contains("lsbx-ci-v1"));
    assert!(allowed.contains("lsbx-web-v1"));
}

/// A real, honest characteristic of the byte-exact fixture files, found
/// while reconciling this test against them (see this file's module doc
/// comment): a golden's `base` field is not required to — and, for
/// `agent-base` specifically, does not — resolve to an entry in the same
/// file's `images[]` array. This is not a schema violation; it reflects
/// that `base` may name a previously-built golden/content-hash image
/// rather than a raw ISO entry. Asserted explicitly here (rather than
/// silently relied upon) so a future schema change that starts requiring
/// that cross-reference shows up as an obvious, intentional diff against
/// this test, not a silent behavior change.
#[test]
fn agent_base_base_field_does_not_resolve_to_an_images_entry() {
    let registry = ImageRegistry::load(&fixture_path("images.json")).expect("images.json should load");
    let golden = registry
        .find_golden("agent-base")
        .expect("images.json must contain an agent-base golden");
    let image_keys: Vec<&str> = registry.images.iter().map(|i| i.key.as_str()).collect();
    assert!(
        !image_keys.contains(&golden.base.as_str()),
        "expected agent-base.base ('{}') to NOT match any images[] key ({:?}) in the real \
         fixture file — if this now passes because the schema changed, that's a real change \
         worth flagging explicitly, not one this assertion should silently absorb",
        golden.base,
        image_keys
    );
}
