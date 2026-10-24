//! Windows Hello for the reviewer key and the one-click merge flow (plan 4a Task 28: QA-088,
//! QA-020, QA-036; P4a-R23, P4a-R38).
//!
//! No real Windows Hello: every test injects a fake `HelloPlatform` (the Windows Hello seam), a
//! fake `Unlocker` or a fake `Authenticator`, and uses a temp home. The three modules under test
//! need no GitHub and no git, so this file compiles them in directly, as `hook_patterns.rs`
//! does: `reviewer_key` (the key file and its age encryption), `reviewer_hello` (enrol, status,
//! remove, the unlock order and the authenticator) and `merge_flow` (the flow's order and the
//! merge gate). `lil dev approve`, `lil dev merge` and `lil dev merge-flow` call these same
//! functions; their wiring (the real flow steps, the authenticator's construction, the gate
//! `lil dev merge` uses) has its own unit tests in `src/dev/reviewer.rs` and `src/dev/merge.rs`.

#[allow(dead_code, reason = "only part of the key module is used here")]
#[path = "../src/dev/reviewer_key.rs"]
mod reviewer_key;

#[allow(dead_code, reason = "the CLI wiring isn't used here")]
#[path = "../src/dev/reviewer_hello.rs"]
mod reviewer_hello;

#[allow(dead_code, reason = "the CLI wiring isn't used here")]
#[path = "../src/dev/merge_flow.rs"]
mod merge_flow;

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use merge_flow::{AskOnEveryMerge, Facts, MergeFlowSteps, MergeGate, merge_flow};
use reviewer_hello::{
    Authenticator, CREDENTIAL, HelloFile, HelloPlatform, KeyAuthenticator, NoTerminal,
    PassphraseUnlocker, QUOTE, Route, Unlocker, discard_enrolment_beside, enroll, enrolled_hello,
    hello_age_path_in, hello_file_path_in, presentation, remove, status, unlock_reviewer_key,
};
use reviewer_key::{KeyError, ReviewerKeyFile, encrypt_key_with_work_factor, key_path_in};
use zeroize::Zeroizing;

const PEM: &[u8] =
    b"-----BEGIN PRIVATE KEY-----\nnot a real key, for tests\n-----END PRIVATE KEY-----\n";
const PASS: &str = "correct horse battery";
/// Cheap scrypt for tests; real files use `WORK_FACTOR`.
const LOG_N: u8 = 10;
const CHALLENGE: [u8; 32] = [0x5a; 32];
const KEK: [u8; 32] = [0x11; 32];

/// A shared, ordered record of what happened, across fakes.
type Log = Rc<RefCell<Vec<String>>>;

fn log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

fn events(log: &Log) -> Vec<String> {
    log.borrow().clone()
}

// ---- a temp home with the passphrase key -------------------------------------------------------

struct Home {
    _tmp: tempfile::TempDir,
    profile: PathBuf,
}

impl Home {
    fn with_key() -> Self {
        let home = Self::empty();
        ReviewerKeyFile {
            app_id: 123,
            installation_id: 456,
            encrypted_pem: encrypt_key_with_work_factor(PEM, PASS, LOG_N).unwrap(),
        }
        .save(&key_path_in(&home.profile))
        .unwrap();
        home
    }

    fn empty() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        Self { _tmp: tmp, profile }
    }

    fn key(&self) -> ReviewerKeyFile {
        ReviewerKeyFile::load(&key_path_in(&self.profile)).unwrap()
    }

    fn key_bytes(&self) -> Vec<u8> {
        std::fs::read(key_path_in(&self.profile)).unwrap()
    }

    fn hello_files(&self) -> Vec<PathBuf> {
        [
            hello_file_path_in(&self.profile),
            hello_age_path_in(&self.profile),
        ]
        .into_iter()
        .filter(|p| p.exists())
        .collect()
    }
}

// ---- the Windows Hello seam's fake -------------------------------------------------------------

/// What the fake Windows Hello does when asked to prompt.
#[derive(Clone, Copy, PartialEq)]
enum Answer {
    Ok,
    Cancel,
    Fail,
    /// Its window couldn't be brought to the front (`lil_platform::hello::front_or_refuse`).
    NotInFront,
    /// Nobody answered in time (`lil_platform::hello::wait_bounded`).
    TimedOut,
}

/// What the owner clicks in the LILITH window that asks before Windows Hello (fix round 2).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Click {
    Accept,
    Deny,
    Close,
    /// Nothing within the 120 s.
    Nothing,
}

struct FakeHello {
    log: Log,
    supported: bool,
    tpm: bool,
    kek: [u8; 32],
    answer: Answer,
    click: Click,
    remove_fails: bool,
    /// What the window that owns each prompt showed.
    shown: RefCell<Vec<String>>,
}

impl FakeHello {
    fn new(log: &Log) -> Self {
        Self {
            log: log.clone(),
            supported: true,
            tpm: true,
            kek: KEK,
            answer: Answer::Ok,
            click: Click::Accept,
            remove_fails: false,
            shown: RefCell::default(),
        }
    }

    fn answering(log: &Log, answer: Answer) -> Self {
        Self {
            answer,
            ..Self::new(log)
        }
    }

    fn clicking(log: &Log, click: Click) -> Self {
        Self {
            click,
            ..Self::new(log)
        }
    }

    fn note(&self, what: String) {
        self.log.borrow_mut().push(what);
    }

