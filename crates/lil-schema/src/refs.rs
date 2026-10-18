//! References (ARCH-042, CORE-031, CORE-047; I6).
//!
//! - Manifest link forms ([`LinkRef`]): `<asset_id>@<v>`, `proj:<slug>/<asset_id>@<v>`,
//!   `lib:<item>@<v>` and `style@<n>`.
//! - Canonical refs ([`AssetRef`]): a sealed version
//!   `ww://<workspace id>/<project>/<asset_id>@<major.minor.patch>`, optionally followed by
//!   `#sha256=<64 hex>`, and a candidate
//!   `ww://<workspace id>/<project>/<asset_id>@candidate/<run_id>/<file>`, which is for viewing
//!   and provenance only, never a live link or a recipe input.
//!
//! Name rules: a project slug matches `^[a-z0-9][a-z0-9-]{0,62}$`, an asset id
//! `^[a-z][a-z0-9_]{0,62}$` and a run id `^r_\d{8}_\d{4}(_\d+)?$`. A workspace id is the
//! 16 lowercase hex characters of `Env::workspace_hash`.

use std::fmt;
use std::str::FromStr;

use crate::version::Version;

/// A link as written in a manifest's `inputs`, `pinned_inputs` and recipe steps (I6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LinkRef {
    /// `<asset_id>@<version>`: an asset in the same project.
    Local { asset: String, version: Version },
    /// `proj:<slug>/<asset_id>@<version>`: an asset in another project.
    Project {
        slug: String,
        asset: String,
        version: Version,
    },
    /// `lib:<item_id>@<version>`: a library item.
    Library { item: String, version: Version },
    /// `style@<n>`: the project's style bible at version n.
    Style(u32),
}

/// A canonical `ww://` reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AssetRef {
    /// A sealed version; `sha256` is the version's content hash (bare hex), when carried.
    Sealed {
        ws: String,
        project: String,
        asset: String,
        version: Version,
        sha256: Option<String>,
    },
    /// A candidate file, for viewing and provenance only.
    Candidate {
        ws: String,
        project: String,
        asset: String,
        run: String,
        file: String,
    },
}

/// `^[a-z0-9][a-z0-9-]{0,62}$`
pub fn is_slug(s: &str) -> bool {
    name_matches(
        s,
        63,
        |b| matches!(b, b'a'..=b'z' | b'0'..=b'9'),
        |b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'),
    )
}

/// `^[a-z][a-z0-9_]{0,62}$`
pub fn is_asset_id(s: &str) -> bool {
    name_matches(
        s,
        63,
        |b| b.is_ascii_lowercase(),
        |b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'),
    )
}

/// `^r_\d{8}_\d{4}(_\d+)?$` (ASCII digits).
pub fn is_run_id(s: &str) -> bool {
    let digits = |t: &str, n: Option<usize>| {
        !t.is_empty() && n.is_none_or(|n| t.len() == n) && t.bytes().all(|b| b.is_ascii_digit())
    };
    let Some(rest) = s.strip_prefix("r_") else {
        return false;
    };
    let mut parts = rest.split('_');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(d), Some(t), suffix, None) => {
            digits(d, Some(8)) && digits(t, Some(4)) && suffix.is_none_or(|x| digits(x, None))
        }
        _ => false,
    }
}

/// A workspace id: 16 lowercase hex characters.
pub fn is_workspace_id(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A library item id: an asset id, optionally prefixed with a project slug and `_`
/// (`ember-knight_chr_knight`, I7), so `^[a-z0-9][a-z0-9_-]{0,127}$`.
pub fn is_library_item(s: &str) -> bool {
    name_matches(
        s,
        128,
        |b| matches!(b, b'a'..=b'z' | b'0'..=b'9'),
        |b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'),
    )
}

/// 1 to `max` bytes, the first passing `first` and the rest passing `rest`.
fn name_matches(s: &str, max: usize, first: fn(u8) -> bool, rest: fn(u8) -> bool) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b.len() <= max && first(b[0]) && b[1..].iter().all(|&c| rest(c))
}

/// `<name>@<version>`, with exactly one `@`.
fn split_version(s: &str) -> Result<(&str, Version), String> {
    let Some((name, version)) = s.split_once('@') else {
        return Err(format!("`{s}` has no `@<version>`"));
    };
    if version.contains('@') {
        return Err(format!("`{s}` has more than one `@`"));
    }
    Ok((name, version.parse()?))
}

impl FromStr for LinkRef {
    type Err = String;

