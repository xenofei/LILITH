//! `SeqIds`: predictable ids for in-process tests (QA-031).

use std::sync::atomic::{AtomicU64, Ordering};

use ww_platform::IdSource;

/// Ids `<prefix>_<16-digit zero-padded counter>`, counting from 1 per instance and shared by
/// every prefix. Decimal digits are valid lowercase hex, so the ids keep the `IdSource` shape.
/// UUIDs share the same counter: `00000000-0000-4000-8000-<12 hex digits of n>`, a valid
/// version 4, variant `8` shape.
#[derive(Debug, Default)]
pub struct SeqIds {
    last: AtomicU64,
}

impl SeqIds {
    pub fn new() -> SeqIds {
        SeqIds::default()
    }
}

impl IdSource for SeqIds {
    fn id(&self, prefix: &str) -> String {
        let n = self.last.fetch_add(1, Ordering::SeqCst) + 1;
        format!("{prefix}_{n:016}")
    }

    fn uuid(&self) -> String {
        let n = self.last.fetch_add(1, Ordering::SeqCst) + 1;
        format!("00000000-0000-4000-8000-{n:012x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ww_platform::IdSource;

    #[test] // spec: QA-031
    fn seq_ids_are_deterministic() {
        let a = SeqIds::new();
        let b = SeqIds::new();
        assert_eq!(a.id("sess"), "sess_0000000000000001");
        assert_eq!(a.id("trace"), "trace_0000000000000002");
        assert_eq!(b.id("sess"), "sess_0000000000000001");
        assert_eq!(a.id("sess"), "sess_0000000000000003");
        let hex = a.id("k").strip_prefix("k_").unwrap().to_string();
        assert_eq!(hex.len(), 16);
    }

    #[test] // spec: QA-031, CORE-047
    fn seq_uuids_are_deterministic_v4_shapes() {
        let ids = SeqIds::new();
        assert_eq!(ids.uuid(), "00000000-0000-4000-8000-000000000001");
        assert_eq!(ids.id("k"), "k_0000000000000002");
        assert_eq!(ids.uuid(), "00000000-0000-4000-8000-000000000003");
    }
}
