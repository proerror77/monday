use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const DEFAULT_ROOT: &str = "target/latency-audit";
const HELP: &str = concat!(
    "Summarize Bitget latency-audit summary.json artifacts\n\n",
    "Usage: latency-report [OPTIONS] [PATH]...\n\n",
    "Arguments:\n",
    "  [PATH]...       summary.json files or directories (default: target/latency-audit)\n\n",
    "Options:\n",
    "      --csv          emit CSV instead of Markdown\n",
    "      --sort VALUE   sort by engine-p99, engine-p999, queue-p99, queue-p999, or run-id\n",
    "      --limit N      limit rows after sorting; zero means no limit\n",
    "  -h, --help         print help\n",
);

const CSV_FIELDS: [&str; 21] = [
    "run_id",
    "symbol",
    "samples",
    "dropped",
    "queue_kind",
    "idle_timeout_us",
    "receiver_core",
    "engine_core",
    "busy_poll",
    "raw_queue_depth_max",
    "engine_wait_empty_polls",
    "engine_park_calls",
    "engine_recv_timeouts",
    "raw_queue_wait_p99_ns",
    "raw_queue_wait_p999_ns",
    "engine_total_p99_ns",
    "engine_total_p999_ns",
    "event_convert_p99_ns",
    "envelope_parse_p99_ns",
    "ws_receive_gap_p99_ns",
    "summary_path",
];

const MARKDOWN_FIELDS: [&str; 16] = [
    "run_id",
    "symbol",
    "samples",
    "dropped",
    "queue_kind",
    "idle_timeout_us",
    "receiver_core",
    "engine_core",
    "busy_poll",
    "raw_queue_depth_max",
    "engine_wait_empty_polls",
    "engine_park_calls",
    "raw_queue_wait_p99_ns",
    "raw_queue_wait_p999_ns",
    "engine_total_p99_ns",
    "engine_total_p999_ns",
];

#[derive(Debug, Default, Eq, PartialEq)]
struct Args {
    paths: Vec<PathBuf>,
    csv: bool,
    sort: SortKind,
    limit: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SortKind {
    #[default]
    EngineP99,
    EngineP999,
    QueueP99,
    QueueP999,
    RunId,
}

impl SortKind {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "engine-p99" => Ok(Self::EngineP99),
            "engine-p999" => Ok(Self::EngineP999),
            "queue-p99" => Ok(Self::QueueP99),
            "queue-p999" => Ok(Self::QueueP999),
            "run-id" => Ok(Self::RunId),
            _ => Err(format!(
                "invalid --sort value '{value}'; expected engine-p99, engine-p999, queue-p99, queue-p999, or run-id"
            )),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ParseOutcome {
    Run(Args),
    Help,
}

#[derive(Debug, Eq, PartialEq)]
struct ReportRow {
    run_id: String,
    symbol: String,
    samples: u64,
    dropped: u64,
    queue_kind: String,
    idle_timeout_us: String,
    receiver_core: String,
    engine_core: String,
    busy_poll: String,
    raw_queue_depth_max: u64,
    engine_wait_empty_polls: u64,
    engine_park_calls: u64,
    engine_recv_timeouts: u64,
    raw_queue_wait_p99_ns: u64,
    raw_queue_wait_p999_ns: u64,
    engine_total_p99_ns: u64,
    engine_total_p999_ns: u64,
    event_convert_p99_ns: u64,
    envelope_parse_p99_ns: u64,
    ws_receive_gap_p99_ns: u64,
    summary_path: String,
}

impl ReportRow {
    fn csv_values(&self) -> Vec<String> {
        vec![
            self.run_id.clone(),
            self.symbol.clone(),
            self.samples.to_string(),
            self.dropped.to_string(),
            self.queue_kind.clone(),
            self.idle_timeout_us.clone(),
            self.receiver_core.clone(),
            self.engine_core.clone(),
            self.busy_poll.clone(),
            self.raw_queue_depth_max.to_string(),
            self.engine_wait_empty_polls.to_string(),
            self.engine_park_calls.to_string(),
            self.engine_recv_timeouts.to_string(),
            self.raw_queue_wait_p99_ns.to_string(),
            self.raw_queue_wait_p999_ns.to_string(),
            self.engine_total_p99_ns.to_string(),
            self.engine_total_p999_ns.to_string(),
            self.event_convert_p99_ns.to_string(),
            self.envelope_parse_p99_ns.to_string(),
            self.ws_receive_gap_p99_ns.to_string(),
            self.summary_path.clone(),
        ]
    }