    fn from_str(s: &str) -> Result<LinkRef, String> {
        let invalid = |why: &str| format!("`{s}` is not a link: {why}");
        if let Some(n) = s.strip_prefix("style@")
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
        {
            // `0|[1-9]\d*`, like a version part (ruling Q7).
            return crate::version::part(n)
                .and_then(|n| u32::try_from(n).ok())
                .map(LinkRef::Style)
                .ok_or_else(|| invalid("the style version has a leading zero or is too large"));
        }
        if let Some(rest) = s.strip_prefix("proj:") {
            let (target, version) = split_version(rest).map_err(|e| invalid(&e))?;
            let Some((slug, asset)) = target.split_once('/') else {
                return Err(invalid("a `proj:` link names `<slug>/<asset_id>`"));
            };
            if !is_slug(slug) {
                return Err(invalid("the project slug is not valid"));
            }
            if !is_asset_id(asset) {
                return Err(invalid("the asset id is not valid"));
            }
            return Ok(LinkRef::Project {
                slug: slug.into(),
                asset: asset.into(),
                version,
            });
        }
        if let Some(rest) = s.strip_prefix("lib:") {
            let (item, version) = split_version(rest).map_err(|e| invalid(&e))?;
            if !is_library_item(item) {
                return Err(invalid("the library item id is not valid"));
            }
            return Ok(LinkRef::Library {
                item: item.into(),
                version,
            });
        }
        let (asset, version) = split_version(s).map_err(|e| invalid(&e))?;
        if !is_asset_id(asset) {
            return Err(invalid("the asset id is not valid"));
        }
        Ok(LinkRef::Local {
            asset: asset.into(),
            version,
        })
    }
}

impl fmt::Display for LinkRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkRef::Local { asset, version } => write!(f, "{asset}@{version}"),
            LinkRef::Project {
                slug,
                asset,
                version,
            } => write!(f, "proj:{slug}/{asset}@{version}"),
            LinkRef::Library { item, version } => write!(f, "lib:{item}@{version}"),
            LinkRef::Style(n) => write!(f, "style@{n}"),
        }
    }
}

impl FromStr for AssetRef {
    type Err = String;

    fn from_str(s: &str) -> Result<AssetRef, String> {
        let invalid = |why: &str| format!("`{s}` is not a ww:// ref: {why}");
        let Some(rest) = s.strip_prefix("ww://") else {
            return Err(invalid("it must start with `ww://`"));
        };
        let (body, fragment) = match rest.split_once('#') {
            Some((body, fragment)) => (body, Some(fragment)),
            None => (rest, None),
        };
        let mut parts = body.splitn(3, '/');
        let (Some(ws), Some(project), Some(tail)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid("it names `<workspace>/<project>/<asset_id>@…`"));
        };
        let Some((asset, after)) = tail.split_once('@') else {
            return Err(invalid("it has no `@<version>`"));
        };
        if !is_workspace_id(ws) {
            return Err(invalid(
                "the workspace id is not 16 lowercase hex characters",
            ));
        }
        if !is_slug(project) {
            return Err(invalid("the project slug is not valid"));
        }
        if !is_asset_id(asset) {
            return Err(invalid("the asset id is not valid"));
        }
        if let Some(candidate) = after.strip_prefix("candidate/") {
            // `#` only ever starts the sealed form's hash, so a candidate ref holds none: it
            // carries no hash, and a file name containing `#` can't be referenced.
            if fragment.is_some() {
                return Err(invalid(
                    "a candidate ref carries no hash, and its file name can't contain `#`",
                ));
            }
            let Some((run, file)) = candidate.split_once('/') else {
                return Err(invalid("a candidate ref names `candidate/<run_id>/<file>`"));
            };
            if !is_run_id(run) {
                return Err(invalid("the run id is not valid"));
            }
            crate::paths::check_rel_path(file).map_err(|p| invalid(&p.to_string()))?;
            return Ok(AssetRef::Candidate {
                ws: ws.into(),
                project: project.into(),
                asset: asset.into(),
                run: run.into(),
                file: file.into(),
            });
        }
        let version: Version = after.parse().map_err(|e: String| invalid(&e))?;
        let sha256 = match fragment {
            None => None,
            Some(f) => {
                let hex = f
                    .strip_prefix("sha256=")
                    .filter(|h| crate::manifest::bare_sha256(h) == Some(*h))
                    .ok_or_else(|| invalid("the hash must be `#sha256=<64 lowercase hex>`"))?;
                Some(hex.to_string())
            }
        };
        Ok(AssetRef::Sealed {
            ws: ws.into(),
            project: project.into(),
            asset: asset.into(),
            version,
            sha256,
        })
    }
}

