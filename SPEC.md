# SPEC — `lsbx`

The authoritative specification for this repository lives in its own timestamped
spec folder, per the `speccing` standard (see `danialrami/dotfiles` for the skill
itself — it is intentionally not vendored into this repo):

**[docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/SPEC.md](docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/SPEC.md)**

Unit-of-work contracts for that spec are under
[`docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/units/`](docs/specs/2026-08-24T0030Z_rust-lsbx-rewrite/units/).

If this project is respecced later (a v2 feature, a breaking redesign), a new
`docs/specs/<ISO8601-timestamp>_<slug>/` folder is added alongside this one —
old spec folders are never deleted or overwritten; they're the project's own
git-history-adjacent record of how it got here. This file always points at
whichever spec folder is currently authoritative.
