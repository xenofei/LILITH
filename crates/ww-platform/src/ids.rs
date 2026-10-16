//! Identifier generation behind a trait so tests get predictable ids (QA-031). `ww-testkit`
//! supplies `SeqIds`. Only `ww-platform` depends on `getrandom` (ARCH-111).

use std::fmt::Write as _;

/// Makes ids such as session ids, trace ids and idempotency keys.
pub trait IdSource: Send + Sync + std::fmt::Debug {
    /// `<prefix>_<16 lowercase hex>`.
    fn id(&self, prefix: &str) -> String;
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
}
