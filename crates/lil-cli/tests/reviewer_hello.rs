//! Windows Hello for the reviewer key and the one-click merge flow (plan 4a Task 28: QA-088,
//! QA-020, QA-036; P4a-R23, P4a-R38).
//!
//! No real Windows Hello: every test injects a fake `HelloPlatform` (the Windows Hello seam), a
//! fake `Unlocker` or a fake `Authenticator`, and uses a temp home. The three modules under test
//! need no GitHub and no git, so this file compiles them in directly, as `hook_patterns.rs`
//! does: `reviewer_key` (the key file and its age encryption), `reviewer_hello` (enrol, status,
//! remove, the unlock order and the authenticator) and `merge_flow` (the flow's order and the
//! merge gate). `lil dev approve`, `lil dev merge` and `lil dev merge-flow` call these same
//! functions; their wiring (the real flow steps and the authenticator's construction) has its
//! own unit tests in `src/dev/reviewer.rs` and `src/dev/merge.rs`.

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

use merge_flow::{Facts, MergeFlowSteps, MergeGate, NO_AUDIT, Wait, merge_alone, merge_flow};
use reviewer_hello::{
    Authenticator, CREDENTIAL, FaceSignIn, HelloFile, HelloPlatform, KeyAuthenticator, NoTerminal,
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
    /// Windows kept its window behind, and nobody brought it forward within the 120 s
    /// (`lil_platform::hello::front_or_wait`, `ask_then`; Phase B ruling).
    NeverBroughtForward,
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
    /// Whether the key credential is there when `kek_remove` is asked (it reports that).
    credential_left: bool,
    face: RefCell<FaceSignIn>,
    /// Face sign-in from the click on: turned on while the window waited (Phase B review m2).
    face_after_click: Option<FaceSignIn>,
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
            credential_left: false,
            face: RefCell::new(FaceSignIn::Off),
            face_after_click: None,
            shown: RefCell::default(),
        }
    }

    fn with_face(log: &Log, face: FaceSignIn) -> Self {
        Self {
            face: RefCell::new(face),
            ..Self::new(log)
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

    /// The window, as `lil_platform::hello::run_in` runs it: with an accept label, the click
    /// (which can only come once the window is in front), and only after an accept click
    /// Windows Hello (logged as `hello:kek_derive`); without, the window in front first.
    fn window(&self, shown: &str, accept: Option<&str>) -> io::Result<()> {
        self.shown.borrow_mut().push(shown.to_string());
        let Some(label) = accept else {
            if self.answer == Answer::NeverBroughtForward {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the LILITH window wasn't brought forward within 120 s",
                ));
            }
            return Ok(());
        };
        self.note(format!("window:{label}"));
        if self.answer == Answer::NeverBroughtForward {
            self.note("window:never brought forward".into());
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "nothing was chosen in the LILITH window in time",
            ));
        }
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
            Answer::NeverBroughtForward => unreachable!("handled by `window`"),
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
    fn face_sign_in(&self) -> FaceSignIn {
        self.face.borrow().clone()
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
        ready: &dyn Fn() -> io::Result<()>,
    ) -> io::Result<Zeroizing<[u8; 32]>> {
        self.window(shown, accept)?;
        if let Some(face) = &self.face_after_click {
            *self.face.borrow_mut() = face.clone();
        }
        ready()?;
        self.note(format!(
            "hello:kek_derive {credential} {:02x}",
            challenge[0]
        ));
        self.prompt()?;
        Ok(Zeroizing::new(self.kek))
    }
    fn kek_remove(&self, credential: &str) -> io::Result<bool> {
        self.note(format!("hello:kek_remove {credential}"));
        if self.remove_fails {
            return Err(io::Error::other("Windows Hello is switched off"));
        }
        Ok(self.credential_left)
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
        Answer::NeverBroughtForward,
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
    for answer in [Answer::Cancel, Answer::Fail, Answer::TimedOut] {
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

/// QA-088 (I1, Phase B ruling): a Merge window Windows keeps behind flashes and waits; never
/// brought forward in time, it is a refusal, and the passphrase is not asked instead.
#[test] // spec: QA-088
fn a_window_that_cannot_take_the_front_waits_and_then_refuses_without_the_passphrase() {
    let home = Home::with_key();
    enroll_kek(&home, &FakeHello::new(&log()));
    let key = home.key();
    let l = log();
    let hello = FakeHello::answering(&l, Answer::NeverBroughtForward);
    let said = RefCell::new(Vec::new());
    let say = |t: &str| said.borrow_mut().push(t.to_string());
    let enrolled = enrolled_hello(&home.profile, &hello, "m", Some("Merge"), &say).unwrap();
    let ask = asks(&l, PASS);
    let passphrase = PassphraseUnlocker::new(&key.encrypted_pem, &ask);
    let auth = KeyAuthenticator::new(&quiet, &as_is, Some(&enrolled), &passphrase, &say);
    assert_eq!(auth.authenticate("s", "a").unwrap_err(), KeyError::Denied);
    assert_eq!(
        events(&l),
        ["window:Merge", "window:never brought forward"],
        "Windows Hello or the passphrase was asked"
    );
    let said = said.borrow().join("\n");
    assert!(said.contains("nothing was approved or merged"), "{said}");
}

/// QA-088 (I1): a prompt nobody answers in time makes Windows Hello unavailable: the
/// passphrase prompt is used instead; with no terminal, nothing is unlocked.
#[test] // spec: QA-088
fn an_unanswered_prompt_falls_back_or_refuses() {
    {
        let answer = Answer::TimedOut;
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
    /// What every read after the first answers (the read after the wait for CI); `None` reads
    /// `facts` again.
    reread: Option<Result<Facts, &'static str>>,
    /// How many times the facts were read.
    reads: std::cell::Cell<u32>,
    /// Makes the wait for the other checks, before your authentication, fail with this.
    before_auth_error: Option<&'static str>,
    /// The head `merge` asks the gate about.
    merge_head: Option<String>,
    /// What `post_approval` says about the guard re-run.
    rerun: bool,
    /// The head of each approval posted.
    approved: RefCell<Vec<String>>,
    /// Makes the wait for the other checks, before the approval, fail with this.
    others_error: Option<&'static str>,
    /// Makes the wait for the guard check, after the approval, fail with this.
    wait_error: Option<&'static str>,
    /// Makes `post_approval` fail with this.
    post_error: Option<&'static str>,
    /// Makes `merge` panic.
    merge_panics: bool,
    /// Makes `merge` fail with this after the gate let it through.
    merge_error: Option<&'static str>,
    /// Makes `withdraw_approval` fail.
    withdraw_fails: bool,
    /// Ctrl+C comes during the second read of the facts.
    ctrl_c_on_reread: bool,
    /// Whether Ctrl+C came.
    ctrl_c: std::cell::Cell<bool>,
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
            reread: None,
            reads: std::cell::Cell::new(0),
            before_auth_error: None,
            merge_head: None,
            rerun: true,
            approved: RefCell::default(),
            others_error: None,
            wait_error: None,
            post_error: None,
            merge_panics: false,
            merge_error: None,
            withdraw_fails: false,
            ctrl_c_on_reread: false,
            ctrl_c: std::cell::Cell::new(false),
        }
    }
}

impl MergeFlowSteps for FakeSteps {
    fn facts(&self) -> anyhow::Result<Facts> {
        self.log.borrow_mut().push("steps:facts".into());
        let reads = self.reads.get();
        self.reads.set(reads + 1);
        if reads > 0 && self.ctrl_c_on_reread {
            self.ctrl_c.set(true);
        }
        match &self.reread {
            Some(Ok(facts)) if reads > 0 => Ok(facts.clone()),
            Some(Err(e)) if reads > 0 => Err(anyhow::anyhow!("{e}")),
            _ => Ok(self.facts.clone()),
        }
    }
    fn post_approval(&self, pem: &[u8], facts: &Facts) -> anyhow::Result<bool> {
        assert_eq!(pem, PEM);
        self.log
            .borrow_mut()
            .push(format!("steps:post_approval {}", facts.pr));
        if let Some(e) = self.post_error {
            return Err(anyhow::anyhow!("{e}"));
        }
        self.approved.borrow_mut().push(facts.head.clone());
        Ok(self.rerun)
    }
    fn wait_for_checks(&self, _facts: &Facts, wait: Wait) -> anyhow::Result<()> {
        let (event, error) = match wait {
            Wait::BeforeAuth => ("steps:wait before-auth".to_string(), self.before_auth_error),
            Wait::Others => ("steps:wait others".to_string(), self.others_error),
            Wait::Guard { rerun } => (format!("steps:wait guard rerun={rerun}"), self.wait_error),
        };
        self.log.borrow_mut().push(event);
        match error {
            Some(e) => Err(anyhow::anyhow!("{e}")),
            None => Ok(()),
        }
    }
    fn merge(&self, gate: &dyn MergeGate) -> anyhow::Result<String> {
        let head = self.merge_head.as_deref().unwrap_or(&self.facts.head);
        gate.authorize(&subject(self.facts.pr, head))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Some(e) = self.merge_error {
            return Err(anyhow::anyhow!("{e}"));
        }
        assert!(!self.merge_panics, "a bug in the merge step");
        self.log.borrow_mut().push("steps:merge".into());
        Ok("feedface".into())
    }
    fn interrupted(&self) -> bool {
        self.ctrl_c.get()
    }
    fn withdraw_approval(&self, pem: &[u8], facts: &Facts) -> anyhow::Result<String> {
        assert_eq!(pem, PEM);
        self.log
            .borrow_mut()
            .push(format!("steps:withdraw {}", &facts.head[..7]));
        if self.withdraw_fails {
            anyhow::bail!("HTTP 403 from GitHub");
        }
        Ok(format!(
            "the reviewer App's approval of {} was withdrawn and protected-test-check now reports failure",
            &facts.head[..7]
        ))
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
            "steps:wait before-auth".into(),
            "steps:facts".into(),
            "window:Merge".into(),
            format!("hello:kek_derive {CREDENTIAL} 5a"),
            "steps:wait others".into(),
            "steps:post_approval 42".into(),
            "steps:wait guard rerun=true".into(),
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
            "steps:wait before-auth".into(),
            "steps:facts".into(),
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

/// Owner decision 2026-10-24: Windows Hello unlocks the reviewer key only while face sign-in is
/// off. On (Phase B review I1): at approve or merge time LILITH removes its enrolment (both
/// files and the credential; the passphrase file stays), uses the passphrase and says how to
/// re-enrol. Unknown: nothing is removed, the passphrase is used with a warning. `enroll`
/// refuses before asking anything, and `status` shows it.
#[test] // spec: QA-088
fn windows_hello_is_used_only_while_face_sign_in_is_off() {
    for face in [FaceSignIn::On, FaceSignIn::Unknown("WinBio said no".into())] {
        // Enrolled while face was off; now it is on (or can't be told).
        let home = Home::with_key();
        let key_before = home.key_bytes();
        enroll_kek(&home, &FakeHello::new(&log()));
        let l = log();
        let hello = FakeHello::with_face(&l, face.clone());
        let said = RefCell::new(Vec::new());
        let pem = unlock_like_approve(&home, &hello, &asks(&l, PASS), &said).unwrap();
        assert_eq!(&pem[..], PEM);
        let said = said.borrow().join("\n");
        assert!(said.contains("face sign-in"), "{said}");
        assert_eq!(home.key_bytes(), key_before, "the passphrase file changed");
        if face == FaceSignIn::On {
            // All three removed, then the passphrase; Windows Hello never raised.
            assert_eq!(
                events(&l),
                [
                    format!("hello:kek_remove {CREDENTIAL}"),
                    "passphrase:asked".to_string()
                ]
            );
            assert!(home.hello_files().is_empty(), "{:?}", home.hello_files());
            assert!(
                said.contains("Settings > Accounts > Sign-in options"),
                "{said}"
            );
            assert!(said.contains("lil dev reviewer-hello enroll"), "{said}");
        } else {
            // Unknown: nothing removed.
            assert_eq!(events(&l), ["passphrase:asked"], "{face:?}");
            assert_eq!(home.hello_files().len(), 2);
        }

        let text = status(&home.profile, &hello).join("\n");
        assert!(text.contains("Face sign-in: "), "{text}");
        assert!(!text.contains("Face sign-in: off"), "{text}");

        // `enroll` refuses before the passphrase or Windows Hello is asked.
        let home = Home::with_key();
        let l = log();
        let e = enroll(
            &home.profile,
            Route::Kek,
            &FakeHello::with_face(&l, face.clone()),
            &never_asked,
            CHALLENGE,
            LOG_N,
            &quiet,
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("face sign-in"), "{e:#}");
        assert!(events(&l).is_empty(), "{:?}", events(&l));
        assert!(home.hello_files().is_empty());
    }
    // Off: status says Windows Hello can unlock the key.
    let home = Home::with_key();
    let text = status(&home.profile, &FakeHello::new(&log())).join("\n");
    assert!(text.contains("Face sign-in: off"), "{text}");

    // `status` with face sign-in on removes the enrolment too.
    let home = Home::with_key();
    enroll_kek(&home, &FakeHello::new(&log()));
    let l = log();
    let text = status(&home.profile, &FakeHello::with_face(&l, FaceSignIn::On)).join("\n");
    assert!(text.contains("Face sign-in: on"), "{text}");
    assert!(text.contains("not enrolled"), "{text}");
    assert!(home.hello_files().is_empty());
    assert_eq!(events(&l), [format!("hello:kek_remove {CREDENTIAL}")]);
}

/// Phase B re-review N1: face sign-in on, but removing the credential fails. The passphrase is
/// still used and the advice is to run `remove`; that `remove` then removes the credential left
/// behind although both files are already gone.
#[test] // spec: QA-088
fn a_credential_left_behind_by_a_failed_removal_is_removed_by_remove() {
    let home = Home::with_key();
    enroll_kek(&home, &FakeHello::new(&log()));
    let l = log();
    let hello = FakeHello {
        remove_fails: true,
        ..FakeHello::with_face(&l, FaceSignIn::On)
    };
    let said = RefCell::new(Vec::new());
    let pem = unlock_like_approve(&home, &hello, &asks(&l, PASS), &said).unwrap();
    assert_eq!(&pem[..], PEM);
    assert_eq!(events(&l).last().unwrap(), "passphrase:asked");
    let said = said.borrow().join("\n");
    assert!(said.contains("lil dev reviewer-hello remove"), "{said}");
    assert!(home.hello_files().is_empty());

    // The advice works: no files, but the credential is still there and goes.
    let l = log();
    let hello = FakeHello {
        credential_left: true,
        ..FakeHello::new(&l)
    };
    let removed = remove(&home.profile, &hello).unwrap();
    assert_eq!(events(&l), [format!("hello:kek_remove {CREDENTIAL}")]);
    assert_eq!(
        removed,
        [format!("the Windows Hello key credential {CREDENTIAL}")]
    );
    // Nothing enrolled and no credential: still asked, nothing reported, no error even if
    // Windows Hello is off.
    let l = log();
    let hello = FakeHello {
        remove_fails: true,
        ..FakeHello::new(&l)
    };
    assert!(remove(&home.profile, &hello).unwrap().is_empty());
    assert_eq!(events(&l), [format!("hello:kek_remove {CREDENTIAL}")]);
}

/// Phase B review m2: the face check runs again right before Windows Hello is raised. Face
/// sign-in turned on while the window waited: the enrolment is removed, Windows Hello isn't
/// raised, and the passphrase is asked.
#[test] // spec: QA-088
fn face_sign_in_is_checked_again_right_before_the_prompt() {
    let home = Home::with_key();
    enroll_kek(&home, &FakeHello::new(&log()));
    let l = log();
    let hello = FakeHello {
        face_after_click: Some(FaceSignIn::On),
        ..FakeHello::new(&l)
    };
    let said = RefCell::new(Vec::new());
    let pem = unlock_like_approve(&home, &hello, &asks(&l, PASS), &said).unwrap();
    assert_eq!(&pem[..], PEM);
    assert_eq!(
        events(&l),
        [
            "window:Approve".to_string(),
            format!("hello:kek_remove {CREDENTIAL}"),
            "passphrase:asked".to_string()
        ]
    );
    assert!(home.hello_files().is_empty());
    let said = said.borrow().join("\n");
    assert!(said.contains("face sign-in is on"), "{said}");
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
    // Removing again is fine: the credential is still asked for (re-review N1), finds none,
    // and nothing is reported.
    let l = log();
    assert!(
        remove(&home.profile, &FakeHello::new(&l))
            .unwrap()
            .is_empty()
    );
    assert_eq!(events(&l), [format!("hello:kek_remove {CREDENTIAL}")]);

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
        assert_eq!(
            &e[..auth_at],
            ["steps:facts", "steps:wait before-auth", "steps:facts"],
            "{e:?}"
        );
        assert_eq!(e.last().unwrap(), "steps:merge");
        // The approval is posted on every merge, protected files or not (owner decision
        // 2026-10-26), and a failed guard check is waited on after its re-run was asked for (M8).
        assert!(e.contains(&"steps:post_approval 42".to_string()), "{e:?}");
        assert!(
            e.contains(&"steps:wait guard rerun=true".to_string()),
            "{e:?}"
        );
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
    // Neither the flow nor `lil dev merge` on its own approves or merges anything without your
    // authentication, protected files or not.
    for protected in [&["the protected file `x` was modified."][..], &[]] {
        for alone in [false, true] {
            for answer in [
                KeyError::Cancelled,
                KeyError::WrongPassphrase,
                KeyError::Unavailable,
                KeyError::Damaged,
            ] {
                let l = log();
                let auth = FakeAuth::new(&l, Err(answer));
                let steps = FakeSteps::new(&l, protected);
                let e = if alone {
                    merge_alone(&steps, &auth)
                } else {
                    merge_flow("s", "a", &steps, &auth)
                }
                .unwrap_err();
                let e = format!("{e:#}");
                assert!(e.contains("nothing was approved or merged"), "{e}");
                assert_eq!(
                    events(&l),
                    [
                        "steps:facts",
                        "steps:wait before-auth",
                        "steps:facts",
                        "auth"
                    ],
                    "{answer:?}"
                );
                assert!(steps.approved.borrow().is_empty());
                assert!(!e.contains("withdrawn"), "{e}");
            }
        }
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

/// Owner decision 2026-10-26 (enforce on GitHub): every merge, protected files or not and
/// whether or not the App approved the head before, posts the reviewer App's approval of exactly
/// the head you authenticated, after your authentication and before the CI wait and the merge.
/// The merge itself is let through only for that head. Owner decision 2026-10-27: before your
/// authentication the flow waits for CI with nothing unlocked and reads the facts again.
#[test] // spec: QA-036, QA-021, QA-033
fn every_merge_posts_the_approval_of_the_head_you_authenticated() {
    const HEAD: &str = "1234567890abcdef1234567890abcdef12345678";
    for protected in [&["the protected file `x` was modified."][..], &[]] {
        for already_approved in [false, true] {
            for alone in [false, true] {
                let l = log();
                let auth = FakeAuth::new(&l, Ok(()));
                let mut steps = FakeSteps::new(&l, protected);
                steps.facts.already_approved = already_approved;
                let merged = if alone {
                    merge_alone(&steps, &auth)
                } else {
                    merge_flow("s", "a", &steps, &auth)
                };
                assert_eq!(merged.unwrap(), "feedface");
                assert_eq!(
                    events(&l),
                    [
                        "steps:facts",
                        "steps:wait before-auth",
                        "steps:facts",
                        "auth",
                        "steps:wait others",
                        "steps:post_approval 42",
                        "steps:wait guard rerun=true",
                        "steps:merge",
                    ],
                    "protected {protected:?}, approved {already_approved}, alone {alone}"
                );
                assert_eq!(steps.approved.borrow().as_slice(), [HEAD]);
            }
        }
    }
    // The head is pinned: after the approval, a merge of another head is refused.
    for alone in [false, true] {
        let l = log();
        let auth = FakeAuth::new(&l, Ok(()));
        let mut steps = FakeSteps::new(&l, &[]);
        steps.merge_head = Some("0000000000000000000000000000000000000abc".into());
        let e = if alone {
            merge_alone(&steps, &auth)
        } else {
            merge_flow("s", "a", &steps, &auth)
        }
        .unwrap_err();
        assert!(format!("{e:#}").contains("nothing was merged"), "{e:#}");
        assert_eq!(steps.approved.borrow().as_slice(), [HEAD]);
        assert!(!events(&l).contains(&"steps:merge".to_string()));
        assert_eq!(events(&l).last().unwrap(), "steps:withdraw 1234567");
    }
}

/// Review round 1, I-3: an approval is a standing grant for its commit, so the flow asks for
/// nothing while another required check has already failed. A failed guard check alone, and
/// checks still running or not yet reported, don't stop it.
#[test] // spec: QA-033, QA-036
fn a_failed_check_stops_the_flow_before_anything_is_asked() {
    for alone in [false, true] {
        for failed in ["failure", "cancelled", "skipped"] {
            let l = log();
            let auth = FakeAuth::new(&l, Ok(()));
            let mut steps = FakeSteps::new(&l, &[]);
            steps.facts.checks = vec![
                ("protected-test-check".into(), "failure".into()),
                ("every-push".into(), "success".into()),
                ("pr-ready".into(), failed.into()),
            ];
            let e = if alone {
                merge_alone(&steps, &auth)
            } else {
                merge_flow("s", "a", &steps, &auth)
            }
            .unwrap_err();
            let e = format!("{e:#}");
            assert!(e.contains("pr-ready") && e.contains(failed), "{e}");
            assert!(e.contains("nothing was asked, approved or merged"), "{e}");
            assert_eq!(events(&l), ["steps:facts"], "{failed}");
        }
        let l = log();
        let auth = FakeAuth::new(&l, Ok(()));
        let mut steps = FakeSteps::new(&l, &[]);
        steps.facts.checks = vec![
            ("protected-test-check".into(), "failure".into()),
            ("every-push".into(), "pending".into()),
            ("pr-ready".into(), "missing".into()),
        ];
        if alone {
            merge_alone(&steps, &auth)
        } else {
            merge_flow("s", "a", &steps, &auth)
        }
        .unwrap();
        assert_eq!(events(&l).last().unwrap(), "steps:merge");
    }
}

/// Review round 2, I-2: the approval comes at the last moment. Every other required check is
/// waited on first, after your authentication; only once they are green is the approval posted
/// and the guard check waited on. A failure or the wait running out before that posts nothing
/// and withdraws nothing.
#[test] // spec: QA-033, QA-036
fn the_approval_is_posted_only_after_the_other_checks_are_green() {
    for alone in [false, true] {
        // Pending, then green: the approval follows the wait.
        let l = log();
        let auth = FakeAuth::new(&l, Ok(()));
        let mut steps = FakeSteps::new(&l, &[]);
        steps.facts.checks = vec![("pr-ready".into(), "pending".into())];
        if alone {
            merge_alone(&steps, &auth)
        } else {
            merge_flow("s", "a", &steps, &auth)
        }
        .unwrap();
        assert_eq!(
            events(&l),
            [
                "steps:facts",
                "steps:wait before-auth",
                "steps:facts",
                "auth",
                "steps:wait others",
                "steps:post_approval 42",
                "steps:wait guard rerun=true",
                "steps:merge",
            ]
        );

        // Pending, then failed (or the wait ran out): nothing posted, nothing to withdraw.
        for error in [
            "CI check pr-ready finished as failure on 1234567; nothing was approved or merged",
            "CI isn't green on 1234567 after 20 minutes; nothing was approved or merged",
        ] {
            let l = log();
            let auth = FakeAuth::new(&l, Ok(()));
            let mut steps = FakeSteps::new(&l, &[]);
            steps.facts.checks = vec![("pr-ready".into(), "pending".into())];
            steps.others_error = Some(error);
            let e = if alone {
                merge_alone(&steps, &auth)
            } else {
                merge_flow("s", "a", &steps, &auth)
            }
            .unwrap_err();
            assert_eq!(format!("{e:#}"), error);
            assert_eq!(
                events(&l),
                [
                    "steps:facts",
                    "steps:wait before-auth",
                    "steps:facts",
                    "auth",
                    "steps:wait others"
                ]
            );
            assert!(steps.approved.borrow().is_empty());
        }
    }
}

/// Owner decision 2026-10-27: a check that fails during the wait before your authentication,
/// the wait running out and Ctrl+C during it ask nothing and post nothing.
#[test] // spec: QA-033
fn a_stop_during_the_wait_before_authentication_asks_nothing() {
    for alone in [false, true] {
        for error in [
            "CI check pr-ready finished as failure on 1234567; nothing was asked, approved or merged",
            "CI isn't green on 1234567 after 100 minutes (pr-ready: pending); nothing was asked, approved or merged",
            "stopped by Ctrl+C while waiting for CI; nothing was asked, approved or merged",
        ] {
            let l = log();
            let auth = FakeAuth::new(&l, Ok(()));
            let mut steps = FakeSteps::new(&l, &[]);
            steps.facts.checks = vec![("pr-ready".into(), "pending".into())];
            steps.before_auth_error = Some(error);
            let e = if alone {
                merge_alone(&steps, &auth)
            } else {
                merge_flow("s", "a", &steps, &auth)
            }
            .unwrap_err();
            assert_eq!(format!("{e:#}"), error);
            assert_eq!(events(&l), ["steps:facts", "steps:wait before-auth"]);
            assert!(auth.seen.borrow().is_empty(), "asked: {error}");
            assert!(steps.approved.borrow().is_empty());
        }
    }
}

/// Review I-1: a Ctrl+C that comes during the second read, after the wait for CI, stops the flow
/// before you are asked; the authenticator is never called.
#[test] // spec: QA-033
fn a_ctrl_c_during_the_second_read_never_asks() {
    for alone in [false, true] {
        let l = log();
        let auth = FakeAuth::new(&l, Ok(()));
        let mut steps = FakeSteps::new(&l, &[]);
        steps.ctrl_c_on_reread = true;
        let e = if alone {
            merge_alone(&steps, &auth)
        } else {
            merge_flow("s", "a", &steps, &auth)
        }
        .unwrap_err();
        assert_eq!(
            format!("{e:#}"),
            "stopped by Ctrl+C before your authentication; nothing was asked, approved or merged"
        );
        assert!(auth.seen.borrow().is_empty(), "asked after Ctrl+C");
        assert_eq!(
            events(&l),
            ["steps:facts", "steps:wait before-auth", "steps:facts"]
        );
        assert!(steps.approved.borrow().is_empty());
    }
}

/// Owner decision 2026-10-27: the facts are read again after the wait for CI. A head that
/// moved, a PR that is no longer open or a check that failed by then asks nothing.
#[test] // spec: QA-033
fn a_change_seen_after_the_wait_for_ci_asks_nothing() {
    const MOVED: &str = "0000000000000000000000000000000000000abc";
    for alone in [false, true] {
        let run = |steps: &FakeSteps, auth: &FakeAuth| {
            if alone {
                merge_alone(steps, auth)
            } else {
                merge_flow("s", "a", steps, auth)
            }
        };

        // The head moved.
        let l = log();
        let auth = FakeAuth::new(&l, Ok(()));
        let mut steps = FakeSteps::new(&l, &[]);
        let mut moved = steps.facts.clone();
        moved.head = MOVED.into();
        steps.reread = Some(Ok(moved));
        let e = format!("{:#}", run(&steps, &auth).unwrap_err());
        assert!(
            e.contains("PR #42 moved from 1234567 to 0000000 while waiting for CI"),
            "{e}"
        );
        assert_eq!(
            e.contains("the summary and audit were for 1234567"),
            !alone,
            "{e}"
        );
        assert!(e.contains("nothing was asked, approved or merged"), "{e}");
        assert_eq!(
            events(&l),
            ["steps:facts", "steps:wait before-auth", "steps:facts"]
        );
        assert!(auth.seen.borrow().is_empty());

        // The PR was closed (the read refuses it).
        let l = log();
        let auth = FakeAuth::new(&l, Ok(()));
        let mut steps = FakeSteps::new(&l, &[]);
        steps.reread = Some(Err(
            "PR #42 is closed, not open; nothing was approved or merged",
        ));
        let e = format!("{:#}", run(&steps, &auth).unwrap_err());
        assert!(e.contains("PR #42 is closed, not open"), "{e}");
        assert!(e.contains("nothing was asked"), "{e}");
        assert_eq!(
            events(&l),
            ["steps:facts", "steps:wait before-auth", "steps:facts"]
        );
        assert!(auth.seen.borrow().is_empty());

        // A check failed by the second read.
        let l = log();
        let auth = FakeAuth::new(&l, Ok(()));
        let mut steps = FakeSteps::new(&l, &[]);
        let mut failed = steps.facts.clone();
        failed.checks = vec![("every-push".into(), "failure".into())];
        steps.reread = Some(Ok(failed));
        let e = format!("{:#}", run(&steps, &auth).unwrap_err());
        assert!(e.contains("CI check every-push finished as failure"), "{e}");
        assert!(e.contains("nothing was asked, approved or merged"), "{e}");
        assert!(auth.seen.borrow().is_empty());
        assert!(steps.approved.borrow().is_empty());
    }
}

/// Owner decision 2026-10-27: what you authenticate is what lil read after the wait for CI, and
/// a failed check whose workflow runs again reads as pending, so it doesn't stop the flow.
#[test] // spec: QA-033
fn the_authentication_shows_the_facts_read_after_the_wait() {
    let l = log();
    let auth = FakeAuth::new(&l, Ok(()));
    let mut steps = FakeSteps::new(&l, &[]);
    steps.facts.checks = vec![
        ("every-push".into(), "success".into()),
        (
            "pr-ready".into(),
            "pending (a newer run is in progress)".into(),
        ),
    ];
    let mut green = steps.facts.clone();
    green.title = "Add the thing, read again".into();
    green.checks = vec![
        ("every-push".into(), "success".into()),
        ("pr-ready".into(), "success".into()),
    ];
    steps.reread = Some(Ok(green));
    merge_flow("s", "a", &steps, &auth).unwrap();
    let seen = auth.seen.borrow();
    let (shown, _) = &seen[0];
    assert!(shown.contains("Add the thing, read again"), "{shown}");
    assert!(shown.contains("  pr-ready: success"), "{shown}");
    assert!(!shown.contains("pending"), "{shown}");
}

/// Review round 2, M-1: a failed post (GitHub may have created the review and lost the answer)
/// and a panic after the post both withdraw too; the panic still ends the flow.
#[test] // spec: QA-033, QA-036
fn a_failed_post_or_a_panic_withdraws_the_approval_too() {
    let l = log();
    let auth = FakeAuth::new(&l, Ok(()));
    let mut steps = FakeSteps::new(&l, &[]);
    steps.post_error = Some("posting the approval on PR #42 failed");
    let e = format!("{:#}", merge_alone(&steps, &auth).unwrap_err());
    assert!(e.contains("was withdrawn"), "{e}");
    assert_eq!(events(&l).last().unwrap(), "steps:withdraw 1234567");

    let l = log();
    let auth = FakeAuth::new(&l, Ok(()));
    let mut steps = FakeSteps::new(&l, &[]);
    steps.merge_panics = true;
    let panicked =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| merge_alone(&steps, &auth)));
    assert!(panicked.is_err(), "the panic was swallowed");
    assert_eq!(events(&l).last().unwrap(), "steps:withdraw 1234567");
}

/// Review round 1, I-3: every exit that doesn't merge, once the approval is posted, withdraws
/// it and says so: a CI failure or the wait running out, a gate refusal (a moved head), a failed
/// merge call. A failed withdrawal says the approval still stands. A merge withdraws nothing.
#[test] // spec: QA-033, QA-036
fn every_exit_without_a_merge_after_the_approval_withdraws_it() {
    type Setup = fn(&mut FakeSteps);
    let paths: [(&str, Setup); 4] = [
        ("a CI failure", |s| {
            s.wait_error = Some("CI check every-push finished as failure; nothing was merged")
        }),
        ("the wait running out", |s| {
            s.wait_error = Some("CI isn't green after 20 minutes; nothing was merged")
        }),
        ("a moved head", |s| {
            s.merge_head = Some("0000000000000000000000000000000000000abc".into())
        }),
        ("a refused merge", |s| {
            s.merge_error = Some("GitHub won't merge the PR: not mergeable")
        }),
    ];
    for alone in [false, true] {
        for (what, setup) in paths {
            for withdraw_fails in [false, true] {
                let l = log();
                let auth = FakeAuth::new(&l, Ok(()));
                let mut steps = FakeSteps::new(&l, &[]);
                setup(&mut steps);
                steps.withdraw_fails = withdraw_fails;
                let e = if alone {
                    merge_alone(&steps, &auth)
                } else {
                    merge_flow("s", "a", &steps, &auth)
                }
                .unwrap_err();
                let e = format!("{e:#}");
                let ev = events(&l);
                assert_eq!(
                    ev.last().unwrap(),
                    "steps:withdraw 1234567",
                    "{what}: {ev:?}"
                );
                assert_eq!(
                    ev.iter()
                        .filter(|x| x.starts_with("steps:withdraw"))
                        .count(),
                    1,
                    "{what}"
                );
                assert!(!ev.contains(&"steps:merge".to_string()), "{what}");
                if withdraw_fails {
                    assert!(e.contains("could NOT be withdrawn"), "{what}: {e}");
                    assert!(e.contains("may still stand"), "{what}: {e}");
                    assert!(e.contains("re-run protected-test-check"), "{what}: {e}");
                } else {
                    assert!(e.contains("was withdrawn"), "{what}: {e}");
                }
            }
        }
    }
    // A merge withdraws nothing.
    let l = log();
    let steps = FakeSteps::new(&l, &[]);
    merge_alone(&steps, &FakeAuth::new(&l, Ok(()))).unwrap();
    assert!(!events(&l).iter().any(|x| x.starts_with("steps:withdraw")));
}

/// `lil dev merge` on its own is the flow without an agent's summary or audit: it shows lil's
/// facts, says there is no audit, and asks for your authentication once per merge.
#[test] // spec: QA-036
fn dev_merge_asks_for_authentication_on_every_merge() {
    for protected in [&["the protected file `x` was modified."][..], &[]] {
        let l = log();
        let auth = FakeAuth::new(&l, Ok(()));
        let steps = FakeSteps::new(&l, protected);
        for _ in 0..3 {
            merge_alone(&steps, &auth).unwrap();
        }
        let seen = auth.seen.borrow();
        assert_eq!(seen.len(), 3, "once per merge");
        let (summary, audit) = &seen[0];
        for detail in [
            "PR #42",
            "1234567",
            "Add the thing",
            "an-agent",
            "Into main",
            "README.md",
        ] {
            assert!(summary.contains(detail), "{detail}: {summary}");
        }
        assert!(summary.contains("lil dev merge"), "{summary}");
        assert_eq!(audit, NO_AUDIT);
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
                "steps:wait before-auth".into(),
                "steps:facts".into(),
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