    /// The window, as `lil_platform::hello::ask_then` runs it: in front first, then the click,
    /// and only after an accept click Windows Hello (logged as `hello:kek_derive`).
    fn window(&self, shown: &str, accept: Option<&str>) -> io::Result<()> {
        self.shown.borrow_mut().push(shown.to_string());
        if self.answer == Answer::NotInFront {
            return Err(io::Error::other(
                "the Windows Hello window couldn't be brought to the front of the other windows",
            ));
        }
        let Some(label) = accept else {
            return Ok(());
        };
        self.note(format!("window:{label}"));
        let why = match self.click {
            Click::Accept => return Ok(()),
            Click::Deny => "you chose Deny",
            Click::Close => "the LILITH window was closed",
            Click::Nothing => "nothing was chosen in the LILITH window in time",
        };
        self.note(format!("window:refused {:?}", self.click));
        Err(io::Error::new(io::ErrorKind::PermissionDenied, why))
    }

    fn prompt(&self) -> io::Result<()> {
        match self.answer {
            Answer::Ok => Ok(()),
            Answer::Cancel => Err(io::Error::new(io::ErrorKind::Interrupted, "UserCanceled")),
            Answer::Fail => Err(io::Error::other("SecurityDeviceLocked")),
            Answer::NotInFront => unreachable!("handled by `window`"),
            Answer::TimedOut => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the Windows Hello prompt wasn't answered within 120 s, so it was cancelled",
            )),
        }
    }
}

impl HelloPlatform for FakeHello {
    fn supported(&self) -> bool {
        self.supported
    }
    fn hardware_held(&self) -> bool {
        self.tpm
    }
    fn kek_enroll(&self, credential: &str, shown: &str) -> io::Result<()> {
        self.note(format!("hello:kek_enroll {credential}"));
        self.window(shown, None)?;
        self.prompt()
    }
    fn kek_derive(
        &self,
        credential: &str,
        challenge: &[u8; 32],
        shown: &str,
        accept: Option<&str>,
    ) -> io::Result<Zeroizing<[u8; 32]>> {
        self.window(shown, accept)?;
        self.note(format!(
            "hello:kek_derive {credential} {:02x}",
            challenge[0]
        ));
        self.prompt()?;
        Ok(Zeroizing::new(self.kek))
    }
    fn kek_remove(&self, credential: &str) -> io::Result<()> {
        self.note(format!("hello:kek_remove {credential}"));
        if self.remove_fails {
            return Err(io::Error::other("Windows Hello is switched off"));
        }
        Ok(())
    }
}

// ---- passphrase prompts --------------------------------------------------------------------------

/// A passphrase prompt that answers `answer` and logs that it was asked.
fn asks<'a>(log: &'a Log, answer: &'a str) -> impl Fn(&str) -> Zeroizing<String> + 'a {
    move |_label| {
        log.borrow_mut().push("passphrase:asked".into());
        Zeroizing::new(answer.to_string())
    }
}

fn never_asked(_label: &str) -> Zeroizing<String> {
    panic!("the passphrase was asked for")
}

fn quiet(_: &str) {}

fn as_is(text: &str) -> String {
    text.to_string()
}

fn enroll_kek(home: &Home, hello: &FakeHello) {
    let l = log();
    enroll(
        &home.profile,
        Route::Kek,
        hello,
        &asks(&l, PASS),
        CHALLENGE,
        LOG_N,
        &quiet,
    )
    .unwrap();
}

/// Unlocks as `lil dev approve` does: Hello first when enrolled, then the passphrase.
fn unlock_like_approve(
    home: &Home,
    hello: &FakeHello,
    ask: &dyn Fn(&str) -> Zeroizing<String>,
    said: &RefCell<Vec<String>>,
) -> Result<Zeroizing<Vec<u8>>, KeyError> {
    let say = |t: &str| said.borrow_mut().push(t.to_string());
    let key = home.key();
    let passphrase = PassphraseUnlocker::new(&key.encrypted_pem, ask);
    let enrolled = enrolled_hello(
        &home.profile,
        hello,
        "LILITH: approve PR #42",
        Some("Approve"),
        &say,
    );
    unlock_reviewer_key(
        enrolled.as_ref().map(|h| h as &dyn Unlocker),
        &passphrase,
        &say,
    )
}

// ---- the Step 1 tests ------------------------------------------------------------------------------

