//! GitHub access for `ww dev`: a small trait, the real REST client and (in `github_fake`) an
//! in-memory fake. The real [`RestClient`] is never exercised by unit tests (QA-031: no network);
//! tests cover the pure request-building and response-parsing functions and the fake.
//!
//! Endpoints used (all against `https://api.github.com`):
//!
//! | Method | Endpoint |
//! |---|---|
//! | `pr` | `GET /repos/{o}/{r}/pulls/{n}` |
//! | `check` | `GET /repos/{o}/{r}/commits/{sha}/check-runs?check_name={name}&filter=latest`, trusting only runs from the `github-actions` app |
//! | `reviews` | `GET /repos/{o}/{r}/pulls/{n}/reviews?per_page=100`, following `Link: rel="next"` |
//! | `merge` | `PUT /repos/{o}/{r}/pulls/{n}/merge` with `{"sha", "merge_method": "merge"}` |
//! | `post_review_as_app` | `POST /repos/{o}/{r}/pulls/{n}/reviews` with the installation token |
//! | `commit_pulls` | `GET /repos/{o}/{r}/commits/{sha}/pulls?per_page=100`, following `Link: rel="next"` |
//!
//! The owner and repo always come from the committed `dev/merge-guard.toml` (ruling R12); there
//! are no hard-coded defaults here.

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Method;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, LINK, USER_AGENT};
use serde_json::{Value, json};
use std::fmt;
use std::time::Duration;
use zeroize::Zeroizing;

pub const API_BASE: &str = "https://api.github.com";
/// The branch whose committed `dev/merge-guard.toml` `ww dev merge` trusts.
pub const BASE_BRANCH: &str = "main";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub head_sha: String,
    pub base_ref: String,
    pub state: String,
    pub draft: bool,
}

/// A pull request GitHub associates with a commit (`GET .../commits/{sha}/pulls`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPull {
    pub number: u64,
    /// The PR's head commit; for a merged PR, the head it was merged at.
    pub head_sha: String,
    pub base_ref: String,
    /// GitHub's `merged_at` is set.
    pub merged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckState {
    Success,
    Pending,
    /// Carries GitHub's `conclusion` (`failure`, `neutral`, `skipped`, `cancelled`, ...).
    Failure(String),
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub login: String,
    pub state: String,
    pub commit_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged {
        sha: String,
    },
    /// GitHub answered 409: the PR head no longer equals the expected SHA.
    HeadMoved,
    /// GitHub answered 405: the PR is not mergeable (carries GitHub's message).
    NotMergeable(String),
}

pub trait GitHubApi {
    fn pr(&self, number: u64) -> Result<PrInfo>;
    /// The most recent check run named `name` on `sha`.
    fn check(&self, sha: &str, name: &str) -> Result<CheckState>;
    /// Every review on the PR (all pages).
    fn reviews(&self, number: u64) -> Result<Vec<Review>>;
    /// `PUT .../merge` with `"sha": expected_head_sha` and `"merge_method": "merge"`.
    fn merge(&self, number: u64, expected_head_sha: &str) -> Result<MergeOutcome>;
    /// Posts an APPROVE review using `installation_token` (the app's), not the user's token.
    fn post_review_as_app(
        &self,
        number: u64,
        sha: &str,
        body: &str,
        installation_token: &str,
    ) -> Result<()>;
    /// Every PR GitHub associates with commit `sha` (all pages). For a commit on the default
    /// branch that is the merged PR that brought it in; an empty list means no PR. An error is
    /// an error: callers treat it as a flag, never as "no PR" or as a pass.
    fn commit_pulls(&self, sha: &str) -> Result<Vec<CommitPull>>;
}

// ---------------------------------------------------------------------------------------------
// Pure request builders (paths are relative to `API_BASE`).
// ---------------------------------------------------------------------------------------------

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn build_pr_request(owner: &str, repo: &str, number: u64) -> (Method, String) {
    (Method::GET, format!("/repos/{owner}/{repo}/pulls/{number}"))
}

