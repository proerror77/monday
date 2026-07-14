use regex::Regex;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

const FORBIDDEN_PATHS: &[&str] = &[
    "config/.env.shared",
    "config/.env.unified",
    "rust_hft/config/secrets.yaml",
    "rust_hft/clickhouse_credentials.txt",
    "rust_hft/deployment/k8s/secrets.yaml",
    "rust_hft/hft-admin-ssh-20250926144355.pem",
    "rust_hft/hft-collector-key-new.pem",
    "rust_hft/hft-collector-key.pem",
    "rust_hft/k8s/bitget/clickhouse-secret.yaml",
];
const SKIP_PREFIXES: &[&str] = &[
    "docs/",
    "rust_hft/docs/",
    "rust_hft/tests/",
    "rust_hft/specs/",
];
const SKIP_SUFFIXES: &[&str] = &[".example", ".sample", ".md", ".rs"];
const SCANNED_SUFFIXES: &[&str] = &[
    ".sh", ".txt", ".yaml", ".yml", ".env", ".shared", ".unified",
];
const ALLOWED_PREFIXES: &[&str] = &[
    "${",
    "$",
    "<",
    "CHANGE_ME",
    "YOUR_",
    "your_",
    "example_",
    "EXAMPLE_",
    "REPLACE_",
    "replace_",
];
const ALLOWED_EXACT: &[&str] = &["", "\"\"", "''", "null", "None"];

#[derive(Debug)]
struct TrackedFile {
    path: String,
    contents: Option<Vec<u8>>,
}

impl TrackedFile {
    fn present(path: impl Into<String>, contents: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            contents: Some(contents.into()),
        }
    }

    fn missing(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            contents: None,
        }
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct ScanReport {
    failures: Vec<String>,
}

impl ScanReport {
    fn is_clean(&self) -> bool {
        self.failures.is_empty()
    }

    fn output(&self) -> String {
        if self.is_clean() {
            "tracked secret check passed\n".to_string()
        } else {
            format!("{}\n", self.failures.join("\n"))
        }
    }
}

enum ViolationLabel {
    Capture(usize),
    Static(&'static str),
}

struct SecretPattern {
    regex: Regex,
    value_capture: usize,
    violation_label: ViolationLabel,
}

fn secret_patterns() -> &'static [SecretPattern] {
    static PATTERNS: OnceLock<Vec<SecretPattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            SecretPattern {
                regex: Regex::new(
                    r#"^\s*(?:export\s+)?(?:Environment=)?["']?([A-Za-z0-9_]*(?:SECRET|PASSWORD|PASSWD|PASSPHRASE|PRIVATE_KEY|API_KEY|ACCESS_KEY|SIGNING_KEY|PAGERDUTY_KEY|WEBHOOK(?:_URL)?|TOKEN|CREDENTIAL)[A-Za-z0-9_]*)\s*=\s*(.+?)["']?\s*$"#,
                )
                .expect("primary secret assignment regex"),
                value_capture: 2,
                violation_label: ViolationLabel::Capture(1),
            },
            SecretPattern {
                regex: Regex::new(
                    r"^\s*(api_secret|passphrase|secret_key|private_key|password)\s*:\s*(.+?)\s*$",
                )
                .expect("snake-case YAML secret regex"),
                value_capture: 2,
                violation_label: ViolationLabel::Capture(1),
            },
            SecretPattern {
                regex: Regex::new(
                    r"(?i)^\s*([A-Za-z0-9_-]*(?:api-key|api-secret|passphrase|password|private-key|webhook-url|pagerduty-key))\s*:\s*(.+?)\s*$",
                )
                .expect("kebab-case YAML secret regex"),
                value_capture: 2,
                violation_label: ViolationLabel::Capture(1),
            },
            SecretPattern {
                regex: Regex::new(r"^\s*Password:\s*(.+?)\s*$")
                    .expect("capitalized password regex"),
                value_capture: 1,
                violation_label: ViolationLabel::Static("Password"),
            },
        ]
    })
}

