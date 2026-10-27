//! The automatic retry after a confirmation (APP-029): what an app's own call that returned
//! `pending_confirmation` does next.
//!
//! [`PendingCall`] watches one confirmation through the `confirmations` topic, and through a
//! snapshot of `app/confirmations.list` when the topic's events may have been missed (a
//! `resync_required`, or a reconnect). It answers each step with a [`PendingStep`]:
//! - approved: [`PendingStep::Resend`] with the call's retry token, **once**, however many ways
//!   the answer is learnt (an event and a snapshot, or the event replayed after a reconnect);
//! - denied: [`PendingStep::Settle`] with `confirm.denied`; expired, or cancelled by the core (a
//!   workspace switch, the app's process gone): `confirm.expired`, which is what the core
//!   answers a retry of a cancelled confirmation with. Neither is sent again;
//! - anything else: [`PendingStep::Wait`]. The app never retries without an answer.
//!
//! When the app closes first ([`PendingCall::on_close`]) the call is cancelled. A settled or
//! resent call answers [`PendingStep::Wait`] to everything after.
//!
//! Qt-free: the bridge's runtime feeds it and acts on its steps.

use lil_protocol::codes::{CONFIRM_DENIED, CONFIRM_EXPIRED};
use lil_protocol::{CodeSpec, ErrorBody, EventKind, EventParams, Outcome};
use serde_json::Value;

/// The topic confirmation events come on.
pub const CONFIRMATIONS: &str = "confirmations";

/// What to do next with a pending call.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingStep {
    /// Nothing yet.
    Wait,
    /// The confirmation was approved: send the call again with this idempotency key.
    Resend { retry_token: String },
    /// The call is over, with this outcome (the wire form of an `Outcome` error).
    Settle(Value),
}

/// One call waiting on its confirmation (module docs).
#[derive(Debug, Clone)]
pub struct PendingCall {
    confirmation_id: String,
    retry_token: String,
    /// The call's trace id, carried by the errors it settles with (ARCH-118).
    trace_id: Option<String>,
    done: bool,
}

impl PendingCall {
    pub fn new(confirmation_id: String, retry_token: String) -> PendingCall {
        PendingCall {
            confirmation_id,
            retry_token,
            trace_id: None,
            done: false,
        }
    }

    /// The errors this call settles with carry `trace_id`, the call's own (ARCH-118); without
    /// it, a fresh one.
    pub fn traced(mut self, trace_id: String) -> PendingCall {
        self.trace_id = Some(trace_id);
        self
    }

    /// The confirmation this call waits on.
    pub fn confirmation_id(&self) -> &str {
        &self.confirmation_id
    }

    /// Whether `ev` means the `confirmations` topic's events may have been missed, so the
    /// caller should read `app/confirmations.list` and call [`PendingCall::on_snapshot`].
    pub fn wants_snapshot(ev: &EventParams) -> bool {
        ev.topic == CONFIRMATIONS && ev.kind != EventKind::Event
    }

    /// An event of the `confirmations` topic (others are ignored).
    pub fn on_event(&mut self, ev: &EventParams) -> PendingStep {
        if self.done || ev.topic != CONFIRMATIONS || ev.kind != EventKind::Event {
            return PendingStep::Wait;
        }
        let confirmation = &ev.data["confirmation"];
        if confirmation["id"].as_str() != Some(self.confirmation_id.as_str()) {
            return PendingStep::Wait;
        }
        let state = confirmation["state"].as_str().unwrap_or_default();
        match ev.data["kind"].as_str() {
            Some("answered") => self.answered(state),
            Some("expired") => self.answered("expired"),
            Some("cancelled") => self.answered("cancelled"),
            _ => PendingStep::Wait,
        }
    }

    /// A snapshot of the confirmations: the ids still pending, and the answered ones with
    /// their state (`approved`, `denied`, `expired` or `cancelled`).
    pub fn on_snapshot(
        &mut self,
        pending_ids: &[String],
        answered: &[(String, String)],
    ) -> PendingStep {
        if self.done || pending_ids.contains(&self.confirmation_id) {
            return PendingStep::Wait;
        }
        match answered.iter().find(|(id, _)| *id == self.confirmation_id) {
            Some((_, state)) => self.answered(state),
            // Not listed (pruned, or another workspace's): keep waiting, never guess.
            None => PendingStep::Wait,
        }
    }

    /// The app closes before an answer: the call is cancelled.
    pub fn on_close(&mut self) -> PendingStep {
        if self.done {
            return PendingStep::Wait;
        }
        self.done = true;
        self.settle(
            &CONFIRM_EXPIRED,
            "The app closed before the confirmation was answered, so the call wasn't made.",
        )
    }

    fn answered(&mut self, state: &str) -> PendingStep {
        let step = match state {
            "approved" => PendingStep::Resend {
                retry_token: self.retry_token.clone(),
            },
            "denied" => self.settle(&CONFIRM_DENIED, "The confirmation was denied."),
            "expired" => self.settle(
                &CONFIRM_EXPIRED,
                "The confirmation expired before it was answered.",
            ),
            "cancelled" => self.settle(
                &CONFIRM_EXPIRED,
                "The confirmation was cancelled before it was answered.",
            ),
            _ => return PendingStep::Wait,
        };
        self.done = true;
        step
    }
}