    fn markdown_values(&self) -> Vec<String> {
        vec![
            self.run_id.clone(),
            self.symbol.clone(),
            self.samples.to_string(),
            self.dropped.to_string(),
            self.queue_kind.clone(),
            self.idle_timeout_us.clone(),
            self.receiver_core.clone(),
            self.engine_core.clone(),
            self.busy_poll.clone(),
            self.raw_queue_depth_max.to_string(),
            self.engine_wait_empty_polls.to_string(),
            self.engine_park_calls.to_string(),
            self.raw_queue_wait_p99_ns.to_string(),
            self.raw_queue_wait_p999_ns.to_string(),
            self.engine_total_p99_ns.to_string(),
            self.engine_total_p999_ns.to_string(),
        ]
    }
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args_os().skip(1)) {
        Ok(ParseOutcome::Run(args)) => args,
        Ok(ParseOutcome::Help) => {
            print_help();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("latency-report: {error}");
            eprintln!("Try 'latency-report --help' for usage.");
            return ExitCode::FAILURE;
        }
    };
    let paths = if args.paths.is_empty() {
        vec![PathBuf::from(DEFAULT_ROOT)]
    } else {
        args.paths
    };

    let mut rows = match load_rows(&paths) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("latency-report: {error}");
            return ExitCode::FAILURE;
        }
    };
    sort_rows(&mut rows, args.sort);
    if args.limit > 0 {
        rows.truncate(args.limit);
    }

    if args.csv {
        print!("{}", render_csv(&rows));
    } else if rows.is_empty() {
        println!("No Bitget latency summary.json files found.");
    } else {
        print!("{}", render_markdown(&rows));
    }
    if rows.is_empty() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn parse_args(arguments: impl IntoIterator<Item = OsString>) -> Result<ParseOutcome, String> {
    let mut arguments = arguments.into_iter();
    let mut args = Args::default();
    let mut positional_only = false;

    while let Some(argument) = arguments.next() {
        let utf8_argument = argument.to_str().map(str::to_owned);
        if positional_only {
            args.paths.push(PathBuf::from(argument));
            continue;
        }

        match utf8_argument.as_deref() {
            Some("--") => positional_only = true,
            Some("--csv") => args.csv = true,
            Some("--help" | "-h") => return Ok(ParseOutcome::Help),
            Some("--sort") => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--sort requires a value".to_string())?;
                args.sort = parse_sort_argument(&value)?;
            }
            Some(value) if value.starts_with("--sort=") => {
                args.sort = SortKind::parse(&value["--sort=".len()..])?;
            }
            Some("--limit") => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--limit requires a value".to_string())?;
                args.limit = parse_limit_argument(&value)?;
            }
            Some(value) if value.starts_with("--limit=") => {
                args.limit = parse_limit(&value["--limit=".len()..])?;
            }
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option '{value}'"));
            }
            _ => args.paths.push(PathBuf::from(argument)),
        }
    }

    Ok(ParseOutcome::Run(args))
}

fn parse_sort_argument(value: &OsString) -> Result<SortKind, String> {
    let value = value
        .to_str()
        .ok_or_else(|| "--sort value must be valid UTF-8".to_string())?;
    SortKind::parse(value)
}

fn parse_limit_argument(value: &OsString) -> Result<usize, String> {
    let value = value
        .to_str()
        .ok_or_else(|| "--limit value must be valid UTF-8".to_string())?;
    parse_limit(value)
}

fn parse_limit(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("invalid --limit value '{value}'; expected a non-negative integer"))
}

fn print_help() {
    print!("{HELP}");
}

fn load_rows(paths: &[PathBuf]) -> Result<Vec<ReportRow>, String> {
    collect_summary_paths(paths)?
        .into_iter()
        .map(|path| load_row(&path))
        .collect()
}

fn collect_summary_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut summaries = BTreeSet::new();
    for path in paths {
        if path.is_file() {
            summaries.insert(path.clone());
        } else if path.is_dir() {
            collect_directory(path, &mut summaries)?;
        }
    }
    Ok(summaries.into_iter().collect())
}

fn collect_directory(directory: &Path, summaries: &mut BTreeSet<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("failed to read entry in {}: {error}", directory.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", entry.path().display()))?;
        if file_type.is_dir() {
            collect_directory(&entry.path(), summaries)?;
        } else if file_type.is_file() && entry.file_name() == "summary.json" {
            summaries.insert(entry.path());
        }
    }
    Ok(())
}

