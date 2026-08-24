//! TTL-based reap loop (Unit 09).
//!
//! Sweeps lease-expired sandboxes, destroys them, and separately
//! reconciles orphaned ephemeral keys. See [`reap`]'s doc comment for the
//! detailed resolution of the `allowed_goldens` ambiguity flagged in this
//! unit's own contract and in this crate's PR description.

use lsbx_kernel::backend::Backend;
use lsbx_kernel::clock::Clock;
use lsbx_kernel::error::LsbxError;
use lsbx_store::sandbox_store::SandboxStore;
use std::collections::HashSet;
use std::time::Duration;

/// Report of one `reap` sweep.
pub struct ReapReport {
    /// Sandbox ids actually destroyed this sweep (via `Backend::destroy`
    /// succeeding, followed by keypair cleanup and store deletion).
    pub destroyed: Vec<String>,
    /// Sandbox ids that *would* have been destroyed had `dry_run` been
    /// `false`. Only ever populated when `dry_run` is `true`; empty
    /// otherwise (never both populated in the same call).
    pub would_destroy: Vec<String>,
    /// Count of orphaned ephemeral keys revoked this sweep, via
    /// `lsbx_keys::reconcile::reconcile_orphaned_keys`.
    pub keys_reconciled: usize,
}

/// Sweeps every sandbox in `store` whose lease has expired
/// (`lease::is_expired`, driven by `clock` — never a real wall-clock
/// sleep), destroys each one, and separately reconciles orphaned ephemeral
/// keys against the set of currently-known labels (every non-expired
/// sandbox's `key_name`, plus every expired sandbox this sweep just
/// destroyed keeps its own key cleaned up via `destroy`'s own
/// `cleanup_keypair` call — so it is correctly absent from "known" by the
/// time reconciliation runs).
///
/// ## Ordering and partial-failure handling
/// Each expired sandbox is destroyed via this crate's own [`crate::create::destroy`]
/// (the same `Backend::destroy` → `cleanup_keypair` → `SandboxStore::delete`
/// ordering `destroy` documents). If `Backend::destroy` fails for one
/// sandbox (the named `PartialDestroyFailure` scenario), that sandbox's id
/// is simply omitted from `destroyed` and its record is left exactly as it
/// was in the store — `destroy`'s own ordering guarantee (record survives
/// any partial failure) is what makes "retried on the next reap pass,
/// never silently forgotten" true without `reap` needing any special-case
/// logic of its own. A failure for one sandbox never aborts the sweep for
/// the rest: every expired sandbox gets an independent destroy attempt.
///
/// `dry_run: true` reports what *would* be destroyed (every lease-expired
/// sandbox id) in `would_destroy`, without calling `Backend::destroy` at
/// all, and does not touch key reconciliation either — a dry run previews,
/// it does not mutate any state, backend-side or store-side.
///
/// ## Resolving the `allowed_goldens` ambiguity
/// The live unit contract's prose says: "The reaper consults
/// `lsbx-golden::allowed_goldens()` before considering any golden for
/// cleanup, so a golden a live sandbox still depends on is never removed
/// out from under it." Taken completely literally, that sentence describes
/// protecting a *golden* from *golden* cleanup — but this unit's own
/// Boundaries state plainly that it "does not parse the golden registry...
/// only consumes `allowed_goldens()`'s output as an opaque set," and there
/// is no golden-file-deletion code anywhere in this crate's scope (nor is
/// there a `lsbx-golden` dependency to call into). This unit's `reap`
/// cannot protect a golden from being deleted, because this unit never
/// deletes goldens — that capability doesn't exist here at all, today.
///
/// Reconciling those two statements the way this implementation does:
/// `allowed_goldens` is treated as an opaque, forward-looking safety gate
/// on the one destructive action this function *does* take — destroying a
/// *sandbox* — rather than as a golden-deletion guard this function has no
/// mechanism to enforce. Concretely: a lease-expired sandbox whose
/// `profile` field is present in `allowed_goldens` is swept normally (its
/// golden is confirmed still allowed/known-good, so nothing about reaping
/// this expired sandbox is exceptional). A lease-expired sandbox whose
/// `profile` is **absent** from `allowed_goldens` is still swept — refusing
/// to reap an expired lease just because its golden reference looks stale
/// would leave an expired VM running indefinitely, which is a worse
/// outcome than the ambiguity this parameter is trying to resolve — but
/// this is exactly the case that will matter once Unit 08's real golden
/// registry exists: at that point, this same gate is where "was this
/// golden already deleted out from under a sandbox that still referenced
/// it" becomes checkable. `allowed_goldens` is accepted and threaded
/// through today specifically so that future wiring — Unit 08 producing a
/// real `ImageRegistry::allowed_goldens()` set, and a caller (Unit 10's
/// `lsbx-ops::reap`) passing it in — is a call-site change, not a
/// signature change to this function.
///
/// This resolution is flagged explicitly in this crate's PR description as
/// the one acceptance criterion whose exact purpose is subtle enough to
/// warrant a second read of the live contract and an explicit call on how
/// it's implemented, per this unit's own instructions.
pub async fn reap(
    backend: &dyn Backend,
    store: &SandboxStore,
    clock: &dyn Clock,
    allowed_goldens: &HashSet<String>,
    ttl: Duration,
    dry_run: bool,
) -> Result<ReapReport, LsbxError> {
    let all_records = store.list()?;

    // `ttl` names the interface contract's parameter, but the actual sweep
    // predicate is `lease::is_expired`, which compares each record's own
    // persisted `lease_expires_at` against `clock.now()` — not "now minus
    // ttl". `ttl` exists so a caller without a per-sandbox lease (or a
    // caller that wants a *shorter* effective window than what was leased)
    // can still bound the sweep; a record already past its own
    // `lease_expires_at` is swept regardless of `ttl`, and `ttl` further
    // restricts the sweep to records that are *also* older than `ttl` past
    // their own expiry, so a fresh-just-expired lease and a
    // long-forgotten one aren't reaped in the same instant a lease flips
    // from valid to expired if the caller wants to give a short grace
    // window. With `ttl` of zero (the common case — see `crate::reap`'s
    // tests), this reduces to exactly "swept iff already expired."
    let mut expired: Vec<_> = all_records
        .into_iter()
        .filter(|record| crate::lease::is_expired(record, clock))
        .filter(|record| is_past_grace_window(record, clock, ttl))
        .collect();

    // Deterministic ordering for reproducible reports/tests — `store.list()`
    // iterates a directory listing, whose order is not guaranteed by any
    // filesystem.
    expired.sort_by(|a, b| a.id.cmp(&b.id));

    if dry_run {
        let would_destroy: Vec<String> = expired.into_iter().map(|r| r.id).collect();
        return Ok(ReapReport {
            destroyed: Vec::new(),
            would_destroy,
            keys_reconciled: 0,
        });
    }

    let mut destroyed = Vec::new();
    for record in &expired {
        let golden_known = allowed_goldens.contains(&record.profile);
        // See this function's own doc comment ("Resolving the
        // `allowed_goldens` ambiguity") for why an unknown golden does not
        // block the sweep — it is recorded for diagnosability via tracing
        // rather than silently ignored, but never used to skip destroying
        // an already lease-expired sandbox.
        if !golden_known {
            tracing_unknown_golden(&record.id, &record.profile);
        }

        match crate::create::destroy(backend, store, &record.id).await {
            Ok(()) => destroyed.push(record.id.clone()),
            // Matches this crate's own `destroy` ordering guarantee: on
            // failure the record is untouched, so it remains in the store
            // to be retried on the next sweep. Never surfaced as a hard
            // error from `reap` itself — one sandbox's destroy failure
            // must not abort the sweep for every other expired sandbox.
            Err(_) => continue,
        }
    }

    let known_labels: Vec<String> = store
        .list()?
        .into_iter()
        .filter_map(|r| r.key_name)
        .collect();

    let keys_reconciled = reconcile_keys(backend, &known_labels).await?;

    Ok(ReapReport {
        destroyed,
        would_destroy: Vec::new(),
        keys_reconciled,
    })
}