pub fn build_check_runs_request(
    owner: &str,
    repo: &str,
    sha: &str,
    name: &str,
) -> (Method, String) {
    (
        Method::GET,
        format!(
            "/repos/{owner}/{repo}/commits/{sha}/check-runs?check_name={}&filter=latest",
            percent_encode(name)
        ),
    )
}

pub fn build_reviews_request(owner: &str, repo: &str, number: u64) -> (Method, String) {
    (
        Method::GET,
        format!("/repos/{owner}/{repo}/pulls/{number}/reviews?per_page=100"),
    )
}

pub fn build_commit_pulls_request(owner: &str, repo: &str, sha: &str) -> (Method, String) {
    (
        Method::GET,
        format!(
            "/repos/{owner}/{repo}/commits/{}/pulls?per_page=100",
            percent_encode(sha)
        ),
    )
}

pub fn build_merge_request(
    owner: &str,
    repo: &str,
    number: u64,
    sha: &str,
) -> (Method, String, Value) {
    (
        Method::PUT,
        format!("/repos/{owner}/{repo}/pulls/{number}/merge"),
        json!({ "sha": sha, "merge_method": "merge" }),
    )
}

pub fn build_review_request(
    owner: &str,
    repo: &str,
    number: u64,
    sha: &str,
    body: &str,
) -> (Method, String, Value) {
    (
        Method::POST,
        format!("/repos/{owner}/{repo}/pulls/{number}/reviews"),
        json!({ "commit_id": sha, "event": "APPROVE", "body": body }),
    )
}

// ---------------------------------------------------------------------------------------------
// Pure response parsers.
// ---------------------------------------------------------------------------------------------

pub fn parse_pr(v: &Value) -> Result<PrInfo> {
    let text = |val: Option<&Value>, what: &str| -> Result<String> {
        val.and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("PR response is missing `{what}`"))
    };
    Ok(PrInfo {
        number: v["number"]
            .as_u64()
            .ok_or_else(|| anyhow!("PR response is missing `number`"))?,
        head_sha: text(v.pointer("/head/sha"), "head.sha")?,
        base_ref: text(v.pointer("/base/ref"), "base.ref")?,
        state: text(v.get("state"), "state")?,
        // Fail closed: a response without `draft` is not read as "ready for review".
        draft: v["draft"]
            .as_bool()
            .ok_or_else(|| anyhow!("PR response is missing `draft`"))?,
    })
}

/// The only check-run app the merge gate trusts (controller ruling R11). Any app with
/// `checks:write` can post a same-named run, so runs from other apps are treated as absent.
/// Matched on the app slug; the numeric app id is deliberately not hard-coded.
pub const TRUSTED_CHECK_APP_SLUG: &str = "github-actions";

/// Maps the latest trusted run in a check-runs response. `neutral` and `skipped` are failures:
/// they are not success. A run that has not completed (`queued`, `in_progress`, ...) is
/// `Pending`. Runs whose `app.slug` is not [`TRUSTED_CHECK_APP_SLUG`] are ignored, so an empty
/// list (or one holding only foreign runs) is `Missing`. A body without a `check_runs` array
/// (HTML, garbage, an error object) is an error, never `Missing`.
pub fn parse_check_state(v: &Value) -> Result<CheckState> {
    let runs = v["check_runs"]
        .as_array()
        .ok_or_else(|| anyhow!("check-runs response has no `check_runs` array"))?;
    let latest = runs
        .iter()
        .filter(|r| r.pointer("/app/slug").and_then(Value::as_str) == Some(TRUSTED_CHECK_APP_SLUG))
        .max_by_key(|r| r["id"].as_u64().unwrap_or(0));
    let Some(run) = latest else {
        return Ok(CheckState::Missing);
    };
    if run["status"].as_str() != Some("completed") {
        return Ok(CheckState::Pending);
    }
    Ok(match run["conclusion"].as_str() {
        Some("success") => CheckState::Success,
        Some(other) => CheckState::Failure(other.to_owned()),
        None => CheckState::Failure("none".to_owned()),
    })
}

