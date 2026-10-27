//! Request ids (APP-028): each `Core.call`, `Core.appCall` and `Core.setWorkspace` returns a
//! string id at once, and its outcome comes later as a signal carrying that id. QML never holds
//! a request object, only the id.
//!
//! [`Requests`] keeps the open ones with whatever the bridge needs to stop each (its task).
//! An id is open from [`Requests::open`] until [`Requests::finish`] takes it (its outcome is
//! delivered) or [`Requests::cancel`] drops it: an outcome that arrives for an id no longer open
//! is dropped, so a cancelled call never reaches its callback. Ids are `rq_<n>`, never reused
//! within a process.
//!
//! [`client_error`] is the error a call reports when the core couldn't be used: the core's own
//! errors keep their code; a core that can't be reached is `core.unreachable` (ruling P3-R25,
//! as `lil-mcp` reports it), with the client's plain-language message.
//!
//! Qt-free: the bridge's `Core` owns one, on the GUI thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use lil_client::ClientError;
use lil_protocol::ErrorBody;
use lil_protocol::codes::{CORE_UNREACHABLE, PROTOCOL_INCOMPATIBLE};
use serde_json::json;

/// The next request number, for the whole process: ids stay unique across `Core` objects.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// The open requests and their handles (module docs).
#[derive(Debug)]
pub struct Requests<T> {
    open: HashMap<String, T>,
}

impl<T> Default for Requests<T> {
    fn default() -> Self {
        Requests {
            open: HashMap::new(),
        }
    }
}

impl<T> Requests<T> {
    pub fn new() -> Requests<T> {
        Requests::default()
    }

    /// A fresh id, open from now on.
    pub fn open(&mut self, handle: T) -> String {
        let id = format!("rq_{}", NEXT.fetch_add(1, Ordering::Relaxed));
        self.open.insert(id.clone(), handle);
        id
    }

    /// Replaces the handle of the open request `id`; `false` when it isn't open.
    pub fn set(&mut self, id: &str, handle: T) -> bool {
        match self.open.get_mut(id) {
            Some(slot) => {
                *slot = handle;
                true
            }
            None => false,
        }
    }

    /// Whether `id` is open.
    pub fn is_open(&self, id: &str) -> bool {
        self.open.contains_key(id)
    }

    /// Takes `id`'s handle to deliver its final outcome; `None` when it isn't open (cancelled,
    /// or already finished), and then the outcome is dropped.
    pub fn finish(&mut self, id: &str) -> Option<T> {
        self.open.remove(id)
    }

    /// Cancels `id`: its handle, to stop its work; `None` for an id that isn't open.
    pub fn cancel(&mut self, id: &str) -> Option<T> {
        self.open.remove(id)
    }

    /// Every open request's handle, closing them all (the app closes).
    pub fn drain(&mut self) -> Vec<(String, T)> {
        self.open.drain().collect()
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }
}

/// The error result for a call the core couldn't take or finish (module docs). `trace_id` is
/// the call's.
pub fn client_error(e: &ClientError, trace_id: &str) -> ErrorBody {
    match e {
        ClientError::Refused(error) | ClientError::UnknownWorkspace { error, .. } => error.clone(),
        ClientError::Incompatible { core_protocol } => {
            let mut error = ErrorBody::new(&PROTOCOL_INCOMPATIBLE, e.to_string(), trace_id);
            if !core_protocol.is_empty() {
                error.data = Some(json!({ "core_protocol": core_protocol }));
            }
            error
        }
        ClientError::NotConnected => ErrorBody::new(
            &CORE_UNREACHABLE,
            "The core isn't connected, so nothing was sent. Try again once it is.",
            trace_id,
        ),
        ClientError::Closed => ErrorBody::new(
            &CORE_UNREACHABLE,
            "The connection to the core closed while the call was under way. It may have run.",
            trace_id,
        ),
        other => ErrorBody::new(&CORE_UNREACHABLE, other.to_string(), trace_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-028
    fn cancel_of_an_unknown_id_is_false() {
        let mut r: Requests<u32> = Requests::new();
        assert!(r.cancel("rq_404").is_none(), "an id never opened");
        let id = r.open(7);
        assert!(id.starts_with("rq_"), "{id}");
        assert!(r.is_open(&id));
        assert_eq!(r.cancel(&id), Some(7));
        assert!(r.cancel(&id).is_none(), "cancelled already");
        // Its outcome, arriving late, is dropped.
        assert!(r.finish(&id).is_none());
    }

    #[test] // spec: APP-028
    fn ids_are_unique_and_finish_once() {
        let mut a: Requests<()> = Requests::new();
        let mut b: Requests<()> = Requests::new();
        let (x, y) = (a.open(()), b.open(()));
        assert_ne!(x, y, "unique across objects too");
        assert!(a.finish(&x).is_some());
        assert!(a.finish(&x).is_none(), "a second outcome is dropped");
        assert!(!a.set(&x, ()), "a finished id takes no handle");
        assert_eq!((a.len(), b.len()), (0, 1));
        assert_eq!(b.drain().len(), 1);
        assert!(b.is_empty());
    }

    #[test] // spec: APP-028
    fn a_core_that_cannot_be_used_is_core_unreachable() {
        let e = client_error(&ClientError::NotConnected, "tr_0000000000000001");
        assert_eq!(e.code, "core.unreachable");
        assert_eq!(e.trace_id, "tr_0000000000000001");
        assert!(e.message.contains("nothing was sent"), "{}", e.message);
        let e = client_error(&ClientError::Closed, "tr_1");
        assert!(e.message.contains("may have run"), "{}", e.message);
        let refused = ErrorBody::new(
            &lil_protocol::codes::WORKSPACE_UNSUPPORTED,
            "not served",
            "t_1",
        );
        let e = client_error(&ClientError::Refused(refused), "tr_1");
        assert_eq!(e.code, "workspace.unsupported", "the core's own code");
        let e = client_error(
            &ClientError::Incompatible {
                core_protocol: "2.0".into(),
            },
            "tr_1",
        );
        assert_eq!(e.code, "protocol.incompatible");
        assert_eq!(e.data, Some(json!({"core_protocol": "2.0"})));
    }
}