impl fmt::Display for AssetRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssetRef::Sealed {
                ws,
                project,
                asset,
                version,
                sha256,
            } => {
                write!(f, "ww://{ws}/{project}/{asset}@{version}")?;
                if let Some(hex) = sha256 {
                    write!(f, "#sha256={hex}")?;
                }
                Ok(())
            }
            AssetRef::Candidate {
                ws,
                project,
                asset,
                run,
                file,
            } => write!(f, "ww://{ws}/{project}/{asset}@candidate/{run}/{file}"),
        }
    }
}

impl AssetRef {
    /// The live topic (ARCH-044): `asset:` plus the ref without `@<version>` and hash, so one
    /// topic covers every version of the asset.
    pub fn live_topic(&self) -> String {
        let (AssetRef::Sealed {
            ws, project, asset, ..
        }
        | AssetRef::Candidate {
            ws, project, asset, ..
        }) = self;
        format!("asset:ww://{ws}/{project}/{asset}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "5a9db589b4907d4cfb2fb9a3a1da4efe5b2fa01d1b2aef2fed24ce2124a5a2a2";

    #[test] // spec: ARCH-042
    fn link_refs_round_trip_all_four_forms() {
        let cases = [
            (
                "chr_knight_concept@1.2.0",
                LinkRef::Local {
                    asset: "chr_knight_concept".into(),
                    version: Version::new(1, 2, 0),
                },
            ),
            (
                "proj:harbor/env_harbor_wall@1.0.0",
                LinkRef::Project {
                    slug: "harbor".into(),
                    asset: "env_harbor_wall".into(),
                    version: Version::new(1, 0, 0),
                },
            ),
            (
                "lib:ember-knight_chr_knight@2.0.0",
                LinkRef::Library {
                    item: "ember-knight_chr_knight".into(),
                    version: Version::new(2, 0, 0),
                },
            ),
            ("style@2", LinkRef::Style(2)),
            ("style@0", LinkRef::Style(0)),
            ("style@10", LinkRef::Style(10)),
            // An asset called `style` is a plain local ref: its version has three parts.
            (
                "style@1.0.0",
                LinkRef::Local {
                    asset: "style".into(),
                    version: Version::new(1, 0, 0),
                },
            ),
        ];
        for (text, want) in cases {
            let got: LinkRef = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(got, want, "{text}");
            assert_eq!(got.to_string(), text);
        }
    }

    #[test] // spec: ARCH-042, CORE-047, ARCH-044
    fn asset_ref_with_hash_round_trips_and_names_its_live_topic() {
        let text = format!("ww://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0#sha256={HEX}");
        let r: AssetRef = text.parse().unwrap();
        assert_eq!(
            r,
            AssetRef::Sealed {
                ws: "0123456789abcdef".into(),
                project: "sketchbook".into(),
                asset: "chr_paper_crane".into(),
                version: Version::new(1, 0, 0),
                sha256: Some(HEX.into()),
            }
        );
        assert_eq!(r.to_string(), text);
        assert_eq!(
            r.live_topic(),
            "asset:ww://0123456789abcdef/sketchbook/chr_paper_crane"
        );

        let bare: AssetRef = "ww://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0"
            .parse()
            .unwrap();
        assert!(matches!(&bare, AssetRef::Sealed { sha256: None, .. }));
        assert_eq!(
            bare.to_string(),
            "ww://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0"
        );
        assert_eq!(bare.live_topic(), r.live_topic());
    }

    #[test] // spec: ARCH-042, CORE-031
    fn candidate_ref_is_view_only_form() {
        let text = "ww://0123456789abcdef/sketchbook/chr_paper_crane@candidate/r_20261003_1400/crane_c.png";
        let r: AssetRef = text.parse().unwrap();
        assert_eq!(
            r,
            AssetRef::Candidate {
                ws: "0123456789abcdef".into(),
                project: "sketchbook".into(),
                asset: "chr_paper_crane".into(),
                run: "r_20261003_1400".into(),
                file: "crane_c.png".into(),
            }
        );
        assert_eq!(r.to_string(), text);
        assert_eq!(
            r.live_topic(),
            "asset:ww://0123456789abcdef/sketchbook/chr_paper_crane"
        );
        // A suffixed run id is fine; a candidate never carries a hash.
        assert!(
            "ww://0123456789abcdef/sketchbook/chr_paper_crane@candidate/r_20261003_1400_2/a.png"
                .parse::<AssetRef>()
                .is_ok()
        );
        assert!(
            format!(
                "ww://0123456789abcdef/sketchbook/chr_paper_crane@candidate/r_20261003_1400/a.png#sha256={HEX}"
            )
            .parse::<AssetRef>()
            .is_err()
        );
        // The candidate form is never a manifest link.
        assert!(
            "chr_paper_crane@candidate/r_20261003_1400/a.png"
                .parse::<LinkRef>()
                .is_err()
        );
    }

    #[test] // spec: ARCH-042, CORE-047
    fn bad_refs_are_rejected() {
        let hex63 = &HEX[..63];
        let bad_asset_refs = [
            "ww://0123456789abcdef/Sketchbook/chr_paper_crane@1.0.0".to_string(),
            "ww://0123456789abcdef/sketchbook/chr_paper_crane@1.0".to_string(),
            format!("ww://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0#sha256={hex63}"),
            format!("ww://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0#sha256={HEX}0"),
            format!(
                "ww://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0#sha256={}",
                HEX.to_uppercase()
            ),
            "ww://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0#md5=00".to_string(),
            "ww://0123456789abcdef/sketchbook/chr_paper_crane".to_string(),
            "ww://0123456789ABCDEF/sketchbook/chr_paper_crane@1.0.0".to_string(),
            "ww://0123/sketchbook/chr_paper_crane@1.0.0".to_string(),
            "ww://0123456789abcdef/sketchbook/Chr_paper_crane@1.0.0".to_string(),
            "ww://0123456789abcdef/sketchbook/1chr@1.0.0".to_string(),
            "ww://0123456789abcdef/-sketchbook/chr_paper_crane@1.0.0".to_string(),
            "ww://0123456789abcdef/sketchbook/extra/chr_paper_crane@1.0.0".to_string(),
            "ww:/0123456789abcdef/sketchbook/chr_paper_crane@1.0.0".to_string(),
            "ww://0123456789abcdef/sketchbook/chr_paper_crane@candidate/run1/a.png".to_string(),
            "ww://0123456789abcdef/sketchbook/chr_paper_crane@candidate/r_20261003_1400/"
                .to_string(),
            "ww://0123456789abcdef/sketchbook/chr_paper_crane@candidate/r_20261003_1400/../x"
                .to_string(),
            "ww://0123456789abcdef/sketchbook/chr_paper_crane@candidate/r_20261003_1400/con.png"
                .to_string(),
            // `#` is refused in candidate file names (it only starts the sealed hash).
            "ww://0123456789abcdef/sketchbook/chr_paper_crane@candidate/r_20261003_1400/a#b.png"
                .to_string(),
            // Ruling Q7: no leading zeros.
            "ww://0123456789abcdef/sketchbook/chr_paper_crane@01.0.0".to_string(),
        ];
        for text in &bad_asset_refs {
            assert!(text.parse::<AssetRef>().is_err(), "{text} parsed");
        }
        for text in [
            "Chr@1.0.0",
            "chr@1.0",
            "chr@v1.0.0",
            "proj:Harbor/chr@1.0.0",
            "proj:harbor@1.0.0",
            "proj:harbor/chr",
            "lib:pal_ember",
            "lib:Pal@1.0.0",
            "style@",
            "style@x",
            "style@-1",
            "",
            "@1.0.0",
            "chr@1.0.0@1.0.0",
            "style@02",
            "style@00",
            "chr@1.00.0",
            "style@99999999999",
        ] {
            assert!(text.parse::<LinkRef>().is_err(), "{text} parsed");
        }

        let long_slug = format!("a{}", "b".repeat(62));
        assert!(is_slug(&long_slug));
        assert!(!is_slug(&format!("{long_slug}c")));
        assert!(is_slug("0-sketch"));
        assert!(!is_slug("sketch_book"));
        assert!(!is_slug(""));
        assert!(is_asset_id("chr_paper_crane"));
        assert!(!is_asset_id("_chr"));
        assert!(!is_asset_id("chr-crane"));
        assert!(!is_asset_id(&format!("a{}", "b".repeat(63))));
        assert!(is_run_id("r_20261003_1400"));
        assert!(is_run_id("r_20261003_1400_12"));
        assert!(!is_run_id("r_20261003_1400_"));
        assert!(!is_run_id("r_2026100_1400"));
        assert!(!is_run_id("R_20261003_1400"));
        assert!(!is_run_id("r_20261003_1400_x"));
    }
}
