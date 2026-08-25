use chrono::Duration as ChronoDuration;
use lsbx_broker::labels::is_eligible;
use lsbx_broker::poll::{PollConfig, QueuedJob, FALLBACK_QUEUE_LABEL};
use std::time::{Duration, SystemTime};

#[test]
fn test_fallback_delay() {
    let cfg = PollConfig {
        poll_interval: Duration::from_secs(15),
        repo_refresh_interval: Duration::from_secs(300),
        fallback_delay: Duration::from_secs(60),
        queue_labels: vec![FALLBACK_QUEUE_LABEL.to_string()],
    };

    let now = SystemTime::now();
    let now_chrono: chrono::DateTime<chrono::Utc> = now.into();

    let job_new = QueuedJob {
        job_id: 1,
        run_id: 1,
        repository: "foo/bar".to_string(),
        name: None,
        labels: vec![FALLBACK_QUEUE_LABEL.to_string()],
        created_at: Some((now_chrono - ChronoDuration::seconds(30)).to_rfc3339()),
    };

    let job_old = QueuedJob {
        job_id: 2,
        run_id: 1,
        repository: "foo/bar".to_string(),
        name: None,
        labels: vec![FALLBACK_QUEUE_LABEL.to_string()],
        created_at: Some((now_chrono - ChronoDuration::seconds(65)).to_rfc3339()),
    };

    assert!(
        !is_eligible(&job_new, &cfg, now),
        "New job should not be eligible for fallback yet"
    );
    assert!(
        is_eligible(&job_old, &cfg, now),
        "Old job should be eligible for fallback"
    );

    // Boundary: exactly at the fallback delay should already be eligible
    // (`>=`, not `>`).
    let job_boundary = QueuedJob {
        job_id: 3,
        run_id: 1,
        repository: "foo/bar".to_string(),
        name: None,
        labels: vec![FALLBACK_QUEUE_LABEL.to_string()],
        created_at: Some((now_chrono - ChronoDuration::seconds(60)).to_rfc3339()),
    };
    assert!(
        is_eligible(&job_boundary, &cfg, now),
        "Job exactly at the fallback delay boundary should be eligible"
    );
}