pub fn parse_reviews(v: &Value) -> Result<Vec<Review>> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("reviews response is not an array"))?;
    Ok(arr
        .iter()
        .map(|r| Review {
            // GitHub returns a null user for deleted accounts; it shows them as "ghost".
            login: r
                .pointer("/user/login")
                .and_then(Value::as_str)
                .unwrap_or("ghost")
                .to_owned(),
            state: r["state"].as_str().unwrap_or_default().to_owned(),
            commit_id: r["commit_id"].as_str().unwrap_or_default().to_owned(),
        })
        .collect())
}

/// Parses one page of `GET .../commits/{sha}/pulls`. Fails closed: a body that is not an array,
/// or an entry without `number`, `head.sha`, `base.ref` or a `merged_at` key (null or a time), is
/// an error, never an empty list.
pub fn parse_commit_pulls(v: &Value) -> Result<Vec<CommitPull>> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("commit pulls response is not an array"))?;
    arr.iter()
        .map(|p| {
            let text = |ptr: &str| -> Result<String> {
                p.pointer(ptr)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("commit pulls entry is missing `{ptr}`"))
            };
            let merged = match p.get("merged_at") {
                Some(Value::Null) => false,
                Some(Value::String(t)) if !t.is_empty() => true,
                _ => bail!("commit pulls entry has no usable `merged_at`"),
            };
            Ok(CommitPull {
                number: p["number"]
                    .as_u64()
                    .ok_or_else(|| anyhow!("commit pulls entry is missing `number`"))?,
                head_sha: text("/head/sha")?,
                base_ref: text("/base/ref")?,
                merged,
            })
        })
        .collect()
}

pub fn parse_merge_response(status: u16, body: &Value) -> Result<MergeOutcome> {
    let message = body["message"].as_str().unwrap_or("no message").to_owned();
    match status {
        200 => Ok(MergeOutcome::Merged {
            sha: body["sha"]
                .as_str()
                .ok_or_else(|| anyhow!("merge response is missing `sha`"))?
                .to_owned(),
        }),
        409 => Ok(MergeOutcome::HeadMoved),
        405 => Ok(MergeOutcome::NotMergeable(message)),
        other => bail!("GitHub merge failed with HTTP {other}: {message}"),
    }
}

/// Extracts the `rel="next"` URL from a `Link` header value.
pub fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        let is_next = params
            .split(';')
            .any(|p| p.trim().replace(' ', "") == "rel=\"next\"");
        let url = url.trim().strip_prefix('<')?.strip_suffix('>')?;
        is_next.then(|| url.to_owned())
    })
}

// ---------------------------------------------------------------------------------------------
// Token handling.
// ---------------------------------------------------------------------------------------------

/// The token for every `ww dev` GitHub call: `GITHUB_TOKEN` (CI), otherwise the agent token
/// stored in Windows Credential Manager, otherwise an error explaining `ww dev agent-token set`
/// ([`super::agent_access::resolve_token`]). There is no fallback to `gh auth token`: that is
/// the owner's full-access login. The token is never printed or logged here.
pub fn github_token() -> Result<Zeroizing<String>> {
    super::agent_access::github_token()
}

// ---------------------------------------------------------------------------------------------
// The real client.
// ---------------------------------------------------------------------------------------------

pub struct RestClient {
    http: Client,
    base: String,
    owner: String,
    repo: String,
    token: Zeroizing<String>,
}

