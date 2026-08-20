//! Integration tests run the compiled binary against fixtures in tests/fixtures/
//! and assert on its JSON output. Tests for optional formats are gated behind
//! the same Cargo feature that gates the format itself, so `cargo test` covers
//! the default build and `cargo test --features full` covers everything.
//!
//! No assert_cmd/predicates dependency on purpose - std::process::Command is
//! enough, and keeping test dependencies as lean as the tool itself matters.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sniff-rs"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// Runs the binary against a fixture with --output-format json, writing to
/// stdout ("-"), and returns the parsed document.
fn run_json(fixture_name: &str, extra_args: &[&str]) -> serde_json::Value {
    let path = fixture(fixture_name);
    let mut args: Vec<&str> = vec![path.to_str().unwrap(), "-", "--output-format", "json"];
    args.extend_from_slice(extra_args);
    let output = Command::new(bin()).args(&args).output().expect("failed to run binary");
    assert!(output.status.success(), "binary exited with an error: {}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("stdout was not valid JSON ({e}): {}", String::from_utf8_lossy(&output.stdout)))
}

fn table<'a>(doc: &'a serde_json::Value, name: &str) -> &'a Vec<serde_json::Value> {
    doc["tables"][name].as_array().unwrap_or_else(|| panic!("table '{name}' not found in {doc}"))
}

fn column<'a>(cols: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    cols.iter().find(|c| c["name"] == name).unwrap_or_else(|| panic!("column '{name}' not found"))
}

#[test]
fn csv_leading_zero_and_date_heuristics() {
    let doc = run_json("sample.csv", &[]);
    let cols = table(&doc, "sample");

    let zip = column(cols, "zip_code");
    assert_eq!(zip["current_type"], "i64", "read_csv-style naive parse should have consumed the leading zero");
    assert_eq!(zip["ideal_type"], "String");
    assert!(zip["notes"].as_str().unwrap().contains("already lost"));

    let date = column(cols, "signup_date");
    assert_eq!(date["ideal_type"], "NaiveDate / DateTime");

    let balance = column(cols, "account_balance");
    assert_eq!(balance["ideal_type"], "f64", "comma-formatted currency string should still resolve to f64");
}

#[test]
fn tsv_reads_with_tab_delimiter() {
    let doc = run_json("sample.tsv", &[]);
    let cols = table(&doc, "sample");
    assert!(cols.iter().any(|c| c["name"] == "name"));
    assert!(cols.iter().any(|c| c["name"] == "score"));
}

#[test]
fn json_flattens_nested_object_and_array_of_objects() {
    let doc = run_json("nested.jsonl", &[]);
    let cols = table(&doc, "nested");

    let metadata = column(cols, "metadata");
    assert_eq!(metadata["current_type"], "object");
    assert!(metadata["notes"].as_str().unwrap().contains("flattened into"));
    assert!(cols.iter().any(|c| c["name"] == "metadata.risk_score"));
    assert!(cols.iter().any(|c| c["name"] == "metadata.source"));

    let events = column(cols, "events");
    assert_eq!(events["current_type"], "Vec<object>");

    // 3 records contribute 2+1+0=3 pooled events; only 1 has a non-null amount.
    let amount = column(cols, "events.amount");
    assert!((amount["missing_pct"].as_f64().unwrap() - 66.7).abs() < 0.01);
}

#[test]
fn mixed_types_report_counts_not_just_a_list() {
    let doc = run_json("mixed_types.jsonl", &[]);
    let cols = table(&doc, "mixed_types");
    let flag = column(cols, "flag");
    let current_type = flag["current_type"].as_str().unwrap();
    assert!(current_type.starts_with("mixed("), "expected a mixed(...) type, got {current_type}");
    assert!(current_type.contains(':'), "mixed types should carry per-type counts: {current_type}");
}

#[test]
fn markdown_output_ends_with_exactly_one_newline() {
    let out = fixture("_scratch_markdown_trailing_newline.md");
    let status = Command::new(bin())
        .args([fixture("sample.csv").to_str().unwrap(), out.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());
    let content = std::fs::read_to_string(&out).unwrap();
    std::fs::remove_file(&out).ok();
    assert!(content.ends_with('\n'));
    assert!(!content.ends_with("\n\n"), "should not have a trailing blank line");
}

#[test]
fn unrecognized_extension_gives_an_actionable_error_not_a_panic() {
    let output = Command::new(bin()).args(["/dev/null.mystery"]).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--format"), "error should point at the --format override: {stderr}");
}

#[cfg(feature = "parquet")]
#[test]
fn parquet_string_column_preserves_leading_zero_with_no_data_loss() {
    let doc = run_json("sample.parquet", &[]);
    let cols = table(&doc, "sample");
    let zip = column(cols, "zip_code");
    // Parquet stores this as a genuine Utf8 column, unlike CSV's naive numeric parse.
    assert_eq!(zip["current_type"], "String");
    assert!(!zip["notes"].as_str().unwrap().contains("already lost"));
}

#[cfg(feature = "parquet")]
#[test]
fn feather_reads_via_the_shared_arrow_batch_profiler() {
    // No dedicated fixture file to keep the repo lean - Parquet already proves
    // the shared profile_arrow_batches path works, so this just checks the
    // format is recognized and doesn't need a rebuild-with-features error.
    let output = Command::new(bin()).args(["--format", "arrow", "nonexistent.feather"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't compiled in"), "arrow feature should cover Feather too: {stderr}");
}

#[cfg(feature = "avro")]
#[test]
fn avro_bridges_to_the_same_json_flattening_path() {
    let doc = run_json("sample.avro", &[]);
    let cols = table(&doc, "sample");
    assert!(cols.iter().any(|c| c["name"] == "metadata.risk_score"), "avro records should flatten just like JSON");
}

#[cfg(feature = "xlsx")]
#[test]
fn excel_writer_silently_mangling_a_zip_code_gets_caught() {
    let doc = run_json("sample.xlsx", &[]);
    let cols = table(&doc, "sample");
    let zip = column(cols, "zip_code");
    // openpyxl/Excel auto-detects "02134" as numeric on write and drops the
    // leading zero before this tool ever sees the file - Current Type should
    // reflect that the damage is already done.
    assert_eq!(zip["current_type"], "i64");
    assert!(zip["notes"].as_str().unwrap().contains("already lost"));
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_reports_multiple_tables_and_catches_a_type_affinity_violation() {
    let doc = run_json("sample.sqlite", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert!(tables.len() >= 2, "fixture has two tables (events, users)");

    let events = table(&doc, "events");
    let amount = column(events, "amount");
    let current_type = amount["current_type"].as_str().unwrap();
    // SQLite let a TEXT value slip into a REAL-affinity column - a real,
    // well-known SQLite quirk this tool is specifically meant to surface.
    assert!(current_type.starts_with("mixed("), "expected a type-affinity violation, got {current_type}");
}