impl PendingCall {
    fn settle(&self, code: &CodeSpec, message: &str) -> PendingStep {
        let trace = self
            .trace_id
            .clone()
            .unwrap_or_else(|| lil_client::mint_id("t"));
        let error = ErrorBody::new(code, message, trace);
        let outcome: Outcome<Value> = Outcome::Error { error };
        PendingStep::Settle(serde_json::to_value(outcome).expect("an outcome serialises"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call() -> PendingCall {
        PendingCall::new("cf_0000000000000001".into(), "ik_00000000000000a1".into())
    }

    fn change(kind: &str, id: &str, state: &str) -> EventParams {
        EventParams {
            topic: CONFIRMATIONS.into(),
            seq: 1,
            kind: EventKind::Event,
            data: json!({"kind": kind, "confirmation": {"id": id, "state": state}}),
        }
    }

    fn resend() -> PendingStep {
        PendingStep::Resend {
            retry_token: "ik_00000000000000a1".into(),
        }
    }

    fn settled_code(step: &PendingStep) -> String {
        match step {
            PendingStep::Settle(v) => {
                assert_eq!(v["status"], "error", "{v}");
                v["error"]["code"].as_str().unwrap().to_string()
            }
            other => panic!("expected a settle, got {other:?}"),
        }
    }

    #[test] // spec: APP-029
    fn pending_call_resends_once_after_approval() {
        let mut p = call();
        let me = "cf_0000000000000001";
        assert_eq!(
            p.on_event(&change("pending", me, "pending")),
            PendingStep::Wait
        );
        // Another confirmation's answer is not this call's.
        assert_eq!(
            p.on_event(&change("answered", "cf_0000000000000002", "approved")),
            PendingStep::Wait
        );
        // Another topic is ignored.
        let mut other = change("answered", me, "approved");
        other.topic = "jobs".into();
        assert_eq!(p.on_event(&other), PendingStep::Wait);
        assert_eq!(p.on_event(&change("answered", me, "approved")), resend());
        // The same answer again (a replay) is not a second resend.
        assert_eq!(
            p.on_event(&change("answered", me, "approved")),
            PendingStep::Wait
        );
        assert_eq!(
            p.on_snapshot(&[], &[(me.into(), "approved".into())]),
            PendingStep::Wait
        );
    }

    #[test] // spec: APP-029
    fn pending_call_settles_denied_and_expired_without_resending() {
        let me = "cf_0000000000000001";
        let mut denied = call();
        let step = denied.on_event(&change("answered", me, "denied"));
        assert_eq!(settled_code(&step), "confirm.denied");
        assert_eq!(
            denied.on_event(&change("answered", me, "approved")),
            PendingStep::Wait
        );

        let mut expired = call();
        assert_eq!(
            settled_code(&expired.on_event(&change("expired", me, "expired"))),
            "confirm.expired"
        );
        assert_eq!(expired.on_close(), PendingStep::Wait, "already settled");

        // The core cancelled it (a switch, the app's process gone): as the core answers a retry.
        let mut cancelled = call();
        assert_eq!(
            settled_code(&cancelled.on_event(&change("cancelled", me, "cancelled"))),
            "confirm.expired"
        );

        // The same from a snapshot.
        let mut from_list = call();
        assert_eq!(
            settled_code(&from_list.on_snapshot(&[], &[(me.into(), "denied".into())])),
            "confirm.denied"
        );
    }

    /// Review Focus 1: the answered event is missed during a reconnect. The snapshot taken on
    /// the topic's `resync_required` shows it answered: one resend. The event, replayed or late,
    /// then makes no second one.
    #[test] // spec: APP-029, ARCH-100
    fn pending_call_retries_once_after_an_answer_across_a_reconnect() {
        let me = "cf_0000000000000001";
        let mut p = call();
        assert_eq!(
            p.on_event(&change("pending", me, "pending")),
            PendingStep::Wait
        );
        let resync = EventParams {
            topic: CONFIRMATIONS.into(),
            seq: 0,
            kind: EventKind::ResyncRequired,
            data: Value::Null,
        };
        assert!(PendingCall::wants_snapshot(&resync));
        assert_eq!(p.on_event(&resync), PendingStep::Wait);
        assert!(!PendingCall::wants_snapshot(&change(
            "pending", me, "pending"
        )));
        // Still pending in a first snapshot: wait.
        assert_eq!(p.on_snapshot(&[me.into()], &[]), PendingStep::Wait);
        // Not listed at all: wait, never guess.
        assert_eq!(p.on_snapshot(&[], &[]), PendingStep::Wait);
        // Answered while the app was away.
        let answered = [
            ("cf_0000000000000002".to_string(), "denied".to_string()),
            (me.to_string(), "approved".to_string()),
        ];
        assert_eq!(p.on_snapshot(&[], &answered), resend());
        assert_eq!(
            p.on_event(&change("answered", me, "approved")),
            PendingStep::Wait
        );
        assert_eq!(p.on_snapshot(&[], &answered), PendingStep::Wait);
    }

    /// ARCH-118: a settled call's error carries the call's own trace id.
    #[test] // spec: APP-029, ARCH-118
    fn a_settled_call_keeps_its_trace_id() {
        let me = "cf_0000000000000001";
        let mut p = call().traced("tr_00000000000000aa".into());
        match p.on_event(&change("answered", me, "denied")) {
            PendingStep::Settle(v) => assert_eq!(v["error"]["trace_id"], "tr_00000000000000aa"),
            other => panic!("expected a settle, got {other:?}"),
        }
        match call().traced("tr_00000000000000bb".into()).on_close() {
            PendingStep::Settle(v) => assert_eq!(v["error"]["trace_id"], "tr_00000000000000bb"),
            other => panic!("expected a settle, got {other:?}"),
        }
    }

    #[test] // spec: APP-029
    fn pending_call_is_cancelled_when_the_app_closes() {
        let me = "cf_0000000000000001";
        let mut p = call();
        assert_eq!(settled_code(&p.on_close()), "confirm.expired");
        // Nothing after the close.
        assert_eq!(
            p.on_event(&change("answered", me, "approved")),
            PendingStep::Wait
        );
        assert_eq!(p.on_close(), PendingStep::Wait);
    }
}
