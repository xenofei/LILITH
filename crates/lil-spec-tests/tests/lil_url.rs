//! `lil:` URLs, a format (QA-018) read through `lil_parse`'s public parser: every form Appendix G
//! names parses to its variant and `open` targets round-trip; anything else is refused with at
//! most 80 characters shown; a raw ref is percent-decoded exactly once (spec APP-020, QA-050;
//! Appendix G "`lil:` URLs", ruling P4a-R3).

mod p4a;
mod support;

use lil_parse::{LilUrl, confirm_url, deny_url, open_url, parse_lil_url, strip_argument, task_url};
use p4a::b64url;

/// The channel scheme these tests parse under.
const SCHEME: &str = "lil-dev";
const CONFIRM: &str = "cf_0123456789abcdef";
const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const JOB: &str = "j_fedcba9876543210";
const REF: &str = "lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0";

/// Characters for generated targets: ASCII that URLs and command lines treat specially,
/// letters from other scripts and emoji; never a control character.
const POOL: &[char] = &[
    'a', 'b', 'z', 'A', 'Q', 'Z', '0', '7', '9', ' ', '%', '#', '?', '&', '=', '+', '/', '\\', ':',
    '.', '-', '_', '"', '\'', '@', '~', 'é', 'ß', 'ж', 'ע', '漢', '字', '😀', '🎨',
];

/// A small deterministic generator (xorshift64), so a failure names a reproducible target.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A target of 1–4,096 UTF-8 bytes from [`POOL`] that isn't itself a `lil://` ref.
    fn target(&mut self) -> String {
        let limit = 1 + (self.next() % 4096) as usize;
        let mut s = String::new();
        loop {
            let c = POOL[(self.next() % POOL.len() as u64) as usize];
            if s.len() + c.len_utf8() > limit {
                break;
            }
            s.push(c);
        }
        if s.is_empty() || s.to_ascii_lowercase().starts_with("lil://") {
            s.insert(0, 'x');
        }
        s
    }
}

#[test] // spec: APP-020, QA-050
fn every_form_round_trips() {
    let ok = |arg: &str| -> LilUrl {
        parse_lil_url(arg, SCHEME).unwrap_or_else(|r| panic!("{arg:?} was refused: {r:?}"))
    };
    assert_eq!(
        ok(&format!("{SCHEME}:confirm/{CONFIRM}")),
        LilUrl::Confirm(CONFIRM.into())
    );
    assert_eq!(
        ok(&format!("LIL-Dev:confirm/{CONFIRM}")),
        LilUrl::Confirm(CONFIRM.into()),
        "the scheme in any case"
    );
    assert_eq!(
        ok(&format!("{SCHEME}:deny/{CONFIRM}/{TOKEN}")),
        LilUrl::Deny {
            id: CONFIRM.into(),
            token: TOKEN.into()
        }
    );
    assert_eq!(
        ok(&format!("{SCHEME}:task/{JOB}")),
        LilUrl::Task(JOB.into())
    );
    assert_eq!(
        ok(&format!("{SCHEME}:task/{JOB}/")),
        LilUrl::Task(JOB.into()),
        "one trailing slash is dropped"
    );
    assert_eq!(ok(REF), LilUrl::Ref(REF.into()));
    assert_eq!(
        parse_lil_url(REF, "lil-test-abc"),
        Ok(LilUrl::Ref(REF.into())),
        "a raw ref under every channel"
    );
    let sealed = format!("{REF}#sha256={}", "ab".repeat(32));
    assert_eq!(ok(&sealed), LilUrl::Ref(sealed.clone()));
    assert_eq!(
        ok(&format!("{SCHEME}:open/{}", b64url(REF.as_bytes()))),
        LilUrl::Open(REF.into())
    );
    assert_eq!(
        ok(strip_argument(&format!("  \"{SCHEME}:task/{JOB}\" "))),
        LilUrl::Task(JOB.into())
    );

    assert_eq!(
        confirm_url(SCHEME, CONFIRM),
        format!("{SCHEME}:confirm/{CONFIRM}")
    );
    assert_eq!(
        deny_url(SCHEME, CONFIRM, TOKEN),
        format!("{SCHEME}:deny/{CONFIRM}/{TOKEN}")
    );
    assert_eq!(task_url(SCHEME, JOB), format!("{SCHEME}:task/{JOB}"));

    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for n in 0..1000 {
        let target = match n {
            0 => "a".to_string(),
            1 => "a".repeat(4096),
            _ => rng.target(),
        };
        let url = open_url(SCHEME, &target);
        assert_eq!(
            url,
            format!("{SCHEME}:open/{}", b64url(target.as_bytes())),
            "base64url without padding of {target:?}"
        );
        assert_eq!(
            parse_lil_url(&url, SCHEME),
            Ok(LilUrl::Open(target.clone())),
            "target {n}: {target:?}"
        );
    }
}

