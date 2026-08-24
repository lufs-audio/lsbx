use lsbx_kernel::error::LsbxError;

/// A backend-supplied `(comment, revoke_fn)` pair describing one key the
/// backend currently has registered. Backend-agnostic on purpose — Units
/// 06/07 (libvirt, exedev) each build their own listing from wherever *they*
/// store authorized keys (a guest `authorized_keys` file vs. exe.dev's
/// key-registration API) and hand it here.
pub struct TaggedKey {
    pub comment: String,
    pub revoke: Box<dyn FnOnce() -> Result<(), LsbxError>>,
}

/// Revokes every `lsbx:*`-tagged key in `tagged_keys` whose label is not in
/// `known_labels`, and returns the count revoked.
///
/// Keys with a comment that doesn't parse as an `lsbx:<label>` tag are left
/// untouched — this only ever reconciles keys `lsbx` itself created.
pub fn reconcile_orphaned_keys(
    tagged_keys: Vec<TaggedKey>,
    known_labels: &[String],
) -> Result<usize, LsbxError> {
    let mut revoked_count = 0;

    for key in tagged_keys {
        let Some(label) = parse_label_tag(&key.comment) else {
            continue;
        };
        if known_labels.contains(&label) {
            continue;
        }
        (key.revoke)()?;
        revoked_count += 1;
    }

    Ok(revoked_count)
}

/// Parses a `lsbx:<label>` comment tag if present; ignores non-`lsbx`-tagged
/// comments (returns `None` rather than treating them as an error, since a
/// backend's key listing may legitimately contain keys `lsbx` didn't create).
pub fn parse_label_tag(comment: &str) -> Option<String> {
    comment.strip_prefix("lsbx:").map(str::to_string)
}