impl fmt::Debug for RestClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestClient")
            .field("base", &self.base)
            .field("owner", &self.owner)
            .field("repo", &self.repo)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl RestClient {
    pub fn new(owner: &str, repo: &str, token: &str) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("ww-dev"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        let http = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            http,
            base: API_BASE.to_owned(),
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            token: Zeroizing::new(token.to_owned()),
        })
    }

    fn auth(token: &str) -> Result<HeaderValue> {
        let mut v = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
            anyhow!("the GitHub token contains characters that are not valid in a header")
        })?;
        v.set_sensitive(true);
        Ok(v)
    }

    /// Sends one request and returns (status, Link header, parsed JSON body or `Null`).
    fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
        token: &str,
    ) -> Result<(u16, Option<String>, Value)> {
        let mut req = self
            .http
            .request(method.clone(), url)
            .header(AUTHORIZATION, Self::auth(token)?);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().with_context(|| format!("{method} {url}"))?;
        let status = resp.status().as_u16();
        let link = resp
            .headers()
            .get(LINK)
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned);
        let text = resp
            .text()
            .with_context(|| format!("reading the response of {method} {url}"))?;
        let json = serde_json::from_str(&text).unwrap_or(Value::Null);
        Ok((status, link, json))
    }

    fn expect_ok(method: &Method, url: &str, status: u16, body: &Value) -> Result<()> {
        if (200..300).contains(&status) {
            return Ok(());
        }
        let message = body["message"].as_str().unwrap_or("no message");
        bail!("GitHub {method} {url} returned HTTP {status}: {message}")
    }

    fn abs(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// GETs `path` and every `Link: rel="next"` page after it, returning each page's body.
    /// Never follows a link off GitHub's API, since the request carries the token.
    fn get_pages(&self, path: &str, what: &str) -> Result<Vec<Value>> {
        let m = Method::GET;
        let mut url = self.abs(path);
        let mut pages = Vec::new();
        // GitHub caps pagination well below this; the bound only stops a pathological loop.
        for _ in 0..100 {
            let (status, link, body) = self.send(m.clone(), &url, None, &self.token)?;
            Self::expect_ok(&m, &url, status, &body)?;
            pages.push(body);
            match link.as_deref().and_then(next_link) {
                Some(next) => {
                    if !next.starts_with(&format!("{}/", self.base)) {
                        bail!("refusing to follow a pagination link outside {}", self.base);
                    }
                    url = next;
                }
                None => return Ok(pages),
            }
        }
        bail!("more than 100 pages of {what}")
    }
}

impl GitHubApi for RestClient {
    fn pr(&self, number: u64) -> Result<PrInfo> {
        let (m, path) = build_pr_request(&self.owner, &self.repo, number);
        let url = self.abs(&path);
        let (status, _, body) = self.send(m.clone(), &url, None, &self.token)?;
        Self::expect_ok(&m, &url, status, &body)?;
        parse_pr(&body)
    }

    fn check(&self, sha: &str, name: &str) -> Result<CheckState> {
        let (m, path) = build_check_runs_request(&self.owner, &self.repo, sha, name);
        let url = self.abs(&path);
        let (status, _, body) = self.send(m.clone(), &url, None, &self.token)?;
        Self::expect_ok(&m, &url, status, &body)?;
        parse_check_state(&body).with_context(|| format!("{m} {url} (HTTP {status})"))
    }

    fn reviews(&self, number: u64) -> Result<Vec<Review>> {
        let (_, path) = build_reviews_request(&self.owner, &self.repo, number);
        let mut all = Vec::new();
        for page in self.get_pages(&path, &format!("reviews on PR #{number}"))? {
            all.extend(parse_reviews(&page)?);
        }
        Ok(all)
    }

    fn merge(&self, number: u64, expected_head_sha: &str) -> Result<MergeOutcome> {
        let (m, path, body) =
            build_merge_request(&self.owner, &self.repo, number, expected_head_sha);
        let (status, _, resp) = self.send(m, &self.abs(&path), Some(&body), &self.token)?;
        parse_merge_response(status, &resp)
    }

    fn post_review_as_app(
        &self,
        number: u64,
        sha: &str,
        body: &str,
        installation_token: &str,
    ) -> Result<()> {
        let (m, path, payload) = build_review_request(&self.owner, &self.repo, number, sha, body);
        let url = self.abs(&path);
        let (status, _, resp) = self.send(m.clone(), &url, Some(&payload), installation_token)?;
        Self::expect_ok(&m, &url, status, &resp)
    }

