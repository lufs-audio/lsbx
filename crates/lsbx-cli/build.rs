//! Bakes the git commit into the binary as `LSBX_GIT_COMMIT`.
//!
//! Why: the broker ran for six days on a binary built before `main` moved, and
//! nothing surfaced the drift — `lsbx --version` printed a bare semver and the
//! process logged nothing about itself. Exposing the commit makes "which build
//! is actually running" answerable from `--version` and from the startup banner.
//!
//! Deliberately tolerant: a tarball export, a vendored tree, or a container
//! with no `.git` must still build. Missing metadata becomes `unknown`, never a
//! build failure.

use std::process::Command;

fn main() {
    // Re-run when HEAD moves (branch switch or new commit). The ref file itself
    // is covered too, since HEAD usually points at `.git/refs/heads/<branch>`.
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=LSBX_GIT_COMMIT={commit}");
}