fn key_material_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(^|/)[^/]+\.(pem|key|p12|pfx|crt)$").expect("key material regex")
    })
}

fn private_key_header_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----$").expect("private key header regex")
    })
}

fn should_scan(path: &str) -> bool {
    !SKIP_PREFIXES.iter().any(|prefix| path.starts_with(prefix))
        && !SKIP_SUFFIXES.iter().any(|suffix| path.ends_with(suffix))
        && SCANNED_SUFFIXES.iter().any(|suffix| path.ends_with(suffix))
}

fn utf8_ignoring_errors(mut bytes: &[u8]) -> String {
    let mut decoded = String::with_capacity(bytes.len());
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                decoded.push_str(valid);
                break;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                decoded.push_str(
                    std::str::from_utf8(&bytes[..valid_up_to])
                        .expect("Utf8Error valid prefix must be valid UTF-8"),
                );
                let Some(error_len) = error.error_len() else {
                    break;
                };
                bytes = &bytes[valid_up_to + error_len..];
            }
        }
    }
    decoded
}

fn python_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();

    while let Some((index, character)) = chars.next() {
        let is_line_break = matches!(
            character,
            '\n' | '\r'
                | '\u{000b}'
                | '\u{000c}'
                | '\u{001c}'
                | '\u{001d}'
                | '\u{001e}'
                | '\u{0085}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !is_line_break {
            continue;
        }

        lines.push(&text[start..index]);
        start = index + character.len_utf8();
        if character == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
            let (next_index, next) = chars.next().expect("peeked CRLF newline");
            start = next_index + next.len_utf8();
        }
    }

    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn normalized_value(value: &str) -> &str {
    value.trim().trim_matches('"').trim_matches('\'')
}

fn is_allowed_value(value: &str) -> bool {
    let value = normalized_value(value);
    ALLOWED_EXACT.contains(&value)
        || ALLOWED_PREFIXES
            .iter()
            .any(|prefix| value.starts_with(prefix))
}

