//! The outside-writers reconciler, simulated and shrinkable (QA-046; ARCH-074, ARCH-079,
//! ARCH-083, ARCH-085, CORE-034, CORE-038).
//!
//! A `proptest` strategy generates sequences of operations: outside edits, slow outside writes
//! seen half-way, deletes, renames, folder moves, outside manifest edits (valid and invalid),
//! `git checkout`s of several files (with lost events, and with candidate files whose run
//! records change in the same checkout), LFS pointers, an outside edit landing between the
//! store's hash check and its replace, core writes and manifest edits, the suite's own moves,
//! and crashes at a write-order step or right after a replace. They run against an in-memory
//! model of the workspace, whose every decision is made by the real pure functions:
//! [`decide_write`] (step 3 of the store's write) and [`watch::reconcile::decide`] (the
//! watcher). A failing sequence shrinks to a minimal one.
//!
//! Invariants, checked after every operation and at the end:
//! - an outside edit is never overwritten, and never left only in a backup file;
//! - a manifest that doesn't parse is marked `needs_repair` and never rewritten;
//! - no human edit comes from an unstable read, from the core's own bytes, or twice in a row
//!   for one SHA-256;
//! - a file the suite expects that disappears is `missing`, never forgotten, and the suite's
//!   own moves never make one `missing`;
//! - after a rescan the index agrees with the disk;
//! - no orphan is deleted;
//! - no store write happens while `index.lock` exists.
//!
//! The case count comes from `test-params.toml` (`ww_testkit::params::proptest_cases`).

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use ww_core::index::FileKind;
use ww_core::store::{Expect, WriteDecision, WsPath, decide_write};
use ww_core::watch::reconcile::{Action, Observed, Recorded, decide};

const ASSET: &str = "projects/p/assets/a";
/// The candidate files of the asset's open run.
const CANDIDATES: usize = 3;
/// The sealed files of `v1.0.0/`.
const SEALED: usize = 2;

fn candidate(i: usize) -> String {
    format!("{ASSET}/candidates/r_1/c{i}.png")
}

fn sealed(i: usize) -> String {
    format!("{ASSET}/v1.0.0/s{i}.png")
}

fn manifest() -> String {
    format!("{ASSET}/a.manifest.json")
}

fn p(rel: &str) -> WsPath {
    WsPath::parse(rel).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// What a file holds: plain bytes (an id), or a git-lfs pointer to the bytes with an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    Bytes(u32),
    Lfs(u32),
}

/// The SHA-256 of bytes with id `id` (distinct per id).
fn sha(id: u32) -> String {
    format!("{:064x}", u64::from(id) + 1)
}

impl Content {
    /// The SHA-256 of the file's bytes (a pointer's own bytes for a pointer).
    fn sha256(self) -> String {
        match self {
            Content::Bytes(id) => sha(id),
            Content::Lfs(id) => format!("{:064x}", 0xF000_0000_u64 + u64::from(id)),
        }
    }

    fn lfs_oid(self) -> Option<String> {
        match self {
            Content::Bytes(_) => None,
            Content::Lfs(id) => Some(sha(id)),
        }
    }
}

/// What the suite records about a tracked path (the manifest, and the store's own hashes).
#[derive(Debug, Clone)]
struct Rec {
    kind: FileKind,
    /// The manifest's hash for it (bare hex).
    sha256: String,
    expected: bool,
    last_human_edit: Option<String>,
}

/// What the index believes about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Indexed {
    Hash(String),
    NotFetched,
    Missing,
}

