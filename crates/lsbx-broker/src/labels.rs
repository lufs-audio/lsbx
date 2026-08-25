//! Placement-label eligibility: deciding whether a queued job should be
//! claimed right now, given the configured `queue_labels` and the fallback
//! delay.
//!
//! # Reconciling the two label-eligibility approaches
//!
//! Two independent implementations of `is_eligible` existed for this unit.
//! One decided "dedicated vs. fallback" by checking whether a label was
//! merely *not* [`FALLBACK_QUEUE_LABEL`] — never checking whether that label
//! was actually in `cfg.queue_labels` at all. That means a job carrying some
//! totally unrelated, unconfigured label (e.g. `"windows-runner"`, nothing
//! to do with this broker) would be wrongly treated as an eligible dedicated
//! match, since it's simply *not* the fallback label string. Under that
//! logic `cfg.queue_labels` becomes a dead parameter except for reading
//! `fallback_delay` off the same struct — the acceptance criterion's
//! "dedicated placement labels (anything other than the fallback label)"
//! phrasing means "anything else *this broker is configured to claim*", not
//! literally any string that happens not to equal `"lsbx-default"`.
//!
//! The implementation below checks membership in `cfg.queue_labels` first,
//! for every label on the job, and only then branches on whether the
//! matched label is the fallback label or a dedicated one. A job with an
//! unrelated, unconfigured label and no configured label at all is never
//! eligible — see `test_label_eligibility.rs`'s `job_unrelated` case.

use crate::poll::{PollConfig, QueuedJob, FALLBACK_QUEUE_LABEL};
use chrono::DateTime;
use std::time::SystemTime;

/// Returns None if `created_at` is missing or unparseable — callers must treat
/// None as "not eligible," never as "eligible by default."
pub fn queued_age_seconds(job: &QueuedJob, now: SystemTime) -> Option<u64> {
    let created_at_str = job.created_at.as_ref()?;
    let created_at = DateTime::parse_from_rfc3339(created_at_str).ok()?;
    let created_at_sys: SystemTime = created_at.into();

    let duration = now.duration_since(created_at_sys).ok()?;
    Some(duration.as_secs())
}

/// Decides whether `job` is eligible to be claimed right now.
///
/// For each of the job's labels, only counts it if it is actually present in
/// `cfg.queue_labels` — an unconfigured label is never eligible, dedicated or
/// otherwise. Among the job's *configured* labels: a dedicated label (any
/// configured label other than [`FALLBACK_QUEUE_LABEL`]) is eligible
/// immediately; the fallback label alone requires
/// `queued_age_seconds(job, now) >= cfg.fallback_delay`, and a malformed or
/// missing `created_at` (`queued_age_seconds` returning `None`) fails
/// closed — never eligible, regardless of how old the job might actually be.
pub fn is_eligible(job: &QueuedJob, cfg: &PollConfig, now: SystemTime) -> bool {
    let mut matches_dedicated = false;
    let mut matches_fallback = false;

    for label in &job.labels {
        if cfg.queue_labels.iter().any(|configured| configured == label) {
            if label == FALLBACK_QUEUE_LABEL {
                matches_fallback = true;
            } else {
                matches_dedicated = true;
            }
        }
    }

    if matches_dedicated {
        return true;
    }

    if matches_fallback {
        if let Some(age) = queued_age_seconds(job, now) {
            return age >= cfg.fallback_delay.as_secs();
        }
    }

    false
}
