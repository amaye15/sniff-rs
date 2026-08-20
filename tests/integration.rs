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
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Runs the binary against a fixture with the given --output-format, writing
/// to stdout ("-"), and returns the parsed document.
fn run_with_format(
    fixture_name: &str,
    output_format: &str,
    extra_args: &[&str],
) -> serde_json::Value {
    let path = fixture(fixture_name);
    let mut args: Vec<&str> = vec![
        path.to_str().unwrap(),
        "-",
        "--output-format",
        output_format,
    ];
    args.extend_from_slice(extra_args);
    let output = Command::new(bin())
        .args(&args)
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "binary exited with an error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not valid JSON ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Runs the binary against a fixture with --output-format json, writing to
/// stdout ("-"), and returns the parsed document.
fn run_json(fixture_name: &str, extra_args: &[&str]) -> serde_json::Value {
    run_with_format(fixture_name, "json", extra_args)
}

fn table<'a>(doc: &'a serde_json::Value, name: &str) -> &'a Vec<serde_json::Value> {
    doc["tables"][name]
        .as_array()
        .unwrap_or_else(|| panic!("table '{name}' not found in {doc}"))
}

fn column<'a>(cols: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    cols.iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("column '{name}' not found"))
}

#[test]
fn csv_leading_zero_and_date_heuristics() {
    let doc = run_json("sample.csv", &[]);
    let cols = table(&doc, "sample");

    let zip = column(cols, "zip_code");
    assert_eq!(
        zip["current_type"], "i64",
        "read_csv-style naive parse should have consumed the leading zero"
    );
    assert_eq!(zip["ideal_type"], "String");
    assert!(zip["notes"].as_str().unwrap().contains("already lost"));

    let date = column(cols, "signup_date");
    assert_eq!(date["ideal_type"], "NaiveDate / DateTime");

    let balance = column(cols, "account_balance");
    assert_eq!(
        balance["ideal_type"], "f64",
        "comma-formatted currency string should still resolve to f64"
    );
}

#[test]
fn json_schema_output_maps_types_and_nullability() {
    let doc = run_with_format("sample.csv", "json-schema", &[]);
    assert_eq!(doc["$schema"], "http://json-schema.org/draft-07/schema#");

    let schema = &doc["tables"]["sample"];
    assert_eq!(schema["type"], "object");

    // Leading-zero heuristic keeps this a string, not an integer.
    assert_eq!(schema["properties"]["zip_code"]["type"], "string");
    assert_eq!(schema["properties"]["account_balance"]["type"], "number");
    assert_eq!(
        schema["properties"]["signup_date"],
        serde_json::json!({"type": "string", "format": "date-time"})
    );

    // "age" has one missing value in the fixture -> nullable union, and
    // excluded from "required"; "zip_code" has none -> required.
    assert_eq!(
        schema["properties"]["age"]["type"],
        serde_json::json!(["integer", "null"])
    );
    let required = schema["required"].as_array().unwrap();
    assert!(
        !required.iter().any(|v| v == "age"),
        "a nullable column shouldn't be in required: {required:?}"
    );
    assert!(
        required.iter().any(|v| v == "zip_code"),
        "a fully-populated column should be in required: {required:?}"
    );
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
    assert!(
        metadata["notes"]
            .as_str()
            .unwrap()
            .contains("flattened into")
    );
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
    assert!(
        current_type.starts_with("mixed("),
        "expected a mixed(...) type, got {current_type}"
    );
    assert!(
        current_type.contains(':'),
        "mixed types should carry per-type counts: {current_type}"
    );
}

