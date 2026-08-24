// See test_keygen.rs for the rationale on this file-scoped allow — this is a
// tests/*.rs integration binary, not shipped production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_kernel::error::LsbxError;
use lsbx_keys::reconcile::{parse_label_tag, reconcile_orphaned_keys, TaggedKey};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn tagged_key(comment: &str, revoked_counter: Arc<AtomicUsize>) -> TaggedKey {
    TaggedKey {
        comment: comment.to_string(),
        revoke: Box::new(move || {
            revoked_counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
    }
}

#[test]
fn parse_label_tag_extracts_label() {
    assert_eq!(
        parse_label_tag("lsbx:my-label"),
        Some("my-label".to_string())
    );
    assert_eq!(parse_label_tag("lsbx:"), Some(String::new()));
}

#[test]
fn parse_label_tag_ignores_non_lsbx_comments() {
    assert_eq!(parse_label_tag("someone@somewhere"), None);
    assert_eq!(parse_label_tag(""), None);
    assert_eq!(parse_label_tag("not-lsbx:label"), None);
}

#[test]
fn revokes_orphaned_lsbx_tagged_keys_only() {
    let revoked = Arc::new(AtomicUsize::new(0));
    let known_labels = vec!["keep-me".to_string()];

    let keys = vec![
        tagged_key("lsbx:keep-me", revoked.clone()),
        tagged_key("lsbx:orphan-1", revoked.clone()),
        tagged_key("lsbx:orphan-2", revoked.clone()),
        // Not lsbx-tagged at all — must never be touched.
        tagged_key("someone@laptop", revoked.clone()),
    ];

    let count = reconcile_orphaned_keys(keys, &known_labels).unwrap();

    assert_eq!(count, 2);
    assert_eq!(revoked.load(Ordering::SeqCst), 2);
}

#[test]
fn no_orphans_when_all_labels_known() {
    let revoked = Arc::new(AtomicUsize::new(0));
    let known_labels = vec!["a".to_string(), "b".to_string()];

    let keys = vec![
        tagged_key("lsbx:a", revoked.clone()),
        tagged_key("lsbx:b", revoked.clone()),
    ];

    let count = reconcile_orphaned_keys(keys, &known_labels).unwrap();

    assert_eq!(count, 0);
    assert_eq!(revoked.load(Ordering::SeqCst), 0);
}

#[test]
fn empty_input_revokes_nothing() {
    let count = reconcile_orphaned_keys(Vec::new(), &[]).unwrap();
    assert_eq!(count, 0);
}

/// A revoke failure partway through must propagate rather than being
/// swallowed — the caller (a reaper loop) needs to know reconciliation was
/// incomplete, not that it succeeded.
#[test]
fn propagates_revoke_failure() {
    let known_labels: Vec<String> = Vec::new();
    let keys = vec![TaggedKey {
        comment: "lsbx:will-fail".to_string(),
        revoke: Box::new(|| Err(LsbxError::BackendUnavailable("revoke failed".to_string()))),
    }];

    let result = reconcile_orphaned_keys(keys, &known_labels);
    assert!(result.is_err());
}
