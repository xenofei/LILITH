//! Concurrent reservations never overshoot a cap (CORE-087, QA-047): 16 threads × 100
//! reservations against one real suite `state.db` in a temp folder, through two connections
//! (two `StateRegistry`s on the same file, so `BEGIN IMMEDIATE` really contends), mixing calls
//! in two workspaces and in none, one provider with a cap of its own, and settles in between.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lil_core::events::EventBus;
use lil_core::meter::{Caps, ReservationState, ReserveFor};
use lil_core::state::StateRegistry;
use lil_core::store::FailPoints;
use lil_core::tools::{BillingBasis, Estimate, ReserveOutcome};
use lil_platform::{Clock, IdSource};
use lil_protocol::{ErrorBody, codes};
use lil_schema::money::{Currency, Money};
use lil_testkit::{FakeClock, SeqIds, TestEnv};

const THREADS: u64 = 16;
const PER_THREAD: u64 = 100;
/// Tries per call in [`retried`].
const TRIES: u32 = 20;

/// `call`, tried again while it returns `state.unavailable`, which the error table makes
/// `transient` with retry `same_idempotency_key`: its caller retries it. Here it is SQLite's
/// `SQLITE_BUSY` ("database is locked"). The two connections contend for `BEGIN IMMEDIATE`, and
/// SQLite's busy handler is not fair: on a slow disk (a CI runner) one connection can wait past
/// the 5 s busy timeout while the other commits back to back. The core itself has one
/// connection per database (CORE-004), and its actor queues the calls, so the core's callers
/// never contend like this. Any other error fails the test. A refused `BEGIN IMMEDIATE` wrote
/// nothing, so a retry can't count twice, and the totals below would show it if it did.
async fn retried<T, F>(mut call: impl FnMut() -> F) -> T
where
    F: Future<Output = Result<T, ErrorBody>>,
{
    for _ in 1..TRIES {
        match call().await {
            Err(e) if e.code == codes::STATE_UNAVAILABLE.code => {}
            done => return done.unwrap(),
        }
    }
    call().await.unwrap()
}

fn caps_on(env: &TestEnv, clock: &Arc<FakeClock>, ids: &Arc<dyn IdSource>) -> Arc<Caps> {
    let states = Arc::new(
        StateRegistry::open(&env.env, Arc::new(|what: &str| panic!("fatal: {what}"))).unwrap(),
    );
    let clock: Arc<dyn Clock> = clock.clone();
    Caps::new(
        states,
        env.env.clone(),
        clock.clone(),
        Arc::new(EventBus::new(clock)),
        FailPoints::none(),
        ids.clone(),
    )
}

#[test] // spec: CORE-087, QA-047, ARCH-099
fn concurrent_reserves_never_overshoot() {
    let env = TestEnv::new();
    lil_testkit::files::write_file(
        &env.env.suite_toml(),
        b"[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"50\"\n\
          [caps.provider_monthly]\np1 = \"20\"\n",
    );
    let clock = FakeClock::new();
    let ids: Arc<dyn IdSource> = Arc::new(SeqIds::new());
    let connections = [caps_on(&env, &clock, &ids), caps_on(&env, &clock, &ids)];
    let reserved = AtomicU64::new(0);
    let reserved_p1 = AtomicU64::new(0);
    std::thread::scope(|s| {
        for t in 0..THREADS {
            let caps = connections[usize::try_from(t % 2).unwrap()].clone();
            let (reserved, reserved_p1) = (&reserved, &reserved_p1);
            s.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    let provider = if t % 4 == 0 { "p1" } else { "p2" };
                    let est = Estimate::External {
                        provider: provider.into(),
                        amount: Money::parse_decimal("0.1").unwrap(),
                        currency: Currency::parse("USD").unwrap(),
                        basis: BillingBasis::Payg,
                    };
                    let workspace = ["none", "ws_a", "ws_b"][usize::try_from(t % 3).unwrap()];
                    for i in 0..PER_THREAD {
                        let owner = format!("c_{t:08}{i:08}");
                        let who = ReserveFor {
                            owner: &owner,
                            workspace,
                        };
                        match retried(|| caps.reserve(&est, who, None)).await {
                            ReserveOutcome::Reserved(rv) => {
                                reserved.fetch_add(1, Ordering::SeqCst);
                                if provider == "p1" {
                                    reserved_p1.fetch_add(1, Ordering::SeqCst);
                                }
                                // Half are settled at once; the rest stay held.
                                if i % 2 == 0 {
                                    retried(|| caps.settle(&rv, None, true)).await;
                                }
                            }
                            ReserveOutcome::NeedsCap(hits) => assert!(!hits.is_empty()),
                            ReserveOutcome::None => panic!("payg money always reserves"),
                        }
                    }
                });
            });
        }
    });
    // 0.1 a reservation: at most 500 under the monthly cap, 200 for p1. Every refusal of a p2
    // call means the month was full, and there were far more attempts than room, so the room
    // is used exactly.
    assert_eq!(reserved.load(Ordering::SeqCst), 500);
    assert!(reserved_p1.load(Ordering::SeqCst) <= 200);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let caps = &connections[0];
        let view = caps.view().await.unwrap();
        let row = |cap: &str| view.caps.iter().find(|c| c.cap == cap).unwrap().clone();
        let monthly = row("monthly");
        let used = Money::parse_decimal(&monthly.settled)
            .unwrap()
            .checked_add(Money::parse_decimal(&monthly.held).unwrap())
            .unwrap();
        assert_eq!(used, Money::parse_decimal("50").unwrap(), "{monthly:?}");
        let p1 = row("provider:p1");
        let p1_used = Money::parse_decimal(&p1.settled)
            .unwrap()
            .checked_add(Money::parse_decimal(&p1.held).unwrap())
            .unwrap();
        assert!(p1_used <= Money::parse_decimal("20").unwrap(), "{p1:?}");
        // The rows agree with the totals: every one held or settled, from both connections.
        let rows = caps.list(&view.period).await.unwrap();
        assert_eq!(rows.len(), 500);
        let settled = rows
            .iter()
            .filter(|r| r.state == ReservationState::Settled)
            .count();
        assert_eq!(
            Money::from_micros(i64::try_from(settled).unwrap() * 100_000).to_decimal_string(),
            monthly.settled
        );
        for ws in ["none", "ws_a", "ws_b"] {
            assert!(rows.iter().any(|r| r.workspace == ws), "{ws}");
        }
    });
}
