//! `[engines.comfyui] pass_env` (ruling P3-R31): which listed names the engine environment scrub
//! (ARCH-063, P3-R19) may still let through, which it refuses even when listed, and which can
//! never name a variable. Shared by `lil-core` (the scrub) and `lil-client`/`lil` (the
//! `lil doctor` report), so both agree on one list.
//!
//! Refused even when listed: the suite's own variables (`LIL_*`, `CLAUDE_*`) and the paid-provider
//! keys (`ANTHROPIC_*` and the rest of the P3-R19 prefix list). Allowed: the hub tokens
//! (`HF_TOKEN`, `HUGGING_FACE_HUB_TOKEN`, `CIVITAI_*`) and names caught only by the generic parts
//! (`GITHUB_TOKEN`, …) — this is what `pass_env` is for.

/// The prefixes a `pass_env` entry may not start with, even when listed (ruling P3-R31). It is
/// the P3-R19 provider prefix list, with `CIVITAI_` left out so the Civitai hub token stays
/// listable. `lil-core`'s scrub pins that this matches its own `SECRET_PREFIXES`.
pub const REFUSED_PREFIXES: [&str; 18] = [
    "ANTHROPIC_",
    "OPENAI_",
    "OPENROUTER_",
    "NANOGPT_",
    "GEMINI_",
    "FAL_",
    "COMFY_API",
    "DEEPSEEK_",
    "MOONSHOT_",
    "DASHSCOPE_",
    "ZAI_",
    "ZHIPU",
    "MINIMAX_",
    "XAI_",
    "MISTRAL_",
    "REPLICATE_",
    "LIL_",
    "CLAUDE_",
];

/// `Ok` when a `pass_env` entry may be let through, `Err(why)` when the scrub refuses it even
/// though it is listed (a suite or paid-provider prefix) or it can never name an environment
/// variable (empty, holding whitespace, or holding `=`, `*`, `?` or `%`).
pub fn classify(entry: &str) -> Result<(), String> {
    if entry.trim().is_empty() {
        return Err("it is empty".to_string());
    }
    if entry.chars().any(char::is_whitespace) {
        return Err("it has whitespace, so it can't name an environment variable".to_string());
    }
    if let Some(c) = entry.chars().find(|c| matches!(c, '=' | '*' | '?' | '%')) {
        return Err(format!(
            "it holds `{c}`; pass_env names a whole variable, never a pattern"
        ));
    }
    let upper = entry.to_ascii_uppercase();
    if upper.starts_with("LIL_") || upper.starts_with("CLAUDE_") {
        return Err(
            "it is one of the suite's own variables (LIL_*, CLAUDE_*), which ComfyUI never needs"
                .to_string(),
        );
    }
    if let Some(prefix) = REFUSED_PREFIXES
        .iter()
        .find(|p| upper.starts_with(**p) && **p != "LIL_" && **p != "CLAUDE_")
    {
        return Err(format!(
            "names starting with {prefix} are provider keys; a node could spend through them \
             outside the meter and the caps"
        ));
    }
    Ok(())
}

/// The `[engines.comfyui] pass_env` entries of a parsed `suite.toml`, split into the ones the
/// scrub lets through and the ones it rejects (`(entry, why)`, ruling P3-R31). An entry that
/// isn't a string is rejected as "not a string", and a `pass_env` that isn't an array as
/// "pass_env is not a list" (M3), so `lil doctor` names it. A missing `pass_env`, or no
/// `[engines.comfyui]` table, gives two empty lists.
pub fn split(suite: &crate::SuiteToml) -> (Vec<String>, Vec<(String, String)>) {
    let text = |v: &toml::Value| v.as_str().map_or_else(|| v.to_string(), str::to_string);
    let mut pass = Vec::new();
    let mut rejected = Vec::new();
    let Some(value) = suite
        .other
        .get("engines")
        .and_then(|e| e.get("comfyui"))
        .and_then(|c| c.get("pass_env"))
    else {
        return (pass, rejected);
    };
    let Some(entries) = value.as_array() else {
        rejected.push((text(value), "pass_env is not a list".to_string()));
        return (pass, rejected);
    };
    for v in entries {
        let Some(entry) = v.as_str() else {
            rejected.push((text(v), "not a string".to_string()));
            continue;
        };
        match classify(entry) {
            Ok(()) => pass.push(entry.to_string()),
            Err(why) => rejected.push((entry.to_string(), why)),
        }
    }
    (pass, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_tokens_pass_and_suite_or_provider_prefixes_are_refused() {
        for ok in [
            "HF_TOKEN",
            "HUGGING_FACE_HUB_TOKEN",
            "CIVITAI_API_TOKEN",
            "GITHUB_TOKEN",
        ] {
            assert!(classify(ok).is_ok(), "{ok}");
        }
        for bad in [
            "LIL_ENGINE",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "COMFY_API_KEY",
            "fal_key",
        ] {
            assert!(classify(bad).is_err(), "{bad}");
        }
        for impossible in ["", "  ", "HF_*", "A=B", "X?", "pct%"] {
            assert!(classify(impossible).is_err(), "{impossible:?}");
        }
    }

    fn split_of(text: &str) -> (Vec<String>, Vec<(String, String)>) {
        split(&crate::SuiteToml::parse(text).unwrap())
    }

    #[test]
    fn entries_that_are_not_strings_and_a_pass_env_that_is_not_a_list_are_rejected() {
        let (pass, rejected) =
            split_of("[engines.comfyui]\npass_env = ['HF_TOKEN', 1, true, ['X']]\n");
        assert_eq!(pass, ["HF_TOKEN"]);
        let named: Vec<(&str, &str)> = rejected
            .iter()
            .map(|(e, w)| (e.as_str(), w.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                ("1", "not a string"),
                ("true", "not a string"),
                ("[\"X\"]", "not a string")
            ]
        );
        let (pass, rejected) = split_of("[engines.comfyui]\npass_env = 'HF_TOKEN'\n");
        assert!(pass.is_empty());
        assert_eq!(
            rejected,
            [("HF_TOKEN".to_string(), "pass_env is not a list".to_string())]
        );
        // No table, or no pass_env, is nothing to report.
        assert_eq!(split_of(""), (vec![], vec![]));
        assert_eq!(split_of("[engines.comfyui]\n"), (vec![], vec![]));
    }
}