#[derive(Debug, Clone)]
enum Op {
    OutsideEdit {
        file: u8,
        content: u8,
    },
    SlowOutsideWrite {
        file: u8,
        content: u8,
    },
    Delete {
        file: u8,
    },
    Rename {
        file: u8,
    },
    FolderMove,
    PartialManifestEdit,
    InvalidManifest,
    Checkout {
        files: Vec<(u8, u8)>,
        overflow: bool,
        records_too: bool,
    },
    LfsPointer {
        file: u8,
        same: bool,
    },
    EditBetweenCheckAndReplace {
        file: u8,
        core: u8,
        outside: u8,
    },
    CoreWrite {
        file: u8,
        content: u8,
    },
    CoreManifestEdit,
    CoreMove {
        file: u8,
    },
    CrashAtStep {
        after_manifest: bool,
    },
    CrashAfterReplace {
        file: u8,
        content: u8,
    },
    /// An observation of a file whose bytes didn't change (a metadata-only event).
    Touch {
        file: u8,
    },
}

/// The tracked files ops address: candidates, then sealed files.
fn tracked_file(i: u8) -> String {
    let i = usize::from(i) % (CANDIDATES + SEALED);
    if i < CANDIDATES {
        candidate(i)
    } else {
        sealed(i - CANDIDATES)
    }
}

fn op() -> impl Strategy<Value = Op> {
    let file = 0_u8..8;
    let content = 0_u8..6;
    prop_oneof![
        (file.clone(), content.clone())
            .prop_map(|(file, content)| Op::OutsideEdit { file, content }),
        (file.clone(), content.clone())
            .prop_map(|(file, content)| Op::SlowOutsideWrite { file, content }),
        file.clone().prop_map(|file| Op::Delete { file }),
        file.clone().prop_map(|file| Op::Rename { file }),
        Just(Op::FolderMove),
        Just(Op::PartialManifestEdit),
        Just(Op::InvalidManifest),
        (
            prop::collection::vec((file.clone(), content.clone()), 1..5),
            any::<bool>(),
            any::<bool>()
        )
            .prop_map(|(files, overflow, records_too)| Op::Checkout {
                files,
                overflow,
                records_too
            }),
        (file.clone(), any::<bool>()).prop_map(|(file, same)| Op::LfsPointer { file, same }),
        (file.clone(), content.clone(), content.clone()).prop_map(|(file, core, outside)| {
            Op::EditBetweenCheckAndReplace {
                file,
                core,
                outside,
            }
        }),
        (file.clone(), content.clone()).prop_map(|(file, content)| Op::CoreWrite { file, content }),
        Just(Op::CoreManifestEdit),
        file.clone().prop_map(|file| Op::CoreMove { file }),
        any::<bool>().prop_map(|after_manifest| Op::CrashAtStep { after_manifest }),
        (file.clone(), content.clone())
            .prop_map(|(file, content)| Op::CrashAfterReplace { file, content }),
        file.prop_map(|file| Op::Touch { file }),
    ]
}

/// The workspace, the suite's records and the index, in memory.
#[derive(Debug, Default)]
struct Model {
    disk: BTreeMap<String, Content>,
    manifest_valid: bool,
    rec: BTreeMap<String, Rec>,
    generated: BTreeMap<String, String>,
    /// `[new, old]` (or `[new]` for a new target) of a pending store replace.
    pending: BTreeMap<String, Vec<String>>,
    index: BTreeMap<String, Indexed>,
    missing: BTreeSet<String>,
    needs_repair: bool,
    human_edits: Vec<(String, String)>,
    /// The latest outside write of each path that the suite hasn't accepted as its own.
    outside: BTreeMap<String, String>,
    /// Paths the suite moved itself (never `missing` because of it).
    moved_by_suite: BTreeSet<String>,
    orphans: BTreeSet<String>,
    index_lock: bool,
    deferred: BTreeSet<String>,
    /// Core writes waiting for git.
    queued_writes: Vec<(String, u32)>,
    /// Ids for fresh manifest bytes and for new file contents written by the core.
    next_id: u32,
    runs: u32,
}

