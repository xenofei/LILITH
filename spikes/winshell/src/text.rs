//! Pure helpers: the raw command line, toast and `.wsb` XML, percentiles, base64url, hex.

/// Splits a raw `GetCommandLineW` string into the program name and the untouched rest, by the
/// rule `CommandLineToArgvW` uses for the first token: a leading `"` runs to the next `"`
/// (no escapes), otherwise the name runs to the first space or tab. Spaces and tabs between
/// the name and the rest are dropped; nothing in the rest is re-split or unquoted.
pub fn split_first_arg(cmdline: &str) -> (&str, &str) {
    let (exe, after) = if let Some(quoted) = cmdline.strip_prefix('"') {
        match quoted.find('"') {
            Some(end) => (&quoted[..end], &quoted[end + 1..]),
            None => (quoted, ""),
        }
    } else {
        match cmdline.find([' ', '\t']) {
            Some(end) => (&cmdline[..end], &cmdline[end..]),
            None => (cmdline, ""),
        }
    };
    (exe, after.trim_start_matches([' ', '\t']))
}

/// Surrounding spaces and one pair of surrounding `"` removed (Appendix G's `lil:` URL input).
pub fn strip_one_quote_pair(arg: &str) -> &str {
    let t = arg.trim_matches(' ');
    match t.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(inner) => inner,
        None => t,
    }
}

/// XML-escapes text for an element body or a double-quoted attribute.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// One toast, in Appendix G's shape.
#[derive(Debug, Default)]
pub struct ToastSpec<'a> {
    /// The body's protocol URL (`activationType="protocol"`); `None` gives a plain `<toast>`.
    pub launch: Option<&'a str>,
    pub title: &'a str,
    pub body: &'a str,
    /// `(content, protocol URL)` per button.
    pub actions: Vec<(&'a str, &'a str)>,
    /// Adds Appendix G's data-bound `<progress>` element.
    pub progress: bool,
}

pub fn toast_xml(t: &ToastSpec) -> String {
    let mut x = String::new();
    match t.launch {
        Some(url) => x.push_str(&format!(
            r#"<toast launch="{}" activationType="protocol">"#,
            xml_escape(url)
        )),
        None => x.push_str("<toast>"),
    }
    x.push_str(r#"<visual><binding template="ToastGeneric">"#);
    x.push_str(&format!(
        "<text>{}</text><text>{}</text>",
        xml_escape(t.title),
        xml_escape(t.body)
    ));
    if t.progress {
        x.push_str(r#"<progress value="{progressValue}" valueStringOverride="{progressValueString}" status="{progressStatus}"/>"#);
    }
    x.push_str("</binding></visual>");
    if !t.actions.is_empty() {
        x.push_str("<actions>");
        for (content, url) in &t.actions {
            x.push_str(&format!(
                r#"<action content="{}" arguments="{}" activationType="protocol"/>"#,
                xml_escape(content),
                xml_escape(url)
            ));
        }
        x.push_str("</actions>");
    }
    x.push_str("</toast>");
    x
}

/// One `<MappedFolder>` of a `.wsb` file.
#[derive(Debug)]
pub struct Mapped<'a> {
    pub host: &'a str,
    pub sandbox: &'a str,
    pub read_only: bool,
}

/// A Windows Sandbox configuration: networking, clipboard, audio and video input and printers
/// off, the mapped folders, one logon command.
pub fn wsb_xml(mapped: &[Mapped], logon_command: &str) -> String {
    let mut x = String::from("<Configuration>\r\n");
    // Nothing reaches the host but the mapped folders (fix round 1, M2).
    for setting in [
        "Networking",
        "ClipboardRedirection",
        "AudioInput",
        "VideoInput",
        "PrinterRedirection",
    ] {
        x.push_str(&format!("  <{setting}>Disable</{setting}>\r\n"));
    }
    x.push_str("  <MappedFolders>\r\n");
    for m in mapped {
        x.push_str(&format!(
            "    <MappedFolder>\r\n      <HostFolder>{}</HostFolder>\r\n      <SandboxFolder>{}</SandboxFolder>\r\n      <ReadOnly>{}</ReadOnly>\r\n    </MappedFolder>\r\n",
            xml_escape(m.host),
            xml_escape(m.sandbox),
            if m.read_only { "true" } else { "false" }
        ));
    }
    x.push_str("  </MappedFolders>\r\n");
    x.push_str(&format!(
        "  <LogonCommand>\r\n    <Command>{}</Command>\r\n  </LogonCommand>\r\n",
        xml_escape(logon_command)
    ));
    x.push_str("</Configuration>\r\n");
    x
}

/// Nearest-rank percentile (1-based rank `ceil(p/100 * n)`); `None` for no samples.
pub fn percentile(samples: &[f64], p: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * s.len() as f64).ceil().max(1.0) as usize;
    Some(s[rank.min(s.len()) - 1])
}

/// `{n, p50, p95, p99, max}` of a series, as spikes/README.md asks.
pub fn summary(samples: &[f64]) -> serde_json::Value {
    serde_json::json!({
        "n": samples.len(),
        "p50": percentile(samples, 50.0),
        "p95": percentile(samples, 95.0),
        "p99": percentile(samples, 99.0),
        "max": percentile(samples, 100.0),
    })
}