fn load_row(path: &Path) -> Result<ReportRow, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let summary: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    let summary = summary
        .as_object()
        .ok_or_else(|| format!("{} must contain a JSON object", path.display()))?;
    let metrics = object_field(summary, "metrics", path)?;
    let engine_wait = object_field(summary, "engine_wait", path)?;

    Ok(ReportRow {
        run_id: path
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        symbol: scalar_string(summary.get("symbol")),
        samples: unsigned_value(summary.get("samples"), "samples", path)?,
        dropped: unsigned_value(summary.get("dropped"), "dropped", path)?,
        queue_kind: scalar_string(summary.get("queue_kind")),
        idle_timeout_us: scalar_string(summary.get("idle_timeout_us")),
        receiver_core: scalar_string(summary.get("receiver_core")),
        engine_core: scalar_string(summary.get("engine_core")),
        busy_poll: scalar_string(summary.get("busy_poll")),
        raw_queue_depth_max: metric(metrics, "raw_queue_depth", "max", path)?,
        engine_wait_empty_polls: unsigned_value(
            engine_wait.and_then(|value| value.get("empty_polls")),
            "engine_wait.empty_polls",
            path,
        )?,
        engine_park_calls: unsigned_value(
            engine_wait.and_then(|value| value.get("park_calls")),
            "engine_wait.park_calls",
            path,
        )?,
        engine_recv_timeouts: unsigned_value(
            engine_wait.and_then(|value| value.get("recv_timeouts")),
            "engine_wait.recv_timeouts",
            path,
        )?,
        raw_queue_wait_p99_ns: metric(metrics, "raw_queue_wait_ns", "p99", path)?,
        raw_queue_wait_p999_ns: metric(metrics, "raw_queue_wait_ns", "p999", path)?,
        engine_total_p99_ns: metric(metrics, "engine_total_ns", "p99", path)?,
        engine_total_p999_ns: metric(metrics, "engine_total_ns", "p999", path)?,
        event_convert_p99_ns: metric(metrics, "event_convert_ns", "p99", path)?,
        envelope_parse_p99_ns: metric(metrics, "envelope_parse_ns", "p99", path)?,
        ws_receive_gap_p99_ns: metric(metrics, "ws_receive_gap_ns", "p99", path)?,
        summary_path: path.to_string_lossy().into_owned(),
    })
}

fn object_field<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    path: &Path,
) -> Result<Option<&'a Map<String, Value>>, String> {
    match object.get(field) {
        None => Ok(None),
        Some(value) => value
            .as_object()
            .map(Some)
            .ok_or_else(|| format!("{}.{} must be a JSON object", path.display(), field)),
    }
}

fn metric(
    metrics: Option<&Map<String, Value>>,
    metric_name: &str,
    percentile: &str,
    path: &Path,
) -> Result<u64, String> {
    let Some(value) = metrics.and_then(|values| values.get(metric_name)) else {
        return Ok(0);
    };
    let object = value.as_object().ok_or_else(|| {
        format!(
            "{}.metrics.{metric_name} must be a JSON object",
            path.display()
        )
    })?;
    unsigned_value(
        object.get(percentile),
        &format!("metrics.{metric_name}.{percentile}"),
        path,
    )
}

fn unsigned_value(value: Option<&Value>, field: &str, path: &Path) -> Result<u64, String> {
    let Some(value) = value else {
        return Ok(0);
    };
    if value.is_null() {
        return Ok(0);
    }
    if let Some(value) = value.as_u64() {
        return Ok(value);
    }
    if let Some(value) = value.as_i64() {
        return u64::try_from(value)
            .map_err(|_| format!("{}.{} must be non-negative", path.display(), field));
    }
    if let Some(value) = value.as_str() {
        return value
            .parse::<u64>()
            .map_err(|_| format!("{}.{} must be an unsigned integer", path.display(), field));
    }
    Err(format!(
        "{}.{} must be an unsigned integer",
        path.display(),
        field
    ))
}

fn scalar_string(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(value) => value.to_string(),
    }
}

