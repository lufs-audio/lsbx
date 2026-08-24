# Fixture provenance note

`images.json` and `images.carnyx.json` in this directory are **reconstructed
from confirmed schema facts, not a byte-exact copy of the original Python
`lufs-sandbox-server` repo's files** (this environment has no access to
verify against that repo directly).

What is confirmed, and therefore load-bearing in these fixtures:

- The exact schema shape (`images[]`, `goldens[]`, `profiles{}` with the
  field sets in `docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/units/08-golden-image-registry-and-build-lifecycle.md`'s
  interface contract).
- The real, live inconsistency named in `SPEC.md`'s Deviation table
  (Deviation 2) and repeated in Unit 08's own contract text: the
  `agent-base` golden's `base` field is `"lsbx-default-v1"` in `images.json`
  but `"lsbx-agent-v1"` in `images.carnyx.json`. That mismatch is preserved
  exactly in these two files and is the fact
  `test_registry_schema_preserves_mismatch` exists to assert.

Everything else in these fixtures (other image/golden keys, cpu/memory
values, healthcheck command text, descriptions) is a minimal,
schema-accurate placeholder invented to make the files parse and exercise
the loader — not a verified real value from the original repo.

**Unit 20** (Workspace Manifest, CI Workflow & Compat Fixtures) owns
`tests/fixtures/` at the workspace root per `SPEC.md` §8 and is the unit
that should reconcile or replace these files with the real, byte-exact
`images.json` / `images.carnyx.json` if/when it has access to the original
`lufs-audio/lufs-sandbox-server` repo. Until then, treat these as
schema-accurate stand-ins, not ground truth.
