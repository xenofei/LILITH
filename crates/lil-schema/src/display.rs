//! The display file and media kinds (Appendix H "`assets.query`" and "Thumbnails"; APP-038,
//! APP-147; P4b-R30, P4b-R41).
//!
//! [`display_file`] is the one answer to "which file shows this asset": the approved version's
//! primary file (the first `selected` name of the run signed off as `approved_version`); else
//! the newest open run's lowest-numbered take's primary, or that take's `preview` companion
//! when the primary's media kind has no renderer; else none. [`media_kind_of`] reads a file's
//! kind from its extension alone; images are confirmed by their magic bytes where they are
//! decoded, not here.

use std::fmt;

use crate::manifest::{CandidateRun, Manifest};
use crate::takes::{file_name, takes_of};

/// A file's media kind (APP-038). Wire values are snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    Model3d,
    Document,
    Text,
    Archive,
    Font,
    Other,
}

impl MediaKind {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
            MediaKind::Model3d => "model_3d",
            MediaKind::Document => "document",
            MediaKind::Text => "text",
            MediaKind::Archive => "archive",
            MediaKind::Font => "font",
            MediaKind::Other => "other",
        }
    }

    /// Whether a renderer draws this kind: only `Image` in plan 4b (`image@1`); every other
    /// kind gets the `icon@1` state.
    pub fn has_renderer(self) -> bool {
        self == MediaKind::Image
    }
}

impl fmt::Display for MediaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The extensions of each kind, lowercase: Appendix H's LFS baseline classes (raster, 3D,
/// audio, video, fonts, archives), and the document and text kinds.
const EXTENSIONS: [(MediaKind, &[&str]); 8] = [
    (
        MediaKind::Image,
        &[
            "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "psd", "exr", "hdr", "tga",
            "dds", "ktx2", "avif",
        ],
    ),
    (
        MediaKind::Model3d,
        &[
            "glb", "gltf", "fbx", "obj", "blend", "usd", "usdc", "usdz", "abc",
        ],
    ),
    (
        MediaKind::Audio,
        &["wav", "flac", "ogg", "mp3", "m4a", "aif", "aiff"],
    ),
    (MediaKind::Video, &["mp4", "mov", "webm", "mkv", "avi"]),
    (MediaKind::Font, &["ttf", "otf", "woff", "woff2"]),
    (MediaKind::Archive, &["zip", "7z", "rar", "tar", "gz"]),
    (
        MediaKind::Document,
        &[
            "pdf", "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx", "odp",
        ],
    ),
    (
        MediaKind::Text,
        &[
            "txt", "md", "json", "yaml", "yml", "toml", "csv", "xml", "ini", "log",
        ],
    ),
];

/// The media kind of a file name or path, from its extension (compared without regard to
/// case); a name without a known extension is `Other`.
pub fn media_kind_of(name: &str) -> MediaKind {
    let name = file_name(name);
    let Some((_, ext)) = name.rsplit_once('.') else {
        return MediaKind::Other;
    };
    let ext = ext.to_ascii_lowercase();
    EXTENSIONS
        .iter()
        .find(|(_, exts)| exts.contains(&ext.as_str()))
        .map_or(MediaKind::Other, |(kind, _)| *kind)
}

/// The extensions the `image@1` renderer decodes (PNG, JPEG, WebP).
const RENDERED: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// Whether a renderer draws this file itself: an `Image` whose extension `image@1` decodes
/// (`png`, `jpg`, `jpeg`, `webp`, compared without regard to case). Other raster formats
/// (`psd`, `tif`, `exr`, …) are `Image` for icons and facets but aren't rendered, so
/// [`display_file`] shows their take's `preview` instead. Task 12 pins this against the
/// renderer registry.
pub fn renderable(name: &str) -> bool {
    media_kind_of(name) == MediaKind::Image
        && file_name(name)
            .rsplit_once('.')
            .is_some_and(|(_, ext)| RENDERED.contains(&ext.to_ascii_lowercase().as_str()))
}

/// The file that shows an asset (P4b-R30).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayFile {
    /// Relative to the asset folder (`v1.0.0/crane_b.png`, `candidates/<run>/x.png`).
    pub path: String,
    /// Bare hex, when the manifest records it (`hashes`, or the run file's `sha256`).
    pub sha256: Option<String>,
    pub media_kind: MediaKind,
}

