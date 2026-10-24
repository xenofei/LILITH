//! `lil:` URLs (APP-020, QA-050; Appendix G "`lil:` URLs", ruling P4a-R3).

mod common;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use lil_parse::{LilUrl, confirm_url, deny_url, open_url, parse_lil_url, strip_argument, task_url};
use lil_schema::AssetRef;
use proptest::prelude::*;

const SCHEME: &str = "lil-dev";
const CONFIRM: &str = "cf_0123456789abcdef";
const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const JOB: &str = "j_fedcba9876543210";
const REF: &str = "lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0";

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn ok(arg: &str) -> LilUrl {
    parse_lil_url(arg, SCHEME).unwrap_or_else(|r| panic!("{arg:?} was refused: {r:?}"))
}

fn refused(arg: &str) {
    let r = parse_lil_url(arg, SCHEME).expect_err(&format!("{arg:?} must be refused"));
    assert!(r.shown.chars().count() <= 80, "{r:?}");
    assert!(!r.reason.is_empty(), "{r:?}");
}

#[test] // spec: APP-020, QA-050
fn confirm_deny_task_open_and_ref_parse() {
    for scheme in [SCHEME, "LIL-DEV", "Lil-Dev"] {
        assert_eq!(
            ok(&format!("{scheme}:confirm/{CONFIRM}")),
            LilUrl::Confirm(CONFIRM.into())
        );
        assert_eq!(
            ok(&format!("{scheme}:deny/{CONFIRM}/{TOKEN}")),
            LilUrl::Deny {
                id: CONFIRM.into(),
                token: TOKEN.into()
            }
        );
        assert_eq!(
            ok(&format!("{scheme}:task/{JOB}")),
            LilUrl::Task(JOB.into())
        );
        assert_eq!(
            ok(&format!("{scheme}:open/{}", b64("a target".as_bytes()))),
            LilUrl::Open("a target".into())
        );
    }
    assert_eq!(ok(REF), LilUrl::Ref(REF.into()));
    assert_eq!(
        ok(&REF.replacen("lil:", "LIL:", 1)),
        LilUrl::Ref(REF.into()),
        "the ref's scheme in any case, written lower-case"
    );
    let candidate =
        "lil://0123456789abcdef/sketchbook/chr_paper_crane@candidate/r_20261024_1200/a.png";
    assert_eq!(ok(candidate), LilUrl::Ref(candidate.into()));
    assert_eq!(
        ok(&format!("{SCHEME}:open/{}", b64(REF.as_bytes()))),
        LilUrl::Open(REF.into())
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
    assert_eq!(open_url("lil", "ab?"), format!("lil:open/{}", b64(b"ab?")));
}

#[test] // spec: APP-020, QA-050
fn deny_needs_an_id_and_a_32_hex_token() {
    refused(&format!("{SCHEME}:deny/{CONFIRM}"));
    refused(&format!("{SCHEME}:deny/{CONFIRM}/"));
    refused(&format!("{SCHEME}:deny/{CONFIRM}//"));
    refused(&format!("{SCHEME}:deny/{CONFIRM}/{}", &TOKEN[..31]));
    refused(&format!("{SCHEME}:deny/{CONFIRM}/{TOKEN}0"));
    refused(&format!(
        "{SCHEME}:deny/{CONFIRM}/{}",
        TOKEN.to_ascii_uppercase()
    ));
    refused(&format!("{SCHEME}:deny/{CONFIRM}/{TOKEN}/extra"));
    refused(&format!("{SCHEME}:deny/cf_0123/{TOKEN}"));
    refused(&format!("{SCHEME}:deny//{TOKEN}"));
}

#[test] // spec: APP-020, QA-050
fn other_channel_verbs_are_refused() {
    refused(&format!("lil:confirm/{CONFIRM}"));
    refused(&format!("lil:task/{JOB}"));
    refused(&format!("lil-test-abc:open/{}", b64(b"x")));
    refused(&format!("lil-dev2:task/{JOB}"));
    refused(&format!(
        "{SCHEME}://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0"
    ));
    refused(&format!("{SCHEME}:frobnicate/{CONFIRM}"));
    refused(&format!("{SCHEME}:confirm"));
    refused(&format!("{SCHEME}:"));
    refused("https://example.com/");
    refused("lil");
    refused("");
    // A raw ref is a ref under every channel.
    for channel in [SCHEME, "lil", "lil-test-abc"] {
        assert_eq!(parse_lil_url(REF, channel), Ok(LilUrl::Ref(REF.into())));
    }
    assert_eq!(
        parse_lil_url(&format!("lil:task/{JOB}"), "lil"),
        Ok(LilUrl::Task(JOB.into()))
    );
}

#[test] // spec: APP-020, QA-050
fn bad_ids_and_padding_are_refused() {
    refused(&format!("{SCHEME}:confirm/cf_0123456789abcde"));
    refused(&format!("{SCHEME}:confirm/cf_0123456789abcdef0"));
    refused(&format!("{SCHEME}:confirm/cf_0123456789ABCDEF"));
    refused(&format!("{SCHEME}:confirm/CF_0123456789abcdef"));
    refused(&format!("{SCHEME}:confirm/cf_0123456789abcdeg"));
    refused(&format!("{SCHEME}:task/j_0123"));
    refused(&format!("{SCHEME}:task/j_FEDCBA9876543210"));
    refused(&format!("{SCHEME}:task/{CONFIRM}"));
    refused(&format!("{SCHEME}:confirm/{JOB}"));
    refused(&format!("{SCHEME}:open/{}=", b64(b"xy")));
    refused(&format!("{SCHEME}:open/{}==", b64(b"x")));
    refused(&format!("{SCHEME}:open/{}?x", b64(b"xyz")));
    refused(&format!("{SCHEME}:open/{}#y", b64(b"xyz")));
    refused(&format!(
        "{SCHEME}:open/{}",
        URL_SAFE_NO_PAD.encode(b"xyz").replace('e', "+")
    ));
    refused(&format!("{SCHEME}:open/"));
    refused(&format!("{SCHEME}:open/A"));
    // Non-canonical trailing bits: `QR` would decode to the same byte as `QQ`.
    refused(&format!("{SCHEME}:open/QR"));
    refused(&format!("{SCHEME}:open/{}", b64(&[0xff, 0xfe])));
    refused(&format!("{SCHEME}:open/{}", b64(b"lil://not a ref")));
    refused(&format!("{SCHEME}:open/{}", b64(b"LIL://not a ref")));
    // The legacy scheme parses as a ref, so it is held to the same rule.
    refused(&format!("{SCHEME}:open/{}", b64(b"ww://not a ref")));
    refused(&format!("{SCHEME}:open/{}", b64(b"WW://not a ref")));
    let legacy = "ww://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0";
    assert_eq!(
        ok(&format!("{SCHEME}:open/{}", b64(legacy.as_bytes()))),
        LilUrl::Open(legacy.into())
    );
    refused(&format!("{SCHEME}:open/{}", b64(&[b'a'; 4097])));
    assert_eq!(
        ok(&format!("{SCHEME}:open/{}", b64(&[b'a'; 4096]))),
        LilUrl::Open("a".repeat(4096))
    );
}

#[test] // spec: APP-020, QA-050
fn decoded_targets_hold_no_control_characters() {
    for c in [
        '\u{0}', '\u{7}', '\t', '\n', '\r', '\u{1f}', '\u{7f}', '\u{80}', '\u{9f}',
    ] {
        let target = format!("a{c}b");
        refused(&format!("{SCHEME}:open/{}", b64(target.as_bytes())));
    }
    // A C1 control escaped as UTF-8 in a raw ref.
    refused("lil://0123456789abcdef/sketchbook/chr%C2%80paper@1.0.0");
    // A literal control in a raw ref.
    refused("lil://0123456789abcdef/sketchbook/chr\u{85}paper@1.0.0");
    assert_eq!(
        ok(&format!(
            "{SCHEME}:open/{}",
            b64("é ж 漢 😀 \u{200e}".as_bytes())
        )),
        LilUrl::Open("é ж 漢 😀 \u{200e}".into())
    );
}

#[test] // spec: APP-020, QA-050
fn one_trailing_slash_is_dropped_two_are_refused() {
    assert_eq!(
        ok(&format!("{SCHEME}:confirm/{CONFIRM}/")),
        LilUrl::Confirm(CONFIRM.into())
    );
    assert_eq!(
        ok(&format!("{SCHEME}:deny/{CONFIRM}/{TOKEN}/")),
        LilUrl::Deny {
            id: CONFIRM.into(),
            token: TOKEN.into()
        }
    );
    assert_eq!(
        ok(&format!("{SCHEME}:task/{JOB}/")),
        LilUrl::Task(JOB.into())
    );
    assert_eq!(
        ok(&format!("{SCHEME}:open/{}/", b64(b"x"))),
        LilUrl::Open("x".into())
    );
    refused(&format!("{SCHEME}:confirm/{CONFIRM}//"));
    refused(&format!("{SCHEME}:deny/{CONFIRM}/{TOKEN}//"));
    refused(&format!("{SCHEME}:task/{JOB}//"));
    refused(&format!("{SCHEME}:open/{}//", b64(b"x")));
}

#[test] // spec: APP-020, QA-050
fn raw_refs_are_percent_decoded_once() {
    assert_eq!(
        ok("lil://0123456789abcdef/sketchbook/chr_paper_cr%61ne@1.0.0"),
        LilUrl::Ref(REF.into())
    );
    assert_eq!(
        ok(
            "lil://0123456789abcdef/sketchbook/chr_paper_cr%61ne@candidate/r_20261024_1200/a%20b.png"
        ),
        LilUrl::Ref(
            "lil://0123456789abcdef/sketchbook/chr_paper_crane@candidate/r_20261024_1200/a b.png"
                .into()
        ),
        "%20 is a space, and the ref grammar allows it in a candidate's file name"
    );
    // A space where the ref grammar allows none.
    refused("lil://0123456789abcdef/sketch%20book/chr_paper_crane@1.0.0");
    // `%2520` decodes once to `%20`: a decoded `%` is refused, never decoded again.
    refused("lil://0123456789abcdef/sketch%2520book/chr_paper_crane@1.0.0");
    refused("lil://0123456789abcdef/sketchbook/chr%25paper@1.0.0");
    refused("lil://0123456789abcdef/sketchbook/chr%00paper@1.0.0");
    refused("lil://0123456789abcdef/sketchbook/chr%1Fpaper@1.0.0");
    refused("lil://0123456789abcdef/sketchbook/chr%7Fpaper@1.0.0");
    refused("lil://0123456789abcdef/sketchbook/chr%7fpaper@1.0.0");
    refused("lil://0123456789abcdef/sketchbook/chr%FFpaper@1.0.0");
    refused("lil://0123456789abcdef/sketchbook/chr%C3paper@1.0.0");
    // A `%` that isn't an escape.
    refused("lil://0123456789abcdef/sketchbook/chr%zzpaper@1.0.0");
    refused("lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0%");
    refused("lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0%6");
    refused("lil://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0/");
    refused("lil://not-a-ref");
    refused("lil://");
}

#[test] // spec: APP-020, QA-050
fn long_arguments_are_refused() {
    // A raw candidate ref whose run id is padded with escaped digits reaches exactly 8,192
    // characters while staying well-formed, so the limit alone decides.
    let base = "lil://0123456789abcdef/sketchbook/chr_paper_crane@candidate/r_20261024_1200_";
    let file = "/a.png";
    let room = 8192 - base.len() - file.len();
    let arg = |extra: usize| {
        format!(
            "{base}{}{}{file}",
            "%31".repeat(room / 3),
            "1".repeat(room % 3 + extra)
        )
    };
    let at_limit = arg(0);
    assert_eq!(at_limit.chars().count(), 8192);
    assert!(matches!(ok(&at_limit), LilUrl::Ref(_)));
    let over = arg(1);
    assert_eq!(over.chars().count(), 8193);
    let r = parse_lil_url(&over, SCHEME).expect_err("8,193 characters are refused");
    assert!(r.shown.chars().count() <= 80, "{r:?}");
    // Characters count, not bytes: 8,192 two-byte characters are within the limit (and then
    // refused for another reason, with a different reason).
    let wide = format!("{SCHEME}:x/{}", "é".repeat(8192 - SCHEME.len() - 3));
    assert_eq!(wide.chars().count(), 8192);
    let r_wide = parse_lil_url(&wide, SCHEME).expect_err("not a verb");
    assert_ne!(r_wide.reason, r.reason);
    let r = parse_lil_url(&"é".repeat(10_000), SCHEME).expect_err("long");
    assert!(r.shown.chars().count() <= 80, "{r:?}");
}

#[test] // spec: APP-020
fn strip_argument_removes_one_pair_of_quotes() {
    assert_eq!(strip_argument("\"x\""), "x");
    assert_eq!(strip_argument("\"\"x\"\""), "\"x\"");
    assert_eq!(strip_argument("  \"x\"  "), "x");
    assert_eq!(strip_argument("  x "), "x");
    assert_eq!(strip_argument("\"x"), "\"x");
    assert_eq!(strip_argument("\""), "\"");
    assert_eq!(strip_argument("\"\""), "");
    assert_eq!(strip_argument(""), "");
}

/// A target of 1–4,096 UTF-8 bytes with no control character, not itself a `lil://` ref.
fn target() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        any::<char>().prop_filter("no control", |c| !c.is_control()),
        1..1100,
    )
    .prop_map(|chars| {
        let mut s = String::new();
        for c in chars {
            if s.len() + c.len_utf8() > 4096 {
                break;
            }
            s.push(c);
        }
        s
    })
    .prop_filter("not ref-like", |s| {
        !["lil://", "ww://"].iter().any(|scheme| {
            s.get(..scheme.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(scheme))
        })
    })
}

