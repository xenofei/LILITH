//! The named mutex behind `lil dev desktop-tests --host`'s single-run lock (Task 30).

use lil_platform::named_mutex::{Taken, try_lock};

/// A name of this test's own, so a real host run's lock is never touched.
fn name(what: &str) -> String {
    format!(
        r"Local\LILITH-test-named-mutex-{what}-{}",
        std::process::id()
    )
}

#[test] // spec: QA-042
fn a_second_contender_is_refused_until_the_first_lets_go() {
    let name = name("contenders");
    let first = match try_lock(&name).unwrap() {
        Taken::Acquired(g) => g,
        other => panic!("the first contender takes it: {other:?}"),
    };
    // Another thread stands for another run (a process's thread would see the same).
    let second = {
        let name = name.clone();
        move || matches!(try_lock(&name).unwrap(), Taken::Busy)
    };
    assert!(
        std::thread::spawn(second.clone()).join().unwrap(),
        "busy while held"
    );
    drop(first);
    let taken = std::thread::spawn({
        let name = name.clone();
        move || matches!(try_lock(&name).unwrap(), Taken::Acquired(_))
    })
    .join()
    .unwrap();
    assert!(taken, "free once released");
}

#[test] // spec: QA-042
fn a_crashed_owner_leaves_it_abandoned_and_takeable() {
    let name = name("abandoned");
    std::thread::spawn({
        let name = name.clone();
        move || match try_lock(&name).unwrap() {
            // Never released: the thread ends owning it, as a killed run would.
            Taken::Acquired(g) => std::mem::forget(g),
            other => panic!("{other:?}"),
        }
    })
    .join()
    .unwrap();
    assert!(
        matches!(try_lock(&name).unwrap(), Taken::Abandoned(_)),
        "Windows hands it to the next taker as abandoned"
    );
}