/// The display file of `m` (P4b-R30): the approved version's primary file; else the newest
/// open run's lowest-numbered take's primary; in both, the take's `preview` companion instead
/// when the primary isn't [`renderable`] (Appendix H "Takes"); else `None`.
///
/// The approved version's primary is the first `selected` name of the run whose
/// `signed_off.version` is `approved_version`, at `v<version>/<name>`; its preview is used
/// only when it was sealed there too (in `hashes`). "Newest" is the open
/// run (not trashed, with at least one take) with the latest `created`, the later one in the
/// list on a tie or when neither has one.
pub fn display_file(m: &Manifest) -> Option<DisplayFile> {
    let view = m.view();
    if let Some(approved) = &view.approved_version {
        let primary = view
            .candidate_runs
            .iter()
            .rev()
            .filter(|r| {
                r.signed_off
                    .as_ref()
                    .is_some_and(|s| &s.version == approved)
            })
            .find_map(|r| r.selected.first().map(|name| (r, file_name(name))));
        if let Some((run, name)) = primary {
            let sealed_path = |name: &str| format!("v{approved}/{name}");
            let preview = if renderable(name) {
                None
            } else {
                takes_of(run)
                    .into_iter()
                    .find(|t| t.primary.eq_ignore_ascii_case(name))
                    .and_then(|t| t.preview)
                    .filter(|p| view.hashes.contains_key(&sealed_path(p)))
            };
            let shown = preview.as_deref().unwrap_or(name);
            let path = sealed_path(shown);
            let sha256 = view.hashes.get(&path).cloned();
            return Some(DisplayFile {
                media_kind: media_kind_of(shown),
                path,
                sha256,
            });
        }
    }

    let newest = view
        .candidate_runs
        .iter()
        .filter(|r| r.status.as_deref() == Some("open") && r.trashed_utc.is_none())
        .filter(|r| !takes_of(r).is_empty())
        .fold(None::<&CandidateRun>, |best, r| match best {
            Some(b) if b.created > r.created => Some(b),
            _ => Some(r),
        })?;
    let take = takes_of(newest).into_iter().min_by_key(|t| t.n)?;
    let name = match &take.preview {
        Some(preview) if !renderable(&take.primary) => preview.clone(),
        _ => take.primary,
    };
    let file = newest.files.iter().find(|f| file_name(&f.path) == name)?;
    Some(DisplayFile {
        path: file.path.clone(),
        sha256: file.sha256.clone(),
        media_kind: media_kind_of(&name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn manifest(v: &Value) -> Manifest {
        Manifest::parse(&v.to_string()).expect("a JSON object").0
    }

    fn run(id: &str, status: &str, created: &str, files: &Value) -> Value {
        json!({"run_id": id, "path": format!("candidates/{id}/"), "status": status,
               "created": created, "files": files})
    }

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test] // spec: APP-038
    fn media_kind_follows_the_extension_table() {
        assert_eq!(media_kind_of("a.PNG"), MediaKind::Image);
        assert_eq!(media_kind_of("v1.0.0/b.jpeg"), MediaKind::Image);
        assert_eq!(media_kind_of("model.glb"), MediaKind::Model3d);
        assert_eq!(media_kind_of("song.flac"), MediaKind::Audio);
        assert_eq!(media_kind_of("clip.webm"), MediaKind::Video);
        assert_eq!(media_kind_of("font.woff2"), MediaKind::Font);
        assert_eq!(media_kind_of("pack.7z"), MediaKind::Archive);
        assert_eq!(media_kind_of("brief.pdf"), MediaKind::Document);
        assert_eq!(media_kind_of("notes.txt"), MediaKind::Text);
        assert_eq!(media_kind_of("no_extension"), MediaKind::Other);
        assert_eq!(media_kind_of("odd.xyz"), MediaKind::Other);
        assert_eq!(media_kind_of("dir.png/file"), MediaKind::Other);
        assert_eq!(MediaKind::Model3d.as_str(), "model_3d");
        let with_renderer: Vec<MediaKind> = [
            MediaKind::Image,
            MediaKind::Video,
            MediaKind::Audio,
            MediaKind::Model3d,
            MediaKind::Document,
            MediaKind::Text,
            MediaKind::Archive,
            MediaKind::Font,
            MediaKind::Other,
        ]
        .into_iter()
        .filter(|k| k.has_renderer())
        .collect();
        assert_eq!(with_renderer, [MediaKind::Image], "only images in plan 4b");
        // Only the image@1 formats are renderable.
        for name in ["a.png", "b.JPG", "c.jpeg", "v1.0.0/d.WebP"] {
            assert!(renderable(name), "{name}");
        }
        for name in ["a.psd", "b.tif", "c.exr", "d.gif", "e.glb", "f.txt", "png"] {
            assert!(!renderable(name), "{name}");
        }
    }

    #[test] // spec: APP-147
    fn display_prefers_the_approved_primary_then_the_newest_take() {
        // An approved version: its run's first selected name, at v<version>/, hash from
        // `hashes`; the open run is ignored.
        let approved = manifest(&json!({
            "asset_id": "a", "project": "p", "approved_version": "1.0.0",
            "candidate_runs": [
                {"run_id": "r1", "status": "selected", "created": "2026-10-01T00:00:00Z",
                 "files": [{"path": "candidates/r1/x.png", "role": "candidate"},
                           {"path": "candidates/r1/y.png", "role": "candidate"}],
                 "selected": ["y.png", "x.png"],
                 "signed_off": {"by": "user", "date": "2026-10-01", "version": "1.0.0"}},
                run("r2", "open", "2026-10-02T00:00:00Z",
                    &json!([{"path": "candidates/r2/z.png", "role": "candidate"}])),
            ],
            "hashes": {"v1.0.0/y.png": format!("sha256:{SHA_A}")},
        }));
        assert_eq!(
            display_file(&approved),
            Some(DisplayFile {
                path: "v1.0.0/y.png".into(),
                sha256: Some(SHA_A.into()),
                media_kind: MediaKind::Image,
            })
        );

        // No approved version: the newest open run (by `created`, not list order), its
        // lowest take's primary.
        let open = manifest(&json!({
            "asset_id": "a", "project": "p",
            "candidate_runs": [
                run("r_new", "open", "2026-10-05T00:00:00Z", &json!([
                    {"path": "candidates/r_new/two.png", "role": "candidate", "take": 2,
                     "primary": true},
                    {"path": "candidates/r_new/one.png", "role": "candidate", "take": 1,
                     "primary": true, "sha256": SHA_B},
                ])),
                run("r_old", "open", "2026-10-01T00:00:00Z",
                    &json!([{"path": "candidates/r_old/old.png", "role": "candidate"}])),
                run("r_closed", "rejected", "2026-10-09T00:00:00Z",
                    &json!([{"path": "candidates/r_closed/c.png", "role": "candidate"}])),
            ],
        }));
        assert_eq!(
            display_file(&open),
            Some(DisplayFile {
                path: "candidates/r_new/one.png".into(),
                sha256: Some(SHA_B.into()),
                media_kind: MediaKind::Image,
            })
        );

        // A primary without a renderer shows its preview; without a preview, itself.
        let model = |files: Value| {
            manifest(&json!({"asset_id": "a", "project": "p",
                             "candidate_runs": [run("r", "open", "2026-10-01T00:00:00Z", &files)]}))
        };
        let with_preview = model(json!([
            {"path": "candidates/r/m.glb", "role": "candidate"},
            {"path": "candidates/r/m.png", "role": "preview", "from": "m.glb"},
        ]));
        let d = display_file(&with_preview).unwrap();
        assert_eq!(
            (d.path.as_str(), d.media_kind),
            ("candidates/r/m.png", MediaKind::Image)
        );
        let bare = model(json!([{"path": "candidates/r/m.glb", "role": "candidate"}]));
        let d = display_file(&bare).unwrap();
        assert_eq!(
            (d.path.as_str(), d.media_kind),
            ("candidates/r/m.glb", MediaKind::Model3d)
        );
        // A raster primary `image@1` can't decode shows its PNG preview too.
        let psd = model(json!([
            {"path": "candidates/r/art.psd", "role": "candidate"},
            {"path": "candidates/r/art.png", "role": "preview", "from": "art.psd"},
        ]));
        let d = display_file(&psd).unwrap();
        assert_eq!(
            (d.path.as_str(), d.media_kind),
            ("candidates/r/art.png", MediaKind::Image)
        );
        // An image primary ignores its preview.
        let image = model(json!([
            {"path": "candidates/r/i.png", "role": "candidate"},
            {"path": "candidates/r/i_small.png", "role": "preview", "from": "i.png"},
        ]));
        assert_eq!(display_file(&image).unwrap().path, "candidates/r/i.png");

        // An approved `.glb` whose preview was sealed beside it shows the sealed PNG; without
        // the sealed preview, the `.glb` itself.
        let approved_model = |hashes: Value| {
            manifest(&json!({
                "asset_id": "a", "project": "p", "approved_version": "1.0.0",
                "candidate_runs": [
                    {"run_id": "r1", "status": "selected", "created": "2026-10-01T00:00:00Z",
                     "files": [{"path": "candidates/r1/m.glb", "role": "candidate"},
                               {"path": "candidates/r1/m.png", "role": "preview",
                                "from": "m.glb"}],
                     "selected": ["m.glb"],
                     "signed_off": {"by": "user", "date": "2026-10-01", "version": "1.0.0",
                                    "companions": ["m.png"]}},
                ],
                "hashes": hashes,
            }))
        };
        let sealed = approved_model(json!({
            "v1.0.0/m.glb": format!("sha256:{SHA_A}"),
            "v1.0.0/m.png": format!("sha256:{SHA_B}"),
        }));
        assert_eq!(
            display_file(&sealed),
            Some(DisplayFile {
                path: "v1.0.0/m.png".into(),
                sha256: Some(SHA_B.into()),
                media_kind: MediaKind::Image,
            })
        );
        let unsealed = approved_model(json!({"v1.0.0/m.glb": format!("sha256:{SHA_A}")}));
        assert_eq!(display_file(&unsealed).unwrap().path, "v1.0.0/m.glb");

        // A trashed open run, or no run at all: none.
        let trashed = manifest(&json!({"asset_id": "a", "project": "p", "candidate_runs": [
            {"run_id": "r", "status": "open", "trashed_utc": "2026-10-03T00:00:00Z",
             "files": [{"path": "candidates/r/t.png", "role": "candidate"}]}]}));
        assert_eq!(display_file(&trashed), None);
        assert_eq!(
            display_file(&manifest(&json!({"asset_id": "a", "project": "p"}))),
            None
        );
        // An approved version no run signed off (a legacy manifest) falls through.
        let legacy = manifest(&json!({"asset_id": "a", "project": "p",
            "approved_version": "1.0.0", "candidate_runs": []}));
        assert_eq!(display_file(&legacy), None);
    }
}
