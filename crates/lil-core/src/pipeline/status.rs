//! Who changes a status (CORE-156; P4b-R41): the status log's `by` for a tool caller. The
//! change itself, with its log entry, is `lil_schema::change_status`; callers outside the
//! pipeline's reach (`projects/staleness.rs`) pass `StatusBy::Core` themselves. And who did the
//! work ([`actor_of`]; CORE-023, P4b-R16): the `actor` of runs, their files, `human_edits[]`
//! entries and redirects; callers outside the pipeline's reach (`watch`, `projects`) are given
//! the actor by the tool that calls them.

use lil_protocol::CallerKind;
use lil_schema::{Actor, ActorKind, StatusBy};

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

/// The `actor` of work done by `caller` (P4b-R16): `owner` for an app (`ui:*`), `claude_code`
/// for `mcp`, `tether` for `assistant`, `cli` for `cli`; no ids.
pub fn actor_of(caller: &CallerKind) -> Actor {
    let kind = match caller {
        CallerKind::Ui(_) => ActorKind::Owner,
        CallerKind::Mcp => ActorKind::ClaudeCode,
        CallerKind::Assistant => ActorKind::Tether,
        CallerKind::Cli => ActorKind::Cli,
    };
    Actor {
        kind,
        task_id: None,
        conversation_id: None,
        session_id: None,
    }
}