/// RFC 4648 §5 base64url, without padding.
pub fn base64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// One argument quoted so that `CommandLineToArgvW` (and the MSVC CRT) read it back
/// unchanged: as is when it is non-empty with no space, tab, newline or `"`; otherwise in `"`,
/// with each `"` and the backslashes before it (or before the closing `"`) escaped.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// The command line `CreateProcessW` gets for `exe args`.
pub fn command_line(exe: &str, args: &[String]) -> String {
    let mut line = quote_arg(exe);
    for a in args {
        line.push(' ');
        line.push_str(&quote_arg(a));
    }
    line
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_the_argument_and_its_quotes() {
        let (exe, rest) =
            split_first_arg(r#""C:\x y\winshell-app.exe" "lilspike:confirm/cf_0123456789abcdef""#);
        assert_eq!(exe, r"C:\x y\winshell-app.exe");
        assert_eq!(rest, r#""lilspike:confirm/cf_0123456789abcdef""#);
    }

    #[test]
    fn split_an_unquoted_exe_and_tabs() {
        assert_eq!(split_first_arg("a.exe\t \tb c"), ("a.exe", "b c"));
        assert_eq!(split_first_arg("a.exe"), ("a.exe", ""));
        assert_eq!(split_first_arg(r#""a b.exe""#), ("a b.exe", ""));
        assert_eq!(split_first_arg(r#""a b.exe"x y"#), ("a b.exe", "x y"));
        assert_eq!(split_first_arg(r#""unterminated"#), ("unterminated", ""));
    }

    #[test]
    fn strip_removes_one_pair_only() {
        assert_eq!(strip_one_quote_pair(r#" "x" "#), "x");
        assert_eq!(strip_one_quote_pair(r#"""x"""#), r#""x""#);
        assert_eq!(strip_one_quote_pair(r#""x"#), r#""x"#);
        assert_eq!(strip_one_quote_pair("x"), "x");
    }

    #[test]
    fn toast_xml_matches_appendix_g_and_escapes() {
        let x = toast_xml(&ToastSpec {
            launch: Some("lilspike:confirm/cf_0123456789abcdef"),
            title: "A <b> & \"c\"",
            body: "body",
            actions: vec![("Open", "lilspike:confirm/cf_0123456789abcdef")],
            progress: false,
        });
        assert_eq!(
            x,
            concat!(
                r#"<toast launch="lilspike:confirm/cf_0123456789abcdef" activationType="protocol">"#,
                r#"<visual><binding template="ToastGeneric"><text>A &lt;b&gt; &amp; &quot;c&quot;</text><text>body</text></binding></visual>"#,
                r#"<actions><action content="Open" arguments="lilspike:confirm/cf_0123456789abcdef" activationType="protocol"/></actions>"#,
                "</toast>"
            )
        );
    }

    #[test]
    fn plain_and_progress_toasts() {
        let x = toast_xml(&ToastSpec {
            title: "t",
            body: "b",
            progress: true,
            ..Default::default()
        });
        assert!(x.starts_with("<toast><visual>"));
        assert!(x.contains(r#"<progress value="{progressValue}" valueStringOverride="{progressValueString}" status="{progressStatus}"/></binding>"#));
        assert!(!x.contains("<actions>"));
    }

    #[test]
    fn wsb_maps_folders_and_escapes() {
        let x = wsb_xml(
            &[
                Mapped {
                    host: r"C:\b&b",
                    sandbox: r"C:\lil\in",
                    read_only: true,
                },
                Mapped {
                    host: r"C:\r",
                    sandbox: r"C:\lil\out",
                    read_only: false,
                },
            ],
            r"powershell.exe -ExecutionPolicy Bypass -File C:\lil\in\inside.ps1",
        );
        assert!(x.starts_with("<Configuration>\r\n  <Networking>Disable</Networking>\r\n"));
        for off in [
            "<ClipboardRedirection>Disable</ClipboardRedirection>",
            "<AudioInput>Disable</AudioInput>",
            "<VideoInput>Disable</VideoInput>",
            "<PrinterRedirection>Disable</PrinterRedirection>",
        ] {
            assert!(x.contains(off), "{off}");
        }
        assert!(x.contains(r"<HostFolder>C:\b&amp;b</HostFolder>"));
        assert!(x.contains(
            "<SandboxFolder>C:\\lil\\in</SandboxFolder>\r\n      <ReadOnly>true</ReadOnly>"
        ));
        assert!(x.contains(
            "<SandboxFolder>C:\\lil\\out</SandboxFolder>\r\n      <ReadOnly>false</ReadOnly>"
        ));
        assert!(x.contains(
            r"<Command>powershell.exe -ExecutionPolicy Bypass -File C:\lil\in\inside.ps1</Command>"
        ));
        assert!(x.ends_with("</Configuration>\r\n"));
    }

    #[test]
    fn quote_arg_follows_the_argv_rules() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), r#""""#);
        assert_eq!(quote_arg(r"C:\x y\a.exe"), r#""C:\x y\a.exe""#);
        assert_eq!(quote_arg(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote_arg(r"a\\b"), r"a\\b");
    }

    #[test]
    fn command_line_joins_quoted_arguments() {
        let args = vec![
            "toast".to_string(),
            "--out".to_string(),
            r"C:\r s\o.json".to_string(),
        ];
        assert_eq!(
            command_line(r"C:\b in\winshell-core.exe", &args),
            r#""C:\b in\winshell-core.exe" toast --out "C:\r s\o.json""#
        );
    }

    #[test]
    fn nearest_rank_percentiles() {
        let s: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&s, 50.0), Some(50.0));
        assert_eq!(percentile(&s, 99.0), Some(99.0));
        assert_eq!(percentile(&s, 100.0), Some(100.0));
        assert_eq!(percentile(&[3.0, 1.0, 2.0], 50.0), Some(2.0));
        assert_eq!(percentile(&[5.0], 0.0), Some(5.0));
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn base64url_rfc4648_vectors_and_url_safe_alphabet() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64url(input.as_bytes()), want);
        }
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0x1f]), "00ab1f");
    }
}
