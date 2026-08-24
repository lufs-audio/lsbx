//! Loads `images.json` and `images.carnyx.json` and asserts the schema
//! parses correctly and the real `agent-base` base-name mismatch
//! (SPEC.md Deviation 2) is preserved, not silently normalized.
//!
//! ## Fixture provenance
//! The fixture files loaded here
//! (`crates/lsbx-golden/tests/fixtures/images.json` and
//! `images.carnyx.json`) are **reconstructed from confirmed schema facts,
//! not a byte-exact copy of the original Python `lufs-sandbox-server`
//! repo's files** — this environment has no access to verify against that
//! repo directly. See `crates/lsbx-golden/tests/fixtures/README.md` for the
//! full provenance note. The two confirmed facts these fixtures preserve
//! (and this test asserts): the schema shape from Unit 08's own interface
//! contract, and the real `agent-base` mismatch named explicitly in
//! `SPEC.md`'s Deviation table (Deviation 2) and repeated in Unit 08's
//! contract text (`lsbx-default-v1` in `images.json` vs. `lsbx-agent-v1` in
//! `images.carnyx.json`).
//!
//! This test is written to FAIL if someone "fixes" the mismatch — that is
//! the point (per Unit 08's own Verification section).
//!
//! The unit contract's original scenario description says these fixtures
//! are "inlined until Unit 20's fixtures land." Unit 20 has not landed in
//! this session, so this crate creates its own minimal, clearly-labeled
//! fixture files under `crates/lsbx-golden/tests/fixtures/` rather than
//! inlining JSON literals in this file, so a future Unit 20 has one obvious
//! place to drop the real byte-exact files without needing to also edit
//! this test's source.

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

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn images_json_parses_with_full_schema_fidelity() {
    let registry = ImageRegistry::load(&fixture_path("images.json")).expect("images.json should load");

    assert_eq!(registry.images.len(), 1);
    assert_eq!(registry.images[0].key, "lsbx-default-v1");

    assert_eq!(registry.goldens.len(), 1);
    let golden = &registry.goldens[0];
    assert_eq!(golden.key, "agent-base");
    assert_eq!(golden.base, "lsbx-default-v1");

    assert!(registry.profiles.contains_key("agent-default"));
}

#[test]
fn images_carnyx_json_parses_with_full_schema_fidelity() {
    let registry =
        ImageRegistry::load(&fixture_path("images.carnyx.json")).expect("images.carnyx.json should load");

    assert_eq!(registry.images.len(), 1);
    assert_eq!(registry.images[0].key, "lsbx-agent-v1");

    assert_eq!(registry.goldens.len(), 1);
    let golden = &registry.goldens[0];
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
    assert!(allowed.contains("lsbx-default-v1"));
}
