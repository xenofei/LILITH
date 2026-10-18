//! Append-log lines (ARCH-081, CORE-009; Appendix E "Append logs").
//!
//! One JSON object per line, ending in `\n`, with `"machine"` and `"seq"` (from 1 per file)
//! first, then the record. Readers skip lines that don't parse and dedupe by
//! `(machine, seq)`, the first line winning.

use serde_json::{Map, Value};

/// One parsed line. `record` is the object without `machine` and `seq`.
#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    pub machine: String,
    pub seq: u64,
    pub record: Value,
}

/// The line for `record`: `{"machine":…,"seq":…,<record>}` and `\n`. Keys named `machine` or
/// `seq` inside `record` are left out, so the line never carries two.
pub fn encode_line(machine: &str, seq: u64, record: &Map<String, Value>) -> String {
    let mut line = Map::new();
    line.insert("machine".into(), Value::String(machine.into()));
    line.insert("seq".into(), Value::Number(seq.into()));
    for (k, v) in record {
        if k != "machine" && k != "seq" {
            line.insert(k.clone(), v.clone());
        }
    }
    let mut text = serde_json::to_string(&line).expect("a JSON map always serialises");
    text.push('\n');
    text
}

/// Every line of `text` that parses as an object with a string `machine` and an unsigned
/// `seq`, in file order, without repeats of a `(machine, seq)` already seen.
pub fn read_lines(text: &str) -> Vec<LogLine> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        let (Some(machine), Some(seq)) = (
            obj.get("machine")
                .and_then(Value::as_str)
                .map(str::to_string),
            obj.get("seq").and_then(Value::as_u64),
        ) else {
            continue;
        };
        if !seen.insert((machine.clone(), seq)) {
            continue;
        }
        obj.shift_remove("machine");
        obj.shift_remove("seq");
        out.push(LogLine {
            machine,
            seq,
            record: Value::Object(obj),
        });
    }
    out
}

/// The highest `seq` of `machine` in `text`, or 0.
pub fn last_seq(text: &str, machine: &str) -> u64 {
    read_lines(text)
        .iter()
        .filter(|l| l.machine == machine)
        .map(|l| l.seq)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test] // spec: ARCH-081
    fn log_lines_put_machine_and_seq_first() {
        let record = obj(json!({"job_id": "j_1", "kind": "generate", "label": "Ünïcödé space"}));
        let line = encode_line("testmachine", 7, &record);
        assert_eq!(
            line,
            "{\"machine\":\"testmachine\",\"seq\":7,\"job_id\":\"j_1\",\"kind\":\"generate\",\"label\":\"Ünïcödé space\"}\n"
        );
        assert_eq!(line.matches('\n').count(), 1);
        // A record that names machine or seq itself never produces a second one.
        let sneaky = obj(json!({"seq": 99, "a": 1, "machine": "other"}));
        assert_eq!(
            encode_line("m", 1, &sneaky),
            "{\"machine\":\"m\",\"seq\":1,\"a\":1}\n"
        );
        // A record with a newline in a string stays on one line.
        let multi = obj(json!({"text": "a\nb"}));
        assert_eq!(encode_line("m", 2, &multi).matches('\n').count(), 1);

        let read = read_lines(&line);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].machine, "testmachine");
        assert_eq!(read[0].seq, 7);
        assert_eq!(read[0].record, Value::Object(record));
    }

    #[test] // spec: ARCH-081, CORE-009
    fn reader_skips_a_torn_last_line_and_dedupes() {
        let a1 = encode_line("a", 1, &obj(json!({"n": 1})));
        let b1 = encode_line("b", 1, &obj(json!({"n": 2})));
        let a2 = encode_line("a", 2, &obj(json!({"n": 3})));
        let a1_again = encode_line("a", 1, &obj(json!({"n": 4})));
        let text = format!(
            "{a1}{b1}not json\n\n[1,2]\n{{\"machine\":\"a\"}}\n{{\"machine\":\"a\",\"seq\":-1}}\n{a2}{a1_again}{{\"machine\":\"a\",\"seq\":3,\"n\""
        );
        let lines = read_lines(&text);
        let got: Vec<(&str, u64, i64)> = lines
            .iter()
            .map(|l| (l.machine.as_str(), l.seq, l.record["n"].as_i64().unwrap()))
            .collect();
        assert_eq!(got, [("a", 1, 1), ("b", 1, 2), ("a", 2, 3)]);
        assert_eq!(last_seq(&text, "a"), 2);
        assert_eq!(last_seq(&text, "b"), 1);
        assert_eq!(last_seq(&text, "c"), 0);
        assert_eq!(last_seq("", "a"), 0);
        // A CRLF line still reads.
        let crlf = a1.replace('\n', "\r\n");
        assert_eq!(read_lines(&crlf).len(), 1);
    }
}