#[test] // spec: APP-020, QA-050
fn refuses_everything_else() {
    let refused = [
        format!("lil:confirm/{CONFIRM}"),
        format!("{SCHEME}:frobnicate/{CONFIRM}"),
        format!("{SCHEME}:confirm/cf_0123456789abcde"),
        format!("{SCHEME}:confirm/cf_0123456789ABCDEF"),
        format!("{SCHEME}:task/j_0123"),
        format!("{SCHEME}:deny/{CONFIRM}"),
        format!("{SCHEME}:deny/{CONFIRM}/"),
        format!("{SCHEME}:deny/{CONFIRM}/{}", &TOKEN[..31]),
        format!("{SCHEME}:deny/{CONFIRM}/{}", TOKEN.to_ascii_uppercase()),
        format!("{SCHEME}:deny/{CONFIRM}/{TOKEN}/extra"),
        format!("{SCHEME}:confirm/{CONFIRM}//"),
        format!("{SCHEME}:open/{}==", b64url(b"x")),
        format!("{SCHEME}:open/{}", b64url("a\u{0007}b".as_bytes())),
        format!("{SCHEME}:open/{}", b64url(b"lil://not a ref")),
        format!("{SCHEME}:open/"),
        "lil://0123456789abcdef/sketchbook/chr%00paper@1.0.0".to_string(),
        "lil://0123456789abcdef/sketchbook/chr%7Fpaper@1.0.0".to_string(),
        "lil://0123456789abcdef/sketchbook/chr%FFpaper@1.0.0".to_string(),
        "https://example.com/".to_string(),
        String::new(),
    ];
    // An 8,193-character argument is refused and its `shown` text cut to 80 characters. It
    // doesn't isolate the 8,192 limit (no well-formed URL is that long: its target would pass
    // 4,096 bytes); Task 4's own `long_arguments_are_refused` checks the limit alone.
    let long = format!(
        "{SCHEME}:open/{}",
        "A".repeat(8193 - SCHEME.len() - ":open/".len())
    );
    assert_eq!(long.chars().count(), 8193);
    for arg in refused.iter().chain([&long]) {
        let refusal =
            parse_lil_url(arg, SCHEME).expect_err(&format!("{:?} must be refused", truncate(arg)));
        assert!(
            refusal.shown.chars().count() <= 80,
            "shown is at most 80 characters: {refusal:?}"
        );
        assert!(!refusal.reason.is_empty(), "{refusal:?}");
    }
}

#[test] // spec: APP-020, QA-050
fn raw_refs_are_percent_decoded_once() {
    assert_eq!(
        parse_lil_url(
            "lil://0123456789abcdef/sketchbook/chr_paper_cr%61ne@1.0.0",
            SCHEME
        ),
        Ok(LilUrl::Ref(REF.into())),
        "%61 is decoded"
    );
    // `%2520` decodes once to `%20`: a decoded `%` is refused, never decoded again.
    assert!(
        parse_lil_url(
            "lil://0123456789abcdef/sketch%2520book/chr_paper_crane@1.0.0",
            SCHEME
        )
        .is_err()
    );
}

/// The first 60 characters, for messages.
fn truncate(s: &str) -> String {
    s.chars().take(60).collect()
}
