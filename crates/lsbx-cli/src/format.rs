//! One formatting path for every subcommand's output (Unit 11).
//!
//! Every subcommand's `LsbxOps` result is routed through [`render_result`]
//! (or, for a bare success value with no natural "did it fail" question —
//! there is none of those on this CLI's surface today, but the seam exists
//! via [`render`] — through `render` directly), producing either a human
//! table (via [`Formattable::to_human_table`]) or the real
//! `lsbx_kernel::envelope::Envelope` JSON shape, selected by `--json`. This
//! is the acceptance criterion this module exists to satisfy: "never two
//! independently-maintained rendering paths per command."

use lsbx_kernel::envelope::Envelope;
use lsbx_kernel::error::LsbxError;
use serde::Serialize;

/// Anything this CLI can print as a human-readable table. Every
/// `LsbxOps` response type this crate touches implements this once, here,
/// rather than each subcommand's handler inventing its own ad hoc
/// `println!` formatting.
pub trait Formattable {
    fn to_human_table(&self) -> String;
}

/// Renders `value` as either a human table or `{"status":"success",...}`
/// JSON, matching the unit contract's exact signature.
pub fn render<T: Serialize + Formattable>(value: &T, as_json: bool) -> String {
    if as_json {
        let envelope = Envelope::Success { data: value };
        // `Envelope`'s own `Serialize` impl is the one, real JSON shape
        // (Unit 01) — this function never hand-builds JSON itself.
        serde_json::to_string(&envelope).unwrap_or_else(|e| {
            // Only reachable if `T`'s `Serialize` impl itself errors (e.g. a
            // non-string map key) — every type this crate actually renders
            // is plain data with no such failure mode, but this still must
            // not panic a formatting path, so fall back to a minimal,
            // still-valid error envelope naming the formatting failure
            // itself rather than crashing the process here.
            format!(
                r#"{{"status":"error","code":5,"message":"failed to serialize response: {e}"}}"#
            )
        })
    } else {
        value.to_human_table()
    }
}

/// Renders a full `Result<T, LsbxError>` through the same one formatting
/// path, on both the success and failure branch — this is what every
/// subcommand handler in `lib.rs` actually calls, so a caller never needs
/// to unpack the `Result` itself before choosing how to render it.
pub fn render_result<T: Serialize + Formattable>(
    result: &Result<T, LsbxError>,
    as_json: bool,
) -> String {
    match result {
        Ok(value) => render(value, as_json),
        Err(e) => render_error(e, as_json),
    }
}

/// Renders a bare `LsbxError` (no success value to consider) through the
/// same envelope shape on the JSON path, or a plain `Error: <message>`
/// line on the human path — used by subcommands whose failure path has no
/// natural `T` to pair it with (e.g. `down`'s per-id failure reporting).
pub fn render_error(error: &LsbxError, as_json: bool) -> String {
    if as_json {
        let envelope: Envelope<()> = Envelope::Error {
            code: error.exit_code() as i32,
            message: error.to_string(),
        };
        serde_json::to_string(&envelope).unwrap_or_else(|e| {
            format!(
                r#"{{"status":"error","code":5,"message":"failed to serialize error response: {e}"}}"#
            )
        })
    } else {
        format!("Error: {error}")
    }
}

/// A minimal key/value table renderer shared by every `Formattable` impl
/// in this crate, so column alignment is consistent across every
/// subcommand's human-readable output rather than each impl hand-rolling
/// its own spacing.
pub fn kv_table(rows: &[(&str, String)]) -> String {
    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    rows.iter()
        .map(|(k, v)| format!("{:<width$}  {}", k, v, width = width))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A minimal row-oriented table renderer (header + rows, space-padded
/// columns) shared by every list-shaped `Formattable` impl.
pub fn row_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return "(none)".to_string();
    }

    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.len());
            }
        }
    }

    let mut out = String::new();
    for (i, h) in headers.iter().enumerate() {
        if i > 0 {
            out.push_str("  ");
        }
        out.push_str(&format!("{:<width$}", h, width = widths[i]));
    }
    out.push('\n');

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            let width = widths.get(i).copied().unwrap_or(cell.len());
            out.push_str(&format!("{:<width$}", cell, width = width));
        }
        out.push('\n');
    }

    out.trim_end().to_string()
}
