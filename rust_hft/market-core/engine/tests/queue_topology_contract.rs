use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SCOPES: [&str; 5] = [
    "market-core",
    "risk-control",
    "strategy-framework",
    "apps",
    "data-pipelines",
];

fn collect_matches(root: &Path, dir: &Path, matches: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_matches(root, &entry.path(), matches)?;
        } else if file_type.is_file() {
            let Ok(text) = fs::read_to_string(entry.path()) else {
                continue;
            };
            let patterns = [
                concat!("unbounded", "_channel"),
                concat!("Unbounded", "Sender"),
                concat!("Unbounded", "Receiver"),
                concat!("Unbounded", "Receiver", "Stream"),
            ];
            if patterns.iter().any(|pattern| text.contains(pattern)) {
                matches.push(entry.path().strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    Ok(())
}

#[test]
fn every_risky_queue_usage_is_classified() {
    let root = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .expect("workspace root must exist");
    let audit_path = root.join("docs/reports/UNBOUNDED_CHANNEL_AUDIT.md");
    let audit = fs::read_to_string(&audit_path)
        .unwrap_or_else(|error| panic!("missing audit doc {}: {error}", audit_path.display()));

    let mut matches = Vec::new();
    for scope in SCOPES {
        collect_matches(&root, &root.join(scope), &mut matches)
            .unwrap_or_else(|error| panic!("failed to scan {scope}: {error}"));
    }
    matches.sort();

    let missing = matches
        .iter()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .filter(|path| !audit.contains(&format!("`{path}`")))
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "unclassified unbounded channel usage:\n{}\nupdate {} with a classification before merging",
        missing.join("\n"),
        audit_path.display()
    );
}