    fn commit_pulls(&self, sha: &str) -> Result<Vec<CommitPull>> {
        let (_, path) = build_commit_pulls_request(&self.owner, &self.repo, sha);
        let mut all = Vec::new();
        for page in self.get_pages(&path, &format!("pull requests for commit {sha}"))? {
            all.extend(parse_commit_pulls(&page)?);
        }
        Ok(all)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev::github_fake::FakeGitHub;
    use reqwest::Method;

    #[test]
    fn merge_request_pins_sha() {
        let (method, path, body) = build_merge_request("xenofei", "Worldwright-Suite", 7, "abc123");
        assert_eq!(method, Method::PUT);
        assert_eq!(path, "/repos/xenofei/Worldwright-Suite/pulls/7/merge");
        assert_eq!(body["sha"], "abc123");
        assert_eq!(body["merge_method"], "merge");
        let text = body.to_string();
        assert!(text.contains("\"sha\":\"abc123\""), "{text}");
        assert!(text.contains("\"merge_method\":\"merge\""), "{text}");
    }

    #[test]
    fn fake_merge_detects_head_move() {
        let fake = FakeGitHub::new();
        fake.add_pr(7, "sha1", "main");
        fake.set_head(7, "sha2");
        assert_eq!(fake.merge(7, "sha1").unwrap(), MergeOutcome::HeadMoved);
        assert_eq!(fake.pr(7).unwrap().state, "open");
        assert_eq!(
            fake.merge(7, "sha2").unwrap(),
            MergeOutcome::Merged {
                sha: "merge-of-sha2".into()
            }
        );
        assert_eq!(fake.pr(7).unwrap().state, "closed");
    }

    #[test]
    fn check_missing_when_no_run() {
        let fake = FakeGitHub::new();
        fake.add_pr(7, "sha1", "main");
        assert_eq!(fake.check("sha1", "guard").unwrap(), CheckState::Missing);
        fake.set_check("sha1", "guard", CheckState::Success);
        assert_eq!(fake.check("sha1", "guard").unwrap(), CheckState::Success);
        assert_eq!(fake.check("sha1", "other").unwrap(), CheckState::Missing);
        assert_eq!(fake.check("sha2", "guard").unwrap(), CheckState::Missing);
    }

    #[test]
    fn token_never_in_debug_output() {
        let client = RestClient::new("xenofei", "Worldwright-Suite", "ghp_SECRETTOKEN123").unwrap();
        let shown = format!("{client:?}");
        assert!(!shown.contains("ghp_SECRETTOKEN123"), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
        assert!(shown.contains("Worldwright-Suite"), "{shown}");
    }

    #[test]
    fn check_runs_request_encodes_name() {
        let (m, p) = build_check_runs_request("xenofei", "Worldwright-Suite", "abc", "ww guard/pr");
        assert_eq!(m, Method::GET);
        assert_eq!(
            p,
            "/repos/xenofei/Worldwright-Suite/commits/abc/check-runs?check_name=ww%20guard%2Fpr&filter=latest"
        );
    }

    #[test]
    fn review_request_uses_approve_event() {
        let (m, p, body) = build_review_request("o", "r", 3, "deadbeef", "ok");
        assert_eq!(m, Method::POST);
        assert_eq!(p, "/repos/o/r/pulls/3/reviews");
        assert_eq!(body["commit_id"], "deadbeef");
        assert_eq!(body["event"], "APPROVE");
        assert_eq!(body["body"], "ok");
    }

    #[test]
    fn reviews_request_pages_at_100() {
        let (m, p) = build_reviews_request("o", "r", 3);
        assert_eq!(m, Method::GET);
        assert_eq!(p, "/repos/o/r/pulls/3/reviews?per_page=100");
    }

    #[test]
    fn pr_request_path() {
        let (m, p) = build_pr_request("o", "r", 3);
        assert_eq!(m, Method::GET);
        assert_eq!(p, "/repos/o/r/pulls/3");
    }

    fn run(status: &str, conclusion: Option<&str>, id: u64, started: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "status": status, "conclusion": conclusion, "started_at": started, "app": {"slug": "github-actions"}})
    }

