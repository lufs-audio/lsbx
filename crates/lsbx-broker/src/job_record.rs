//! Runner-lifecycle marker parsing (Unit 18).
//!
//! Kept separate from [`super::reconcile::Reconciler`]'s control flow, per
//! this unit's own interface contract, so the parsing logic is independently
//! testable without a `LsbxOps`/`CiJobStore`/`GitHubClient` in scope at all —
//! every function here is a pure `&str -> Option<...>` transform.
//!
//! # The four literal patterns, taken directly from this unit's acceptance
//! criteria (not reinvented, not loosened)
//!
//! - `Runner registered: (\S+)` — captures the runner name once GitHub
//!   Actions Runner registration completes inside the dispatched VM.
//! - `Listening for Jobs` — no capture group; a plain marker that the runner
//!   entered its job-polling loop.
//! - `Running job: ` — no capture group in the acceptance criterion's own
//!   text; a plain marker meaning "a job started executing." (The runner's
//!   *name* for the completed job is what `Job (.+) completed with result:
//!   (\S+)` captures below, not this line.)
//! - `Job (.+) completed with result: (\S+)` — captures `(job_name, result)`,
//!   exactly the tuple this unit's own interface contract's
//!   `parse_job_completed` signature promises.
//!
//! All four regexes are compiled once (`OnceLock`) and reused across every
//! call — this file's own `tail_and_update` caller (`reconcile.rs`) may run
//! these against every line of a growing log on every poll, so recompiling a
//! `Regex` per line would be real, avoidable per-call overhead.

use std::sync::OnceLock;

/// Lifecycle-marker regexes over one line of the tailed
/// `/tmp/lsbx-ci-broker-runner.log`. Every pattern here is copied verbatim
/// from this unit's acceptance criteria — see the module doc comment.
pub struct LifecycleMarkers;

fn runner_registered_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"Runner registered: (\S+)").unwrap()
    })
}

fn listening_for_jobs_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"Listening for Jobs").unwrap()
    })
}

fn running_job_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"Running job: ").unwrap()
    })
}

fn job_completed_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"Job (.+) completed with result: (\S+)").unwrap()
    })
}

fn running_job_name_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"Running job: (.+)").unwrap()
    })
}

impl LifecycleMarkers {
    /// Matches `Runner registered: (\S+)` against `line`, returning the
    /// captured runner name if present.
    pub fn parse_runner_registered(line: &str) -> Option<String> {
        runner_registered_re()
            .captures(line)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }

    /// True if `line` contains the plain `Listening for Jobs` marker (no
    /// capture group — this line carries no data beyond "the runner reached
    /// its polling loop").
    pub fn is_listening_for_jobs(line: &str) -> bool {
        listening_for_jobs_re().is_match(line)
    }

    /// True if `line` contains the plain `Running job: ` marker (no capture
    /// group in the acceptance criterion's own text — see the module doc
    /// comment for why the job name instead comes from
    /// [`Self::parse_job_completed`]).
    pub fn is_running_job(line: &str) -> bool {
        running_job_re().is_match(line)
    }

    /// Captures the job name from `Running job: ...` when the runner emits it.
    pub fn parse_running_job(line: &str) -> Option<String> {
        running_job_name_re()
            .captures(line)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().trim().to_string())
    }

    /// True when the runner reports a terminal listener exit.
    pub fn is_exited(line: &str) -> bool {
        line.contains("Exiting runner...")
            || line.contains("Runner listener exit with")
            || line.contains("Runner listener exit")
    }

    /// Matches `Job (.+) completed with result: (\S+)` against `line`,
    /// returning `(job_name, result)` if present — exactly the tuple shape
    /// this unit's interface contract names.
    pub fn parse_job_completed(line: &str) -> Option<(String, String)> {
        let caps = job_completed_re().captures(line)?;
        let job_name = caps.get(1)?.as_str().to_string();
        let result = caps.get(2)?.as_str().to_string();
        Some((job_name, result))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_runner_registered_captures_the_runner_name() {
        let line = "2026-08-24T00:00:00Z Runner registered: lsbx-ci-runner-abc123";
        assert_eq!(
            LifecycleMarkers::parse_runner_registered(line),
            Some("lsbx-ci-runner-abc123".to_string())
        );
    }

    #[test]
    fn parse_runner_registered_returns_none_when_absent() {
        assert_eq!(
            LifecycleMarkers::parse_runner_registered("nothing here"),
            None
        );
    }

    #[test]
    fn is_listening_for_jobs_matches_the_plain_marker() {
        assert!(LifecycleMarkers::is_listening_for_jobs(
            "2026-08-24T00:00:01Z Listening for Jobs"
        ));
        assert!(!LifecycleMarkers::is_listening_for_jobs("unrelated line"));
    }

    #[test]
    fn is_running_job_matches_the_plain_marker() {
        assert!(LifecycleMarkers::is_running_job(
            "2026-08-24T00:00:02Z Running job: build-and-test"
        ));
        assert!(!LifecycleMarkers::is_running_job("unrelated line"));
    }

    #[test]
    fn parse_job_completed_captures_job_name_and_result() {
        let line = "2026-08-24T00:00:03Z Job build-and-test completed with result: Succeeded";
        assert_eq!(
            LifecycleMarkers::parse_job_completed(line),
            Some(("build-and-test".to_string(), "Succeeded".to_string()))
        );
    }

    #[test]
    fn parse_job_completed_handles_a_job_name_containing_spaces() {
        // `(.+)` is greedy but the trailing ` completed with result: (\S+)`
        // anchor still resolves correctly for a job name with internal
        // spaces, since `(\S+)` after it cannot itself contain the literal
        // " completed with result: " substring.
        let line = "Job Build And Test (linux) completed with result: Failed";
        assert_eq!(
            LifecycleMarkers::parse_job_completed(line),
            Some(("Build And Test (linux)".to_string(), "Failed".to_string()))
        );
    }

    #[test]
    fn parse_job_completed_returns_none_when_absent() {
        assert_eq!(LifecycleMarkers::parse_job_completed("nothing here"), None);
    }
}
