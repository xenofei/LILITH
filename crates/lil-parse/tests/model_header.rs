//! Model families from safetensors and GGUF headers (CORE-164, QA-050; Appendix G "Model
//! inventory report", ruling P4a-R28).

mod common;

use std::io::{self, Read};

use lil_parse::{Family, HeaderError, gguf_family, safetensors_family};
use proptest::prelude::*;

const MIB: u64 = 1024 * 1024;

fn safetensors(header: &str) -> Vec<u8> {
    let mut out = (header.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(header.as_bytes());
    out
}

fn st(bytes: &[u8]) -> Result<Family, HeaderError> {
    safetensors_family(&mut &bytes[..])
}

fn gg(bytes: &[u8]) -> Result<Family, HeaderError> {
    gguf_family(&mut &bytes[..])
}

fn known(s: &str) -> Family {
    Family::Known(s.into())
}

/// A reader that serves `data` and then fails, counting the bytes it served.
struct FailAfter {
    data: Vec<u8>,
    pos: usize,
}

impl Read for FailAfter {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.data.len() {
            return Err(io::Error::other("read past the prefix"));
        }
        let n = buf.len().min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

const TENSOR: &str = r#""model.weight":{"dtype":"F16","shape":[2,2],"data_offsets":[0,8]}"#;

#[test] // spec: CORE-164
fn safetensors_reads_modelspec_architecture_then_ss_base_model_version() {
    let both = format!(
        r#"{{{TENSOR},"__metadata__":{{"ss_base_model_version":"sdxl_base_v1-0","modelspec.architecture":"stable-diffusion-xl-v1-base"}}}}"#
    );
    assert_eq!(
        st(&safetensors(&both)).unwrap(),
        known("stable-diffusion-xl-v1-base")
    );
    let ss = format!(r#"{{"__metadata__":{{"ss_base_model_version":"sd_v1"}},{TENSOR}}}"#);
    assert_eq!(st(&safetensors(&ss)).unwrap(), known("sd_v1"));
    // safetensors pads its header with spaces.
    let padded = r#"{"__metadata__":{"modelspec.architecture":"flux-1-dev"}}       "#;
    assert_eq!(st(&safetensors(padded)).unwrap(), known("flux-1-dev"));
    // Keys and values are read as JSON strings, escapes included.
    let escaped = r#"{"__metadata__":{"modelspec.architecture":"flux-1","x":[{"]":"}"}]}}"#;
    assert_eq!(st(&safetensors(escaped)).unwrap(), known("flux-1"));
    // The tensor data after the header is never read.
    let mut file = safetensors(&ss);
    file.extend_from_slice(&[0u8; 64]);
    let mut r = FailAfter {
        pos: 0,
        data: safetensors(&ss),
    };
    assert_eq!(safetensors_family(&mut r).unwrap(), known("sd_v1"));
    assert_eq!(st(&file).unwrap(), known("sd_v1"));
}

#[test] // spec: CORE-164
fn safetensors_without_metadata_is_unknown() {
    for header in [
        format!("{{{TENSOR}}}"),
        "{}".to_string(),
        r#"{"__metadata__":{}}"#.to_string(),
        r#"{"__metadata__":{"format":"pt"}}"#.to_string(),
        r#"{"__metadata__":{"modelspec.architecture":""}}"#.to_string(),
        r#"{"__metadata__":"nope"}"#.to_string(),
        r#"{"__metadata__":{"modelspec.architecture":7}}"#.to_string(),
        format!(
            r#"{{"__metadata__":{{"modelspec.architecture":"{}"}}}}"#,
            "a".repeat(65_537)
        ),
    ] {
        assert_eq!(
            st(&safetensors(&header)).unwrap(),
            Family::Unknown,
            "{header:.80}"
        );
    }
    // A value that isn't a string is skipped for the next key.
    assert_eq!(
        st(&safetensors(
            r#"{"__metadata__":{"modelspec.architecture":7,"ss_base_model_version":"sd_v2"}}"#
        ))
        .unwrap(),
        known("sd_v2")
    );
    // Not a header at all.
    for bad in [
        safetensors("not json"),
        safetensors("[1,2]"),
        safetensors(""),
        safetensors(r#"{"__metadata__":{"#),
    ] {
        assert!(
            matches!(st(&bad), Err(HeaderError::Malformed(_))),
            "{bad:?}"
        );
    }
    assert!(matches!(st(&[1, 2, 3]), Err(HeaderError::Io(_))));
    // The header is shorter than its prefix says.
    let mut short = safetensors(r#"{"__metadata__":{}}"#);
    short.truncate(short.len() - 2);
    assert!(st(&short).is_err());
}

#[test] // spec: CORE-164
fn safetensors_header_over_100_mib_is_unknown_without_reading_it() {
    let mut r = FailAfter {
        data: (100 * MIB + 1).to_le_bytes().to_vec(),
        pos: 0,
    };
    assert_eq!(safetensors_family(&mut r).unwrap(), Family::Unknown);
    assert_eq!(r.pos, 8, "nothing is read past the prefix");
    let mut r = FailAfter {
        data: u64::MAX.to_le_bytes().to_vec(),
        pos: 0,
    };
    assert_eq!(safetensors_family(&mut r).unwrap(), Family::Unknown);
    // Exactly 100 MiB is within the limit, so it is read (and here fails).
    let mut r = FailAfter {
        data: (100 * MIB).to_le_bytes().to_vec(),
        pos: 0,
    };
    assert!(matches!(
        safetensors_family(&mut r),
        Err(HeaderError::Io(_))
    ));
}

/// A GGUF file builder: magic, version, tensor count, then key-value pairs.
struct Gguf {
    version: u32,
    kvs: Vec<u8>,
    count: u64,
}

const T_U8: u32 = 0;
const T_U32: u32 = 4;
const T_F32: u32 = 6;
const T_BOOL: u32 = 7;
const T_STRING: u32 = 8;
const T_ARRAY: u32 = 9;
const T_U64: u32 = 10;
const T_F64: u32 = 12;

fn gstr(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u64).to_le_bytes());
    out.extend_from_slice(s);
}

impl Gguf {
    fn new(version: u32) -> Gguf {
        Gguf {
            version,
            kvs: Vec::new(),
            count: 0,
        }
    }

    /// A key with a value of `ty`, its bytes already encoded.
    fn kv(mut self, key: &str, ty: u32, value: &[u8]) -> Gguf {
        gstr(&mut self.kvs, key.as_bytes());
        self.kvs.extend_from_slice(&ty.to_le_bytes());
        self.kvs.extend_from_slice(value);
        self.count += 1;
        self
    }

    fn string(self, key: &str, value: &str) -> Gguf {
        let mut v = Vec::new();
        gstr(&mut v, value.as_bytes());
        self.kv(key, T_STRING, &v)
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = b"GGUF".to_vec();
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&3_u64.to_le_bytes());
        out.extend_from_slice(&self.count.to_le_bytes());
        out.extend_from_slice(&self.kvs);
        out
    }
}

fn array(ty: u32, n: u64, elements: &[u8]) -> Vec<u8> {
    let mut v = ty.to_le_bytes().to_vec();
    v.extend_from_slice(&n.to_le_bytes());
    v.extend_from_slice(elements);
    v
}

/// A typical llama.cpp header: assorted values, then `general.architecture`.
fn typical(version: u32) -> Gguf {
    let mut tokens = Vec::new();
    for t in ["<s>", "</s>", "hello"] {
        gstr(&mut tokens, t.as_bytes());
    }
    let mut nested = array(T_U8, 2, &[1, 2]);
    nested.extend_from_slice(&array(T_U8, 1, &[3]));
    Gguf::new(version)
        .kv("general.quantization_version", T_U32, &2_u32.to_le_bytes())
        .string("general.name", "Tiny")
        .kv("general.file_type", T_U64, &7_u64.to_le_bytes())
        .kv("x.rope", T_F32, &1.5_f32.to_le_bytes())
        .kv("x.eps", T_F64, &1e-5_f64.to_le_bytes())
        .kv("x.flag", T_BOOL, &[1])
        .kv(
            "tokenizer.ggml.tokens",
            T_ARRAY,
            &array(T_STRING, 3, &tokens),
        )
        .kv("x.bytes", T_ARRAY, &array(T_U8, 4, &[9, 9, 9, 9]))
        .kv("x.nested", T_ARRAY, &array(T_ARRAY, 2, &nested))
        .string("general.architecture", "llama")
        .string("llama.after", "read for duplicates")
}

#[test] // spec: CORE-164
fn gguf_reads_general_architecture() {
    for version in [2, 3] {
        assert_eq!(
            gg(&typical(version).bytes()).unwrap(),
            known("llama"),
            "v{version}"
        );
        let first = Gguf::new(version).string("general.architecture", "qwen2");
        assert_eq!(gg(&first.bytes()).unwrap(), known("qwen2"));
    }
    // The rest of the key-value section is read, and nothing after it.
    let mut file = Gguf::new(3).string("general.architecture", "gemma").bytes();
    let mut r = FailAfter {
        data: file.clone(),
        pos: 0,
    };
    assert_eq!(gguf_family(&mut r).unwrap(), known("gemma"));
    file.extend_from_slice(&[0xff; 32]);
    assert_eq!(gg(&file).unwrap(), known("gemma"));

    // No `general.architecture`, or one that isn't a string.
    assert_eq!(
        gg(&Gguf::new(3).string("general.name", "x").bytes()).unwrap(),
        Family::Unknown
    );
    assert_eq!(gg(&Gguf::new(3).bytes()).unwrap(), Family::Unknown);
    let wrong_type = Gguf::new(3).kv("general.architecture", T_U32, &1_u32.to_le_bytes());
    assert_eq!(gg(&wrong_type.bytes()).unwrap(), Family::Unknown);
    assert_eq!(
        gg(&Gguf::new(3).string("general.architecture", "").bytes()).unwrap(),
        Family::Unknown
    );
    // Another version's layout isn't read.
    assert_eq!(gg(&typical(1).bytes()).unwrap(), Family::Unknown);
    assert_eq!(gg(&typical(4).bytes()).unwrap(), Family::Unknown);

    // Not a GGUF header.
    let mut bad = typical(3).bytes();
    bad[0] = b'X';
    assert!(matches!(gg(&bad), Err(HeaderError::Malformed(_))));
    assert!(matches!(gg(b"GGU"), Err(HeaderError::Io(_))));
    let unknown_type = Gguf::new(3)
        .kv("x", 13, &[0; 8])
        .string("general.architecture", "a");
    assert!(matches!(
        gg(&unknown_type.bytes()),
        Err(HeaderError::Malformed(_))
    ));
    let mut truncated = typical(3).bytes();
    // Cut inside `general.architecture`'s value: the last pair is 50 bytes, that one 45.
    truncated.truncate(truncated.len() - 50 - 3);
    assert!(matches!(gg(&truncated), Err(HeaderError::Io(_))));
    // Cut inside the pair after it: that pair is still read, looking for a repeat.
    let mut truncated = typical(3).bytes();
    truncated.truncate(truncated.len() - 3);
    assert!(matches!(gg(&truncated), Err(HeaderError::Io(_))));
    let invalid_utf8 = Gguf::new(3).kv("general.architecture", T_STRING, &{
        let mut v = Vec::new();
        gstr(&mut v, &[0xff, 0xfe]);
        v
    });
    assert!(matches!(
        gg(&invalid_utf8.bytes()),
        Err(HeaderError::Malformed(_))
    ));
    // An array whose element sizes overflow is refused before anything is read.
    let huge = Gguf::new(3).kv("x", T_ARRAY, &array(T_U64, u64::MAX, &[]));
    assert!(matches!(gg(&huge.bytes()), Err(HeaderError::Malformed(_))));
}

#[test] // spec: CORE-164
fn gguf_with_more_than_1024_keys_first_is_unknown() {
    let with_keys_first = |n: usize| {
        let mut g = Gguf::new(3);
        for i in 0..n {
            g = g.kv(&format!("k.{i}"), T_U8, &[0]);
        }
        g.string("general.architecture", "llama").bytes()
    };
    assert_eq!(gg(&with_keys_first(1024)).unwrap(), known("llama"));
    assert_eq!(gg(&with_keys_first(1025)).unwrap(), Family::Unknown);
}

#[test] // spec: CORE-164
fn gguf_string_over_64_kib_is_unknown() {
    let long = "a".repeat(64 * 1024);
    let at_limit = Gguf::new(3).string("general.architecture", &long);
    assert_eq!(gg(&at_limit.bytes()).unwrap(), known(&long));
    let over = Gguf::new(3).string("general.architecture", &format!("{long}a"));
    assert_eq!(gg(&over.bytes()).unwrap(), Family::Unknown);
    // A long key before the family is held in memory to be compared, so it is refused.
    let key_first = Gguf::new(3)
        .kv(&format!("{long}a"), T_U8, &[0])
        .string("general.architecture", "llama");
    assert_eq!(gg(&key_first.bytes()).unwrap(), Family::Unknown);
    // The long string's bytes are never read: the file ends right after its length.
    let mut head = Gguf::new(3).bytes();
    head[16..24].copy_from_slice(&1_u64.to_le_bytes());
    gstr(&mut head, b"general.architecture");
    head.extend_from_slice(&T_STRING.to_le_bytes());
    head.extend_from_slice(&u64::MAX.to_le_bytes());
    let mut r = FailAfter {
        pos: 0,
        data: head.clone(),
    };
    assert_eq!(gguf_family(&mut r).unwrap(), Family::Unknown);
    assert_eq!(r.pos, head.len());
}

#[test] // spec: CORE-164
fn gguf_long_skipped_values_keep_the_family() {
    // Skipped values aren't held in memory, so the 64 KiB limit doesn't apply to them: a
    // 100 KiB chat template before or after the family, or in an array, doesn't hide it.
    let template = "{% x %}".repeat(100 * 1024 / 7 + 1);
    assert!(template.len() > 100 * 1024);
    let after = Gguf::new(3)
        .string("general.architecture", "llama")
        .string("tokenizer.chat_template", &template);
    assert_eq!(gg(&after.bytes()).unwrap(), known("llama"));
    let before = Gguf::new(3)
        .string("tokenizer.chat_template", &template)
        .string("general.architecture", "llama");
    assert_eq!(gg(&before.bytes()).unwrap(), known("llama"));
    let mut strings = Vec::new();
    gstr(&mut strings, template.as_bytes());
    let in_array = Gguf::new(3).string("general.architecture", "llama").kv(
        "x.templates",
        T_ARRAY,
        &array(T_STRING, 1, &strings),
    );
    assert_eq!(gg(&in_array.bytes()).unwrap(), known("llama"));
    // A skipped value longer than the file is still an unexpected end of file.
    let mut head = Gguf::new(3).string("general.architecture", "llama").bytes();
    head[16..24].copy_from_slice(&2_u64.to_le_bytes());
    gstr(&mut head, b"x");
    head.extend_from_slice(&T_STRING.to_le_bytes());
    head.extend_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(gg(&head), Err(HeaderError::Io(_))));
}

#[test] // spec: CORE-164
fn gguf_repeated_architecture_is_unknown() {
    let twice = Gguf::new(3)
        .string("general.architecture", "llama")
        .string("general.name", "x")
        .string("general.architecture", "qwen2");
    assert_eq!(gg(&twice.bytes()).unwrap(), Family::Unknown);
    let other_type = Gguf::new(3).string("general.architecture", "llama").kv(
        "general.architecture",
        T_U32,
        &1_u32.to_le_bytes(),
    );
    assert_eq!(gg(&other_type.bytes()).unwrap(), Family::Unknown);
}

#[test] // spec: CORE-164
fn gguf_reads_at_most_100_mib() {
    // An array before `general.architecture` that announces 200 MiB of bytes, served by a
    // reader that never ends: the walk stops at 100 MiB.
    let head = Gguf::new(3)
        .kv("x.blob", T_ARRAY, &array(T_U8, 200 * MIB, &[]))
        .string("general.architecture", "llama")
        .bytes();
    let blob_end = head.len() - 45;
    let mut r = (&head[..blob_end]).chain(io::repeat(0));
    assert_eq!(gguf_family(&mut r).unwrap(), Family::Unknown);
    // A short file still ends as an unexpected end of file.
    assert!(matches!(gg(&head[..blob_end]), Err(HeaderError::Io(_))));
}

#[test] // spec: CORE-164
fn gguf_key_shorter_than_its_length_is_an_io_error() {
    let mut head = Gguf::new(3).bytes();
    head[16..24].copy_from_slice(&1_u64.to_le_bytes());
    head.extend_from_slice(&10_u64.to_le_bytes());
    head.extend_from_slice(b"gen");
    assert!(matches!(gg(&head), Err(HeaderError::Io(_))));
}

#[test] // spec: CORE-164
fn safetensors_repeated_keys_give_no_family() {
    // A repeated `__metadata__` is refused, as the reference loader refuses it.
    let twice = r#"{"__metadata__":{"modelspec.architecture":"a"},"__metadata__":{}}"#;
    assert!(matches!(
        st(&safetensors(twice)),
        Err(HeaderError::Malformed(_))
    ));
    // A repeated candidate: loaders disagree on which copy wins, so neither is shown.
    for metadata in [
        r#"{"modelspec.architecture":"a","modelspec.architecture":"b"}"#,
        r#"{"ss_base_model_version":"a","x":"y","ss_base_model_version":"b"}"#,
        r#"{"modelspec.architecture":"a","ss_base_model_version":"c","ss_base_model_version":"d"}"#,
    ] {
        let header = format!(r#"{{"__metadata__":{metadata}}}"#);
        assert_eq!(
            st(&safetensors(&header)).unwrap(),
            Family::Unknown,
            "{metadata}"
        );
    }
}

/// `n` arrays nested in a member of the top object (or of `__metadata__`), so the document is
/// `n + 1` (or `n + 2`) levels deep.
fn nested(n: usize, in_metadata: bool) -> String {
    let value = format!("{}{}", "[".repeat(n), "]".repeat(n));
    if in_metadata {
        format!(r#"{{"__metadata__":{{"x":{value}}}}}"#)
    } else {
        format!(r#"{{"x":{value}}}"#)
    }
}

#[test] // spec: CORE-164, QA-050
fn safetensors_invalid_json_is_malformed() {
    for bad in [
        r#"{"x":[}"#,
        r#"{"x":{]}"#,
        r#"{"x":[1,2}}"#,
        r#"{"x":x"}"#,
        r#"{"x":tru}"#,
        r#"{"x":truex}"#,
        r#"{"x":[1,]}"#,
        r#"{"x":{"a":1,}}"#,
        r#"{"x":{"a" 1}}"#,
        r#"{"x":{1:2}}"#,
        r#"{"x":[1 2]}"#,
        r#"{"x":1,}"#,
        r#"{"x":1}}"#,
        r#"{"x":1} x"#,
        r#"{"__metadata__":{"modelspec.architecture":"a"],"x":1}"#,
    ] {
        assert!(
            matches!(st(&safetensors(bad)), Err(HeaderError::Malformed(_))),
            "{bad}"
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(bad).is_err(),
            "{bad}"
        );
    }
    for good in [
        r#"{"x":[1,-2.5e+3,true,false,null,"]",{"a":[]},{}]}"#,
        " {\n\t\"x\" : [ 1 , { } ] \r} ",
    ] {
        assert_eq!(st(&safetensors(good)).unwrap(), Family::Unknown, "{good}");
    }
    // The nesting limit is serde_json's, which the reference loader inherits.
    for in_metadata in [false, true] {
        for n in 120..135 {
            let json = nested(n, in_metadata);
            let reference = serde_json::from_str::<serde_json::Value>(&json).is_ok();
            let ours = st(&safetensors(&json));
            assert_eq!(
                ours.is_ok(),
                reference,
                "depth {n} (in metadata: {in_metadata}): {ours:?}"
            );
        }
    }
}

#[test] // spec: CORE-164
fn safetensors_family_cap_is_checked_before_decoding() {
    // 65,536 `\u0061` escapes are 6 * 64 KiB + 2 bytes with the quotes and decode to exactly
    // 64 KiB: within the cap, so the pre-decode bound must not refuse it.
    let escaped = "\\u0061".repeat(64 * 1024);
    let header = format!(r#"{{"__metadata__":{{"modelspec.architecture":"{escaped}"}}}}"#);
    assert_eq!(
        st(&safetensors(&header)).unwrap(),
        known(&"a".repeat(64 * 1024))
    );
    // One more byte of text is over the bound, and over the cap once decoded.
    let header = format!(r#"{{"__metadata__":{{"modelspec.architecture":"{escaped}a"}}}}"#);
    assert_eq!(st(&safetensors(&header)).unwrap(), Family::Unknown);
}

/// JSON-ish text built from fragments, so the scanner's deep paths see more than byte flips.
fn json_fragments() -> impl Strategy<Value = String> {
    let fragment = prop_oneof![
        Just("{"),
        Just("}"),
        Just("["),
        Just("]"),
        Just("\""),
        Just("\\"),
        Just(":"),
        Just(","),
        Just(" "),
        Just("\"__metadata__\""),
        Just("\"modelspec.architecture\""),
        Just("\"ss_base_model_version\""),
        Just("\"sdxl\""),
        Just("\"\\u0061\""),
        Just("12"),
        Just("-1.5e3"),
        Just("true"),
        Just("null"),
    ];
    proptest::collection::vec(fragment, 0..64).prop_map(|f| {
        let body = f.concat();
        if body.starts_with('{') {
            body
        } else {
            format!("{{{body}")
        }
    })
}

fn family_within_cap(r: &Result<Family, HeaderError>) -> bool {
    !matches!(r, Ok(Family::Known(f)) if f.is_empty() || f.len() > 64 * 1024)
}

proptest! {
    #![proptest_config(common::config())]

    #[test] // spec: QA-050, CORE-164
    fn fragment_headers_never_panic(json in json_fragments()) {
        let result = st(&safetensors(&json));
        prop_assert!(family_within_cap(&result));
        if result.is_ok() {
            prop_assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok(), "{}", json);
        }
    }

    #[test] // spec: QA-050, CORE-164
    fn truncated_or_random_headers_never_panic(
        cut in 0usize..2000,
        flips in proptest::collection::vec((0usize..2000, any::<u8>()), 0..8),
        random in proptest::collection::vec(any::<u8>(), 0..512),
    ) {
        let st_valid = safetensors(&format!(
            r#"{{{TENSOR},"__metadata__":{{"modelspec.architecture":"sdxl"}}}}"#
        ));
        for base in [st_valid, typical(3).bytes(), typical(2).bytes()] {
            let mut bytes = base.clone();
            for (at, b) in &flips {
                let n = bytes.len();
                bytes[at % n] = *b;
            }
            for data in [&base[..cut.min(base.len())], &bytes[..], &bytes[..cut.min(bytes.len())]] {
                prop_assert!(family_within_cap(&st(data)));
                prop_assert!(family_within_cap(&gg(data)));
            }
        }
        prop_assert!(family_within_cap(&st(&random)));
        prop_assert!(family_within_cap(&gg(&random)));
        let mut gguf_random = b"GGUF\x03\0\0\0".to_vec();
        gguf_random.extend_from_slice(&random);
        prop_assert!(family_within_cap(&gg(&gguf_random)));
        let mut st_random = (random.len() as u64).to_le_bytes().to_vec();
        st_random.extend_from_slice(&random);
        prop_assert!(family_within_cap(&st(&st_random)));
    }
}
