//! Golden flattening & host bootstrap (Unit 19).
//!
//! Owns three narrow, standalone operations, per the unit contract's own
//! module layout — `verify_host`/`HostCheck`/`HostVerification` in
//! [`verify_host`], `SystemdUnitSpec`/`generate_broker_units`/
//! `BootstrapConfig`/`BootstrapReport`/`bootstrap` together in [`systemd`],
//! and `flatten` in [`flatten`]:
//!
//! 1. Verifying a target host is actually capable (libvirt socket
//!    reachable, `qemu-img` present, state directories exist with correct
//!    permissions) before `lsbx` trusts it — "proven, not exited 0" (§2.6
//!    of SPEC.md), reporting each check individually rather than
//!    collapsing them into one boolean.
//! 2. Generating and installing the systemd units for the broker services
//!    (`lsbx-ci-broker`, `lsbx-ci-broker-exe`).
//! 3. Flattening a qcow2 backing-file chain into a single self-contained
//!    image, strictly *before* Unit 08's `golden_build` computes a content
//!    hash over it — this crate never computes that hash itself (Unit 08
//!    owns `content_hash`); it only guarantees flattening happens first.
//!
//! ## Boundaries (do not touch, per the unit contract)
//! Does not implement `create_from_golden` or any domain VM lifecycle
//! (that's Unit 06) — only verifies host capability and performs the
//! flatten step as a standalone operation. Does not compute content hashes
//! (Unit 08 owns `content_hash`).

pub mod flatten;
pub mod systemd;
pub mod verify_host;

pub use flatten::flatten;
pub use systemd::{bootstrap, generate_broker_units, BootstrapConfig, BootstrapReport, SystemdUnitSpec};
pub use verify_host::{verify_host, HostCheck, HostVerification};
