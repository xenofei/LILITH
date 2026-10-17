//! Identifier generation behind a trait so tests get predictable ids (QA-031). `ww-testkit`
//! supplies `SeqIds`. Only `ww-platform` depends on `getrandom` (ARCH-111).

use std::fmt::Write as _;

/// Makes ids such as session ids, trace ids and idempotency keys.
pub trait IdSource: Send + Sync + std::fmt::Debug {
    /// `<prefix>_<16 lowercase hex>`.
    fn id(&self, prefix: &str) -> String;

    /// A UUID version 4 (RFC 9562), lowercase and hyphenated: `xxxxxxxx-xxxx-4xxx-Nxxx-…`
    /// with `N` one of `8`–`b`. Manifests carry it as `creation_uuid` (CORE-047).
    fn uuid(&self) -> String;
}

/// Ids from the operating system's random number generator.
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomIds;

impl IdSource for RandomIds {
    fn id(&self, prefix: &str) -> String {
        let mut bytes = [0u8; 8];
        // The OS generator failing leaves no safe way to make an id; ids must never repeat.
        getrandom::fill(&mut bytes).expect("the operating system random generator failed");
        let mut id = String::with_capacity(prefix.len() + 17);
        id.push_str(prefix);
        id.push('_');
        for b in bytes {
            let _ = write!(id, "{b:02x}");
        }
        id
    }

    fn uuid(&self) -> String {
        let mut bytes = [0u8; 16];
        // As for `id`: a repeated UUID would merge two assets' identities.
        getrandom::fill(&mut bytes).expect("the operating system random generator failed");
        format_uuid_v4(bytes)
    }
}

/// Sets the version (4) and variant (`10xx`) bits of `bytes` and formats them as
/// `8-4-4-4-12` lowercase hex.
fn format_uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let mut out = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: QA-031
    fn random_ids_have_prefix_and_16_hex() {
        let ids = RandomIds;
        let a = ids.id("sess");
        let b = ids.id("sess");
        for id in [&a, &b] {
            let hex = id.strip_prefix("sess_").expect("prefix");
            assert_eq!(hex.len(), 16, "{id}");
            assert!(
                hex.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')),
                "{id}"
            );
        }
        assert_ne!(a, b);
    }

    /// Checks the RFC 9562 shape of a version 4 UUID.
    fn assert_v4(u: &str) {
        let groups: Vec<&str> = u.split('-').collect();
        assert_eq!(
            groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12],
            "{u}"
        );
        assert!(
            u.bytes()
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f' | b'-')),
            "{u}"
        );
        assert_eq!(&groups[2][..1], "4", "version nibble: {u}");
        assert!(
            matches!(&groups[3][..1], "8" | "9" | "a" | "b"),
            "variant: {u}"
        );
    }

    #[test] // spec: CORE-047
    fn random_uuid_is_v4() {
        let ids = RandomIds;
        let a = ids.uuid();
        let b = ids.uuid();
        assert_v4(&a);
        assert_v4(&b);
        assert_ne!(a, b);
        // The bit setting is right whatever the random bytes are.
        assert_eq!(
            format_uuid_v4([0xFF; 16]),
            "ffffffff-ffff-4fff-bfff-ffffffffffff"
        );
        assert_eq!(
            format_uuid_v4([0; 16]),
            "00000000-0000-4000-8000-000000000000"
        );
    }
}
