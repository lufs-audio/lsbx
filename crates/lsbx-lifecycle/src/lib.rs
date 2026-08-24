//! `lsbx-lifecycle` — VM Lifecycle Orchestration & Reaper (Unit 09).
//!
//! Owns `create`/`destroy`/`renew` (`src/create.rs`), the TTL-based reap
//! loop (`src/reap.rs`), and the pure lease-expiry predicate
//! (`src/lease.rs`). Composes Unit 02 (`lsbx-store`), Unit 03
//! (`lsbx-keys`), and the generic `Backend`/`Clock` traits from Unit 01
//! (`lsbx-kernel`) — it implements none of them itself. See this crate's
//! own module docs for the two deliberate scope calls this unit's contract
//! requires documenting rather than silently resolving:
//!
//! - `create::CreateRequest::healthchecks` — a generic `Vec<Vec<String>>`
//!   of commands run via `Backend::run`, standing in for "the golden's
//!   declared healthchecks" until Unit 10 (`lsbx-ops`) wires in Unit 08's
//!   real `lsbx-golden::golden_verify`. This unit's dependency list does
//!   not include `lsbx-golden` (Boundaries), so it cannot resolve a
//!   golden's real healthchecks itself.
//! - `reap::reap`'s `allowed_goldens` parameter — see that function's own
//!   doc comment for the full resolution of the ambiguity between the live
//!   unit contract's prose (which describes protecting a *golden* from
//!   golden-cleanup, a capability this unit does not implement) and its
//!   interface contract (a plain `&HashSet<String>` parameter this unit
//!   actually receives and can act on only against the one destructive
//!   action it does take: destroying a *sandbox*).

pub mod create;
pub mod lease;
pub mod reap;

pub use create::{create, destroy, renew, CreateRequest};
pub use reap::{reap, ReapReport};