impl Model {
    fn new() -> Model {
        let mut m = Model {
            manifest_valid: true,
            next_id: 1000,
            ..Model::default()
        };
        let mut files: Vec<(String, FileKind)> = (0..CANDIDATES)
            .map(|i| (candidate(i), FileKind::Candidate))
            .collect();
        files.extend((0..SEALED).map(|i| (sealed(i), FileKind::Sealed)));
        for (n, (path, kind)) in files.into_iter().enumerate() {
            let id = u32::try_from(n).unwrap() + 100;
            m.disk.insert(path.clone(), Content::Bytes(id));
            m.generated.insert(path.clone(), sha(id));
            m.index.insert(path.clone(), Indexed::Hash(sha(id)));
            m.rec.insert(
                path,
                Rec {
                    kind,
                    sha256: sha(id),
                    expected: true,
                    last_human_edit: None,
                },
            );
        }
        m.disk.insert(manifest(), Content::Bytes(1));
        m.index.insert(manifest(), Indexed::Hash(sha(1)));
        m.rec.insert(
            manifest(),
            Rec {
                kind: FileKind::Manifest,
                sha256: sha(1),
                expected: true,
                last_human_edit: None,
            },
        );
        m
    }

    fn fresh(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    /// The store's own hashes for `path` (`Store::own_hashes`): the pending row's, and the
    /// generated hash when the manifest records another one.
    fn own_hashes(&self, path: &str) -> Vec<String> {
        let mut own = self.pending.get(path).cloned().unwrap_or_default();
        if let (Some(generated), Some(rec)) = (self.generated.get(path), self.rec.get(path))
            && *generated != rec.sha256
        {
            own.push(generated.clone());
        }
        own
    }

    /// The watcher sees `path` (a stable read; unstable reads never get here).
    fn observe(&mut self, path: &str) {
        let observed = match self.disk.get(path) {
            None => Observed::Removed(p(path)),
            Some(_) if path == manifest() && !self.manifest_valid => Observed::BadManifest(p(path)),
            Some(c) => Observed::Changed {
                path: p(path),
                sha256: c.lfs_oid().unwrap_or_else(|| c.sha256()),
                lfs_oid: c.lfs_oid(),
            },
        };
        let recorded = self.rec.get(path).map(|r| Recorded {
            sha256: r.sha256.clone(),
            kind: r.kind,
            manifest: Some(p(&manifest())),
            expected: r.expected,
            pending: self.own_hashes(path),
            last_human_edit: r.last_human_edit.clone(),
        });
        let action = decide(&observed, recorded.as_ref(), self.index_lock);
        self.apply(path, action);
    }

    fn apply(&mut self, path: &str, action: Action) {
        match action {
            Action::DeferUntilGitDone(_) => {
                assert!(self.index_lock, "deferred without git: {path}");
                self.deferred.insert(path.to_string());
            }
            Action::Reindex(_) => match self.disk.get(path) {
                Some(c) => {
                    let c = *c;
                    self.index
                        .insert(path.to_string(), Indexed::Hash(c.sha256()));
                    if self.rec.get(path).is_some_and(|r| r.sha256 == c.sha256()) {
                        self.missing.remove(path);
                    }
                }
                None => {
                    self.index.remove(path);
                    self.missing.remove(path);
                }
            },
            Action::HumanEdit {
                path: edited,
                sha256,
                ..
            } => {
                assert_eq!(edited.as_str(), path);
                // Never the core's own bytes.
                assert!(
                    !self.own_hashes(path).contains(&sha256),
                    "a human edit of the core's own bytes at {path}"
                );
                // Never twice in a row for one SHA-256.
                let last = self
                    .human_edits
                    .iter()
                    .rev()
                    .find(|(p, _)| p == path)
                    .map(|(_, s)| s.clone());
                assert_ne!(last.as_ref(), Some(&sha256), "{path} recorded twice");
                // Only bytes an outside writer left there.
                let on_disk = self.disk[path];
                assert_eq!(
                    on_disk.lfs_oid().unwrap_or_else(|| on_disk.sha256()),
                    sha256,
                    "a human edit of bytes that aren't on disk at {path}"
                );
                self.human_edits.push((path.to_string(), sha256.clone()));
                if let Some(rec) = self.rec.get_mut(path) {
                    rec.last_human_edit = Some(sha256.clone());
                }
                self.index.insert(path.to_string(), Indexed::Hash(sha256));
                self.missing.remove(path);
            }
            Action::NeedsRepair(_) => self.needs_repair = true,
            Action::NotFetched(_) => {
                self.index.insert(path.to_string(), Indexed::NotFetched);
                self.missing.remove(path);
            }
            Action::Missing(_) => {
                assert!(
                    !self.moved_by_suite.contains(path),
                    "the suite's own move made {path} missing"
                );
                self.missing.insert(path.to_string());
                self.index.insert(path.to_string(), Indexed::Missing);
            }
        }
    }

    /// An outside program writes `content` to `path`, and the watcher sees it.
    fn outside_write(&mut self, path: &str, content: Content) {
        self.disk.insert(path.to_string(), content);
        self.outside.insert(path.to_string(), content.sha256());
        self.observe(path);
    }

    fn outside_remove(&mut self, path: &str) {
        if self.disk.remove(path).is_some() {
            self.outside.remove(path);
            self.observe(path);
            if self.rec.get(path).is_some_and(|r| r.expected) && !self.index_lock {
                assert!(self.missing.contains(path), "{path} was forgotten");
            }
        }
    }

    /// The core edits the manifest; a manifest that doesn't parse is refused, never rewritten.
    fn core_manifest_edit(&mut self) -> bool {
        if !self.manifest_valid {
            return false;
        }
        if self.index_lock {
            return false;
        }
        let current = self.disk.get(&manifest()).map(|c| c.sha256());
        let Some(current) = current else {
            return false;
        };
        // An edit reads, applies and writes with `Expect::Hash` of what it read.
        assert_eq!(
            decide_write(&Expect::Hash(current.clone()), Some(&current), None, &[]),
            WriteDecision::Proceed
        );
        let id = self.fresh();
        self.disk.insert(manifest(), Content::Bytes(id));
        self.rec.get_mut(&manifest()).unwrap().sha256 = sha(id);
        self.generated.insert(manifest(), sha(id));
        self.outside.remove(&manifest());
        self.observe(&manifest());
        true
    }

    /// Step 3 of a full write to `path` with `Expect::Generated`.
    fn decision(&self, path: &str) -> WriteDecision {
        let generated = self.generated.get(path).map(String::as_str);
        let pending = self.pending.get(path).cloned().unwrap_or_default();
        let ask =
            |current: Option<&str>| decide_write(&Expect::Generated, current, generated, &pending);
        match self.disk.get(path) {
            None => ask(None),
            Some(c) => {
                let by_sha = ask(Some(&c.sha256()));
                match (by_sha, c.lfs_oid()) {
                    (WriteDecision::Conflict, Some(oid)) => ask(Some(&oid)),
                    (d, _) => d,
                }
            }
        }
    }

    /// A store write of fresh bytes `id` to `path`: refused on a conflict; otherwise the
    /// replace (seen by the watcher while its pending row exists), then step 8 and the
    /// manifest's record.
    fn core_write(&mut self, path: &str, id: u32) {
        if self.index_lock {
            // Step 2: writes wait while git works.
            self.queued_writes.push((path.to_string(), id));
            return;
        }
        let decision = self.decision(path);
        if decision == WriteDecision::Conflict {
            return;
        }
        assert!(
            !self.outside.contains_key(path)
                || self.own_hashes(path).contains(&self.outside[path])
                || self.generated.get(path) == self.outside.get(path),
            "step 3 accepted an outside edit of {path}"
        );
        let old = self.disk.get(path).map(|c| c.sha256());
        let new = sha(id);
        let mut row = vec![new.clone()];
        row.extend(old);
        self.pending.insert(path.to_string(), row);
        self.disk.insert(path.to_string(), Content::Bytes(id));
        self.observe(path);
        self.pending.remove(path);
        self.generated.insert(path.to_string(), new.clone());
        if let Some(rec) = self.rec.get_mut(path) {
            rec.sha256 = new;
        }
        self.outside.remove(path);
        self.core_manifest_edit();
        self.observe(path);
    }

    /// The rescan after git (or after lost events): every path anyone knows.
    fn rescan(&mut self) {
        let paths: BTreeSet<String> = self
            .disk
            .keys()
            .chain(self.rec.keys())
            .chain(self.index.keys())
            .cloned()
            .collect();
        for path in paths {
            self.observe(&path);
        }
    }

    fn run(&mut self, op: &Op) {
        match op.clone() {
            Op::OutsideEdit { file, content } => {
                self.outside_write(&tracked_file(file), Content::Bytes(u32::from(content)));
            }
            Op::SlowOutsideWrite { file, content } => {
                let path = tracked_file(file);
                // Seen half-way: the identity moves during the read, so the read is parked and
                // nothing is decided (no Observed).
                let half = Content::Bytes(500 + u32::from(content));
                self.disk.insert(path.clone(), half);
                // Then the write ends and a stable read follows.
                self.outside_write(&path, Content::Bytes(u32::from(content)));
            }
            Op::Delete { file } => self.outside_remove(&tracked_file(file)),
            Op::Rename { file } => {
                let path = tracked_file(file);
                if let Some(c) = self.disk.get(&path).copied() {
                    let to = format!("{path}.renamed");
                    self.outside_remove(&path);
                    self.outside_write(&to, c);
                }
            }
            Op::FolderMove => {
                for i in 0..CANDIDATES {
                    let path = candidate(i);
                    if let Some(c) = self.disk.get(&path).copied() {
                        self.outside_remove(&path);
                        self.outside_write(&format!("inbox/moved/r_1/c{i}.png"), c);
                    }
                }
            }
            Op::PartialManifestEdit => {
                if self.manifest_valid && self.disk.contains_key(&manifest()) {
                    // A valid edit outside the suite: the manifest is re-parsed.
                    let id = self.fresh();
                    self.outside_write(&manifest(), Content::Bytes(id));
                    self.rec.get_mut(&manifest()).unwrap().sha256 = sha(id);
                    self.outside.remove(&manifest());
                }
            }
            Op::InvalidManifest => {
                self.manifest_valid = false;
                let id = self.fresh();
                self.outside_write(&manifest(), Content::Bytes(id));
                assert!(self.needs_repair, "an invalid manifest isn't marked");
            }
            Op::Checkout {
                files,
                overflow,
                records_too,
            } => {
                self.index_lock = true;
                for (file, content) in files {
                    let path = tracked_file(file);
                    let content = Content::Bytes(u32::from(content) + 50);
                    self.disk.insert(path.clone(), content);
                    self.outside.insert(path.clone(), content.sha256());
                    let is_candidate = path.contains("/candidates/");
                    if records_too && is_candidate && self.manifest_valid {
                        // The checkout brings the run's matching record along.
                        let rec = self.rec.get_mut(&path).unwrap();
                        rec.sha256 = content.sha256();
                        self.outside.remove(&path);
                    }
                    if !overflow {
                        self.observe(&path);
                    }
                }
                if records_too && self.manifest_valid {
                    let id = self.fresh();
                    self.disk.insert(manifest(), Content::Bytes(id));
                    self.rec.get_mut(&manifest()).unwrap().sha256 = sha(id);
                    if !overflow {
                        self.observe(&manifest());
                    }
                }
                // A core write while git works waits for it.
                let path = candidate(0);
                let id = self.fresh();
                self.core_write(&path, id);
                assert!(!self.queued_writes.is_empty(), "a write ran during git");
                self.index_lock = false;
                self.deferred.clear();
                self.rescan();
                for (path, id) in std::mem::take(&mut self.queued_writes) {
                    self.core_write(&path, id);
                }
            }
            Op::LfsPointer { file, same } => {
                let path = tracked_file(file);
                let recorded = self.rec[&path].sha256.clone();
                let id = if same {
                    // A pointer to exactly the recorded bytes: not fetched, not an edit.
                    let n = u64::from_str_radix(&recorded, 16).expect("a model hash");
                    u32::try_from(n - 1).expect("a model id")
                } else {
                    self.fresh()
                };
                self.outside_write(&path, Content::Lfs(id));
                if same {
                    assert_eq!(self.index.get(&path), Some(&Indexed::NotFetched), "{path}");
                    self.outside.remove(&path);
                }
            }
            Op::EditBetweenCheckAndReplace {
                file,
                core,
                outside,
            } => {
                let path = tracked_file(file);
                if self.decision(&path) == WriteDecision::Conflict {
                    return;
                }
                let checked = self.disk.get(&path).map(|c| c.sha256());
                let core_id = 300 + u32::from(core);
                let outside_content = Content::Bytes(u32::from(outside));
                // The outside write lands after step 3...
                self.disk.insert(path.clone(), outside_content);
                self.outside.insert(path.clone(), outside_content.sha256());
                // ...step 6 swaps in the core's bytes, the outside bytes going to the backup...
                let backup = self.disk.insert(path.clone(), Content::Bytes(core_id));
                // ...and step 7 sees the backup isn't what step 3 checked: the outside version
                // is put back, and the core's call fails.
                let backup = backup.map(Content::sha256);
                if backup != checked {
                    self.disk.insert(path.clone(), outside_content);
                } else {
                    // The outside writer wrote the bytes that were there: nothing to keep.
                    self.generated.insert(path.clone(), sha(core_id));
                    self.outside.remove(&path);
                }
                self.observe(&path);
            }
            Op::CoreWrite { file, content } => {
                let id = 200 + u32::from(content);
                self.core_write(&tracked_file(file), id);
            }
            Op::CoreManifestEdit => {
                let before = self.disk.get(&manifest()).copied();
                if !self.core_manifest_edit() && !self.manifest_valid {
                    assert_eq!(self.disk.get(&manifest()).copied(), before, "rewritten");
                }
            }
            Op::CoreMove { file } => {
                let i = usize::from(file) % CANDIDATES;
                let path = candidate(i);
                let Some(c) = self.disk.get(&path).copied() else {
                    return;
                };
                // A sign-off moves only files whose bytes are the recorded ones.
                if self.rec[&path].sha256 != c.sha256() || !self.manifest_valid || self.index_lock {
                    return;
                }
                let to = format!("{ASSET}/v2.0.0/c{i}.png");
                if self.disk.contains_key(&to) {
                    return;
                }
                // The manifest goes first: the run's files are no longer expected there.
                self.rec.get_mut(&path).unwrap().expected = false;
                self.rec.insert(
                    to.clone(),
                    Rec {
                        kind: FileKind::Sealed,
                        sha256: c.sha256(),
                        expected: true,
                        last_human_edit: None,
                    },
                );
                self.core_manifest_edit();
                self.moved_by_suite.insert(path.clone());
                self.disk.remove(&path);
                self.disk.insert(to.clone(), c);
                self.generated.insert(to.clone(), c.sha256());
                self.outside.remove(&path);
                self.observe(&path);
                self.observe(&to);
            }
            Op::CrashAtStep { after_manifest } => {
                if self.index_lock {
                    return;
                }
                self.runs += 1;
                let path = format!("{ASSET}/candidates/r_{}/n.png", self.runs + 1);
                let id = self.fresh();
                // The output lands.
                self.disk.insert(path.clone(), Content::Bytes(id));
                self.generated.insert(path.clone(), sha(id));
                if after_manifest && self.manifest_valid {
                    self.rec.insert(
                        path.clone(),
                        Rec {
                            kind: FileKind::Candidate,
                            sha256: sha(id),
                            expected: true,
                            last_human_edit: None,
                        },
                    );
                    self.core_manifest_edit();
                } else {
                    self.orphans.insert(path.clone());
                }
                // The crash; the next open's recovery deletes no content, and its reconcile
                // sees the files.
                self.observe(&path);
            }
            Op::CrashAfterReplace { file, content } => {
                let path = tracked_file(file);
                if self.index_lock || self.decision(&path) == WriteDecision::Conflict {
                    return;
                }
                let id = 400 + u32::from(content);
                let old = self.disk.get(&path).map(|c| c.sha256());
                let mut row = vec![sha(id)];
                row.extend(old);
                self.pending.insert(path.clone(), row);
                self.disk.insert(path.clone(), Content::Bytes(id));
                // The crash: the manifest never got the new record. The next open's recovery
                // finds the target holding the row's new hash: it becomes the generated hash.
                self.pending.remove(&path);
                self.generated.insert(path.clone(), sha(id));
                self.outside.remove(&path);
                self.observe(&path);
            }
            Op::Touch { file } => {
                let path = tracked_file(file);
                let edits = self.human_edits.len();
                if self.disk.contains_key(&path) {
                    self.observe(&path);
                    assert_eq!(self.human_edits.len(), edits, "{path}: no change, no edit");
                }
            }
        }
    }

    fn check(&self, op: &Op) {
        // Outside edits stay on disk, never only in a backup.
        for (path, sha) in &self.outside {
            let on_disk = self.disk.get(path).map(|c| c.sha256());
            assert_eq!(
                on_disk.as_ref(),
                Some(sha),
                "after {op:?}: the outside edit of {path} is gone"
            );
        }
        // An invalid manifest stays invalid until someone outside fixes it.
        if !self.manifest_valid {
            assert!(self.needs_repair, "after {op:?}: needs_repair not set");
        }
        // Expected files that are gone are missing.
        if !self.index_lock {
            for (path, rec) in &self.rec {
                if rec.expected && !self.disk.contains_key(path) {
                    assert!(
                        self.missing.contains(path),
                        "after {op:?}: {path} forgotten"
                    );
                }
            }
        }
        for path in &self.moved_by_suite {
            assert!(
                !self.missing.contains(path),
                "after {op:?}: {path} missing after a suite move"
            );
        }
        for orphan in &self.orphans {
            assert!(
                self.disk.contains_key(orphan),
                "after {op:?}: orphan {orphan} deleted"
            );
        }
        assert!(self.queued_writes.is_empty() || self.index_lock);
    }

    /// After a final rescan the index agrees with the disk.
    fn converged(&mut self) {
        self.rescan();
        for (path, content) in &self.disk {
            if path == &manifest() && !self.manifest_valid {
                // A manifest that needs repair keeps whatever row it had.
                continue;
            }
            let tracked = self
                .rec
                .get(path)
                .filter(|r| matches!(r.kind, FileKind::Candidate | FileKind::Sealed));
            let want = match (tracked, content.lfs_oid()) {
                // A pointer to the recorded bytes (or to the recorded human edit) isn't fetched;
                // any other pointer is a human edit, indexed by its oid.
                (Some(r), Some(oid))
                    if r.sha256 == oid || r.last_human_edit.as_ref() == Some(&oid) =>
                {
                    Indexed::NotFetched
                }
                (Some(_), Some(oid)) => Indexed::Hash(oid),
                _ => Indexed::Hash(content.sha256()),
            };
            let got = self.index.get(path).cloned();
            assert_eq!(
                got.as_ref(),
                Some(&want),
                "{path}: the index disagrees with the disk"
            );
        }
        for (path, row) in &self.index {
            if !self.disk.contains_key(path) {
                assert_eq!(
                    row,
                    &Indexed::Missing,
                    "{path}: a row for a file that is gone"
                );
                assert!(self.rec.get(path).is_some_and(|r| r.expected), "{path}");
            }
        }
    }
}

fn config() -> ProptestConfig {
    ProptestConfig::with_cases(ww_testkit::params::proptest_cases())
}

proptest! {
    #![proptest_config(config())]

    #[test] // spec: QA-046, ARCH-074, ARCH-079, CORE-034, CORE-038
    fn outside_writers_never_lose_edits(ops in prop::collection::vec(op(), 1..24)) {
        let mut model = Model::new();
        for op in &ops {
            model.run(op);
            model.check(op);
        }
        model.converged();
    }
}
