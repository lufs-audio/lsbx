# Unit Spec — reap must retire records whose VM is already gone

**Timestamp:** `2026-11-07T1440Z`
**Slug:** `reap-orphaned-records`
**Status:** Proposed. Small, self-contained bug fix. Found in live state on carnyx.
**Evidence:** `lsbx reap` on carnyx, 2026-11-07, with 15 expired records.

---

## 0. Symptom

`lsbx reap --dry-run` correctly reported 15 expired sandbox ids as
`would_destroy`. `lsbx reap` then printed 15 lines of

```
libvirt: QEMU Driver error : Domain not found: no domain with matching name 'sbx-18cf19f8b8609477-bd613b18'
```

reported `{"destroyed": [], "keys_reconciled": 0}`, and **left all 15 records in
the store**, so `lsbx list` still showed 15 sandboxes that do not exist. The
sweep can never converge on this host: every subsequent run repeats exactly the
same failure.

## 1. Root cause (traced, not guessed)

1. `LibvirtBackend::destroy` starts with `lookup_domain(vm_tag)?`
   (`crates/lsbx-backend-libvirt/src/lib.rs:607-608`).
2. `lookup_domain` maps a missing domain to `LsbxError::NotFound`
   (`map_virt_err`, `lib.rs:116-121`; test `map_virt_err_no_domain_becomes_not_found`).
3. `lsbx_lifecycle::create::destroy` propagates it with `?`
   (`crates/lsbx-lifecycle/src/create.rs:392`), so `store.delete(id)` at the end
   of that function never runs.
4. `lsbx_lifecycle::reap::reap` catches it as `Err(_) => continue`
   (`crates/lsbx-lifecycle/src/reap.rs:152-158`) and keeps the record.

Step 4's `continue` is **correct for transient failures** — the comment says one
sandbox's destroy failure must not abort the sweep, and the test
`reap_retains_record_on_partial_destroy_failure_and_succeeds_on_retry` guards
it. The defect is the missing distinction between "destroy failed, retry later"
and "there is nothing left to destroy".

## 2. Decision

In `reap`, a destroy that fails with `LsbxError::NotFound` is **terminal
success**: the VM is already absent, so delete the record (and let the existing
key reconciliation run). Everything else keeps today's retain-and-retry
behavior.

```rust
match crate::create::destroy(backend, store, &record.id).await {
    Ok(()) => destroyed.push(record.id.clone()),
    // The VM is already gone (host rebuilt, domain removed out-of-band, or an
    // earlier destroy partially succeeded). Nothing left to destroy, so retire
    // the record instead of retaining it forever.
    Err(LsbxError::NotFound(_)) => {
        if store.delete(&record.id).is_ok() {
            destroyed.push(record.id.clone());
        }
    }
    Err(_) => continue,
}
```

Why here and not in `create::destroy`: making `destroy` itself idempotent would
also change `lsbx down <unknown-id>`, which currently exits 4 (`NotFound`) — a
useful signal for a human typo. `reap` is the caller that wants idempotent
semantics.

## 3. Tests

- `reap_retires_record_when_backend_reports_domain_missing`: expired record,
  backend whose `destroy` returns `NotFound` ⇒ report lists the id in
  `destroyed`, `store.list()` is empty, key reconciliation still runs.
- Regression (existing): `reap_retains_record_on_partial_destroy_failure_and_succeeds_on_retry`
  must still pass with a non-`NotFound` error.
- `reap_still_sweeps_expired_sandbox_whose_golden_is_not_in_allowed_set` unchanged.
- `lsbx down` on an unknown id still exits 4 — asserted in the CLI test, so the
  D-decision boundary is guarded.

## 4. Operational note

The 15 records on carnyx were removed manually on 2026-11-07 because the fix
did not exist yet: `~/lsbx-state/state/sbx-*.json` deleted after a backup to
`~/lsbx-state-backups/state-before-reap-20261107T083137Z.tgz`. `lsbx list` →
`(none)`, `lsbx status` → `sandbox_count 0`. This spec does not depend on that
backup existing; it only records where it is.

## 5. Open question

- **O1** Should `reap` also retire records whose `golden` no longer exists in the
  registry **and** whose VM is absent, in one pass? It does the retire today via
  the same `NotFound` path, so the trace message is the only difference.
  *Leaning:* no change — the existing `tracing_unknown_golden` diagnostic is
  enough, and the golden set shrinks rarely.
- **O2** `destroy_with_key` (exedev) may also surface a not-found from its own
  transport. Confirm the same mapping holds there before relying on this fix on
  molimo. *Leaning:* check when the broker next runs; exedev records are reaped
  by lease, and molimo's store was empty at last recon.