#[test]
fn markdown_output_ends_with_exactly_one_newline() {
    let out = fixture("_scratch_markdown_trailing_newline.md");
    let status = Command::new(bin())
        .args([
            fixture("sample.csv").to_str().unwrap(),
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let content = std::fs::read_to_string(&out).unwrap();
    std::fs::remove_file(&out).ok();
    assert!(content.ends_with('\n'));
    assert!(
        !content.ends_with("\n\n"),
        "should not have a trailing blank line"
    );
}

#[test]
fn unrecognized_extension_gives_an_actionable_error_not_a_panic() {
    let output = Command::new(bin())
        .args(["/dev/null.mystery"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--format"),
        "error should point at the --format override: {stderr}"
    );
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
fn parquet_map_and_dictionary_columns_are_handled() {
    let doc = run_json("nested_types.parquet", &[]);
    let cols = table(&doc, "nested_types");

    // Dictionary-encoded strings (Parquet's low-cardinality string encoding)
    // should resolve transparently to the value type underneath, not report
    // the encoding itself as the type.
    let category = column(cols, "category");
    assert_eq!(category["current_type"], "String");
    assert_eq!(category["sample_values"][0], "gold");

    // A Map column bridges through the same JSON flattener as Struct/List -
    // it becomes a JSON object per row, then flattens into dot-notation
    // sub-columns exactly like a nested object would.
    let attributes = column(cols, "attributes");
    assert_eq!(attributes["current_type"], "object");
    assert!(cols.iter().any(|c| c["name"] == "attributes.color"));
    assert!(cols.iter().any(|c| c["name"] == "attributes.size"));

    let color = column(cols, "attributes.color");
    assert_eq!(color["current_type"], "String");
}

#[cfg(feature = "parquet")]
#[test]
fn feather_reads_via_the_shared_arrow_batch_profiler() {
    // No dedicated fixture file to keep the repo lean - Parquet already proves
    // the shared profile_arrow_batches path works, so this just checks the
    // format is recognized and doesn't need a rebuild-with-features error.
    let output = Command::new(bin())
        .args(["--format", "arrow", "nonexistent.feather"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("isn't compiled in"),
        "arrow feature should cover Feather too: {stderr}"
    );
}

#[cfg(feature = "avro")]
#[test]
fn avro_bridges_to_the_same_json_flattening_path() {
    let doc = run_json("sample.avro", &[]);
    let cols = table(&doc, "sample");
    assert!(
        cols.iter().any(|c| c["name"] == "metadata.risk_score"),
        "avro records should flatten just like JSON"
    );
}

#[cfg(feature = "msgpack")]
#[test]
fn msgpack_reads_concatenated_records_and_preserves_string_types() {
    let doc = run_json("sample.msgpack", &[]);
    let cols = table(&doc, "sample");

    let user_id = column(cols, "user_id");
    assert_eq!(user_id["missing_pct"].as_f64().unwrap(), 0.0);

    // MessagePack (unlike CSV) genuinely stores this as a string - the
    // leading zero was never at risk of being consumed by a numeric parse.
    let zip = column(cols, "zip_code");
    assert_eq!(zip["current_type"], "String");
    assert!(!zip["notes"].as_str().unwrap().contains("already lost"));

    let age = column(cols, "age");
    assert!((age["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.01);
}

#[cfg(feature = "toml")]
#[test]
fn toml_profiles_the_whole_document_as_one_row_and_flattens_array_of_tables() {
    let doc = run_json("sample.toml", &[]);
    let cols = table(&doc, "sample");

    // Top-level scalar keys become their own columns, each with exactly one
    // value - a TOML document is one record, not a table of many rows.
    let title = column(cols, "title");
    assert_eq!(title["current_type"], "String");
    assert_eq!(title["missing_pct"].as_f64().unwrap(), 0.0);

    // A plain table ([owner]) flattens into dot-notation sub-columns just
    // like a nested JSON object would.
    assert!(cols.iter().any(|c| c["name"] == "owner.name"));
    let owner_zip = column(cols, "owner.zip_code");
    assert!(
        owner_zip["notes"]
            .as_str()
            .unwrap()
            .contains("leading zeros")
    );

    // An array of tables ([[servers]]) becomes a Vec<object> column that
    // pools both entries and flattens the same way.
    let servers = column(cols, "servers");
    assert_eq!(servers["current_type"], "Vec<object>");
    assert!(cols.iter().any(|c| c["name"] == "servers.name"));
    let server_names = column(cols, "servers.name");
    assert_eq!(server_names["missing_pct"].as_f64().unwrap(), 0.0);
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_reads_a_multi_document_stream_as_one_record_per_document() {
    let doc = run_json("sample.yaml", &[]);
    let cols = table(&doc, "sample");

    // 3 `---`-separated documents in the fixture -> 3 pooled records.
    let user_id = column(cols, "user_id");
    assert_eq!(user_id["sample_values"].as_array().unwrap().len(), 3);
    assert_eq!(user_id["missing_pct"].as_f64().unwrap(), 0.0);

    let zip = column(cols, "zip_code");
    assert!(zip["notes"].as_str().unwrap().contains("leading zeros"));

    let date = column(cols, "signup_date");
    assert_eq!(date["ideal_type"], "NaiveDate / DateTime");

    // "active" only appears in 1 of the 3 documents.
    let active = column(cols, "active");
    assert!((active["missing_pct"].as_f64().unwrap() - 66.7).abs() < 0.01);
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

#[cfg(feature = "xlsx")]
#[test]
fn excel_reports_one_table_per_sheet() {
    let doc = run_json("multi_sheet.xlsx", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(
        tables.len(),
        2,
        "fixture has two sheets (customers, products), expected one table each"
    );

    let customers = table(&doc, "customers");
    assert!(customers.iter().any(|c| c["name"] == "customer_id"));
    let zip = column(customers, "zip_code");
    assert_eq!(zip["current_type"], "i64");
    assert!(zip["notes"].as_str().unwrap().contains("already lost"));

    let products = table(&doc, "products");
    assert!(products.iter().any(|c| c["name"] == "sku"));
    let in_stock = column(products, "in_stock");
    assert_eq!(in_stock["ideal_type"], "bool");
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
    assert!(
        current_type.starts_with("mixed("),
        "expected a type-affinity violation, got {current_type}"
    );
}
