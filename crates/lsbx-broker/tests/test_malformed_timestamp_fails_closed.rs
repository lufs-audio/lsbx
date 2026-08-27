use lsbx_broker::labels::{is_eligible, queued_age_seconds};
use lsbx_broker::poll::{PollConfig, QueuedJob, FALLBACK_QUEUE_LABEL};
use std::time::{Duration, SystemTime};

fn cfg() -> PollConfig {
    PollConfig {
        poll_interval: Duration::from_secs(15),
        repo_refresh_interval: Duration::from_secs(300),
        fallback_delay: Duration::from_secs(60),
        queue_labels: vec![FALLBACK_QUEUE_LABEL.to_string()],
        repos: None,
    }
}

#[test]
fn test_malformed_timestamp_fails_closed() {
    let cfg = cfg();
    let now = SystemTime::now();

    let job = QueuedJob {
        job_id: 1,
        run_id: 1,
        repository: "foo/bar".to_string(),
        name: None,
        labels: vec![FALLBACK_QUEUE_LABEL.to_string()],
        created_at: Some("not-a-timestamp".to_string()),
    };

    assert_eq!(queued_age_seconds(&job, now), None);
    assert!(
        !is_eligible(&job, &cfg, now),
        "Malformed timestamp must fail closed and never be eligible"
    );

    // Even arbitrarily far into the future, a malformed timestamp must
    // never become eligible by accident (e.g. via an unsigned-subtraction
    // wraparound or a default-to-zero-age bug).
    let far_future = now + Duration::from_secs(3600);
    assert!(!is_eligible(&job, &cfg, far_future));
}

#[test]
fn test_missing_timestamp_fails_closed() {
    let cfg = cfg();
    let now = SystemTime::now();

    // `created_at: None` — GitHub returned a job with no timestamp at all,
    // not merely an unparseable one. Must fail closed exactly the same way
    // as the malformed-string case above, not be treated as "no
    // information, so allow it."
    let job = QueuedJob {
        job_id: 2,
        run_id: 1,
        repository: "foo/bar".to_string(),
        name: None,
        labels: vec![FALLBACK_QUEUE_LABEL.to_string()],
        created_at: None,
    };

    assert_eq!(queued_age_seconds(&job, now), None);
    assert!(
        !is_eligible(&job, &cfg, now),
        "Missing timestamp must fail closed and never be eligible"
    );

    let far_future = now + Duration::from_secs(3600);
    assert!(!is_eligible(&job, &cfg, far_future));
}