fn sort_rows(rows: &mut [ReportRow], sort: SortKind) {
    rows.sort_by(|left, right| {
        let order = match sort {
            SortKind::EngineP99 => left.engine_total_p99_ns.cmp(&right.engine_total_p99_ns),
            SortKind::EngineP999 => left.engine_total_p999_ns.cmp(&right.engine_total_p999_ns),
            SortKind::QueueP99 => left.raw_queue_wait_p99_ns.cmp(&right.raw_queue_wait_p99_ns),
            SortKind::QueueP999 => left
                .raw_queue_wait_p999_ns
                .cmp(&right.raw_queue_wait_p999_ns),
            SortKind::RunId => Ordering::Equal,
        };
        order.then_with(|| left.run_id.cmp(&right.run_id))
    });
}

fn render_markdown(rows: &[ReportRow]) -> String {
    let mut output = String::new();
    output.push_str("| ");
    output.push_str(&MARKDOWN_FIELDS.join(" | "));
    output.push_str(" |\n| ");
    output.push_str(&vec!["---"; MARKDOWN_FIELDS.len()].join(" | "));
    output.push_str(" |\n");
    for row in rows {
        output.push_str("| ");
        output.push_str(
            &row.markdown_values()
                .into_iter()
                .map(|value| escape_markdown(&value))
                .collect::<Vec<_>>()
                .join(" | "),
        );
        output.push_str(" |\n");
    }
    output
}