/// `ttl` grace-window check: a record is only swept once it is both past
/// its own `lease_expires_at` (already checked by `lease::is_expired`) and
/// `ttl` has additionally elapsed since that expiry. `ttl == Duration::ZERO`
/// (the common/default case) makes this always `true` for anything already
/// expired, so behavior reduces to "sweep iff expired" — the simplest,
/// most literal reading of the interface contract's `ttl` parameter.
fn is_past_grace_window(
    record: &lsbx_kernel::types::SandboxRecord,
    clock: &dyn Clock,
    ttl: Duration,
) -> bool {
    if ttl.is_zero() {
        return true;
    }

    let Some(expires_at) = record.lease_expires_at.as_deref() else {
        return true;
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
        return true;
    };

    let now: chrono::DateTime<chrono::Utc> = clock.now().into();
    let grace_deadline = parsed.with_timezone(&chrono::Utc)
        + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::zero());
    grace_deadline < now
}

fn tracing_unknown_golden(sandbox_id: &str, profile: &str) {
    tracing::warn!(
        sandbox_id,
        profile,
        "reaping a lease-expired sandbox whose golden/profile is not in allowed_goldens; \
         sweeping anyway (an expired lease is never left running), but this is the point \
         at which Unit 08's real golden registry would confirm whether the golden itself \
         is still allowed"
    );
}

/// Reconciles orphaned ephemeral keys against `known_labels` via Unit 03's
/// `reconcile_orphaned_keys`. The `TaggedKey` listing itself is
/// backend-specific (Unit 03's own doc comment: each backend builds its own
/// listing from wherever *it* stores authorized keys) and neither the
/// generic `Backend` trait (Unit 01) nor this unit's contract exposes a
/// method for it — `list_vms`/`run`/etc. have no "list registered keys"
/// operation. Until a backend-specific key-listing hook exists (a
/// reasonable Unit 06/07 follow-up, or an addition to the `Backend` trait
/// itself), this reconciliation pass has no `TaggedKey`s to check and
/// therefore always revokes zero keys against the generic `&dyn Backend`
/// this unit is scoped to. The call to `reconcile_orphaned_keys` itself is
/// real (not stubbed out) so the moment a backend can supply a real
/// `Vec<TaggedKey>`, this function's signature does not need to change —
/// only its call site does.
async fn reconcile_keys(
    _backend: &dyn Backend,
    known_labels: &[String],
) -> Result<usize, LsbxError> {
    let tagged_keys: Vec<lsbx_keys::reconcile::TaggedKey> = Vec::new();
    lsbx_keys::reconcile::reconcile_orphaned_keys(tagged_keys, known_labels)
}
