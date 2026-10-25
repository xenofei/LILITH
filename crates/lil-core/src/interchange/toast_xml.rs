//! The toast XML (Appendix G "Toast XML"; P4a-R8, P4a-R9):
//!
//! `<toast launch="URL" activationType="protocol"><visual><binding template="ToastGeneric">
//! <text>TITLE</text><text>BODY</text></binding></visual>ACTIONS</toast>`
//!
//! TITLE and BODY are [`lil_parse::shown_text`] output (controls removed, bidirectional and
//! zero-width characters shown as `⟨U+XXXX⟩`, at most [`SHOWN_CHARS`] `char`s), then
//! XML-escaped; the URLs are escaped too. ACTIONS is Open and Deny for a confirmation toast
//! and empty otherwise. A toast with no target (a `notify.claude_code` message) has no
//! `launch` attribute, so a click only dismisses it (plan 4a clarification, 2026-10-25). No
//! toast approves anything (owner decision 2026-10-22), and none carries a progress binding
//! (P4a-R36).

/// The most `char`s of a toast's title or body (Appendix G).
pub const SHOWN_CHARS: usize = 200;

/// A toast's buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastActions {
    None,
    /// A confirmation toast's "Open" (`<scheme>:confirm/<id>`) and "Deny"
    /// (`<scheme>:deny/<id>/<token>`), both by protocol (P4a-R9).
    OpenAndDeny {
        open_url: String,
        deny_url: String,
    },
}

/// What a toast says and where a click goes. `title` and `body` are untrusted text, shown
/// through [`lil_parse::shown_text`]; `launch` is `None` for a toast with no target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastText {
    pub title: String,
    pub body: String,
    pub launch: Option<String>,
    pub actions: ToastActions,
}

/// `s` with `&`, `<`, `>`, `"` and `'` written as XML references.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The text of a `<text>` element: shown, then escaped.
fn shown(s: &str) -> String {
    escape(&lil_parse::shown_text(s, SHOWN_CHARS))
}

/// The toast XML of `t` (module docs).
pub fn toast_xml(t: &ToastText) -> String {
    let launch = t
        .launch
        .as_deref()
        .map(|url| format!(" launch=\"{}\"", escape(url)))
        .unwrap_or_default();
    let actions = match &t.actions {
        ToastActions::None => String::new(),
        ToastActions::OpenAndDeny { open_url, deny_url } => format!(
            "<actions><action content=\"Open\" arguments=\"{}\" activationType=\"protocol\"/>\
             <action content=\"Deny\" arguments=\"{}\" activationType=\"protocol\"/></actions>",
            escape(open_url),
            escape(deny_url)
        ),
    };
    format!(
        "<toast{launch} activationType=\"protocol\"><visual><binding template=\"ToastGeneric\">\
         <text>{}</text><text>{}</text></binding></visual>{actions}</toast>",
        shown(&t.title),
        shown(&t.body)
    )
}

/// `url` with a Deny URL's token cut: `…/deny/<id>/<token>` becomes `…/deny/<id>/…`, so no
/// log line holds a token (P4a-R9). Any other text is returned as it is.
pub fn cut_token(url: &str) -> String {
    match url.find(":deny/") {
        Some(at) => {
            let rest = &url[at + ":deny/".len()..];
            match rest.find('/') {
                Some(slash) => format!("{}…", &url[..at + ":deny/".len() + slash + 1]),
                None => url.to_string(),
            }
        }
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-022
    fn a_deny_url_loses_its_token() {
        assert_eq!(
            cut_token("lil-test-a:deny/cf_0123456789abcdef/0123456789abcdef0123456789abcdef"),
            "lil-test-a:deny/cf_0123456789abcdef/…"
        );
        assert_eq!(cut_token("lil:confirm/cf_1"), "lil:confirm/cf_1");
    }

    #[test] // spec: APP-022
    fn a_toast_with_no_target_has_no_launch() {
        let xml = toast_xml(&ToastText {
            title: "Claude Code".into(),
            body: "done".into(),
            launch: None,
            actions: ToastActions::None,
        });
        assert!(
            xml.starts_with("<toast activationType=\"protocol\">"),
            "{xml}"
        );
    }
}