#[test] // spec: QA-088
fn enroll_needs_the_existing_passphrase_key() {
    // No passphrase key: nothing to enrol, and Windows Hello is never asked.
    let home = Home::empty();
    let l = log();
    let hello = FakeHello::new(&l);
    let e = enroll(
        &home.profile,
        Route::Kek,
        &hello,
        &never_asked,
        CHALLENGE,
        LOG_N,
        &quiet,
    )
    .unwrap_err();
    let e = format!("{e:#}");
    assert!(e.contains("reviewer-setup"), "{e}");
    assert!(home.hello_files().is_empty());
    assert!(events(&l).is_empty(), "{:?}", events(&l));

    // A wrong passphrase: nothing is written and Windows Hello is never asked.
    let home = Home::with_key();
    let before = home.key_bytes();
    let l = log();
    let hello = FakeHello::new(&l);
    let e = enroll(
        &home.profile,
        Route::Kek,
        &hello,
        &asks(&l, "not the passphrase"),
        CHALLENGE,
        LOG_N,
        &quiet,
    )
    .unwrap_err();
    assert!(format!("{e:#}").contains("wrong passphrase"), "{e:#}");
    assert!(home.hello_files().is_empty());
    assert_eq!(events(&l), ["passphrase:asked"]);

    // The right one: the passphrase first, then Windows Hello; the Hello files are written and
    // the passphrase file is untouched.
    let l = log();
    let hello = FakeHello::new(&l);
    enroll(
        &home.profile,
        Route::Kek,
        &hello,
        &asks(&l, PASS),
        CHALLENGE,
        LOG_N,
        &quiet,
    )
    .unwrap();
    assert_eq!(
        events(&l),
        [
            "passphrase:asked",
            &format!("hello:kek_enroll {CREDENTIAL}"),
            &format!("hello:kek_derive {CREDENTIAL} 5a"),
        ]
    );
    assert_eq!(home.key_bytes(), before, "the passphrase file changed");
    let file = HelloFile::load(&hello_file_path_in(&home.profile)).unwrap();
    assert_eq!(file.route, Route::Kek);
    assert_eq!(file.credential, CREDENTIAL);
    assert_eq!(file.challenge, CHALLENGE);
    let text = std::fs::read_to_string(hello_file_path_in(&home.profile)).unwrap();
    assert!(text.contains(r#"route = "kek""#), "{text}");
    assert!(text.contains(&"5a".repeat(32)), "{text}");
    // P4a-R23: the PEM under age's scrypt, the passphrase the KEK's lowercase hex.
    let sealed = std::fs::read(hello_age_path_in(&home.profile)).unwrap();
    let kek_hex: String = KEK.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(reviewer_key::decrypt_key(&sealed, &kek_hex).unwrap(), PEM);
    // The window that owned both prompts said what they were for.
    assert!(
        hello
            .shown
            .borrow()
            .iter()
            .all(|s| s.contains("set up Windows Hello"))
    );

    // Enrolling again over an enrolment is refused until it is removed.
    let l = log();
    let hello = FakeHello::new(&l);
    assert!(
        enroll(
            &home.profile,
            Route::Kek,
            &hello,
            &asks(&l, PASS),
            CHALLENGE,
            LOG_N,
            &quiet
        )
        .is_err()
    );
}

#[test]
fn a_failed_or_cancelled_enrolment_leaves_nothing_behind() {
    for answer in [
        Answer::Cancel,
        Answer::Fail,
        Answer::NotInFront,
        Answer::TimedOut,
    ] {
        let home = Home::with_key();
        let before = home.key_bytes();
        let l = log();
        let hello = FakeHello::answering(&l, answer);
        assert!(
            enroll(
                &home.profile,
                Route::Kek,
                &hello,
                &asks(&l, PASS),
                CHALLENGE,
                LOG_N,
                &quiet
            )
            .is_err()
        );
        assert!(home.hello_files().is_empty());
        assert_eq!(home.key_bytes(), before);
        // A credential that may have been created is removed again.
        assert!(events(&l).contains(&format!("hello:kek_remove {CREDENTIAL}")));
    }
    // Not supported: refused before any prompt.
    let home = Home::with_key();
    let l = log();
    let hello = FakeHello {
        supported: false,
        ..FakeHello::new(&l)
    };
    let e = enroll(
        &home.profile,
        Route::Kek,
        &hello,
        &asks(&l, PASS),
        CHALLENGE,
        LOG_N,
        &quiet,
    )
    .unwrap_err();
    assert!(format!("{e:#}").contains("Windows Hello"), "{e:#}");
    assert!(!events(&l).iter().any(|e| e.starts_with("hello:")));
    assert!(home.hello_files().is_empty());
}

#[test] // spec: QA-088
fn approve_uses_hello_when_enrolled() {
    // One Windows Hello signature unlocks the key; the passphrase is never asked.
    let home = Home::with_key();
    enroll_kek(&home, &FakeHello::new(&log()));
    let l = log();
    let hello = FakeHello::new(&l);
    let said = RefCell::new(Vec::new());
    let pem = unlock_like_approve(&home, &hello, &never_asked, &said).unwrap();
    assert_eq!(&pem[..], PEM);
    assert_eq!(
        events(&l),
        [
            "window:Approve".to_string(),
            format!("hello:kek_derive {CREDENTIAL} 5a")
        ]
    );

    // Not enrolled: the passphrase, and Windows Hello is never asked.
    let home = Home::with_key();
    let l = log();
    let hello = FakeHello::new(&l);
    let pem = unlock_like_approve(&home, &hello, &asks(&l, PASS), &said).unwrap();
    assert_eq!(&pem[..], PEM);
    assert_eq!(events(&l), ["passphrase:asked"]);
}

#[test] // spec: QA-088
fn approve_falls_back_to_the_passphrase_when_hello_is_cancelled_or_fails() {
    for answer in [
        Answer::Cancel,
        Answer::Fail,
        Answer::NotInFront,
        Answer::TimedOut,
    ] {
        let home = Home::with_key();
        enroll_kek(&home, &FakeHello::new(&log()));
        let l = log();
        let hello = FakeHello::answering(&l, answer);
        let said = RefCell::new(Vec::new());
        let pem = unlock_like_approve(&home, &hello, &asks(&l, PASS), &said).unwrap();
        assert_eq!(&pem[..], PEM);
        let e = events(&l);
        assert_eq!(
            e.last().map(String::as_str),
            Some("passphrase:asked"),
            "{e:?}"
        );
        let said = said.borrow().join("\n");
        assert!(said.contains("passphrase"), "{said}");
        // A wrong passphrase after a failed Hello is still just a wrong passphrase.
        let l = log();
        let hello = FakeHello::answering(&l, answer);
        let r = unlock_like_approve(&home, &hello, &asks(&l, "wrong"), &RefCell::default());
        assert_eq!(r.unwrap_err(), KeyError::WrongPassphrase);
    }
    // A KEK that no longer opens the Hello file (a different credential answered): fallback.
    let home = Home::with_key();
    enroll_kek(&home, &FakeHello::new(&log()));
    let l = log();
    let hello = FakeHello {
        kek: [0x22; 32],
        ..FakeHello::new(&l)
    };
    let pem = unlock_like_approve(&home, &hello, &asks(&l, PASS), &RefCell::default()).unwrap();
    assert_eq!(&pem[..], PEM);
    assert_eq!(events(&l).last().unwrap(), "passphrase:asked");
}

/// QA-088 (I1): where the suite can't bring the prompt to the front (or nobody answers it in
/// time), the passphrase prompt is used instead; with no terminal, nothing is unlocked.
#[test] // spec: QA-088
fn a_prompt_that_cannot_come_to_the_front_falls_back_or_refuses() {
    for answer in [Answer::NotInFront, Answer::TimedOut] {
        let home = Home::with_key();
        enroll_kek(&home, &FakeHello::new(&log()));
        let key = home.key();

        // At a terminal: the passphrase.
        let l = log();
        let hello = FakeHello::answering(&l, answer);
        let enrolled = enrolled_hello(&home.profile, &hello, "m", Some("Merge"), &quiet).unwrap();
        let ask = asks(&l, PASS);
        let passphrase = PassphraseUnlocker::new(&key.encrypted_pem, &ask);
        let auth = KeyAuthenticator::new(&quiet, &as_is, Some(&enrolled), &passphrase, &quiet);
        assert_eq!(&auth.authenticate("s", "a").unwrap()[..], PEM);
        assert_eq!(events(&l).last().unwrap(), "passphrase:asked");

        // No terminal: refused; the passphrase is never asked.
        let l = log();
        let hello = FakeHello::answering(&l, answer);
        let said = RefCell::new(Vec::new());
        let say = |t: &str| said.borrow_mut().push(t.to_string());
        let enrolled = enrolled_hello(&home.profile, &hello, "m", Some("Merge"), &say).unwrap();
        let no_terminal = NoTerminal { say: &say };
        let auth = KeyAuthenticator::new(&quiet, &as_is, Some(&enrolled), &no_terminal, &say);
        assert_eq!(
            auth.authenticate("s", "a").unwrap_err(),
            KeyError::Unavailable
        );
        assert!(!events(&l).contains(&"passphrase:asked".to_string()));
        let said = said.borrow().join("\n");
        assert!(said.contains("Windows Hello isn't used"), "{said}");
        assert!(said.contains("no terminal"), "{said}");
    }
}

/// P4a-R38 (I2): the window that owns the Windows Hello prompt shows the summary and the audit,
/// as the authenticator's renderer made them, the agent's lines quoted.
#[test] // spec: QA-036, QA-088
fn the_window_that_owns_the_prompt_shows_the_summary_and_the_audit() {
    let home = Home::with_key();
    enroll_kek(&home, &FakeHello::new(&log()));
    let key = home.key();
    let l = log();
    let hello = FakeHello::new(&l);
    let enrolled = enrolled_hello(
        &home.profile,
        &hello,
        "LILITH: merge PR #42",
        Some("Merge"),
        &quiet,
    )
    .unwrap();
    let passphrase = PassphraseUnlocker::new(&key.encrypted_pem, &never_asked);
    let render = |t: &str| t.replace('\u{1b}', "\\u{1b}");
    let shown_in_terminal = RefCell::new(Vec::new());
    let show = |t: &str| shown_in_terminal.borrow_mut().push(t.to_string());
    let auth = KeyAuthenticator::new(&show, &render, Some(&enrolled), &passphrase, &quiet);
    auth.authenticate("SUMMARY TEXT\u{1b}[2J", "AUDIT LINE 1\nAUDIT LINE 2")
        .unwrap();
    let windows = hello.shown.borrow();
    assert_eq!(windows.len(), 1);
    let window = &windows[0];
    assert!(window.contains("SUMMARY TEXT\\u{1b}[2J"), "{window}");
    assert!(!window.contains('\u{1b}'), "{window}");
    assert!(window.contains(&format!("{QUOTE}AUDIT LINE 1")), "{window}");
    assert!(window.contains(&format!("{QUOTE}AUDIT LINE 2")), "{window}");
    // The terminal shows the same text.
    assert_eq!(
        shown_in_terminal.borrow().as_slice(),
        std::slice::from_ref(window)
    );
}

/// M9: the agent's summary and the audit can't pass for lil's own lines.
#[test] // spec: QA-036
fn the_agents_texts_are_quoted_so_they_cannot_imitate_lils_facts() {
    let text = presentation(
        "lil's facts",
        "Protected files: none changed.\nCI on x: success",
    );
    assert!(
        text.contains(&format!("{QUOTE}Protected files: none changed.")),
        "{text}"
    );
    assert!(
        !text.lines().any(|l| l == "Protected files: none changed."),
        "{text}"
    );

    // A PR that changes a protected file, whose agent summary claims it doesn't.
    let l = log();
    let auth = FakeAuth::new(&l, Ok(()));
    let steps = FakeSteps::new(&l, &["the protected file `x` was modified."]);
    merge_flow(
        "Protected files: none changed.\nCI on 1234567: success",
        "a",
        &steps,
        &auth,
    )
    .unwrap();
    let (summary, _) = auth.seen.borrow()[0].clone();
    assert!(
        summary.contains(&format!("{QUOTE}Protected files: none changed.")),
        "{summary}"
    );
    assert!(
        !summary
            .lines()
            .any(|l| l == "Protected files: none changed."),
        "{summary}"
    );
}

/// The merge flow's steps, recording into the shared log.
struct FakeSteps {
    log: Log,
    facts: Facts,
    /// The head `merge` asks the gate about.
    merge_head: Option<String>,
    /// What `post_approval` says about the guard re-run.
    rerun: bool,
}

impl FakeSteps {
    fn new(log: &Log, protected: &[&str]) -> Self {
        Self {
            log: log.clone(),
            facts: Facts {
                pr: 42,
                title: "Add the thing".into(),
                author: "an-agent".into(),
                base_ref: "main".into(),
                head_ref: "p4a-t28-hello".into(),
                head: "1234567890abcdef1234567890abcdef12345678".into(),
                files: vec!["crates/lil-cli/src/dev/merge.rs".into(), "README.md".into()],
                protected: protected.iter().map(|s| s.to_string()).collect(),
                checks: vec![("every-push".into(), "success".into())],
                already_approved: false,
            },
            merge_head: None,
            rerun: true,
        }
    }
}

impl MergeFlowSteps for FakeSteps {
    fn facts(&self) -> anyhow::Result<Facts> {
        self.log.borrow_mut().push("steps:facts".into());
        Ok(self.facts.clone())
    }
    fn post_approval(&self, pem: &[u8], facts: &Facts) -> anyhow::Result<bool> {
        assert_eq!(pem, PEM);
        self.log
            .borrow_mut()
            .push(format!("steps:post_approval {}", facts.pr));
        Ok(self.rerun)
    }
    fn wait_for_checks(&self, _facts: &Facts, guard_rerun: bool) -> anyhow::Result<()> {
        self.log
            .borrow_mut()
            .push(format!("steps:wait_for_checks rerun={guard_rerun}"));
        Ok(())
    }
    fn merge(&self, gate: &dyn MergeGate) -> anyhow::Result<String> {
        let head = self.merge_head.as_deref().unwrap_or(&self.facts.head);
        gate.authorize(&subject(self.facts.pr, head))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        self.log.borrow_mut().push("steps:merge".into());
        Ok("feedface".into())
    }
}

/// An authenticator that records what it was shown and answers `answer`.
struct FakeAuth {
    log: Log,
    seen: RefCell<Vec<(String, String)>>,
    answer: Result<(), KeyError>,
}

impl FakeAuth {
    fn new(log: &Log, answer: Result<(), KeyError>) -> Self {
        Self {
            log: log.clone(),
            seen: RefCell::default(),
            answer,
        }
    }
}

impl Authenticator for FakeAuth {
    fn authenticate(&self, summary: &str, audit: &str) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        self.log.borrow_mut().push("auth".into());
        self.seen
            .borrow_mut()
            .push((summary.to_string(), audit.to_string()));
        self.answer.map(|()| Zeroizing::new(PEM.to_vec()))
    }
}

#[test] // spec: QA-088, QA-020
fn no_network_call_before_the_key_is_unlocked() {
    // Through Windows Hello: nothing that uses the key or changes anything on GitHub happens
    // before the key is unlocked; only the read-only facts come first.
    let home = Home::with_key();
    enroll_kek(&home, &FakeHello::new(&log()));
    let l = log();
    let hello = FakeHello::new(&l);
    let key = home.key();
    let passphrase = PassphraseUnlocker::new(&key.encrypted_pem, &never_asked);
    let enrolled = enrolled_hello(
        &home.profile,
        &hello,
        "LILITH: merge PR #42",
        Some("Merge"),
        &quiet,
    )
    .unwrap();
    let auth = KeyAuthenticator::new(&quiet, &as_is, Some(&enrolled), &passphrase, &quiet);
    let steps = FakeSteps::new(&l, &["the protected file `x` was modified."]);
    merge_flow("the change", "the audit", &steps, &auth).unwrap();
    assert_eq!(
        events(&l),
        [
            "steps:facts".to_string(),
            "window:Merge".into(),
            format!("hello:kek_derive {CREDENTIAL} 5a"),
            "steps:post_approval 42".into(),
            "steps:wait_for_checks rerun=true".into(),
            "steps:merge".into(),
        ]
    );

    // Hello fails and the passphrase is wrong: nothing after the facts.
    let l = log();
    let hello = FakeHello::answering(&l, Answer::Fail);
    let wrong = asks(&l, "not it");
    let passphrase = PassphraseUnlocker::new(&key.encrypted_pem, &wrong);
    let enrolled = enrolled_hello(
        &home.profile,
        &hello,
        "LILITH: merge PR #42",
        Some("Merge"),
        &quiet,
    )
    .unwrap();
    let auth = KeyAuthenticator::new(&quiet, &as_is, Some(&enrolled), &passphrase, &quiet);
    let steps = FakeSteps::new(&l, &["the protected file `x` was modified."]);
    assert!(merge_flow("the change", "the audit", &steps, &auth).is_err());
    assert_eq!(
        events(&l),
        [
            "steps:facts".to_string(),
            "window:Merge".into(),
            format!("hello:kek_derive {CREDENTIAL} 5a"),
            "passphrase:asked".into(),
        ]
    );
}

#[test] // spec: QA-088
fn status_names_the_route_and_whether_the_key_is_hardware_held() {
    let l = log();
    let text = |home: &Home, hello: &FakeHello| status(&home.profile, hello).join("\n");

    let home = Home::with_key();
    let none = text(&home, &FakeHello::new(&l));
    assert!(none.contains("not enrolled"), "{none}");
    assert!(
        none.contains(&key_path_in(&home.profile).display().to_string()),
        "{none}"
    );

    enroll_kek(&home, &FakeHello::new(&log()));
    let with_tpm = text(&home, &FakeHello::new(&l));
    assert!(with_tpm.contains("KEK"), "{with_tpm}");
    // A TPM's presence doesn't prove this key is in it (Task 28 review, M1).
    assert!(with_tpm.contains("hardware-held: probably"), "{with_tpm}");
    assert!(with_tpm.contains("TPM"), "{with_tpm}");
    let no_tpm = text(
        &home,
        &FakeHello {
            tpm: false,
            ..FakeHello::new(&l)
        },
    );
    assert!(no_tpm.contains("KEK"), "{no_tpm}");
    assert!(no_tpm.contains("hardware-held: no"), "{no_tpm}");

    for t in [&none, &with_tpm, &no_tpm] {
        assert!(!t.contains("hardware-bound"), "{t}");
        assert!(!t.contains("hardware-held: yes"), "{t}");
    }
    // Status never prompts.
    assert!(!events(&l).iter().any(|e| e.contains("derive")));
}

#[test] // spec: QA-088
fn remove_deletes_the_hello_files_and_the_credential_and_keeps_the_passphrase_file() {
    let home = Home::with_key();
    let before = home.key_bytes();
    enroll_kek(&home, &FakeHello::new(&log()));
    assert_eq!(home.hello_files().len(), 2);
    let l = log();
    remove(&home.profile, &FakeHello::new(&l)).unwrap();
    assert!(home.hello_files().is_empty());
    assert_eq!(events(&l), [format!("hello:kek_remove {CREDENTIAL}")]);
    assert_eq!(home.key_bytes(), before);
    // Removing again is fine, and has no credential to delete.
    let l = log();
    remove(&home.profile, &FakeHello::new(&l)).unwrap();
    assert!(events(&l).is_empty(), "{:?}", events(&l));

    // M5: a credential that can't be removed doesn't keep the files.
    let home = Home::with_key();
    let before = home.key_bytes();
    enroll_kek(&home, &FakeHello::new(&log()));
    let hello = FakeHello {
        remove_fails: true,
        ..FakeHello::new(&log())
    };
    let e = format!("{:#}", remove(&home.profile, &hello).unwrap_err());
    assert!(e.contains("files are removed"), "{e}");
    assert!(home.hello_files().is_empty());
    assert_eq!(home.key_bytes(), before);
}

/// M3: a new key from `reviewer-setup` takes the old enrolment's files with it.
#[test] // spec: QA-088
fn a_new_reviewer_key_discards_the_old_enrolment() {
    let home = Home::with_key();
    enroll_kek(&home, &FakeHello::new(&log()));
    let removed = discard_enrolment_beside(&key_path_in(&home.profile)).unwrap();
    assert_eq!(removed.len(), 2);
    assert!(home.hello_files().is_empty());
    assert!(key_path_in(&home.profile).exists());
    assert!(
        discard_enrolment_beside(&key_path_in(&home.profile))
            .unwrap()
            .is_empty()
    );
}

#[test] // spec: QA-088
fn a_damaged_hello_file_never_blocks_the_passphrase_path() {
    type Damage = fn(&Path);
    let damages: &[(&str, Damage)] = &[
        ("not TOML", |p| {
            std::fs::write(hello_file_path_in(p), "{{{").unwrap()
        }),
        ("an unknown key", |p| {
            let mut t = std::fs::read_to_string(hello_file_path_in(p)).unwrap();
            t.push_str("extra = 1\n");
            std::fs::write(hello_file_path_in(p), t).unwrap();
        }),
        ("an unknown route", |p| {
            let t = std::fs::read_to_string(hello_file_path_in(p)).unwrap();
            std::fs::write(hello_file_path_in(p), t.replace("\"kek\"", "\"tpm\"")).unwrap();
        }),
        ("the DPAPI route, which isn't built", |p| {
            let t = std::fs::read_to_string(hello_file_path_in(p)).unwrap();
            std::fs::write(hello_file_path_in(p), t.replace("\"kek\"", "\"dpapi\"")).unwrap();
        }),
        ("a short challenge", |p| {
            let t = std::fs::read_to_string(hello_file_path_in(p)).unwrap();
            std::fs::write(hello_file_path_in(p), t.replace(&"5a".repeat(32), "5a5a")).unwrap();
        }),
        ("another credential", |p| {
            let t = std::fs::read_to_string(hello_file_path_in(p)).unwrap();
            std::fs::write(hello_file_path_in(p), t.replace(CREDENTIAL, "someone-else")).unwrap();
        }),
        ("no sealed key", |p| {
            std::fs::remove_file(hello_age_path_in(p)).unwrap()
        }),
        ("a damaged sealed key", |p| {
            std::fs::write(hello_age_path_in(p), b"garbage").unwrap()
        }),
        ("a folder in place of the file", |p| {
            std::fs::remove_file(hello_file_path_in(p)).unwrap();
            std::fs::create_dir(hello_file_path_in(p)).unwrap();
        }),
    ];
    for (what, damage) in damages {
        let home = Home::with_key();
        enroll_kek(&home, &FakeHello::new(&log()));
        damage(&home.profile);
        let l = log();
        let hello = FakeHello::new(&l);
        let said = RefCell::new(Vec::new());
        let pem = unlock_like_approve(&home, &hello, &asks(&l, PASS), &said)
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(&pem[..], PEM, "{what}");
        assert_eq!(events(&l).last().unwrap(), "passphrase:asked", "{what}");
        // Status says so instead of failing, and remove cleans it up.
        let s = status(&home.profile, &hello).join("\n");
        assert!(s.contains("damaged"), "{what}: {s}");
        remove(&home.profile, &hello).unwrap_or_else(|e| panic!("{what}: {e:#}"));
        assert!(home.hello_files().is_empty(), "{what}");
    }
}

#[test] // spec: QA-036, QA-088
fn merge_flow_shows_the_summary_and_the_audit_before_authenticating() {
    for protected in [&["the protected file `x` was modified."][..], &[]] {
        let l = log();
        let auth = FakeAuth::new(&l, Ok(()));
        let steps = FakeSteps::new(&l, protected);
        merge_flow("Adds the thing.", "Reviewed: looks right.", &steps, &auth).unwrap();
        let seen = auth.seen.borrow();
        assert_eq!(seen.len(), 1, "authenticated once");
        let (summary, audit) = &seen[0];
        assert!(summary.contains("Adds the thing."), "{summary}");
        assert!(summary.contains("PR #42"), "{summary}");
        assert!(summary.contains("1234567"), "{summary}");
        assert!(summary.contains("README.md"), "{summary}");
        assert!(summary.contains("every-push"), "{summary}");
        // Fix round 2: the title, author, base and head branch, as GitHub reports them.
        for detail in ["Add the thing", "an-agent", "main", "p4a-t28-hello"] {
            assert!(summary.contains(detail), "{detail}: {summary}");
        }
        for p in protected {
            assert!(summary.contains(p), "{summary}");
        }
        assert_eq!(audit, "Reviewed: looks right.");
        let e = events(&l);
        let auth_at = e.iter().position(|x| x == "auth").unwrap();
        assert_eq!(&e[..auth_at], ["steps:facts"], "{e:?}");
        assert_eq!(e.last().unwrap(), "steps:merge");
        // The approval is posted only when protected files changed, and a failed guard check is
        // waited on only after its re-run was asked for (M8).
        assert_eq!(
            e.iter().any(|x| x.starts_with("steps:post_approval")),
            !protected.is_empty(),
            "{e:?}"
        );
        let wait = format!("steps:wait_for_checks rerun={}", !protected.is_empty());
        assert!(e.contains(&wait), "{e:?}");
    }
    // The real authenticator shows both texts before it unlocks anything.
    let home = Home::with_key();
    enroll_kek(&home, &FakeHello::new(&log()));
    let l = log();
    let hello = FakeHello::new(&l);
    let key = home.key();
    let passphrase = PassphraseUnlocker::new(&key.encrypted_pem, &never_asked);
    let enrolled = enrolled_hello(&home.profile, &hello, "m", Some("Merge"), &quiet).unwrap();
    let shown = |t: &str| l.borrow_mut().push(format!("shown:{t}"));
    let auth = KeyAuthenticator::new(&shown, &as_is, Some(&enrolled), &passphrase, &quiet);
    auth.authenticate("SUMMARY TEXT", "AUDIT TEXT").unwrap();
    let e = events(&l);
    assert!(e[0].starts_with("shown:"), "{e:?}");
    assert!(
        e[0].contains("SUMMARY TEXT") && e[0].contains("AUDIT TEXT"),
        "{e:?}"
    );
    assert_eq!(e[1], "window:Merge", "{e:?}");
    assert!(e[2].starts_with("hello:kek_derive"), "{e:?}");
}

#[test] // spec: QA-036
fn merge_flow_merges_nothing_without_authentication() {
    for answer in [
        KeyError::Cancelled,
        KeyError::WrongPassphrase,
        KeyError::Unavailable,
        KeyError::Damaged,
    ] {
        let l = log();
        let auth = FakeAuth::new(&l, Err(answer));
        let steps = FakeSteps::new(&l, &["the protected file `x` was modified."]);
        let e = merge_flow("s", "a", &steps, &auth).unwrap_err();
        let e = format!("{e:#}");
        assert!(e.contains("nothing was approved or merged"), "{e}");
        assert_eq!(events(&l), ["steps:facts", "auth"], "{answer:?}");
    }
    // The flow's authentication covers only the head it showed: a merge of another head is
    // refused by the gate.
    let l = log();
    let auth = FakeAuth::new(&l, Ok(()));
    let mut steps = FakeSteps::new(&l, &[]);
    steps.merge_head = Some("0000000000000000000000000000000000000abc".into());
    assert!(merge_flow("s", "a", &steps, &auth).is_err());
    assert!(!events(&l).contains(&"steps:merge".to_string()));
}

#[test] // spec: QA-036
fn dev_merge_asks_for_authentication_on_every_merge() {
    // `lil dev merge` on its own: every merge, protected files or not, asks the owner.
    let l = log();
    let auth = FakeAuth::new(&l, Ok(()));
    let gate = AskOnEveryMerge::new(&auth);
    for pr in [1, 2, 3] {
        gate.authorize(&subject(pr, "1234567890abcdef1234567890abcdef12345678"))
            .unwrap();
    }
    assert_eq!(events(&l), ["auth", "auth", "auth"]);
    let seen = auth.seen.borrow();
    assert!(seen[0].0.contains("PR #1"), "{:?}", seen[0]);
    assert!(seen[2].0.contains("1234567"), "{:?}", seen[2]);
    assert!(!seen[0].1.is_empty(), "the audit slot says there is none");
    // Re-review m5: which PR it is, and into which branch.
    for detail in ["Add the thing", "an-agent", "Into main"] {
        assert!(seen[0].0.contains(detail), "{detail}: {:?}", seen[0]);
    }
    drop(seen);

    for answer in [KeyError::Cancelled, KeyError::WrongPassphrase] {
        let auth = FakeAuth::new(&log(), Err(answer));
        let gate = AskOnEveryMerge::new(&auth);
        let e = gate
            .authorize(&subject(7, "1234567890abcdef1234567890abcdef12345678"))
            .unwrap_err();
        assert!(e.contains("nothing was merged"), "{e}");
    }
}

/// Fix round 2: Windows Hello Face can complete a prompt with no deliberate act, so the LILITH
/// window asks first ("Merge" / "Approve", or "Deny"). "Deny", closing the window or no choice in
/// time refuses outright: Windows Hello is never raised, the passphrase is never asked, and
/// nothing is approved or merged.
#[test] // spec: QA-036, QA-088
fn deny_close_or_no_choice_refuses_and_never_raises_windows_hello() {
    for click in [Click::Deny, Click::Close, Click::Nothing] {
        let home = Home::with_key();
        enroll_kek(&home, &FakeHello::new(&log()));
        let key = home.key();

        // The merge flow: only the read-only facts happen.
        let l = log();
        let hello = FakeHello::clicking(&l, click);
        let enrolled = enrolled_hello(&home.profile, &hello, "m", Some("Merge"), &quiet).unwrap();
        // A terminal is there, but a refusal is not a reason to ask for the passphrase.
        let ask = asks(&l, PASS);
        let passphrase = PassphraseUnlocker::new(&key.encrypted_pem, &ask);
        let auth = KeyAuthenticator::new(&quiet, &as_is, Some(&enrolled), &passphrase, &quiet);
        let steps = FakeSteps::new(&l, &["the protected file `x` was modified."]);
        let e = format!("{:#}", merge_flow("s", "a", &steps, &auth).unwrap_err());
        assert!(
            e.contains("nothing was approved or merged"),
            "{click:?}: {e}"
        );
        assert_eq!(
            events(&l),
            [
                "steps:facts".to_string(),
                "window:Merge".into(),
                format!("window:refused {click:?}"),
            ],
            "{click:?}"
        );

        // `lil dev approve`'s unlock: the same refusal.
        let l = log();
        let hello = FakeHello::clicking(&l, click);
        let r = unlock_like_approve(&home, &hello, &asks(&l, PASS), &RefCell::default());
        assert_eq!(r.unwrap_err(), KeyError::Denied, "{click:?}");
        assert!(
            !events(&l).iter().any(|e| e.starts_with("hello:")),
            "{click:?}"
        );
        assert!(!events(&l).contains(&"passphrase:asked".to_string()));
    }

    // "Merge": the window first, then Windows Hello.
    let home = Home::with_key();
    enroll_kek(&home, &FakeHello::new(&log()));
    let l = log();
    let hello = FakeHello::clicking(&l, Click::Accept);
    let said = RefCell::new(Vec::new());
    unlock_like_approve(&home, &hello, &never_asked, &said).unwrap();
    assert_eq!(
        events(&l),
        [
            "window:Approve".to_string(),
            format!("hello:kek_derive {CREDENTIAL} 5a")
        ]
    );
}

/// A merge to ask a gate about.
fn subject(pr: u64, head: &str) -> merge_flow::MergeSubject<'_> {
    merge_flow::MergeSubject {
        pr,
        head,
        title: "Add the thing",
        author: "an-agent",
        base_branch: "main",
    }
}

