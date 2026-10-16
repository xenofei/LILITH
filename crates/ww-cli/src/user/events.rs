//! `ww events --follow --topic <t>… [--since <topic>=<seq>]…` (ARCH-016, ARCH-032): one
//! `EventParams` JSON object per line, flushed per line, until the core goes away (exit 0).

use std::collections::BTreeMap;
use std::io;

use super::{EXIT_OK, Exit, GlobalArgs, cannot_run, connect, print_line};

pub(super) fn run(global: &GlobalArgs, topics: Vec<String>, since: &[String]) -> Result<i32, Exit> {
    let since = cursors(&topics, since).map_err(cannot_run)?;
    let Some(client) = connect(global)? else {
        return Ok(EXIT_OK);
    };
    let events = client.subscribe(topics, since).map_err(cannot_run)?;
    for event in events {
        let line = serde_json::to_string(&event)
            .map_err(|e| cannot_run(format!("Cannot print an event: {e}")))?;
        match print_line(&line) {
            Ok(()) => {}
            // Whoever read the events has gone: nothing more to do.
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(EXIT_OK),
            Err(e) => return Err(cannot_run(format!("Cannot print an event: {e}"))),
        }
    }
    Ok(EXIT_OK)
}

/// The `--since TOPIC=SEQ` cursors. The sequence number is after the last `=` (a topic's
/// suffix may contain one), and each topic must be one of `--topic`.
fn cursors(topics: &[String], since: &[String]) -> Result<BTreeMap<String, u64>, String> {
    let mut out = BTreeMap::new();
    for entry in since {
        let parsed = entry
            .rsplit_once('=')
            .and_then(|(topic, seq)| Some((topic, seq.parse::<u64>().ok()?)))
            .filter(|(topic, _)| !topic.is_empty());
        let Some((topic, seq)) = parsed else {
            return Err(format!(
                "--since {entry} is not TOPIC=SEQ (for example test:a=0)."
            ));
        };
        if !topics.iter().any(|t| t == topic) {
            return Err(format!(
                "--since {entry} names {topic}, which no --topic follows."
            ));
        }
        out.insert(topic.to_string(), seq);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-032
    fn since_cursors_are_topic_equals_seq() {
        let topics = ["test:a".to_string(), "test:x=y".to_string()];
        let got = cursors(&topics, &["test:a=0".into(), "test:x=y=17".into()]).unwrap();
        assert_eq!(
            got,
            BTreeMap::from([("test:a".to_string(), 0), ("test:x=y".to_string(), 17)])
        );
        assert!(cursors(&topics, &[]).unwrap().is_empty());
        for bad in ["test:a", "test:a=", "test:a=-1", "test:a=x", "=3"] {
            let err = cursors(&topics, &[bad.to_string()]).unwrap_err();
            assert!(err.contains("TOPIC=SEQ"), "{bad}: {err}");
        }
        let err = cursors(&topics, &["test:b=1".into()]).unwrap_err();
        assert!(err.contains("no --topic"), "{err}");
    }
}
