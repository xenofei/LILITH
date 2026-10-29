//! Counts as the apps show them (ruling 2026-10-29, beside the owner's money format), Qt-free:
//! [`count_text`] puts thousands separators into a count the core sends ("412000" → "412,000";
//! a decimal such as GPU-seconds keeps its decimals as they came: "1840.5" → "1,840.5"). Input
//! that isn't `-?digits[.digits]` comes back as it was. Exports keep the raw numbers.

/// `value` (a JSON number's text or a decimal string) with thousands separators (module docs).
pub fn count_text(value: &str) -> String {
    let (sign, rest) = match value.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", value),
    };
    let (whole, part) = match rest.split_once('.') {
        Some((whole, part)) => (whole, Some(part)),
        None => (rest, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || part.is_some_and(|p| !digits(p)) {
        return value.to_string();
    }
    let mut out = String::from(sign);
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if let Some(part) = part {
        out.push('.');
        out.push_str(part);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-117
    fn count_text_groups_thousands() {
        assert_eq!(count_text("412000"), "412,000");
        assert_eq!(count_text("1234567"), "1,234,567");
        assert_eq!(count_text("999"), "999");
        assert_eq!(count_text("1000"), "1,000");
        assert_eq!(count_text("0"), "0");
        assert_eq!(count_text("-12345"), "-12,345");
        assert_eq!(count_text("1840.5"), "1,840.5", "decimals as they came");
        assert_eq!(count_text("9.25"), "9.25");
        assert_eq!(count_text("12345.6789"), "12,345.6789");
    }

    #[test] // spec: APP-117
    fn count_text_leaves_anything_else_as_it_came() {
        for raw in ["", "abc", "1e6", "1,000", "1.", ".5", "+1", "5m", "--1"] {
            assert_eq!(count_text(raw), raw, "{raw:?}");
        }
    }
}
