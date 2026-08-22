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

#[test]
fn fixed_width_slices_columns_by_declared_character_widths() {
    let doc = run_with_format(
        "sample.fwf",
        "json",
        &["--format", "fixed-width", "--widths", "8,4,9,8"],
    );
    let cols = table(&doc, "sample");

    let age = column(cols, "age");
    assert_eq!(age["current_type"], "i64");
    assert!((age["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.01);

    // Leading-zero heuristic works the same as CSV once fields are sliced.
    let zip = column(cols, "zip_code");
    assert_eq!(zip["current_type"], "i64");
    assert!(zip["notes"].as_str().unwrap().contains("already lost"));

    let plan = column(cols, "plan");
    assert_eq!(plan["current_type"], "String");
}

#[test]
fn fixed_width_without_widths_gives_an_actionable_error() {
    let output = Command::new(bin())
        .args([
            fixture("sample.fwf").to_str().unwrap(),
            "-",
            "--format",
            "fixed-width",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--widths"),
        "error should point at the --widths flag: {stderr}"
    );
}

#[test]
fn gzip_input_reads_transparently_as_its_inner_format() {
    let doc = run_json("sample.csv.gz", &[]);

    // The reported file stays the real (compressed) name, but detection,
    // table naming, and the heuristics all operate on the decompressed
    // inner CSV exactly as if it had never been gzipped.
    assert_eq!(doc["file"], "sample.csv.gz");
    assert_eq!(doc["format"], "csv");
    let cols = table(&doc, "sample");
    let zip = column(cols, "zip_code");
    assert_eq!(zip["current_type"], "i64");
    assert!(zip["notes"].as_str().unwrap().contains("already lost"));
}

#[test]
fn gzip_with_an_invalid_header_gives_an_actionable_error_not_a_panic() {
    let bad_gz = fixture("_scratch_not_actually_gzip.csv.gz");
    std::fs::write(&bad_gz, b"this is not gzip data").unwrap();
    let output = Command::new(bin())
        .args([bad_gz.to_str().unwrap(), "-"])
        .output()
        .unwrap();
    std::fs::remove_file(&bad_gz).ok();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("gzip"),
        "error should mention gzip, not panic: {stderr}"
    );
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_input_reads_transparently_as_its_inner_format() {
    let doc = run_json("nested.jsonl.zst", &[]);
    assert_eq!(doc["file"], "nested.jsonl.zst");
    assert_eq!(doc["format"], "json");
    let cols = table(&doc, "nested");
    assert!(cols.iter().any(|c| c["name"] == "metadata.risk_score"));
}

#[cfg(not(feature = "zstd"))]
#[test]
fn zstd_without_the_feature_gives_an_actionable_error() {
    let output = Command::new(bin())
        .args([fixture("nested.jsonl.zst").to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--features zstd"),
        "error should point at the --features zstd rebuild: {stderr}"
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

#[cfg(feature = "cbor")]
#[test]
fn cbor_reads_concatenated_records_and_preserves_string_types() {
    let doc = run_json("sample.cbor", &[]);
    let cols = table(&doc, "sample");

    let user_id = column(cols, "user_id");
    assert_eq!(user_id["missing_pct"].as_f64().unwrap(), 0.0);

    // Same story as MessagePack: CBOR genuinely stores this as a string, so
    // the leading zero was never at risk of a numeric parse eating it.
    let zip = column(cols, "zip_code");
    assert_eq!(zip["current_type"], "String");
    assert!(!zip["notes"].as_str().unwrap().contains("already lost"));

    let age = column(cols, "age");
    assert!((age["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.01);
}

#[cfg(feature = "xml")]
#[test]
fn xml_treats_homogeneous_children_as_records_and_attributes_as_at_columns() {
    let doc = run_json("sample.xml", &[]);
    let cols = table(&doc, "sample");

    // 3 <user> elements under the root, all the same tag - each is a record.
    let id = column(cols, "@id");
    assert_eq!(id["sample_values"].as_array().unwrap().len(), 3);

    // Attributes become @-prefixed columns rather than being dropped.
    let active = column(cols, "@active");
    assert_eq!(active["ideal_type"], "bool");

    // Child elements with only text content are the bare string, not
    // wrapped in a {"#text": ...} object.
    let zip = column(cols, "zip_code");
    assert_eq!(zip["current_type"], "String");
    assert!(zip["notes"].as_str().unwrap().contains("leading zeros"));

    let date = column(cols, "signup_date");
    assert_eq!(date["ideal_type"], "NaiveDate / DateTime");
}

#[cfg(feature = "npy")]
#[test]
fn npy_structured_array_gives_one_column_per_named_field() {
    let doc = run_json("sample_structured.npy", &[]);
    let cols = table(&doc, "sample_structured");

    // current_type reflects the declared numpy dtype (this format actually
    // knows it, unlike CSV's naive text parse), so there's no spurious
    // "numeric strings" note the way there would be for an already-typed
    // field.
    let age = column(cols, "age");
    assert_eq!(age["current_type"], "i64");
    assert_eq!(age["notes"], "");

    // A fixed-width byte-string field ('S5') still triggers the
    // leading-zero heuristic on its decoded text.
    let zip = column(cols, "zip_code");
    assert_eq!(zip["current_type"], "String");
    assert!(zip["notes"].as_str().unwrap().contains("leading zeros"));

    let active = column(cols, "active");
    assert_eq!(active["current_type"], "bool");
}

#[cfg(feature = "npy")]
#[test]
fn npy_plain_2d_array_gets_positional_columns_in_row_major_order() {
    let doc = run_json("sample_matrix.npy", &[]);
    let cols = table(&doc, "sample_matrix");

    assert!(cols.iter().any(|c| c["name"] == "col_0"));
    let col0 = column(cols, "col_0");
    assert_eq!(col0["current_type"], "f64");
    // Row-major: col_0 should be the first element of each row (1.5, 4.5, 7.5).
    assert_eq!(
        col0["sample_values"],
        serde_json::json!(["1.5", "4.5", "7.5"])
    );
}

#[cfg(feature = "npy")]
#[test]
fn npz_reports_one_table_per_named_array() {
    let doc = run_json("sample.npz", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(
        tables.len(),
        2,
        "fixture has two named arrays (users, scores)"
    );

    let scores = table(&doc, "scores");
    let value = column(scores, "value");
    assert_eq!(value["current_type"], "i64");

    let users = table(&doc, "users");
    assert!(users.iter().any(|c| c["name"] == "user_id"));
}

#[cfg(feature = "weblog")]
#[test]
fn combined_log_splits_request_and_treats_dash_as_missing() {
    let doc = run_with_format("sample_combined.log", "json", &["--format", "combined-log"]);
    let cols = table(&doc, "sample_combined");

    // "-" is the format's own placeholder for "not present", not a literal
    // value - ident is "-" on every line in the fixture.
    let ident = column(cols, "ident");
    assert_eq!(ident["missing_pct"].as_f64().unwrap(), 100.0);

    // The quoted request splits into its own columns rather than staying
    // one opaque field.
    let method = column(cols, "method");
    assert!(
        method["sample_values"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("GET"))
    );
    let path = column(cols, "path");
    assert!(
        path["sample_values"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("/login"))
    );

    // The Apache/Combined timestamp format resolves to a real date type.
    let timestamp = column(cols, "timestamp");
    assert_eq!(timestamp["ideal_type"], "NaiveDate / DateTime");

    let status = column(cols, "status");
    assert_eq!(status["current_type"], "i64");

    // Combined-only columns are present; a "-" bytes field (the 401 line)
    // is missing rather than the literal string "-".
    let bytes = column(cols, "bytes");
    assert!((bytes["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.01);
    assert!(cols.iter().any(|c| c["name"] == "referer"));
    assert!(cols.iter().any(|c| c["name"] == "user_agent"));
}

#[cfg(feature = "weblog")]
#[test]
fn common_log_has_no_referer_or_user_agent_columns() {
    let doc = run_with_format("sample_common.log", "json", &["--format", "common-log"]);
    let cols = table(&doc, "sample_common");
    assert!(!cols.iter().any(|c| c["name"] == "referer"));
    assert!(!cols.iter().any(|c| c["name"] == "user_agent"));
    let status = column(cols, "status");
    assert_eq!(status["current_type"], "i64");
}

#[cfg(feature = "weblog")]
#[test]
fn combined_log_line_rejects_common_log_format_with_an_actionable_error() {
    // sample_combined.log has trailing referer/user-agent fields the
    // Common Log grammar doesn't expect - it shouldn't silently truncate
    // or misparse them, it should say so.
    let output = Command::new(bin())
        .args([
            fixture("sample_combined.log").to_str().unwrap(),
            "-",
            "--format",
            "common-log",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Common Log"),
        "error should name the format that failed to match: {stderr}"
    );
}

#[cfg(feature = "syslog")]
#[test]
fn syslog_rfc3164_decodes_pri_and_extracts_pid() {
    let doc = run_with_format("sample_rfc3164.log", "json", &["--format", "syslog"]);
    let cols = table(&doc, "sample_rfc3164");

    // PRI 34 = facility 4 (auth) * 8 + severity 2 (critical).
    let facility = column(cols, "facility");
    assert!(
        facility["sample_values"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("auth"))
    );
    let severity = column(cols, "severity");
    assert!(
        severity["sample_values"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("critical"))
    );

    // Only the first and third lines have a [PID] on the tag.
    let pid = column(cols, "pid");
    assert_eq!(pid["current_type"], "i64");
    assert!((pid["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.01);

    let message = column(cols, "message");
    assert!(
        message["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "'su root' failed for lonvick on /dev/pts/8")
    );
}

#[cfg(feature = "syslog")]
#[test]
fn syslog_rfc5424_treats_nilvalue_dash_as_missing() {
    let doc = run_with_format("sample_rfc5424.log", "json", &["--format", "syslog5424"]);
    let cols = table(&doc, "sample_rfc5424");

    // "-" is RFC 5424's own nilvalue convention for "field not specified".
    let procid = column(cols, "procid");
    assert!((procid["missing_pct"].as_f64().unwrap() - 66.7).abs() < 0.01);
    let structured_data = column(cols, "structured_data");
    assert!((structured_data["missing_pct"].as_f64().unwrap() - 66.7).abs() < 0.01);

    let version = column(cols, "version");
    assert_eq!(version["current_type"], "i64");
}

#[cfg(feature = "syslog")]
#[test]
fn syslog_line_that_does_not_match_the_grammar_is_an_actionable_error() {
    let bad = fixture("_scratch_not_syslog.log");
    std::fs::write(&bad, "<34>Oct 11 22:14:15 mymachine su[1234]: ok\nnope\n").unwrap();
    let output = Command::new(bin())
        .args([bad.to_str().unwrap(), "-", "--format", "syslog"])
        .output()
        .unwrap();
    std::fs::remove_file(&bad).ok();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("line 2"),
        "error should name the offending line: {stderr}"
    );
}

#[cfg(feature = "dbase")]
#[test]
fn dbase_reveals_a_numeric_field_that_is_really_an_integer() {
    let doc = run_json("sample.dbf", &[]);
    let cols = table(&doc, "sample");

    // dBase's Numeric field type doesn't distinguish int from float at the
    // storage level - current_type reflects that (f64), while ideal_type
    // still independently re-derives from the actual values and correctly
    // narrows to i64, exactly the current-vs-ideal gap this tool exists to
    // surface.
    let age = column(cols, "AGE");
    assert_eq!(age["current_type"], "f64");
    assert_eq!(age["ideal_type"], "i64");

    let balance = column(cols, "BALANCE");
    assert_eq!(balance["current_type"], "f64");
    assert_eq!(balance["ideal_type"], "f64");

    let active = column(cols, "ACTIVE");
    assert_eq!(active["current_type"], "bool");

    // dBase's own Date rendering (YYYYMMDD) resolves via the date format
    // added to DATE_FORMATS specifically for it.
    let signup = column(cols, "SIGNUP");
    assert_eq!(signup["ideal_type"], "NaiveDate / DateTime");
}

#[cfg(feature = "stata")]
#[test]
fn stata_treats_missing_marker_as_absent_and_recovers_int_from_a_double() {
    let doc = run_json("sample.dta", &[]);
    let cols = table(&doc, "sample");

    // The fixture has one NaN age - Stata's own "." missing-value marker,
    // not a value this tool invented - omitted from raw_values entirely
    // rather than kept as a literal string.
    let age = column(cols, "age");
    assert!((age["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.01);

    // pandas wrote this column as a Stata double (forced by the NaN), but
    // the two present values are genuinely integers - ideal_type still
    // catches that independently of current_type.
    assert_eq!(age["current_type"], "f64");
    assert_eq!(age["ideal_type"], "i64");

    let user_id = column(cols, "user_id");
    assert_eq!(user_id["current_type"], "String");
}

#[cfg(feature = "sas7bdat")]
#[test]
fn sas7bdat_format_is_recognized() {
    // No dedicated fixture committed here: unlike every other format in this
    // suite, there's no tool available in this environment that can write a
    // real .sas7bdat file (SAS's binary format is proprietary; pyreadstat,
    // the usual option, only writes .dta/.sav/.xport, not sas7bdat itself),
    // and copying a third-party sample file of unclear provenance into this
    // repo wasn't worth the licensing risk. columns_from_sas7bdat was
    // manually verified against the sas7bdat crate's own bundled test
    // fixture during development (schema, non-ASCII text, and the same
    // current_type=f64/ideal_type=i64 gap as Stata and dBase, since SAS
    // also stores nearly all numeric data as doubles internally). This test
    // only confirms the format is wired up, mirroring how Feather - the
    // other format without its own dedicated fixture - is tested.
    let output = Command::new(bin())
        .args(["--format", "sas7bdat", "nonexistent.sas7bdat"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("isn't compiled in"),
        "sas7bdat feature should be wired up: {stderr}"
    );
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

#[cfg(feature = "ini")]
#[test]
fn ini_reports_one_table_per_section_and_pools_duplicate_keys() {
    let doc = run_json("sample.ini", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(
        tables.len(),
        3,
        "fixture has a default section plus [owner] and [database]"
    );

    // Keys before the first [header] land in an implicit default section.
    let default = table(&doc, "(default)");
    assert!(default.iter().any(|c| c["name"] == "app_name"));

    let owner = table(&doc, "owner");
    let zip = column(owner, "zip_code");
    assert!(zip["notes"].as_str().unwrap().contains("leading zeros"));

    // INI allows a key to repeat within a section - both values should be
    // pooled into one column rather than the second silently winning.
    let database = table(&doc, "database");
    let tag = column(database, "tag");
    assert_eq!(tag["current_type"], "Vec<String>");
    assert_eq!(tag["sample_values"].as_array().unwrap().len(), 2);
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

#[test]
fn csv_treats_missing_value_sentinels_as_null_not_literal_strings() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    // "age" has "NA" (row 2) and "null" (row 6) among otherwise-clean
    // integers - without sentinel recognition those two literal strings
    // would derail i64 detection entirely and undercount missing_pct.
    let age = column(cols, "age");
    assert_eq!(age["current_type"], "i64");
    assert_eq!(age["ideal_type"], "i64");
    assert_eq!(age["missing_pct"].as_f64().unwrap(), 25.0);
    assert!(
        age["notes"].as_str().unwrap().contains("missing values"),
        "notes: {:?}",
        age["notes"]
    );
}

#[test]
fn csv_flags_a_constant_column_even_on_a_small_file() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    // "status" is "active" on all 8 rows - 12.5% cardinality, which the old
    // ratio-only (< 5%) check would have missed.
    let status = column(cols, "status");
    assert_eq!(status["ideal_type"], "enum / category");
    assert!(status["notes"].as_str().unwrap().contains("constant"));
}

#[test]
fn csv_recognizes_uuid_email_ipv4_ipv6_and_url_columns() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(column(cols, "ipv6_address")["ideal_type"], "IPv6");
    assert_eq!(column(cols, "homepage")["ideal_type"], "URL");
}

#[test]
fn csv_normalizes_percentages_and_parenthesized_negative_currency() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    // "10%", "25%", ... all strip to clean integers -> i64, with a note
    // distinct from plain currency/thousands-separator stripping.
    let discount = column(cols, "discount_pct");
    assert_eq!(discount["ideal_type"], "i64");
    assert!(discount["notes"].as_str().unwrap().contains('%'));

    // "(45.00)" is standard accounting notation for -45.00.
    let adjustment = column(cols, "adjustment");
    assert_eq!(adjustment["ideal_type"], "f64");
    assert!(!adjustment["notes"].as_str().unwrap().contains('%'));
}

#[test]
fn csv_recognizes_rfc3339_timestamps_and_time_of_day() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    // "2024-01-15T10:00:00Z" - UTC 'Z' suffix, ubiquitous in JSON APIs.
    let created = column(cols, "created_at");
    assert_eq!(created["ideal_type"], "NaiveDate / DateTime");

    // "2024-01-15T10:00:00+00:00" - numeric offset instead of 'Z'.
    let updated = column(cols, "updated_at");
    assert_eq!(updated["ideal_type"], "NaiveDate / DateTime");

    // "09:00:00" - time-of-day only, no date component at all. Also proves
    // the leading-zero heuristic no longer preempts a structured time match
    // ("09" looks like a leading-zero-then-digit ID prefix on its own).
    let checkin = column(cols, "checkin_time");
    assert_eq!(checkin["ideal_type"], "NaiveTime");
    assert!(!checkin["notes"].as_str().unwrap().contains("leading zeros"));
}

#[test]
fn csv_recognizes_hex_literals_and_mac_addresses() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let hex = column(cols, "hex_value");
    assert_eq!(hex["current_type"], "String");
    assert_eq!(hex["ideal_type"], "i64");
    assert!(hex["notes"].as_str().unwrap().contains("0x"));

    let mac = column(cols, "mac_address");
    assert_eq!(mac["ideal_type"], "MAC Address");
}

#[test]
fn csv_recognizes_iban_and_credit_card_numbers_via_checksum() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let iban = column(cols, "iban");
    assert_eq!(iban["ideal_type"], "IBAN");

    // Card numbers are plain digit strings that fit i64 - current_type says
    // i64, but ideal_type correctly identifies an opaque identifier rather
    // than a quantity, the same current-vs-ideal gap this tool exists for.
    let card = column(cols, "credit_card");
    assert_eq!(card["current_type"], "i64");
    assert_eq!(card["ideal_type"], "Credit Card Number");
}

#[test]
fn csv_recognizes_isbn13_and_ean_upc_barcodes() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let isbn = column(cols, "isbn");
    assert_eq!(isbn["ideal_type"], "ISBN-13");

    let ean = column(cols, "ean_upc");
    assert_eq!(ean["ideal_type"], "EAN-13 / UPC-A");
}

#[test]
fn json_schema_maps_semantic_types_to_standard_format_keywords() {
    let doc = run_with_format("type_detection.csv", "json-schema", &[]);
    let props = &doc["tables"]["type_detection"]["properties"];

    assert_eq!(
        props["user_uuid"],
        serde_json::json!({"type": "string", "format": "uuid"})
    );
    assert_eq!(
        props["contact_email"],
        serde_json::json!({"type": "string", "format": "email"})
    );
    assert_eq!(
        props["ip_address"],
        serde_json::json!({"type": "string", "format": "ipv4"})
    );
    assert_eq!(
        props["ipv6_address"],
        serde_json::json!({"type": "string", "format": "ipv6"})
    );
    assert_eq!(
        props["homepage"],
        serde_json::json!({"type": "string", "format": "uri"})
    );
    assert_eq!(
        props["checkin_time"],
        serde_json::json!({"type": "string", "format": "time"})
    );
    // MAC Address, IBAN, and Credit Card Number all have no registered
    // json-schema.org format keyword - still get a plain "string" type
    // rather than falling through to {}.
    assert_eq!(props["mac_address"], serde_json::json!({"type": "string"}));
    assert_eq!(props["iban"], serde_json::json!({"type": "string"}));
    assert_eq!(props["credit_card"], serde_json::json!({"type": "string"}));
    assert_eq!(props["isbn"], serde_json::json!({"type": "string"}));
    assert_eq!(props["ean_upc"], serde_json::json!({"type": "string"}));
}
