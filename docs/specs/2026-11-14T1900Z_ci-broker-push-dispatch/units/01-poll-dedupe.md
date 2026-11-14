# Unit 01 — poll-dedupe

## Objective
One broker poll lists each repo's queued/in-progress runs and their jobs **once**, then matches every queue label in memory — same behaviour, a third of the API calls.

## Context
- `crates/lsbx-broker/src/poll.rs`: `Poller::tick` loops `for repo in &self.repos { for label in &self.config.queue_labels { queued_jobs(client, label, repo) } }`; `queued_jobs` calls `workflow_runs(repo, status)` for both `RUN_STATUSES` and `run_jobs(repo, run.id)` per run, then filters `job.labels` for one label.
- Measured on carnyx (3 labels, 14 repos): ~87 requests per poll; ~4,300/h at `LSBX_CI_POLL_INTERVAL=45`. Phase SPEC → Problem.
- Optional in this unit: conditional requests. GitHub returns `304 Not Modified` for a matching `If-None-Match` ETag and a 304 does **not** count against the primary rate limit. `crates/lsbx-broker/src/github_client.rs` owns the HTTP layer.

## Acceptance criteria
- [ ] New `queued_jobs_multi(client, labels: &[String], repo) -> Vec<(String /*label*/, QueuedJob)>` (or equivalent) issues exactly `2 + N_active_runs` requests per repo regardless of label count; `tick` uses it.
- [ ] A job whose labels match several queue labels is dispatched **once** (dedupe by `job_id`), with the first matching label in `queue_labels` order recorded.
- [ ] Unit test with a mock client: 3 labels × 2 repos → request count equals the 1-label count.
- [ ] (if ETags implemented) a second identical poll against the mock returns cached results on 304 and the test asserts no re-parse.
- [ ] Measured on carnyx after deploy: ≤ 35 requests per idle poll (rate_limit sampled every 5 s across 3 polls; numbers in the PR).
- [ ] All existing broker tests pass (`cargo test -p lsbx-broker`).

## Interface contract
```rust
pub async fn queued_jobs_multi<C: RunsAndJobs>(
    client: &C,
    labels: &[String],
    repo: &str,
) -> Result<Vec<(String, QueuedJob)>, LsbxError>;
```

## Boundaries
- **Touches:** `crates/lsbx-broker/src/poll.rs`, `crates/lsbx-broker/src/github_client.rs` (ETag only), their tests.
- **Does NOT touch:** `reconcile.rs`, `lsbx-store`, goldens, any service file on carnyx (Daniel restarts the broker).

## Verification
```bash
cargo test -p lsbx-broker
# on carnyx after rebuild + restart (token never printed):
ssh carnyx 'bash -s' <<'EOS'
set -a; . /home/carnyx/ISOs/images/ci-broker.env; set +a
for i in $(seq 1 30); do curl -s -H "Authorization: Bearer $GH_TOKEN" https://api.github.com/rate_limit | jq .resources.core.used; sleep 5; done
EOS
```