/// Re-review m3 and n1: a long agent summary is cut, by lines and by characters, so it can't
/// bury the audit; the cut is said in lil's own words, outside the agent's quote.
#[test] // spec: QA-036
fn a_long_summary_is_cut_before_the_audit() {
    let shown = |summary: &str| {
        let l = log();
        let auth = FakeAuth::new(&l, Ok(()));
        let steps = FakeSteps::new(&l, &[]);
        merge_flow(summary, "the audit", &steps, &auth).unwrap();
        auth.seen.borrow()[0].0.clone()
    };
    let quoted_chars = |text: &str| -> usize {
        text.lines()
            .filter_map(|l| l.strip_prefix(QUOTE))
            .map(|l| l.chars().count())
            .sum()
    };
    let cut_line = |text: &str| -> String {
        let cut: Vec<&str> = text.lines().filter(|l| l.contains("not shown")).collect();
        assert_eq!(cut.len(), 1, "{text}");
        assert!(!cut[0].starts_with(QUOTE), "the cut is lil's line: {text}");
        // After the agent's text.
        assert!(!text.lines().last().unwrap().starts_with(QUOTE), "{text}");
        cut[0].to_string()
    };

    // Many short lines.
    let long: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    let summary = shown(&long);
    assert!(summary.contains(&format!("{QUOTE}line 60\n")), "{summary}");
    assert!(!summary.contains("line 61\n"), "{summary}");
    let rest: usize = (61..=200).map(|i| format!("line {i}").len()).sum();
    assert!(
        cut_line(&summary).contains(&format!("{rest} more characters")),
        "{summary}"
    );

    // One 64 KiB line, with no newline.
    let one_line = "x".repeat(64 * 1024);
    let summary = shown(&one_line);
    assert_eq!(
        quoted_chars(&summary),
        merge_flow::SUMMARY_CHARS,
        "{summary}"
    );
    let rest = 64 * 1024 - merge_flow::SUMMARY_CHARS;
    assert!(
        cut_line(&summary).contains(&format!("{rest} more characters")),
        "{summary}"
    );

    // Short text is shown whole, with no cut.
    assert_eq!(
        merge_flow::capped("a\nb\n", 60, 4_000),
        ("a\nb".to_string(), None)
    );
    // A cut inside a multi-byte character is never made.
    let (kept, cut) = merge_flow::capped("ééé", 60, 2);
    assert_eq!(kept, "éé");
    assert!(cut.unwrap().contains("1 more character "));
}
