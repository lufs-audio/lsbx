use lsbx_broker::labels::is_eligible;
use lsbx_broker::poll::{PollConfig, QueuedJob, FALLBACK_QUEUE_LABEL};
use std::time::{Duration, SystemTime};

fn cfg() -> PollConfig {
    PollConfig {
        poll_interval: Duration::from_secs(15),
        repo_refresh_interval: Duration::from_secs(300),
        fallback_delay: Duration::from_secs(60),
        queue_labels: vec![
            FALLBACK_QUEUE_LABEL.to_string(),
            "dedicated-label".to_string(),
        ],
    }
}

#[test]
fn test_label_eligibility() {
    let cfg = cfg();
    let now = SystemTime::now();

    let job_dedicated = QueuedJob {
        job_id: 1,
        run_id: 1,
        repository: "foo/bar".to_string(),
        labels: vec!["dedicated-label".to_string()],
        // Even if created_at is right now (age 0), it should be immediately eligible.
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    // A real, unconfigured label — not in `cfg.queue_labels` at all, and not
    // the fallback label either. This is the case that catches the bug the
    // other candidate for this unit had: deciding "dedicated" by checking
    // only `label != FALLBACK_QUEUE_LABEL`, which would wrongly treat this
    // job as an eligible dedicated match. It must never be eligible.
    let job_unrelated = QueuedJob {
        job_id: 2,
        run_id: 1,
        repository: "foo/bar".to_string(),
        labels: vec!["other-label".to_string()],
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    assert!(
        is_eligible(&job_dedicated, &cfg, now),
        "Dedicated label should be claimed immediately"
    );
    assert!(
        !is_eligible(&job_unrelated, &cfg, now),
        "Unrelated, unconfigured label should never be eligible"
    );
}

#[test]
fn test_job_with_both_dedicated_and_fallback_labels_is_eligible_immediately() {
    let cfg = cfg();
    let now = SystemTime::now();

    // A job carrying both a configured dedicated label and the fallback
    // label is eligible immediately via the dedicated match, regardless of
    // age.
    let job_both = QueuedJob {
        job_id: 3,
        run_id: 1,
        repository: "foo/bar".to_string(),
        labels: vec!["dedicated-label".to_string(), FALLBACK_QUEUE_LABEL.to_string()],
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    assert!(is_eligible(&job_both, &cfg, now));
}

#[test]
fn test_unconfigured_label_that_is_not_the_fallback_string_is_never_eligible() {
    let cfg = cfg();
    let now = SystemTime::now();

    // A job whose only label looks like it could be a "dedicated" placement
    // label (it is not the literal fallback string), but was never added to
    // `cfg.queue_labels`. This must not be conflated with a real dedicated
    // match — this broker was never configured to claim it.
    let job = QueuedJob {
        job_id: 4,
        run_id: 1,
        repository: "foo/bar".to_string(),
        labels: vec!["windows-runner".to_string()],
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    assert!(!is_eligible(&job, &cfg, now));
}

#[test]
fn test_job_with_no_labels_is_never_eligible() {
    let cfg = cfg();
    let now = SystemTime::now();

    let job = QueuedJob {
        job_id: 5,
        run_id: 1,
        repository: "foo/bar".to_string(),
        labels: vec![],
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    assert!(!is_eligible(&job, &cfg, now));
}
