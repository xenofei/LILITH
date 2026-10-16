//! `SeqIds`: predictable ids for in-process tests (QA-031).

use std::sync::atomic::{AtomicU64, Ordering};

use ww_platform::IdSource;

/// Ids `<prefix>_<16-digit zero-padded counter>`, counting from 1 per instance and shared by
/// every prefix. Decimal digits are valid lowercase hex, so the ids keep the `IdSource` shape.
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
}
