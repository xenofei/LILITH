//! `lil:` URLs (APP-020, QA-050; Appendix G "`lil:` URLs"): `<scheme>:confirm/<id>`,
//! `<scheme>:deny/<id>/<token>`, `<scheme>:task/<job id>`, `<scheme>:open/<base64url target>`
//! and a raw `lil://` ref.

/// What an accepted argument asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LilUrl {
    /// `confirm/<cf_ id>`.
    Confirm(String),
    /// `deny/<cf_ id>/<32 hex token>`.
    Deny { id: String, token: String },
    /// `task/<j_ id>`.
    Task(String),
    /// `open/<base64url>`: the decoded target.
    Open(String),
    /// A raw `lil://` ref, after one percent-decode.
    Ref(String),
}

/// A refused argument: nothing happens, and the app says "This link isn't one LILITH opens:
/// <shown>".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlRefusal {
    /// [`crate::shown_text`] of the argument, at most 80 characters.
    pub shown: String,
    pub reason: &'static str,
}

/// The argument without surrounding spaces and one pair of surrounding `"`.
pub fn strip_argument(raw: &str) -> &str {
    let _ = raw;
    todo!("plan 4a Task 4")
}

/// Parses one command-line argument under the channel's scheme (`lil`, `lil-dev` or
/// `lil-test-<id>`).
///
/// # Errors
/// [`UrlRefusal`] for anything Appendix G doesn't accept.
pub fn parse_lil_url(arg: &str, channel_scheme: &str) -> Result<LilUrl, UrlRefusal> {
    let _ = (arg, channel_scheme);
    todo!("plan 4a Task 4")
}

/// `<scheme>:open/<base64url of the UTF-8 target, without padding>`.
pub fn open_url(channel_scheme: &str, target: &str) -> String {
    let _ = (channel_scheme, target);
    todo!("plan 4a Task 4")
}

/// `<scheme>:confirm/<id>`.
pub fn confirm_url(channel_scheme: &str, id: &str) -> String {
    let _ = (channel_scheme, id);
    todo!("plan 4a Task 4")
}

/// `<scheme>:deny/<id>/<token>`.
pub fn deny_url(channel_scheme: &str, id: &str, token: &str) -> String {
    let _ = (channel_scheme, id, token);
    todo!("plan 4a Task 4")
}

/// `<scheme>:task/<job id>`.
pub fn task_url(channel_scheme: &str, job_id: &str) -> String {
    let _ = (channel_scheme, job_id);
    todo!("plan 4a Task 4")
}
