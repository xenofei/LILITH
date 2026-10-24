//! `FakeDesktop` (QA-066's test fakes): the [`Desktop`] of every test-channel core. It posts no
//! real toast and touches nothing on the desktop; it records each post and remove, and reports
//! the fullscreen state, foreground process, toasts state, shortcut and handler states a test
//! set (through `test.desktop`), counting each fullscreen read as a poll.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use crate::desktop::{Desktop, Fullscreen, HandlerState, ShortcutState, Toast, ToastsState};
use crate::env::Env;

/// A post or a remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastOp {
    Post,
    Remove,
}

/// One recorded call. A post carries the `Toast.xml` handed to the desktop, whether it was
/// quiet, and the title, body and launch URL read back from that XML (named and numeric
/// references decoded); a remove carries only its tag and group. The actions (a
/// confirmation's Open and Deny URLs) aren't split out: a test that needs them reads `xml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastRecord {
    pub op: ToastOp,
    pub tag: String,
    pub group: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub launch: Option<String>,
    pub silent: Option<bool>,
    pub xml: Option<String>,
}

#[derive(Debug)]
struct State {
    fullscreen: Fullscreen,
    foreground: Option<u32>,
    toasts: ToastsState,
    shortcut: ShortcutState,
    handlers: HandlerState,
    records: Vec<ToastRecord>,
    power_held: bool,
    polls: u32,
}

/// The fake desktop. It starts not fullscreen, with no foreground process, toasts ready, the
/// shortcut and handlers `Ok`, nothing recorded and no power request held.
#[derive(Debug)]
pub struct FakeDesktop {
    state: Mutex<State>,
}

impl Default for FakeDesktop {
    fn default() -> Self {
        FakeDesktop::new()
    }
}

impl FakeDesktop {
    pub fn new() -> FakeDesktop {
        FakeDesktop {
            state: Mutex::new(State {
                fullscreen: Fullscreen::No,
                foreground: None,
                toasts: ToastsState::Ready,
                shortcut: ShortcutState::Ok,
                handlers: HandlerState::Ok,
                records: Vec::new(),
                power_held: false,
                polls: 0,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_fullscreen(&self, fullscreen: Fullscreen) {
        self.lock().fullscreen = fullscreen;
    }

    pub fn set_foreground(&self, pid: Option<u32>) {
        self.lock().foreground = pid;
    }

    pub fn set_shortcut(&self, shortcut: ShortcutState) {
        self.lock().shortcut = shortcut;
    }

    pub fn set_handlers(&self, handlers: HandlerState) {
        self.lock().handlers = handlers;
    }

    pub fn set_toasts_state(&self, toasts: ToastsState) {
        self.lock().toasts = toasts;
    }

    /// Every post and remove since the start or the last [`FakeDesktop::clear`], in order.
    pub fn records(&self) -> Vec<ToastRecord> {
        self.lock().records.clone()
    }

    /// Forgets the records.
    pub fn clear(&self) {
        self.lock().records.clear();
    }

    /// Whether the power request is held.
    pub fn power_held(&self) -> bool {
        self.lock().power_held
    }

    /// How many times the fullscreen state was read.
    pub fn polls(&self) -> u32 {
        self.lock().polls
    }
}

impl Desktop for FakeDesktop {
    fn fullscreen(&self) -> Fullscreen {
        let mut state = self.lock();
        state.polls = state.polls.saturating_add(1);
        state.fullscreen
    }

    fn foreground_pid(&self) -> Option<u32> {
        self.lock().foreground
    }

    fn toasts_state(&self) -> ToastsState {
        self.lock().toasts
    }

    fn post(&self, t: &Toast) -> Result<(), String> {
        let texts = elements(&t.xml, "text");
        self.lock().records.push(ToastRecord {
            op: ToastOp::Post,
            tag: t.tag.clone(),
            group: t.group.clone(),
            title: texts.first().cloned(),
            body: texts.get(1).cloned(),
            launch: attribute(&t.xml, "toast", "launch"),
            silent: Some(t.silent),
            xml: Some(t.xml.clone()),
        });
        Ok(())
    }

    fn remove(&self, tag: &str, group: &str) {
        self.lock().records.push(ToastRecord {
            op: ToastOp::Remove,
            tag: tag.to_string(),
            group: group.to_string(),
            title: None,
            body: None,
            launch: None,
            silent: None,
            xml: None,
        });
    }

    fn set_power_request(&self, held: bool) -> std::io::Result<()> {
        self.lock().power_held = held;
        Ok(())
    }

    fn power_request_available(&self) -> bool {
        true
    }

    fn shortcut_state(&self, _env: &Env, _exe: &Path) -> ShortcutState {
        self.lock().shortcut
    }

    fn handler_state(&self, _env: &Env, _exe: &Path) -> HandlerState {
        self.lock().handlers.clone()
    }
}

/// The unescaped text of each `<name>…</name>` element of the toast's XML, in order. The toast
/// XML of Appendix G has no nested or self-closing `<text>`, so a plain scan is enough here.
fn elements(xml: &str, name: &str) -> Vec<String> {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        found.push(unescape(&after[..end]));
        rest = &after[end + close.len()..];
    }
    found
}

/// The unescaped value of `attr="…"` on the first `<element …>` of the XML.
fn attribute(xml: &str, element: &str, attr: &str) -> Option<String> {
    let start = xml.find(&format!("<{element} "))?;
    let tag = &xml[start..start + xml[start..].find('>')?];
    let key = format!(" {attr}=\"");
    let value = &tag[tag.find(&key)? + key.len()..];
    Some(unescape(&value[..value.find('"')?]))
}

/// XML's references, in one pass: the five predefined entities and the numeric references
/// `&#<decimal>;` and `&#x<hex>;`. A reference that isn't one of those, or names no character,
/// is kept as written.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').and_then(|semi| {
            let name = &rest[1..semi];
            let c = match name {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "amp" => Some('&'),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .map(|hex| u32::from_str_radix(hex, 16))
                    .or_else(|| name.strip_prefix('#').map(str::parse::<u32>))
                    .and_then(Result::ok)
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, semi))
        });
        match decoded {
            Some((c, semi)) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: QA-066
    fn unescape_reads_named_and_numeric_references() {
        assert_eq!(
            unescape("a &amp;lt; b &lt;&gt;&quot;&apos; &#65;&#x42;&#X43; &#x1F600;"),
            "a &lt; b <>\"' ABC \u{1F600}"
        );
        // Not references: kept as written.
        assert_eq!(
            unescape("& &unknown; &#xZZ; &#55296; x&"),
            "& &unknown; &#xZZ; &#55296; x&"
        );
    }
}
