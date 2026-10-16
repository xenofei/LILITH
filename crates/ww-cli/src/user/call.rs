//! `ww call <tool> [<JSON object> | -] [--idempotency-key <k>]` (ARCH-016): prints the
//! `Outcome` as pretty JSON. Exit 0 for `ok`, 1 for `error`, 4 for `pending_confirmation`.
//! Without a key, `ww-client` mints one, so the call runs once.

use std::io::Read as _;

use serde_json::{Map, Value};
use ww_protocol::Outcome;

use super::{
    EXIT_ERROR_RESULT, EXIT_OK, EXIT_PENDING, Exit, GlobalArgs, cannot_run, connect, print_line,
};

pub(super) fn run(
    global: &GlobalArgs,
    tool: &str,
    args: Option<&str>,
    key: Option<String>,
) -> Result<i32, Exit> {
    // Checked before any core is involved: a usage error never starts one.
    let args = arguments(args)?;
    let Some(client) = connect(global)? else {
        return Ok(EXIT_OK);
    };
    let outcome = client.call_tool(tool, args, key).map_err(cannot_run)?;
    let text = serde_json::to_string_pretty(&outcome)
        .map_err(|e| cannot_run(format!("Cannot print the core's answer: {e}")))?;
    let _ = print_line(&text);
    Ok(match outcome {
        Outcome::Ok(_) => EXIT_OK,
        Outcome::Error { .. } => EXIT_ERROR_RESULT,
        Outcome::PendingConfirmation(_) => EXIT_PENDING,
    })
}

/// The tool's arguments: `{}` when none are given, stdin for `-`, else the text itself. It
/// must be a JSON object.
fn arguments(raw: Option<&str>) -> Result<Map<String, Value>, Exit> {
    let text = match raw {
        None => return Ok(Map::new()),
        Some("-") => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| cannot_run(format!("Cannot read the arguments from stdin: {e}")))?;
            text
        }
        Some(text) => text.to_string(),
    };
    parse_object(&text).map_err(cannot_run)
}

/// `text` as a JSON object, or a plain message saying why not.
fn parse_object(text: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(other) => Err(format!(
            "The arguments must be a JSON object, like {{\"text\":\"hi\"}}, not {}.",
            kind(&other)
        )),
        Err(e) => Err(format!(
            "The arguments are not valid JSON ({e}); give a JSON object, like {{\"text\":\"hi\"}}."
        )),
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-016
    fn arguments_must_be_a_json_object() {
        let map = parse_object("{\"text\":\"Ünïcödé space\"}").unwrap();
        assert_eq!(map["text"], "Ünïcödé space");
        assert!(parse_object(" {} \n").unwrap().is_empty());
        for (text, said) in [
            ("[1]", "a list"),
            ("\"x\"", "a string"),
            ("3", "a number"),
            ("null", "null"),
            ("true", "true or false"),
        ] {
            let err = parse_object(text).unwrap_err();
            assert!(err.contains("JSON object"), "{text}: {err}");
            assert!(err.contains(said), "{text}: {err}");
        }
        for bad in ["", "{", "{text: 1}"] {
            let err = parse_object(bad).unwrap_err();
            assert!(err.contains("not valid JSON"), "{bad}: {err}");
        }
        assert!(arguments(None).unwrap().is_empty());
    }
}