    #[test]
    fn check_state_mapping() {
        let one = |s: &str, c: Option<&str>| {
            parse_check_state(
                &serde_json::json!({"check_runs": [run(s, c, 1, "2026-01-01T00:00:00Z")]}),
            )
            .unwrap()
        };
        assert_eq!(one("completed", Some("success")), CheckState::Success);
        assert_eq!(one("in_progress", None), CheckState::Pending);
        assert_eq!(one("queued", None), CheckState::Pending);
        assert_eq!(
            one("completed", Some("failure")),
            CheckState::Failure("failure".into())
        );
        assert_eq!(
            one("completed", Some("neutral")),
            CheckState::Failure("neutral".into())
        );
        assert_eq!(
            one("completed", Some("skipped")),
            CheckState::Failure("skipped".into())
        );
        assert_eq!(
            one("completed", Some("cancelled")),
            CheckState::Failure("cancelled".into())
        );
        assert_eq!(
            one("completed", Some("timed_out")),
            CheckState::Failure("timed_out".into())
        );
        assert_eq!(
            parse_check_state(&serde_json::json!({"check_runs": []})).unwrap(),
            CheckState::Missing
        );
    }

    #[test]
    fn check_state_takes_most_recent_run() {
        let v = serde_json::json!({"check_runs": [
            run("completed", Some("success"), 1, "2026-01-01T00:00:00Z"),
            run("completed", Some("failure"), 3, "2026-01-03T00:00:00Z"),
            run("completed", Some("success"), 2, "2026-01-02T00:00:00Z"),
        ]});
        assert_eq!(
            parse_check_state(&v).unwrap(),
            CheckState::Failure("failure".into())
        );
        // Same timestamp: the higher id wins.
        let v = serde_json::json!({"check_runs": [
            run("completed", Some("failure"), 5, "2026-01-01T00:00:00Z"),
            run("in_progress", None, 9, "2026-01-01T00:00:00Z"),
        ]});
        assert_eq!(parse_check_state(&v).unwrap(), CheckState::Pending);
    }

    #[test]
    fn merge_response_mapping() {
        use serde_json::json;
        assert_eq!(
            parse_merge_response(200, &json!({"sha": "m1", "merged": true})).unwrap(),
            MergeOutcome::Merged { sha: "m1".into() }
        );
        assert_eq!(
            parse_merge_response(409, &json!({"message": "Head branch was modified"})).unwrap(),
            MergeOutcome::HeadMoved
        );
        assert_eq!(
            parse_merge_response(
                405,
                &json!({"message": "Required status check is expected"})
            )
            .unwrap(),
            MergeOutcome::NotMergeable("Required status check is expected".into())
        );
        assert!(parse_merge_response(404, &json!({"message": "Not Found"})).is_err());
        assert!(parse_merge_response(500, &json!({})).is_err());
    }

    #[test]
    fn pr_without_draft_is_an_error() {
        use serde_json::json;
        let complete = json!({
            "number": 7, "state": "open", "draft": false,
            "head": {"sha": "h"}, "base": {"ref": "main"}
        });
        assert!(!parse_pr(&complete).unwrap().draft);
        for bad in [json!(null), json!("yes"), json!(0)] {
            let mut v = complete.clone();
            v["draft"] = bad.clone();
            assert!(parse_pr(&v).is_err(), "draft = {bad}");
        }
        let mut missing = complete.clone();
        missing.as_object_mut().unwrap().remove("draft");
        let err = parse_pr(&missing).unwrap_err().to_string();
        assert!(err.contains("draft"), "{err}");
    }