fn escape_markdown(value: &str) -> String {
    value.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn render_csv(rows: &[ReportRow]) -> String {
    let mut output = String::new();
    output.push_str(&CSV_FIELDS.join(","));
    output.push_str("\r\n");
    for row in rows {
        output.push_str(
            &row.csv_values()
                .into_iter()
                .map(|value| escape_csv(&value))
                .collect::<Vec<_>>()
                .join(","),
        );
        output.push_str("\r\n");
    }
    output
}

fn escape_csv(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "latency-report-{name}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_summary(root: &Path, run_id: &str, value: Value) -> PathBuf {
        let run = root.join(run_id);
        fs::create_dir_all(&run).unwrap();
        let path = run.join("summary.json");
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        path
    }

    fn summary(symbol: &str, engine_p99: u64, engine_p999: u64) -> Value {
        json!({
            "symbol": symbol,
            "samples": 10,
            "dropped": 2,
            "queue_kind": "spsc-spin",
            "idle_timeout_us": 50,
            "receiver_core": 1,
            "engine_core": 2,
            "busy_poll": true,
            "engine_wait": {
                "empty_polls": 3,
                "park_calls": 4,
                "recv_timeouts": 5
            },
            "metrics": {
                "raw_queue_depth": {"max": 6},
                "raw_queue_wait_ns": {"p99": 7, "p999": 8},
                "engine_total_ns": {"p99": engine_p99, "p999": engine_p999},
                "event_convert_ns": {"p99": 9},
                "envelope_parse_ns": {"p99": 10},
                "ws_receive_gap_ns": {"p99": 11}
            }
        })
    }

    #[test]
    fn parses_cli_options_and_paths_without_extra_dependencies() {
        let outcome = parse_args(
            [
                "--csv",
                "--sort=queue-p999",
                "--limit",
                "2",
                "reports",
                "--",
                "-named-path",
            ]
            .into_iter()
            .map(OsString::from),
        )
        .unwrap();

        assert_eq!(
            outcome,
            ParseOutcome::Run(Args {
                paths: vec![PathBuf::from("reports"), PathBuf::from("-named-path")],
                csv: true,
                sort: SortKind::QueueP999,
                limit: 2,
            })
        );
        assert!(parse_args([OsString::from("--limit=-1")]).is_err());
        assert!(parse_args([OsString::from("--sort=unknown")]).is_err());
    }

    #[test]
    fn recursively_collects_deduplicates_sorts_and_renders_markdown() {
        let directory = TestDirectory::new("markdown");
        let slow = write_summary(directory.path(), "run-slow", summary("BTCUSDT", 200, 300));
        write_summary(directory.path(), "run-fast", summary("SOLUSDT", 100, 400));
        fs::write(directory.path().join("not-a-summary.json"), b"{}").unwrap();

        let mut rows = load_rows(&[directory.path().to_path_buf(), slow]).unwrap();
        sort_rows(&mut rows, SortKind::EngineP99);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].run_id, "run-fast");
        assert_eq!(rows[1].run_id, "run-slow");
        assert_eq!(
            render_markdown(&rows),
            "| run_id | symbol | samples | dropped | queue_kind | idle_timeout_us | receiver_core | engine_core | busy_poll | raw_queue_depth_max | engine_wait_empty_polls | engine_park_calls | raw_queue_wait_p99_ns | raw_queue_wait_p999_ns | engine_total_p99_ns | engine_total_p999_ns |\n\
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n\
| run-fast | SOLUSDT | 10 | 2 | spsc-spin | 50 | 1 | 2 | True | 6 | 3 | 4 | 7 | 8 | 100 | 400 |\n\
| run-slow | BTCUSDT | 10 | 2 | spsc-spin | 50 | 1 | 2 | True | 6 | 3 | 4 | 7 | 8 | 200 | 300 |\n"
        );
    }

    #[test]
    fn csv_contains_full_schema_and_escapes_values() {
        let directory = TestDirectory::new("csv");
        write_summary(
            directory.path(),
            "run-1",
            json!({
                "symbol": "BTC,USDT",
                "samples": 1,
                "queue_kind": "spsc\"spin",
                "receiver_core": null,
                "busy_poll": false,
                "engine_wait": {"recv_timeouts": 2},
                "metrics": {"engine_total_ns": {"p99": 3}}
            }),
        );

        let rows = load_rows(&[directory.path().to_path_buf()]).unwrap();
        let csv = render_csv(&rows);

        assert!(csv.starts_with(
            "run_id,symbol,samples,dropped,queue_kind,idle_timeout_us,receiver_core,engine_core,busy_poll,raw_queue_depth_max,engine_wait_empty_polls,engine_park_calls,engine_recv_timeouts,raw_queue_wait_p99_ns,raw_queue_wait_p999_ns,engine_total_p99_ns,engine_total_p999_ns,event_convert_p99_ns,envelope_parse_p99_ns,ws_receive_gap_p99_ns,summary_path\r\n"
        ));
        assert!(csv.contains("run-1,\"BTC,USDT\",1,0,\"spsc\"\"spin\","));
        assert!(csv.contains(",False,0,0,0,2,0,0,3,0,0,0,0,"));
        assert_eq!(
            render_csv(&[]),
            "run_id,symbol,samples,dropped,queue_kind,idle_timeout_us,receiver_core,engine_core,busy_poll,raw_queue_depth_max,engine_wait_empty_polls,engine_park_calls,engine_recv_timeouts,raw_queue_wait_p99_ns,raw_queue_wait_p999_ns,engine_total_p99_ns,engine_total_p999_ns,event_convert_p99_ns,envelope_parse_p99_ns,ws_receive_gap_p99_ns,summary_path\r\n"
        );
    }

    #[test]
    fn sort_modes_use_run_id_as_a_stable_tie_breaker() {
        let directory = TestDirectory::new("sort");
        let mut z_run = summary("BTCUSDT", 100, 10);
        z_run["metrics"]["raw_queue_wait_ns"]["p99"] = json!(30);
        z_run["metrics"]["raw_queue_wait_ns"]["p999"] = json!(5);
        let mut a_run = summary("SOLUSDT", 100, 20);
        a_run["metrics"]["raw_queue_wait_ns"]["p99"] = json!(10);
        a_run["metrics"]["raw_queue_wait_ns"]["p999"] = json!(40);
        write_summary(directory.path(), "z-run", z_run);
        write_summary(directory.path(), "a-run", a_run);
        let mut rows = load_rows(&[directory.path().to_path_buf()]).unwrap();

        sort_rows(&mut rows, SortKind::EngineP99);
        assert_eq!(rows[0].run_id, "a-run");
        sort_rows(&mut rows, SortKind::EngineP999);
        assert_eq!(rows[0].run_id, "z-run");
        sort_rows(&mut rows, SortKind::QueueP99);
        assert_eq!(rows[0].run_id, "a-run");
        sort_rows(&mut rows, SortKind::QueueP999);
        assert_eq!(rows[0].run_id, "z-run");
        sort_rows(&mut rows, SortKind::RunId);
        assert_eq!(rows[0].run_id, "a-run");
    }

    #[test]
    fn malformed_summary_reports_the_source_path() {
        let directory = TestDirectory::new("invalid");
        let run = directory.path().join("broken-run");
        fs::create_dir_all(&run).unwrap();
        let path = run.join("summary.json");
        fs::write(&path, b"not-json").unwrap();

        let error = load_rows(&[directory.path().to_path_buf()]).unwrap_err();

        assert!(error.contains(&path.display().to_string()));
        assert!(error.contains("failed to parse"));
    }
}
