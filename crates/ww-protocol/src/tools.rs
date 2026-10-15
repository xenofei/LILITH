//! Tool descriptors and the `tools/*` params (ARCH-037, ARCH-051).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A tool's effect class (ARCH-051): `read | write | destructive | exec` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EffectClass {
    Read,
    Write,
    Destructive,
    Exec,
}

impl EffectClass {
    /// Agent calls of `destructive` and `exec` tools need your confirmation.
    pub fn needs_agent_confirmation(self) -> bool {
        matches!(self, EffectClass::Destructive | EffectClass::Exec)
    }
}

/// Longest tool description, in characters (ARCH-037).
pub const MAX_DESCRIPTION_CHARS: usize = 2000;

/// One tool's descriptor (ARCH-037). The schemas are JSON Schema 2020-12 from `schemars`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub effect: EffectClass,
    pub module: String,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub annotations: BTreeMap<String, serde_json::Value>,
}

/// `tools/list` params: none.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolsListParams {}

/// `tools/list` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDescriptor>,
}

/// `tools/call` params. The result is `Outcome<ToolOutput>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolCallParams {
    pub name: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test] // spec: ARCH-051
    fn effect_class_wire_strings() {
        for (e, w) in [
            (EffectClass::Read, "read"),
            (EffectClass::Write, "write"),
            (EffectClass::Destructive, "destructive"),
            (EffectClass::Exec, "exec"),
        ] {
            assert_eq!(serde_json::to_value(e).unwrap(), json!(w));
            assert_eq!(serde_json::from_value::<EffectClass>(json!(w)).unwrap(), e);
        }
        assert!(serde_json::from_value::<EffectClass>(json!("Read")).is_err());
        assert!(serde_json::from_value::<EffectClass>(json!("delete")).is_err());
        assert_eq!(MAX_DESCRIPTION_CHARS, 2000);

        let call: ToolCallParams =
            serde_json::from_value(json!({"name": "test.echo", "arguments": {"text": "x"}}))
                .unwrap();
        assert_eq!(call.arguments["text"], "x");
        assert!(call.idempotency_key.is_none() && call.trace_id.is_none());
        assert!(
            serde_json::from_value::<ToolCallParams>(
                json!({"name": "test.echo", "arguments": {}, "extra": 1})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<ToolsListParams>(json!({"extra": 1})).is_err());
        serde_json::from_value::<ToolsListParams>(json!({})).unwrap();
    }

    #[test] // spec: ARCH-051
    fn needs_agent_confirmation_is_destructive_or_exec() {
        assert!(!EffectClass::Read.needs_agent_confirmation());
        assert!(!EffectClass::Write.needs_agent_confirmation());
        assert!(EffectClass::Destructive.needs_agent_confirmation());
        assert!(EffectClass::Exec.needs_agent_confirmation());
    }
}