    #[test]
    fn pr_and_review_parsing() {
        use serde_json::json;
        let pr = parse_pr(&json!({
            "number": 7, "state": "open", "draft": true,
            "head": {"sha": "h"}, "base": {"ref": "main"}
        }))
        .unwrap();
        assert_eq!(
            pr,
            PrInfo {
                number: 7,
                head_sha: "h".into(),
                base_ref: "main".into(),
                state: "open".into(),
                draft: true
            }
        );
        assert!(parse_pr(&json!({"number": 7})).is_err());

        let rs = parse_reviews(&json!([
            {"user": {"login": "a"}, "state": "APPROVED", "commit_id": "c1"},
            {"user": null, "state": "COMMENTED", "commit_id": "c2"}
        ]))
        .unwrap();
        assert_eq!(rs.len(), 2);
        assert_eq!(
            rs[0],
            Review {
                login: "a".into(),
                state: "APPROVED".into(),
                commit_id: "c1".into()
            }
        );
        assert_eq!(rs[1].login, "ghost");
    }

    #[test]
    fn next_link_parsing() {
        let h = "<https://api.github.com/repositories/1/pulls/3/reviews?per_page=100&page=2>; rel=\"next\", <https://api.github.com/repositories/1/pulls/3/reviews?per_page=100&page=5>; rel=\"last\"";
        assert_eq!(
            next_link(h).as_deref(),
            Some("https://api.github.com/repositories/1/pulls/3/reviews?per_page=100&page=2")
        );
        let last = "<https://api.github.com/x?page=1>; rel=\"prev\", <https://api.github.com/x?page=1>; rel=\"first\"";
        assert_eq!(next_link(last), None);
        assert_eq!(next_link(""), None);
    }