fn scan_tracked_files(tracked: &[TrackedFile]) -> ScanReport {
    let tracked_paths: HashSet<&str> = tracked.iter().map(|file| file.path.as_str()).collect();
    let mut failures = Vec::new();

    for forbidden in FORBIDDEN_PATHS {
        if tracked_paths.contains(forbidden) {
            failures.push(format!("tracked secret file still present: {forbidden}"));
        }
    }

    let key_hits: Vec<&str> = tracked
        .iter()
        .map(|file| file.path.as_str())
        .filter(|path| key_material_pattern().is_match(path))
        .collect();
    if !key_hits.is_empty() {
        failures.push(format!(
            "tracked key material detected:\n{}",
            key_hits.join("\n")
        ));
    }

    for file in tracked {
        if !should_scan(&file.path) {
            continue;
        }
        let Some(contents) = &file.contents else {
            continue;
        };
        let text = utf8_ignoring_errors(contents);
        for (index, line) in python_splitlines(&text).into_iter().enumerate() {
            let stripped = line.trim();
            if stripped.is_empty() || stripped.starts_with('#') {
                continue;
            }
            if private_key_header_pattern().is_match(stripped) {
                failures.push(format!(
                    "suspicious tracked secret in {}:{} (private key marker)",
                    file.path,
                    index + 1
                ));
                continue;
            }

            for pattern in secret_patterns() {
                let Some(captures) = pattern.regex.captures(line) else {
                    continue;
                };
                let value = captures
                    .get(pattern.value_capture)
                    .expect("secret pattern value capture")
                    .as_str();
                if is_allowed_value(value) {
                    break;
                }
                let label = match pattern.violation_label {
                    ViolationLabel::Capture(group) => captures
                        .get(group)
                        .expect("secret pattern label capture")
                        .as_str(),
                    ViolationLabel::Static(label) => label,
                };
                failures.push(format!(
                    "suspicious tracked secret in {}:{} ({label})",
                    file.path,
                    index + 1
                ));
                break;
            }
        }
    }

    ScanReport { failures }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

fn load_repository_files(root: &Path) -> Result<Vec<TrackedFile>, String> {
    let output = Command::new("git")
        .args(["-C", root.to_str().ok_or("repository path is not UTF-8")?])
        .args(["ls-files"])
        .output()
        .map_err(|error| format!("failed to run git ls-files: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let tracked = String::from_utf8(output.stdout)
        .map_err(|error| format!("git ls-files returned non-UTF-8 paths: {error}"))?;

    tracked
        .lines()
        .map(|relative| {
            let path = root.join(relative);
            let contents =
                if path.is_file() {
                    Some(fs::read(&path).map_err(|error| {
                        format!("failed to read tracked file {relative}: {error}")
                    })?)
                } else {
                    None
                };
            Ok(TrackedFile {
                path: relative.to_string(),
                contents,
            })
        })
        .collect()
}

#[test]
fn repository_has_no_tracked_secrets() {
    let root = repository_root();
    let files = load_repository_files(&root).expect("load tracked repository files");
    let report = scan_tracked_files(&files);

    assert!(report.is_clean(), "{}", report.output());
    print!("{}", report.output());
}

#[test]
fn primary_pattern_covers_export_systemd_webhook_and_secret_key_forms() {
    for sample in [
        "Environment=CLICKHOUSE_PASSWORD=CHANGE_ME_LITERAL",
        "Environment=\"PAGERDUTY_KEY=CHANGE_ME_LITERAL\"",
        "ALERT_WEBHOOK_URL=CHANGE_ME_LITERAL",
        "BITGET_SECRET_KEY=CHANGE_ME_LITERAL",
    ] {
        assert!(
            secret_patterns()[0].regex.is_match(sample),
            "secret scanner self-check failed for {}",
            sample.split('=').next().unwrap_or(sample)
        );
    }
}

#[test]
fn clean_fixture_preserves_allowlist_and_false_positive_boundaries() {
    let allowed_values = [
        "",
        "\"\"",
        "''",
        "null",
        "None",
        "${TOKEN}",
        "$TOKEN",
        "<secret>",
        "CHANGE_ME_TOKEN",
        "YOUR_TOKEN",
        "your_token",
        "example_token",
        "EXAMPLE_TOKEN",
        "REPLACE_TOKEN",
        "replace_token",
    ];
    let mut config = String::from("# API_TOKEN=not-a-secret\nordinary=value\n");
    for value in allowed_values {
        config.push_str(&format!("API_TOKEN={value}\n"));
    }
    config.push_str("api_secret: CHANGE_ME_SECRET\n");
    config.push_str("service-api-key: example_key\n");
    config.push_str("Password: YOUR_PASSWORD\n");
    config.push_str("lowercase_token=is-not-in-the-case-sensitive-contract\n");

    let files = vec![
        TrackedFile::present("config/runtime.env", config),
        TrackedFile::present("docs/unsafe.env", "API_TOKEN=literal"),
        TrackedFile::present("rust_hft/docs/unsafe.yaml", "password: literal"),
        TrackedFile::present("rust_hft/tests/unsafe.env", "API_TOKEN=literal"),
        TrackedFile::present("rust_hft/specs/unsafe.env", "API_TOKEN=literal"),
        TrackedFile::present("config/unsafe.env.example", "API_TOKEN=literal"),
        TrackedFile::present("config/unsafe.env.sample", "API_TOKEN=literal"),
        TrackedFile::present("config/unsafe.md", "API_TOKEN=literal"),
        TrackedFile::present("config/unsafe.rs", "API_TOKEN=literal"),
        TrackedFile::present("config/unsafe.toml", "API_TOKEN=literal"),
        TrackedFile::missing("config/missing.env"),
    ];

    assert_eq!(
        scan_tracked_files(&files).output(),
        "tracked secret check passed\n"
    );
}

#[test]
fn violation_fixture_preserves_rule_order_line_numbers_and_redacted_labels() {
    let files = vec![
        TrackedFile::missing("config/.env.shared"),
        TrackedFile::present("certs/runtime.crt", [0, 159, 146, 150]),
        TrackedFile::present(
            "config/runtime.yaml",
            concat!(
                "API_TOKEN=literal-token\n",
                "api_secret: literal-secret\n",
                "service-api-key: literal-api-key\n",
                "Password: literal-password\n",
                "-----BEGIN EC PRIVATE KEY-----\n",
            ),
        ),
    ];

    let output = scan_tracked_files(&files).output();
    assert_eq!(
        output,
        concat!(
            "tracked secret file still present: config/.env.shared\n",
            "tracked key material detected:\n",
            "certs/runtime.crt\n",
            "suspicious tracked secret in config/runtime.yaml:1 (API_TOKEN)\n",
            "suspicious tracked secret in config/runtime.yaml:2 (api_secret)\n",
            "suspicious tracked secret in config/runtime.yaml:3 (service-api-key)\n",
            "suspicious tracked secret in config/runtime.yaml:4 (Password)\n",
            "suspicious tracked secret in config/runtime.yaml:5 (private key marker)\n",
        )
    );
    for secret_value in [
        "literal-token",
        "literal-secret",
        "literal-api-key",
        "literal-password",
    ] {
        assert!(!output.contains(secret_value));
    }
}

#[test]
fn invalid_utf8_is_ignored_and_large_files_are_scanned_to_the_end() {
    let mut binary = b"safe=prefix\xffsuffix\n".to_vec();
    binary.extend_from_slice(b"API_TOKEN=binary-secret\n");

    let mut large = vec![b'a'; 1024 * 1024];
    large.extend_from_slice(b"\npassword: large-file-secret\n");

    let output = scan_tracked_files(&[
        TrackedFile::present("config/binary.env", binary),
        TrackedFile::present("config/large.yaml", large),
    ])
    .output();
    assert_eq!(
        output,
        concat!(
            "suspicious tracked secret in config/binary.env:2 (API_TOKEN)\n",
            "suspicious tracked secret in config/large.yaml:2 (password)\n",
        )
    );
    assert!(!output.contains("binary-secret"));
    assert!(!output.contains("large-file-secret"));
}

#[test]
fn python_line_boundaries_preserve_crlf_and_legacy_config_line_numbers() {
    let contents = concat!(
        "safe=value\r\n",
        "API_TOKEN=first-secret\r",
        "safe=value\u{0085}",
        "password: second-secret\u{2028}",
        "safe=value",
    );

    let output = scan_tracked_files(&[TrackedFile::present("config/lines.env", contents)]).output();
    assert_eq!(
        output,
        concat!(
            "suspicious tracked secret in config/lines.env:2 (API_TOKEN)\n",
            "suspicious tracked secret in config/lines.env:4 (password)\n",
        )
    );
    assert!(!output.contains("first-secret"));
    assert!(!output.contains("second-secret"));
}

#[test]
fn key_material_extension_boundary_matches_the_original_scanner() {
    let files = [
        "keys/runtime.pem",
        "keys/runtime.key",
        "keys/runtime.p12",
        "keys/runtime.pfx",
        "keys/runtime.crt",
        "keys/runtime.PEM",
        "keys/.pem",
        "keys/runtime.pem.example",
    ]
    .into_iter()
    .map(TrackedFile::missing)
    .collect::<Vec<_>>();

    assert_eq!(
        scan_tracked_files(&files).output(),
        concat!(
            "tracked key material detected:\n",
            "keys/runtime.pem\n",
            "keys/runtime.key\n",
            "keys/runtime.p12\n",
            "keys/runtime.pfx\n",
            "keys/runtime.crt\n",
        )
    );
}
