//! QML's topic subscriptions (APP-028; review I-1, re-review R-M1): a count per topic, and
//! whether the session holds the topic.
//!
//! - The session subscribes a topic for its first QML subscriber and unsubscribes it after the
//!   last ([`Topics::subscribe`], [`Topics::unsubscribe`] say when).
//! - **A failed subscribe** (lil-client drops the topic on `Timeout`, `Refused` or `Protocol`)
//!   leaves the topic counted but not held ([`Topics::failed`]). The next subscriber of that
//!   topic sends the subscribe again, and so does the next connection ([`Topics::unheld`]),
//!   until one is answered. A failure reported for an older subscribe (the topic was left and
//!   followed again since) changes nothing: each subscribe carries its generation.
//!
//! Qt-free: the bridge's runtime keeps one, and sends what it says to the session in order.

use std::collections::BTreeMap;

/// One followed topic.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    count: usize,
    /// Whether the session holds it, or has been asked to by the latest subscribe.
    held: bool,
    /// The generation of the latest subscribe sent for it.
    generation: u64,
}

/// The topics QML follows (module docs).
#[derive(Debug, Default)]
pub struct Topics {
    entries: BTreeMap<String, Entry>,
    next_generation: u64,
}

impl Topics {
    pub fn new() -> Topics {
        Topics::default()
    }

    /// One more subscriber of `topic`: the generation of a subscribe to send, when the session
    /// doesn't hold it (the first subscriber, or one after a failure).
    pub fn subscribe(&mut self, topic: &str) -> Option<u64> {
        self.next_generation += 1;
        let generation = self.next_generation;
        let entry = self.entries.entry(topic.to_string()).or_insert(Entry {
            count: 0,
            held: false,
            generation,
        });
        entry.count += 1;
        if entry.held {
            return None;
        }
        entry.held = true;
        entry.generation = generation;
        Some(generation)
    }

    /// One subscriber of `topic` fewer: `true` after the last, when the session should
    /// unsubscribe it. An unsubscribe of a topic not followed does nothing.
    pub fn unsubscribe(&mut self, topic: &str) -> bool {
        match self.entries.get_mut(topic) {
            Some(entry) if entry.count > 1 => {
                entry.count -= 1;
                false
            }
            Some(_) => {
                self.entries.remove(topic);
                true
            }
            None => false,
        }
    }

    /// The session's subscribe of generation `generation` failed: the topic, still counted, is
    /// no longer held, unless a newer subscribe was sent since.
    pub fn failed(&mut self, topic: &str, generation: u64) {
        if let Some(entry) = self.entries.get_mut(topic)
            && entry.generation == generation
        {
            entry.held = false;
        }
    }

    /// The topics followed but not held, each with the generation of a subscribe to send now
    /// (a new connection).
    pub fn unheld(&mut self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for (topic, entry) in &mut self.entries {
            if !entry.held {
                self.next_generation += 1;
                entry.held = true;
                entry.generation = self.next_generation;
                out.push((topic.clone(), entry.generation));
            }
        }
        out
    }

    /// Whether QML follows `topic`.
    pub fn follows(&self, topic: &str) -> bool {
        self.entries.contains_key(topic)
    }

    /// How many subscribers `topic` has.
    pub fn count(&self, topic: &str) -> usize {
        self.entries.get(topic).map_or(0, |e| e.count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-028
    fn the_first_subscriber_subscribes_and_the_last_unsubscribes() {
        let mut t = Topics::new();
        assert!(t.subscribe("jobs").is_some());
        assert!(t.subscribe("jobs").is_none(), "the second shares it");
        assert!(!t.unsubscribe("jobs"), "one is left");
        assert!(t.follows("jobs"));
        assert!(t.unsubscribe("jobs"), "the last");
        assert!(!t.follows("jobs"));
        assert!(
            !t.unsubscribe("jobs"),
            "an unpaired unsubscribe does nothing"
        );
        assert_eq!(t.count("jobs"), 0);
    }

    /// Re-review R-M1: a failed subscribe is sent again by the next subscriber, or on the next
    /// connection; a failure of an older subscribe changes nothing.
    #[test] // spec: APP-028
    fn a_failed_subscribe_is_sent_again() {
        let mut t = Topics::new();
        let first = t.subscribe("jobs").unwrap();
        t.failed("jobs", first);
        assert!(t.follows("jobs"), "still counted");
        let again = t
            .subscribe("jobs")
            .expect("the next subscriber sends it again");
        assert_eq!(t.count("jobs"), 2);
        assert!(t.subscribe("jobs").is_none(), "held again");
        t.failed("jobs", again);
        let resent = t.unheld();
        assert_eq!(resent.len(), 1);
        assert_eq!(resent[0].0, "jobs");
        assert!(t.unheld().is_empty(), "once");

        // Left and followed again: the old subscribe's failure is stale.
        let mut t = Topics::new();
        let old = t.subscribe("job:j1").unwrap();
        assert!(t.unsubscribe("job:j1"));
        let new = t.subscribe("job:j1").unwrap();
        assert_ne!(old, new);
        t.failed("job:j1", old);
        assert!(t.subscribe("job:j1").is_none(), "the new subscribe stands");
        assert!(t.unheld().is_empty());
        // A failure for a topic no longer followed is ignored.
        t.failed("gone", 1);
        assert!(!t.follows("gone"));
    }
}
