//! `.lilref` files (CORE-047, APP-021, APP-055): a sealed ref with its content hash.

use lil_schema::{LilRef, parse_lilref, write_lilref};

const HASH: &str = "5a9db589b4907d4cfb2fb9a3a1da4efe5b2fa01d1b2aef2fed24ce2124a5a2a2";

fn sealed() -> String {
    format!("lil://0123456789abcdef/sketchbook/a@1.0.0#sha256={HASH}")
}

#[test] // spec: CORE-047
fn lilref_round_trips_and_requires_the_hash() {
    let text = format!(r#"{{"lilref": 1, "ref": "{}"}}"#, sealed());
    let parsed = parse_lilref(&text).unwrap();
    assert_eq!(parsed.r#ref, sealed());
    assert_eq!(parsed.title, None);
    assert_eq!(write_lilref(&parsed), text, "byte for byte");

    let titled = format!(
        r#"{{"lilref": 1, "ref": "{}", "title": "Knight \"final\" é"}}"#,
        sealed()
    );
    let parsed = parse_lilref(&titled).unwrap();
    assert_eq!(parsed.title.as_deref(), Some("Knight \"final\" é"));
    assert_eq!(write_lilref(&parsed), titled, "byte for byte, with a title");

    let no_hash = r#"{"lilref": 1, "ref": "lil://0123456789abcdef/sketchbook/a@1.0.0"}"#;
    let err = parse_lilref(no_hash).unwrap_err();
    assert!(err.contains("sha256"), "{err}");

    let v2 = format!(r#"{{"lilref": 2, "ref": "{}"}}"#, sealed());
    let err = parse_lilref(&v2).unwrap_err();
    assert!(err.contains("version"), "{err}");
}

#[test] // spec: CORE-047, APP-057
fn lilref_refuses_what_is_not_a_sealed_ref() {
    let candidate = r#"{"lilref": 1, "ref": "lil://0123456789abcdef/sketchbook/a@candidate/r_20261018_0900/x.png"}"#;
    assert!(parse_lilref(candidate).is_err(), "a candidate is view-only");
    for text in [
        "not json",
        r#"{"ref": "x"}"#,
        r#"{"lilref": 1}"#,
        r#"{"lilref": "1", "ref": "x"}"#,
        r#"{"lilref": 1, "ref": "ww://0123456789abcdef/sketchbook/a@1.0.0"}"#,
    ] {
        assert!(parse_lilref(text).is_err(), "{text}");
    }
    let extra = format!(r#"{{"lilref": 1, "ref": "{}", "note": "x"}}"#, sealed());
    assert!(parse_lilref(&extra).is_err(), "unknown fields are refused");
}

#[test]
#[should_panic(expected = "sealed ref with its hash")]
fn writing_a_ref_without_its_hash_is_a_bug() {
    let _ = write_lilref(&LilRef {
        r#ref: "lil://0123456789abcdef/sketchbook/a@1.0.0".into(),
        title: None,
    });
}

#[test] // spec: CORE-047, APP-021
fn lilref_titles_are_plain_and_short() {
    let with_title = |title: &str| {
        let title = serde_json::Value::String(title.to_string());
        format!(
            r#"{{"lilref": 1, "ref": "{}", "title": {title}}}"#,
            sealed()
        )
    };
    assert!(parse_lilref(&with_title(&"a".repeat(200))).is_ok());
    assert!(
        parse_lilref(&with_title(&"a".repeat(201))).is_err(),
        "too long"
    );
    for bad in [
        "two\nlines",
        "bell\u{7}",
        "nul\u{0}",
        "rtl \u{202E}gnp.exe",
        "iso \u{2066}x",
        "mark\u{200F}",
    ] {
        assert!(parse_lilref(&with_title(bad)).is_err(), "{bad:?}");
    }
    assert!(parse_lilref(&with_title("Knight, final (v2) é")).is_ok());
}

#[test]
#[should_panic(expected = "title")]
fn writing_a_bad_title_is_a_bug() {
    let _ = write_lilref(&LilRef {
        r#ref: sealed(),
        title: Some("a\nb".into()),
    });
}