/// `^<prefix>[0-9a-f]{<hex>}$`.
fn is_id(s: &str, prefix: &str, hex: usize) -> bool {
    s.strip_prefix(prefix).is_some_and(|h| {
        h.len() == hex && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Any string up to 10,000 characters, often behind a prefix that reaches the deeper paths.
fn argument() -> impl Strategy<Value = String> {
    let prefix = prop_oneof![
        Just(""),
        Just("lil-dev:"),
        Just("lil-dev:open/"),
        Just("lil-dev:deny/cf_0123456789abcdef/"),
        Just("lil-dev:confirm/"),
        Just("lil-dev:confirm/cf_"),
        Just("lil-dev:task/j_"),
        Just("lil://"),
        Just("lil://0123456789abcdef/sketchbook/"),
    ];
    let body = prop_oneof![
        proptest::collection::vec(any::<char>(), 0..10_000)
            .prop_map(|c| c.into_iter().collect::<String>()),
        "[A-Za-z0-9_%/@.#=:-]{0,200}",
        // Near-valid ids and tokens, so the accepting branches are reached.
        "[0-9a-fA/]{14,36}",
        "[a-z_]{1,20}@[0-9.]{1,8}",
    ];
    (prefix, body).prop_map(|(p, b)| {
        let mut s = format!("{p}{b}");
        while s.chars().count() > 10_000 {
            s.pop();
        }
        s
    })
}

proptest! {
    #![proptest_config(common::config())]

    #[test] // spec: APP-020, QA-050
    fn open_url_round_trips(target in target()) {
        let url = open_url(SCHEME, &target);
        prop_assert_eq!(&url, &format!("{SCHEME}:open/{}", b64(target.as_bytes())));
        prop_assert_eq!(parse_lil_url(&url, SCHEME), Ok(LilUrl::Open(target)));
    }

    #[test] // spec: QA-050
    fn any_argument_is_classified_or_refused_without_panic(arg in argument()) {
        let parsed = parse_lil_url(&arg, SCHEME);
        if arg.chars().count() > 8192 {
            prop_assert!(parsed.is_err());
        }
        match parsed {
            Ok(LilUrl::Open(t)) => {
                prop_assert!(!t.is_empty() && t.len() <= 4096);
                prop_assert!(!t.chars().any(char::is_control));
            }
            Ok(LilUrl::Ref(r)) => {
                prop_assert!(r.starts_with("lil://"));
                prop_assert!(r.parse::<AssetRef>().is_ok());
            }
            Ok(LilUrl::Confirm(id)) => prop_assert!(is_id(&id, "cf_", 16)),
            Ok(LilUrl::Deny { id, token }) => {
                prop_assert!(is_id(&id, "cf_", 16));
                prop_assert!(is_id(&token, "", 32));
            }
            Ok(LilUrl::Task(id)) => prop_assert!(is_id(&id, "j_", 16)),
            Err(r) => {
                prop_assert!(r.shown.chars().count() <= 80);
                prop_assert!(!r.shown.chars().any(char::is_control));
            }
        }
    }
}
