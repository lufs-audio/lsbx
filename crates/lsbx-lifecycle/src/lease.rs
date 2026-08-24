//! Lease-expiry predicate (Unit 09).
//!
//! Deliberately the smallest possible surface: one pure function, no I/O, no
//! backend, no store. `reap` (`src/reap.rs`) is the only caller inside this
//! crate, but the function is `pub` so a future caller (Unit 10's
//! `lsbx-ops::status`, for instance) can ask "is this record expired right
//! now" without going through a full reap sweep.

use lsbx_kernel::clock::Clock;
use lsbx_kernel::types::SandboxRecord;

/// True if `record.lease_expires_at` parses as RFC3339 and is strictly in
/// the past relative to `clock.now()`.
///
/// Two states are deliberately treated as "not expired" rather than
/// "expired" or "error":
///
/// - `lease_expires_at: None` — a record with no lease deadline set has
///   nothing for this predicate to enforce. Failing closed here (never
///   treating "no lease" as "already expired") matters because `reap` uses
///   this function to decide what to destroy; a bug that dropped
///   `lease_expires_at` on save should never turn into every sandbox in the
///   store being swept on the next reap pass.
/// - An unparseable `lease_expires_at` string — same fail-closed reasoning.
///   A malformed timestamp is a data problem worth surfacing elsewhere (a
///   future `lsbx-ops::status`/`lint` pass is the right place), not a
///   silent trigger for `reap` to destroy a VM it can't actually prove is
///   past its lease.
///
/// Both are intentionally silent (no error returned) because this is a
/// boolean predicate, not a validation pass — `reap` needs a yes/no answer
/// for every record in the store on every sweep, and a record it can't
/// parse a deadline for is exactly the kind of record `reap` must leave
/// alone rather than guess about.
pub fn is_expired(record: &SandboxRecord, clock: &dyn Clock) -> bool {
    let Some(expires_at) = record.lease_expires_at.as_deref() else {
        return false;
    };

    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
        return false;
    };

    let now: chrono::DateTime<chrono::Utc> = clock.now().into();
    parsed.with_timezone(&chrono::Utc) < now
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsbx_kernel::clock::FakeClock;
    use std::time::{Duration, SystemTime};

    fn base_record(lease_expires_at: Option<String>) -> SandboxRecord {
        SandboxRecord {
            id: "sbx-lease-test".to_string(),
            name: "lease-test".to_string(),
            host: "localhost".to_string(),
            profile: "default".to_string(),
            flavor: "default".to_string(),
            streaming: "none".to_string(),
            username: None,
            key_name: None,
            key_path: None,
            key_dir: None,
            pubkey: None,
            task_id: None,
            created_at: None,
            lease_expires_at,
            vm_tag: None,
            https_url: None,
            cleanup_failed: false,
            repository_key: None,
            repository: None,
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn none_lease_is_never_expired() {
        let record = base_record(None);
        let clock = FakeClock {
            now: SystemTime::now(),
        };
        assert!(!is_expired(&record, &clock));
    }

    #[test]
    fn unparseable_lease_is_not_expired() {
        let record = base_record(Some("not-a-timestamp".to_string()));
        let clock = FakeClock {
            now: SystemTime::now(),
        };
        assert!(!is_expired(&record, &clock));
    }

    #[test]
    fn past_lease_is_expired() {
        let now = SystemTime::now();
        let past = now - Duration::from_secs(3600);
        let record = base_record(Some(
            chrono::DateTime::<chrono::Utc>::from(past).to_rfc3339(),
        ));
        let clock = FakeClock { now };
        assert!(is_expired(&record, &clock));
    }

    #[test]
    fn future_lease_is_not_expired() {
        let now = SystemTime::now();
        let future = now + Duration::from_secs(3600);
        let record = base_record(Some(
            chrono::DateTime::<chrono::Utc>::from(future).to_rfc3339(),
        ));
        let clock = FakeClock { now };
        assert!(!is_expired(&record, &clock));
    }

    #[test]
    fn exactly_at_boundary_is_not_expired() {
        // Strictly-less-than semantics: a lease expiring at exactly `now`
        // has not yet passed `now`, so it is not swept this instant. It
        // will be swept on the very next tick once `now` advances past it.
        let now = SystemTime::now();
        let record = base_record(Some(
            chrono::DateTime::<chrono::Utc>::from(now).to_rfc3339(),
        ));
        let clock = FakeClock { now };
        assert!(!is_expired(&record, &clock));
    }
}