    #[test]
    fn fake_reviews_and_app_posts() {
        let fake = FakeGitHub::new();
        fake.add_pr(7, "sha1", "main");
        assert!(fake.reviews(7).unwrap().is_empty());
        fake.add_review(7, "alice", "APPROVED", "sha1");
        fake.post_review_as_app(7, "sha1", "looks fine", "inst-token")
            .unwrap();
        let posted = fake.posted_reviews();
        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0].number, 7);
        assert_eq!(posted[0].sha, "sha1");
        assert_eq!(posted[0].body, "looks fine");
        // The fake records that a token was used, never the token itself.
        assert!(!format!("{posted:?}").contains("inst-token"));
        assert_eq!(fake.reviews(7).unwrap().len(), 2);
    }

    #[test]
    fn fake_unknown_pr_is_an_error_and_not_mergeable_is_scriptable() {
        let fake = FakeGitHub::new();
        assert!(fake.pr(99).is_err());
        assert!(fake.merge(99, "x").is_err());
        fake.add_pr(1, "s", "main");
        fake.set_not_mergeable(1, "checks pending");
        assert_eq!(
            fake.merge(1, "s").unwrap(),
            MergeOutcome::NotMergeable("checks pending".into())
        );
    }

    #[test]
    fn commit_pulls_request_path() {
        let (m, p) = build_commit_pulls_request("o", "r", "abc123");
        assert_eq!(m, Method::GET);
        assert_eq!(p, "/repos/o/r/commits/abc123/pulls?per_page=100");
    }

    #[test]
    fn commit_pulls_parsing_fails_closed() {
        use serde_json::json;
        let ok = json!([
            {"number": 7, "head": {"sha": "h7"}, "base": {"ref": "main"}, "merged_at": "2026-10-13T00:00:00Z"},
            {"number": 8, "head": {"sha": "h8"}, "base": {"ref": "dev"}, "merged_at": null}
        ]);
        assert_eq!(
            parse_commit_pulls(&ok).unwrap(),
            vec![
                CommitPull {
                    number: 7,
                    head_sha: "h7".into(),
                    base_ref: "main".into(),
                    merged: true
                },
                CommitPull {
                    number: 8,
                    head_sha: "h8".into(),
                    base_ref: "dev".into(),
                    merged: false
                },
            ]
        );
        assert_eq!(parse_commit_pulls(&json!([])).unwrap(), vec![]);
        // Never read as "no PR": garbage, error objects and incomplete entries are errors.
        for bad in [
            serde_json::Value::Null,
            json!({}),
            json!({"message": "Bad credentials"}),
            json!("<html>rate limited</html>"),
            json!([{"number": 7, "head": {"sha": "h"}, "base": {"ref": "main"}}]),
            json!([{"number": 7, "head": {"sha": "h"}, "merged_at": null}]),
            json!([{"head": {"sha": "h"}, "base": {"ref": "main"}, "merged_at": null}]),
            json!([{"number": 7, "base": {"ref": "main"}, "merged_at": null}]),
            json!([{"number": 7, "head": {"sha": "h"}, "base": {"ref": "main"}, "merged_at": 5}]),
        ] {
            assert!(parse_commit_pulls(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn fake_commit_pulls_and_errors() {
        let fake = FakeGitHub::new();
        assert!(fake.commit_pulls("c1").unwrap().is_empty());
        fake.add_commit_pull("c1", 7, "h7", "main", true);
        assert_eq!(fake.commit_pulls("c1").unwrap()[0].number, 7);
        fake.fail_commit_pulls("c1", "HTTP 502");
        assert!(fake.commit_pulls("c1").is_err());
    }

    #[test]
    fn percent_encode_keeps_unreserved() {
        assert_eq!(percent_encode("Az09-._~"), "Az09-._~");
        assert_eq!(percent_encode("a b&c"), "a%20b%26c");
    }

    #[test]
    fn garbage_check_response_is_error() {
        use serde_json::{Value, json};
        // `send()` turns an unparseable body into Null; none of these may read as `Missing`.
        for body in [
            Value::Null,
            json!({}),
            json!("<html>rate limited</html>"),
            json!({"message": "Bad credentials"}),
            json!({"check_runs": null}),
            json!({"check_runs": {"0": {}}}),
        ] {
            assert!(parse_check_state(&body).is_err(), "{body}");
        }
        // An empty array is the only way to be Missing.
        assert_eq!(
            parse_check_state(&json!({"total_count": 0, "check_runs": []})).unwrap(),
            CheckState::Missing
        );
    }

    #[test]
    fn check_from_other_app_is_ignored() {
        use serde_json::json;
        let foreign = |id: u64, conclusion: &str| json!({"id": id, "status": "completed", "conclusion": conclusion, "app": {"slug": "evil-app"}});
        let actions = |id: u64, conclusion: &str| json!({"id": id, "status": "completed", "conclusion": conclusion, "app": {"slug": "github-actions"}});
        // Only a foreign run: absent.
        let v = json!({"check_runs": [foreign(10, "success")]});
        assert_eq!(parse_check_state(&v).unwrap(), CheckState::Missing);
        // A foreign success with a higher id must not mask or replace the real run.
        let v = json!({"check_runs": [actions(1, "failure"), foreign(99, "success")]});
        assert_eq!(
            parse_check_state(&v).unwrap(),
            CheckState::Failure("failure".into())
        );
        let v = json!({"check_runs": [actions(1, "success"), foreign(99, "failure")]});
        assert_eq!(parse_check_state(&v).unwrap(), CheckState::Success);
        // A run with no app, or a null app, is untrusted too.
        let v = json!({"check_runs": [
            {"id": 5, "status": "completed", "conclusion": "success"},
            {"id": 6, "status": "completed", "conclusion": "success", "app": null},
        ]});
        assert_eq!(parse_check_state(&v).unwrap(), CheckState::Missing);
    }

    #[test]
    fn fake_merge_of_non_open_pr_is_not_mergeable() {
        let fake = FakeGitHub::new();
        fake.add_pr(4, "s", "main");
        assert!(matches!(
            fake.merge(4, "s").unwrap(),
            MergeOutcome::Merged { .. }
        ));
        // Now closed: a second merge, even with the right sha, is refused.
        assert_eq!(
            fake.merge(4, "s").unwrap(),
            MergeOutcome::NotMergeable("closed".into())
        );
    }
}
