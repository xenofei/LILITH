//! Who changes a status (CORE-156; P4b-R41): the status log's `by` for a tool caller. The
//! change itself, with its log entry, is `lil_schema::change_status`; callers outside the
//! pipeline's reach (`projects/staleness.rs`) pass `StatusBy::Core` themselves.

use lil_protocol::CallerKind;
use lil_schema::StatusBy;

/// The status-log `by` of a change made by `caller`, as `signed_off.by` (CORE-051): `user` for
/// an app (`ui:*`), `assistant`, `claude-code` (`mcp`) or `cli`.
pub fn status_by(caller: &CallerKind) -> StatusBy {
    match caller {
        CallerKind::Ui(_) => StatusBy::User,
        CallerKind::Assistant => StatusBy::Assistant,
        CallerKind::Mcp => StatusBy::ClaudeCode,
        CallerKind::Cli => StatusBy::Cli,
    }
}
