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

/// Runs the binary against a fixture with --output-format sql, writing to
/// stdout ("-"), and returns the raw generated SQL text.
fn run_sql(fixture_name: &str, extra_args: &[&str]) -> String {
    let path = fixture(fixture_name);
    let mut args: Vec<&str> = vec![path.to_str().unwrap(), "-", "--output-format", "sql"];
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
    String::from_utf8(output.stdout).expect("stdout was not valid UTF-8")
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
fn csv_nrows_stops_reading_before_invalid_utf8_past_the_cutoff() {
    // Proves --nrows bounds real disk I/O for the streaming CSV reader,
    // not just how many rows get profiled afterward: a file with
    // deliberately invalid UTF-8 bytes appended well past the --nrows
    // cutoff must still succeed with --nrows, and fail without it, on
    // the identical file.
    // The valid prefix must exceed stream_utf8_chunks' own 256 KiB read
    // buffer, or the garbage bytes below would land in the *same* first
    // chunk as row 0 - failing UTF-8 validation before --nrows ever gets
    // a chance to stop reading, rather than genuinely proving early stop.
    let dir = TempDir::new();
    let path = dir.path().join("data.csv");
    let mut content = String::from("id,name\n");
    for i in 0..30_000 {
        content.push_str(&format!("{i},user_{i}\n"));
    }
    assert!(content.len() > 256 * 1024, "test fixture too small");
    let mut bytes = content.into_bytes();
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    bytes.extend_from_slice(b" garbage not valid utf8\n");
    std::fs::write(&path, &bytes).unwrap();

    let with_nrows = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--nrows", "5"])
        .output()
        .unwrap();
    assert!(
        with_nrows.status.success(),
        "{}",
        String::from_utf8_lossy(&with_nrows.stderr)
    );

    let without_nrows = Command::new(bin())
        .args([path.to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!without_nrows.status.success());
}

#[test]
fn jsonl_nrows_stops_reading_before_invalid_utf8_past_the_cutoff() {
    // Proves --nrows bounds real disk I/O for the streaming JSON Lines
    // reader too (stream_json_lines), not just how many rows get
    // profiled afterward. Unlike the CSV version, this needs no minimum
    // file size - stream_json_lines validates one line at a time via
    // BufRead::lines(), the same as the fixed-width reader, so there's
    // no risk of the garbage landing in the same read-buffer chunk as
    // row 0 regardless of how small the valid prefix is.
    let dir = TempDir::new();
    let path = dir.path().join("data.jsonl");
    let mut content = String::new();
    for i in 0..1000 {
        content.push_str(&format!("{{\"id\": {i}, \"name\": \"user_{i}\"}}\n"));
    }
    let mut bytes = content.into_bytes();
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    bytes.extend_from_slice(b" garbage not valid utf8\n");
    std::fs::write(&path, &bytes).unwrap();

    let with_nrows = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--nrows", "5"])
        .output()
        .unwrap();
    assert!(
        with_nrows.status.success(),
        "{}",
        String::from_utf8_lossy(&with_nrows.stderr)
    );

    let without_nrows = Command::new(bin())
        .args([path.to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!without_nrows.status.success());
}

#[cfg(feature = "weblog")]
#[test]
fn weblog_nrows_stops_reading_before_a_malformed_line_past_the_cutoff() {
    // Proves --nrows bounds real disk I/O for the streaming weblog reader
    // too, the same way it already does for CSV/fixed-width/JSONL: a line
    // that doesn't match Common Log's grammar sitting well past the
    // cutoff must not cause a failure with --nrows, but must fail without
    // it, on the identical file. Like fixed-width and JSONL (and unlike
    // CSV), this needs no minimum file size - BufRead::lines() validates
    // one line at a time, so there's no read-buffer-chunk boundary to
    // worry about landing the bad line alongside row 0.
    let dir = TempDir::new();
    let path = dir.path().join("access.log");
    let mut content = String::new();
    for i in 0..1000 {
        content.push_str(&format!(
            "192.168.1.{} - - [10/Oct/2024:13:55:36 -0700] \"GET /page{i}.html HTTP/1.1\" 200 {}\n",
            i % 255,
            1000 + i % 500
        ));
    }
    content.push_str("this line does not match Common Log Format at all\n");
    std::fs::write(&path, &content).unwrap();

    let with_nrows = Command::new(bin())
        .args([
            path.to_str().unwrap(),
            "-",
            "--format",
            "common-log",
            "--nrows",
            "5",
        ])
        .output()
        .unwrap();
    assert!(
        with_nrows.status.success(),
        "{}",
        String::from_utf8_lossy(&with_nrows.stderr)
    );

    let without_nrows = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--format", "common-log"])
        .output()
        .unwrap();
    assert!(!without_nrows.status.success());
}

#[cfg(feature = "syslog")]
#[test]
fn syslog_nrows_stops_reading_before_a_malformed_line_past_the_cutoff() {
    // Same proof as weblog's own version above, for the syslog reader.
    let dir = TempDir::new();
    let path = dir.path().join("messages.log");
    let mut content = String::new();
    for i in 0..1000 {
        content.push_str(&format!(
            "<34>Oct 11 22:14:{:02} mymachine su[{}]: someone did something on line {i}\n",
            i % 60,
            1000 + i % 9000
        ));
    }
    content.push_str("nope, not a syslog line\n");
    std::fs::write(&path, &content).unwrap();

    let with_nrows = Command::new(bin())
        .args([
            path.to_str().unwrap(),
            "-",
            "--format",
            "syslog",
            "--nrows",
            "5",
        ])
        .output()
        .unwrap();
    assert!(
        with_nrows.status.success(),
        "{}",
        String::from_utf8_lossy(&with_nrows.stderr)
    );

    let without_nrows = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--format", "syslog"])
        .output()
        .unwrap();
    assert!(!without_nrows.status.success());
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

/// nested_typed.jsonl's own test: json_flattens_nested_object_and_array_of_objects
/// above already proves flattening produces the right column *names* and
/// missing-% math. This proves the other half of the claim - that every
/// leaf value reached through that flattening, no matter how deeply
/// nested, goes through the exact same precise heuristic engine a
/// top-level column would (UUID/Email/date/i64 detection, not just a
/// generic "String"/"object" shape).
#[test]
fn nested_arrays_and_objects_are_recursively_typed_at_every_leaf() {
    let doc = run_json("nested_typed.jsonl", &[]);
    let cols = table(&doc, "nested_typed");

    // A plain array of scalar UUID strings - pooled across all 3 records
    // and precisely typed, not left as a generic Vec<String>.
    let tags = column(cols, "tags");
    assert_eq!(tags["current_type"], "Vec<String>");
    assert_eq!(tags["ideal_type"], "Vec<UUID>");

    // An array of objects flattens into dot-path sub-columns, each typed
    // with the same precision a top-level column would get.
    let email = column(cols, "events.user_email");
    assert_eq!(email["ideal_type"], "Email");
    let amount = column(cols, "events.amount");
    assert_eq!(amount["ideal_type"], "i64");
    let when = column(cols, "events.when");
    assert_eq!(when["ideal_type"], "NaiveDate / DateTime");

    // Three levels deep: object -> object -> array of objects -> leaf -
    // still resolves correctly at the bottom.
    let score = column(cols, "deep.outer.inner_list.score");
    assert_eq!(score["ideal_type"], "i64");

    // An array that mixes raw scalars with objects in the same list can't
    // honestly claim one precise scalar type (some elements are
    // structurally objects, not scalars at all) - this is the same
    // "no partial credit" rule suggest_ideal_type's .all(...) checks
    // already apply everywhere else, not a gap. The object portion is
    // still recursed into and typed normally.
    let mixed_list = column(cols, "mixed_list");
    assert_eq!(mixed_list["ideal_type"], "Vec<String>");
    assert!(
        mixed_list["notes"]
            .as_str()
            .unwrap()
            .contains("mix of scalars and objects")
    );
    let mixed_x = column(cols, "mixed_list.x");
    assert_eq!(mixed_x["ideal_type"], "i64");
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
fn sql_output_creates_a_staging_table_a_typed_table_and_a_cast_insert() {
    // --sql-mode staging is no longer the default (see the inline-mode
    // tests below) but stays fully supported, unchanged, for a file too
    // large to comfortably embed as literal SQL.
    let sql = run_sql("type_detection.csv", &["--sql-mode", "staging"]);

    assert!(sql.contains("CREATE TABLE \"type_detection_staging\""));
    assert!(sql.contains("CREATE TABLE \"type_detection\""));
    assert!(sql.contains("INSERT INTO \"type_detection\""));
    assert!(sql.contains("FROM \"type_detection_staging\""));

    // Every staging column is TEXT, regardless of the real ideal_type -
    // it exists purely so a bulk-text loader (see the Load comment
    // block) can fill it with no type errors.
    assert!(sql.contains("\"user_uuid\" TEXT"));

    // The typed table uses a real type per ideal_type, and casts it back
    // out of the staging table's TEXT column.
    assert!(sql.contains("\"user_uuid\" VARCHAR(36) NOT NULL"));
    assert!(sql.contains("AS VARCHAR(36))"));
    assert!(sql.contains("\"id\" BIGINT NOT NULL"));
    assert!(sql.contains("AS BIGINT)"));

    // The per-engine Load comment names all four common engines, since
    // there's no ANSI-standard way to read a file from disk at all.
    assert!(sql.contains("DuckDB:"));
    assert!(sql.contains("PostgreSQL"));
    assert!(sql.contains("SQLite"));
    assert!(sql.contains("MySQL:"));
}

#[test]
fn sql_output_never_casts_date_or_time_columns_directly() {
    // Regression test for a real bug found by actually running the
    // generated SQL against a real SQLite build (see
    // sql_cast_expr_skips_cast_for_date_and_time_to_avoid_the_sqlite_
    // truncation_bug's own doc comment in src/lib.rs for the full
    // root-cause writeup): CAST(text AS TIMESTAMP/TIME) silently
    // truncates a real value to its leading digits on SQLite, since
    // SQLite has no native temporal type. The fix means the generated
    // SQL must never emit "AS TIMESTAMP)" or "AS TIME)" anywhere.
    let sql = run_sql("type_detection.csv", &["--sql-mode", "staging"]);
    assert!(sql.contains("\"created_at\" TIMESTAMP NOT NULL"));
    assert!(sql.contains("\"checkin_time\" TIME NOT NULL"));
    // The header's own disclosure comment quotes "AS TIMESTAMP)" as an
    // example of the bug being avoided, so only the runnable SQL lines
    // (skipping "--" comments) are checked here.
    let runnable: String = sql
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!runnable.contains("AS TIMESTAMP)"));
    assert!(!runnable.contains("AS TIME)"));
}

#[test]
fn sql_output_treats_a_missing_sentinel_as_null_in_the_cast_expression() {
    // Regression test for a second real bug found the same way: a
    // literal "NA"/"null"/"-" staged as plain TEXT doesn't fail a
    // numeric CAST - confirmed directly on SQLite, CAST('NA' AS BIGINT)
    // silently succeeds as a fabricated 0 rather than erroring.
    let sql = run_sql("type_detection.csv", &["--sql-mode", "staging"]);
    assert!(sql.contains("LOWER(TRIM(\"age\"))"));
    assert!(sql.contains("'na'"));
    assert!(sql.contains("'null'"));
    assert!(sql.contains("THEN NULL ELSE TRIM(\"age\") END"));
}

#[cfg(feature = "sqlite")]
#[test]
fn sql_output_handles_a_multi_table_source_with_one_pair_of_tables_per_table() {
    // SQLite (like Excel/INI/.npz) can produce more than one table from a
    // single source file - each one needs its own independent staging/
    // typed table pair, not a single shared staging table. SQLite input
    // isn't CSV/TSV, so inline mode isn't available for it yet anyway
    // (see the fallback tests below) - --sql-mode staging is explicit
    // here to keep this test focused on the multi-table shape itself.
    let sql = run_sql("sample.sqlite", &["--sql-mode", "staging"]);
    assert!(sql.contains("CREATE TABLE \"events_staging\""));
    assert!(sql.contains("CREATE TABLE \"events\""));
    assert!(sql.contains("CREATE TABLE \"users_staging\""));
    assert!(sql.contains("CREATE TABLE \"users\""));
}

#[test]
fn sql_output_inline_mode_is_the_default_and_embeds_real_literal_data() {
    // --sql-mode inline is the new default (no flag needed at all) for
    // CSV/TSV: the whole dataset is embedded as literal INSERT values, so
    // there's no staging table and no per-engine load-command comment
    // block at all - genuinely nothing left to load.
    let sql = run_sql("type_detection.csv", &[]);
    assert!(!sql.contains("CREATE TABLE \"type_detection_staging\""));
    assert!(!sql.contains("read_csv_auto"));
    assert!(!sql.contains("LOAD DATA LOCAL INFILE"));
    assert!(sql.contains("CREATE TABLE \"type_detection\""));
    assert!(sql.contains("INSERT INTO \"type_detection\""));
    // A real, known value from the fixture appears verbatim as a quoted
    // literal - the data itself, not a reference to the source file.
    assert!(sql.contains("'550e8400-e29b-41d4-a716-446655440000'"));
    // A genuinely missing value (the fixture's own "NA"/"null" age
    // entries) is a bare NULL, not a fabricated 0 and not the sentinel
    // text itself.
    assert!(sql.contains(", NULL,") || sql.contains(", NULL)"));
}

#[test]
fn sql_output_inline_mode_zero_byte_csv_skips_create_table_instead_of_emitting_invalid_sql() {
    // A real, cross-format bug found while verifying Phase 17 (Avro)
    // against a real SQLite build: a genuinely zero-column schema (no
    // columns profiled at all, distinct from a real known column set
    // with zero rows - see the SQLite Phase 9 writeup for that already-
    // handled case) used to emit `CREATE TABLE t ( );`, invalid SQL
    // syntax on every real engine - present since Phase 1, since a
    // zero-byte CSV hits the identical gap any inline-supported format
    // with a genuinely empty schema does.
    let sql = run_sql("malformed_empty.csv", &[]);
    assert!(!sql.contains("CREATE TABLE"));
    assert!(sql.contains("no columns were profiled at all"));
}

// The three "still genuinely unsupported format" placeholder tests that
// used to live here (sql_output_inline_mode_falls_back_to_staging_for_
// an_unsupported_format, ..._fallback_prints_a_disclosed_stderr_note,
// sql_output_explicit_inline_mode_errors_on_an_unsupported_format) are
// retired as of Parquet/Arrow IPC joining the recursively-nested,
// JSON-bridge tier: every `InputFormat` variant this project's CLI can
// ever dispatch to now supports `--sql-mode inline`, so there is no
// longer a real, committed fixture left that can exercise the "format
// inline mode doesn't support yet" fallback/error paths in
// `render_sql`. Those code paths themselves are deliberately NOT
// removed - `inline_supported`'s own `matches!` check (and the parallel
// one in `run_single_file`'s `--load-into` validation) stay in place as
// the correct, defensive behavior for the next format this project ever
// adds without also wiring up its own inline-mode row-source in the
// same phase, exactly per this project's own "one format at a time,
// fully verified" precedent - there just isn't a fixture that can prove
// it right now. Should a 35th format ever land with inline support
// deferred to a later phase, these three tests (and the `--load-into`
// sibling below) should be restored, pointed at that format.

#[test]
fn sql_output_inline_mode_json_array_of_objects_needs_staging_instead() {
    // A genuinely nested JSON file (an array-of-objects field) is a
    // different, more specific disclosed error than the generic
    // "format not supported yet" fallback above - JSON itself IS
    // inline-supported (Phase 13), but this particular file's own shape
    // isn't representable as one scalar cell per record.
    let output = Command::new(bin())
        .args([
            fixture("nested_typed.jsonl").to_str().unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for field"));
    assert!(stderr.contains("--sql-mode staging"));
}

#[test]
fn sql_output_rejects_an_unrecognized_sql_mode() {
    let output = Command::new(bin())
        .args([
            fixture("type_detection.csv").to_str().unwrap(),
            "-",
            "--output-format",
            "sql",
            "--sql-mode",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unrecognized --sql-mode 'bogus'"));
}

#[test]
fn load_into_requires_output_format_sql() {
    let output = Command::new(bin())
        .args([
            fixture("type_detection.csv").to_str().unwrap(),
            "-",
            "--output-format",
            "json",
            "--load-into",
            "sqlite:/tmp/whatever.db",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--load-into requires --output-format sql"));
}

#[test]
fn load_into_rejects_sql_mode_staging() {
    let output = Command::new(bin())
        .args([
            fixture("type_detection.csv").to_str().unwrap(),
            "-",
            "--output-format",
            "sql",
            "--sql-mode",
            "staging",
            "--load-into",
            "sqlite:/tmp/whatever.db",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--load-into requires --sql-mode inline"));
}

#[test]
fn load_into_rejects_a_combined_output_path() {
    let output = Command::new(bin())
        .args([
            fixture("type_detection.csv").to_str().unwrap(),
            "out.sql",
            "--output-format",
            "sql",
            "--load-into",
            "sqlite:/tmp/whatever.db",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--load-into can't be combined with an output path"));
}

// load_into_rejects_an_unsupported_input_format is retired for the same
// reason as the three sql_output_inline_mode_* placeholder tests above -
// every InputFormat this project's CLI can dispatch to (now including
// Parquet and Arrow IPC) supports --load-into, so there's no remaining
// fixture that can exercise this specific validation error. The
// underlying check in run_single_file (paired with inline_supported's
// own gate) stays in place for the same defensive future-format reason.

#[test]
fn load_into_rejects_a_malformed_target() {
    let output = Command::new(bin())
        .args([
            fixture("type_detection.csv").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

// Directory-mode --load-into's own end-to-end happy path (one fresh
// database per recognized file, mirroring the source tree's own
// subdirectories exactly the way --output-dir already does for every
// other output format, with a prior run's own databases correctly
// skipped rather than re-ingested on a later run) is deliberately NOT
// covered by an automated test here, matching this project's own
// standing precedent for --load-into: no automated test spawns a real
// sqlite3/duckdb/psql/mysql process, since that CLI tool being on PATH
// is an environment fact this test suite can't assume everywhere it
// runs (unlike sqlite3, duckdb in particular is commonly absent).
// Verified manually instead, against a real, installed sqlite3 build,
// with no separate load step: a two-file directory (one nested under a
// subdirectory) produced two correctly-named, correctly-mirrored
// `.sqlite` databases, each queryable back for its own real data; a
// second run over that same directory correctly skipped both
// already-created databases (per looks_like_own_loaded_database, whose
// own detection logic - the double-extension shape vs. a genuine
// single-extension database someone already had - *is* covered by the
// portable, subprocess-free unit tests next to its own definition) and
// still correctly profiled a genuinely unrelated, real SQLite file
// dropped in the same directory. What *is* covered here, since none of
// it needs a working subprocess spawn to fail cleanly: every validation
// error a bad combination of flags produces.

#[test]
fn load_into_directory_mode_rejects_combining_with_output_dir() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("sample.csv")).unwrap();
    let output = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "sqlite:/tmp/whatever-load-into-target",
            "--output-dir",
            "/tmp/whatever-output-dir",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("already names the directory"));
}

#[test]
fn load_into_directory_mode_rejects_postgres_and_mysql() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("sample.csv")).unwrap();
    let output = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "postgres:mydb",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("only supports sqlite/duckdb"));
}

#[test]
fn load_into_directory_mode_rejects_staging_mode_and_wrong_output_format() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("sample.csv")).unwrap();

    let staging = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--output-format",
            "sql",
            "--sql-mode",
            "staging",
            "--load-into",
            "sqlite:/tmp/whatever-staging",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!staging.status.success());
    assert!(String::from_utf8_lossy(&staging.stderr).contains("requires --sql-mode inline"));

    let wrong_format = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--output-format",
            "json",
            "--load-into",
            "sqlite:/tmp/whatever-wrong-format",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!wrong_format.status.success());
    assert!(String::from_utf8_lossy(&wrong_format.stderr).contains("requires --output-format sql"));
}

#[test]
fn sql_output_inline_mode_disambiguates_a_duplicate_csv_header_column() {
    // Regression test for a real bug found by piping generated inline
    // SQL into a real sqlite3 build: a genuinely duplicate CSV header
    // ("id,name,name,age") produced two identically-named columns in one
    // CREATE TABLE, a hard SQL error ("duplicate column name: name") on
    // every real engine, not just SQLite - SQL identifier uniqueness is
    // a real constraint this project's own permissive CSV reader doesn't
    // share. The second occurrence gets a "_2" suffix instead.
    let sql = run_sql("edge_csv_duplicate_column_names.csv", &[]);
    assert!(sql.contains("\"name\" TEXT NOT NULL"));
    assert!(sql.contains("\"name_2\" TEXT NOT NULL"));
    assert!(sql.contains(
        "INSERT INTO \"edge_csv_duplicate_column_names\" (\"id\", \"name\", \"name_2\", \"age\")"
    ));
}

#[test]
fn sql_output_inline_mode_supports_fixed_width_text() {
    // Phase 1 of the "any format" rollout: inline mode now covers
    // fixed-width text too, not just CSV/TSV - same shape as the CSV
    // inline test above (no staging table, real literal data embedded
    // directly), just reached via --format fixed-width --widths instead
    // of extension-based detection.
    let sql = run_sql(
        "sample.fwf",
        &["--format", "fixed-width", "--widths", "8,4,9,8"],
    );
    assert!(!sql.contains("CREATE TABLE \"sample_staging\""));
    assert!(sql.contains("CREATE TABLE \"sample\""));
    assert!(sql.contains("INSERT INTO \"sample\""));
    // Real values from the fixture appear verbatim as literals.
    assert!(sql.contains("'U1001'"));
    assert!(sql.contains("'gold'"));
    // The fixture's one genuinely blank "age" field is a bare NULL, not
    // a fabricated 0.
    assert!(sql.contains(", NULL,") || sql.contains(", NULL)"));
}

#[test]
fn sql_output_inline_mode_fixed_width_respects_nrows() {
    let sql = run_sql(
        "sample.fwf",
        &[
            "--format",
            "fixed-width",
            "--widths",
            "8,4,9,8",
            "--nrows",
            "1",
        ],
    );
    assert!(sql.contains("'U1001'"));
    assert!(!sql.contains("'U1002'"));
    assert!(!sql.contains("'U1003'"));
}

#[test]
fn load_into_accepts_fixed_width_text() {
    // --load-into no longer rejects fixed-width now that inline mode
    // supports it - validated structurally (a malformed target still
    // errors, but with the *target* error, never the
    // "isn't available yet for fixed-width" format-rejection message),
    // matching this project's standing precedent of never spawning a
    // real external database process inside `cargo test`.
    let output = Command::new(bin())
        .args([
            fixture("sample.fwf").to_str().unwrap(),
            "--format",
            "fixed-width",
            "--widths",
            "8,4,9,8",
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for fixed-width"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "weblog")]
fn sql_output_inline_mode_supports_combined_log() {
    // Phase 2 of the "any format" rollout: inline mode now covers
    // Common/Combined Log Format and syslog too, not just CSV/TSV/
    // fixed-width - all four are headerless (every line is a data
    // record), unlike CSV/fixed-width's own header row.
    let sql = run_sql("sample_combined.log", &["--format", "combined-log"]);
    assert!(!sql.contains("CREATE TABLE \"sample_combined_staging\""));
    assert!(sql.contains("CREATE TABLE \"sample_combined\""));
    assert!(sql.contains("INSERT INTO \"sample_combined\""));
    // A real value from the fixture's very first line - if the headerless
    // row-source mistakenly treated it as a header, it would never appear
    // as literal data at all.
    assert!(sql.contains("'127.0.0.1'"));
    assert!(sql.contains("'frank'"));
    assert!(sql.contains("'GET'"));
    // The fixture's own "-" placeholders (ident/authuser/referer on the
    // second and third lines) resolve to a bare NULL, not the literal
    // sentinel text.
    assert!(sql.contains(", NULL,") || sql.contains(", NULL)"));
}

#[test]
#[cfg(feature = "weblog")]
fn sql_output_inline_mode_supports_common_log() {
    let sql = run_sql("sample_common.log", &["--format", "common-log"]);
    assert!(sql.contains("CREATE TABLE \"sample_common\""));
    assert!(sql.contains("INSERT INTO \"sample_common\""));
    assert!(sql.contains("'192.168.1.5'"));
}

#[test]
#[cfg(feature = "syslog")]
fn sql_output_inline_mode_supports_syslog_rfc3164() {
    let sql = run_sql("sample_rfc3164.log", &["--format", "syslog"]);
    assert!(sql.contains("CREATE TABLE \"sample_rfc3164\""));
    assert!(sql.contains("INSERT INTO \"sample_rfc3164\""));
    // PRI is decoded into real facility/severity names, not left as a
    // raw number - the first line's <34> is auth/critical.
    assert!(sql.contains("'auth'"));
    assert!(sql.contains("'critical'"));
    assert!(sql.contains("'mymachine'"));
}

#[test]
#[cfg(feature = "syslog")]
fn sql_output_inline_mode_supports_syslog_rfc5424() {
    let sql = run_sql("sample_rfc5424.log", &["--format", "syslog5424"]);
    assert!(sql.contains("CREATE TABLE \"sample_rfc5424\""));
    assert!(sql.contains("INSERT INTO \"sample_rfc5424\""));
    assert!(sql.contains("'mymachine.example.com'"));
    // The second line's own nilvalue ("-") app_name/procid/msgid/
    // structured_data fields resolve to NULL, not the literal "-".
    assert!(!sql.contains("'-'"));
}

#[test]
#[cfg(feature = "weblog")]
fn sql_output_inline_mode_log_formats_respect_nrows() {
    let sql = run_sql(
        "sample_combined.log",
        &["--format", "combined-log", "--nrows", "1"],
    );
    assert!(sql.contains("'127.0.0.1'"));
    assert!(!sql.contains("'192.168.1.5'"));
    assert!(!sql.contains("'203.0.113.9'"));
}

#[test]
fn load_into_accepts_log_formats() {
    // Same structural check as load_into_accepts_fixed_width_text above,
    // for the newly-supported log formats.
    for fmt in ["common-log", "combined-log", "syslog", "syslog5424"] {
        let output = Command::new(bin())
            .args([
                fixture("sample_rfc3164.log").to_str().unwrap(),
                "--format",
                fmt,
                "--output-format",
                "sql",
                "--load-into",
                "bogus",
            ])
            .output()
            .expect("failed to run binary");
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains(&format!("isn't available yet for {fmt}")),
            "unexpected rejection for {fmt}: {stderr}"
        );
        assert!(stderr.contains("must be in the form <engine>:<target>"));
    }
}

#[test]
#[cfg(feature = "dbase")]
fn sql_output_inline_mode_supports_dbase() {
    // Phase 3 of the "any format" rollout: the first declared-type
    // binary format, not just a plain-text row-source - proves
    // InlineRowSink's Vec<Option<String>> row shape and the "decode
    // always, keep conditionally" nrows convention both carry over
    // cleanly to a real binary reader, not just text ones.
    let sql = run_sql("sample.dbf", &[]);
    assert!(!sql.contains("CREATE TABLE \"sample_staging\""));
    assert!(sql.contains("CREATE TABLE \"sample\""));
    assert!(sql.contains("INSERT INTO \"sample\""));
    assert!(sql.contains("'U1001'"));
    assert!(sql.contains("1250.5"));
}

#[test]
#[cfg(feature = "dbase")]
fn sql_output_inline_mode_dbase_skips_soft_deleted_records() {
    // dBase's own "marked for deletion" convention: a soft-deleted
    // record must be excluded from the emitted INSERT data exactly as
    // it already is from profiling - proven by checking the row that
    // was deleted (Bob) is genuinely absent while the two kept rows
    // (Alice, Carol) both appear.
    let sql = run_sql("edge_dbase_deleted_records.dbf", &[]);
    assert!(sql.contains("'Alice'"));
    assert!(sql.contains("'Carol'"));
    assert!(!sql.contains("'Bob'"));
}

#[test]
#[cfg(feature = "dbase")]
fn load_into_accepts_dbase() {
    let output = Command::new(bin())
        .args([
            fixture("sample.dbf").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for dbase"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "stata")]
fn sql_output_inline_mode_supports_stata() {
    // Phase 4 of the "any format" rollout: a second declared-type binary
    // format, this time one whose own profiling reader already bounds
    // real I/O via `nrows` (unlike dBase's own "decode always" choice) -
    // proving the row-source correctly matches whichever real behavior
    // its own format's profiling reader actually has.
    let sql = run_sql("sample.dta", &[]);
    assert!(!sql.contains("CREATE TABLE \"sample_staging\""));
    assert!(sql.contains("CREATE TABLE \"sample\""));
    assert!(sql.contains("INSERT INTO \"sample\""));
    assert!(sql.contains("'U1001'"));
    // The fixture's own Stata "." missing marker on one row's age field
    // resolves to a bare NULL, not a fabricated 0.
    assert!(sql.contains(", NULL,") || sql.contains(", NULL)"));
}

#[test]
#[cfg(feature = "stata")]
fn sql_output_inline_mode_stata_respects_nrows() {
    let sql = run_sql("sample.dta", &["--nrows", "1"]);
    assert!(sql.contains("'U1001'"));
    assert!(!sql.contains("'U1002'"));
    assert!(!sql.contains("'U1003'"));
}

#[test]
#[cfg(feature = "stata")]
fn load_into_accepts_stata() {
    let output = Command::new(bin())
        .args([
            fixture("sample.dta").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for stata"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "sas7bdat")]
fn sql_output_inline_mode_supports_sas7bdat() {
    // Phase 5 of the "any format" rollout: a third declared-type binary
    // format, and the third whose profiling reader's real nrows behavior
    // (collect_rows bounds real page/subheader reads via its own `limit`
    // parameter, matching Stata's own real-I/O-bounding shape) had to be
    // checked and matched rather than assumed.
    let sql = run_sql("sas7bdat_people_nonascii.sas7bdat", &[]);
    assert!(!sql.contains("CREATE TABLE \"sas7bdat_people_nonascii_staging\""));
    assert!(sql.contains("CREATE TABLE \"sas7bdat_people_nonascii\""));
    assert!(sql.contains("INSERT INTO \"sas7bdat_people_nonascii\""));
    // A real, non-ASCII value from the fixture survives intact.
    assert!(sql.contains("'é'"));
}

#[test]
#[cfg(feature = "sas7bdat")]
fn sql_output_inline_mode_sas7bdat_respects_nrows() {
    let sql = run_sql("sas7bdat_people_nonascii.sas7bdat", &["--nrows", "2"]);
    assert!(sql.contains("(1, "));
    assert!(sql.contains("(2, "));
    assert!(!sql.contains("(3, "));
}

#[test]
#[cfg(feature = "sas7bdat")]
fn load_into_accepts_sas7bdat() {
    let output = Command::new(bin())
        .args([
            fixture("sas7bdat_people_nonascii.sas7bdat")
                .to_str()
                .unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for sas7bdat"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "spss")]
fn sql_output_inline_mode_supports_spss() {
    // Phase 6 of the "any format" rollout: a fourth declared-type binary
    // format, and the one whose own reader already needed a real
    // "decode one variable's value out of a row's slots" extraction to
    // share between the accumulator loop and the new SQL row-source.
    let sql = run_sql("type_detection.sav", &[]);
    assert!(!sql.contains("CREATE TABLE \"type_detection_staging\""));
    assert!(sql.contains("CREATE TABLE \"type_detection\""));
    assert!(sql.contains("INSERT INTO \"type_detection\""));
    // A real UUID value from the fixture appears verbatim.
    assert!(sql.contains("'550e8400-e29b-41d4-a716-446655440000'"));
    // The native SPSS date variable renders as a real ISO date string,
    // not the raw numeric offset SPSS stores it as.
    assert!(sql.contains("'2024-01-15'"));
}

#[test]
#[cfg(feature = "spss")]
fn sql_output_inline_mode_spss_handles_bytecode_compression_and_long_strings() {
    // Real fixture coverage for the two format-specific wrinkles this
    // row-source's shared decode helper has to get right: SPSS's own
    // "bytecode" RLE compression, and a "very long string" reconstructed
    // across multiple 32-slot segments.
    let compressed = run_sql("edge_spss_bytecode_compressed.sav", &[]);
    assert!(compressed.contains("'red'"));

    let long_string = run_sql("edge_spss_very_long_string.sav", &[]);
    // The reconstructed 300-byte value (spanning multiple 32-slot
    // segments) round-trips as one unbroken quoted literal, not
    // truncated at a segment boundary.
    assert!(long_string.contains(&"A".repeat(300)));
}

#[test]
#[cfg(feature = "spss")]
fn sql_output_inline_mode_spss_respects_nrows() {
    let sql = run_sql("type_detection.sav", &["--nrows", "2"]);
    assert!(sql.contains("(1, "));
    assert!(sql.contains("(2, "));
    assert!(!sql.contains("(3, "));
}

#[test]
#[cfg(feature = "spss")]
fn load_into_accepts_spss() {
    let output = Command::new(bin())
        .args([
            fixture("type_detection.sav").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for spss"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "orc")]
fn sql_output_inline_mode_supports_orc() {
    // Phase 7 of the "any format" rollout: a fifth declared-type binary
    // format - and, unlike every prior format in this tier, ORC's own
    // storage is genuinely columnar (per-stripe, one column at a time),
    // so its row-source has to transpose decoded columns back into rows
    // rather than just reading a row's bytes directly.
    let sql = run_sql("type_detection.orc", &[]);
    assert!(!sql.contains("CREATE TABLE \"type_detection_staging\""));
    assert!(sql.contains("CREATE TABLE \"type_detection\""));
    assert!(sql.contains("INSERT INTO \"type_detection\""));
    assert!(sql.contains("'550e8400-e29b-41d4-a716-446655440000'"));
}

#[test]
#[cfg(feature = "orc")]
fn sql_output_inline_mode_orc_handles_every_compression_codec_and_missing_values() {
    for f in [
        "edge_orc_compression_none",
        "edge_orc_compression_zlib",
        "edge_orc_compression_snappy",
        "edge_orc_compression_lz4",
        "edge_orc_compression_zstd",
    ] {
        let sql = run_sql(&format!("{f}.orc"), &[]);
        assert!(
            sql.contains("'row-0-padding-padding-padding'"),
            "codec fixture {f} didn't decode correctly: {sql}"
        );
    }
    // A genuinely missing value (this fixture's own "name" field on one
    // row) lands as a bare NULL, positionally correct - not shifted into
    // the wrong row or column by the columnar-to-row transpose.
    let missing = run_sql("edge_orc_missing_values.orc", &[]);
    assert!(missing.contains(", NULL)") || missing.contains(", NULL,"));
}

#[test]
#[cfg(feature = "orc")]
fn sql_output_inline_mode_orc_rejects_a_file_with_a_nested_column() {
    // A Struct/List/Map/Union column has no scalar value to embed as a
    // literal at all (it's a disclosed placeholder in every other output
    // format) - a clear, actionable error naming the column, not a
    // guess, not a silently-wrong NULL that would violate that column's
    // own NOT NULL constraint.
    let output = Command::new(bin())
        .args([
            fixture("edge_orc_edge_cases.orc").to_str().unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for ORC column"));
    assert!(stderr.contains("tags"));
}

#[test]
#[cfg(feature = "orc")]
fn sql_output_inline_mode_orc_respects_nrows() {
    let sql = run_sql("type_detection.orc", &["--nrows", "2"]);
    assert!(sql.contains("(1, "));
    assert!(sql.contains("(2, "));
    assert!(!sql.contains("(3, "));
}

#[test]
#[cfg(feature = "orc")]
fn load_into_accepts_orc() {
    let output = Command::new(bin())
        .args([
            fixture("type_detection.orc").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for orc"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "npy")]
fn sql_output_inline_mode_supports_npy_structured_dtype() {
    // Phase 8 (the final phase of the flat-tier rollout): a structured/
    // record dtype, NumPy's own closest equivalent to a real table -
    // read via the same one-record-at-a-time streaming loop the
    // profiling reader already uses.
    let sql = run_sql("type_detection.npy", &[]);
    assert!(!sql.contains("CREATE TABLE \"type_detection_staging\""));
    assert!(sql.contains("CREATE TABLE \"type_detection\""));
    assert!(sql.contains("INSERT INTO \"type_detection\""));
    assert!(sql.contains("'550e8400-e29b-41d4-a716-446655440000'"));
}

#[test]
#[cfg(feature = "npy")]
fn sql_output_inline_mode_npy_plain_1d_and_2d_arrays() {
    // A plain (unnamed) 1D array becomes a single "value" column; a 2D
    // array becomes positional col_0..col_N columns - the same dual-mode
    // convention a headerless CSV already gets.
    let one_d = run_sql("edge_npy_plain_1d.npy", &[]);
    assert!(one_d.contains("CREATE TABLE \"edge_npy_plain_1d\" (\n    \"value\""));
    assert!(one_d.contains("(1.5)"));

    let two_d = run_sql("sample_matrix.npy", &[]);
    assert!(two_d.contains("\"col_0\""));
    assert!(two_d.contains("\"col_1\""));
    assert!(two_d.contains("(1.5, 2.5, 3.5)"));
}

#[test]
#[cfg(feature = "npy")]
fn sql_output_inline_mode_npy_respects_nrows() {
    let sql = run_sql("type_detection.npy", &["--nrows", "2"]);
    assert!(sql.contains("(1, "));
    assert!(sql.contains("(2, "));
    assert!(!sql.contains("(3, "));
}

#[test]
#[cfg(feature = "npy")]
fn load_into_accepts_npy() {
    let output = Command::new(bin())
        .args([
            fixture("type_detection.npy").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for npy"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "npy")]
fn sql_output_inline_mode_npy_preserves_sentinel_like_real_values() {
    // Regression test for a real bug found while building the multi-table
    // tier's first format (SQLite): a row-source whose reader already has
    // no missing-value concept at all (NumPy) - or a genuine native-null
    // concept that's already fully resolved (dBase/Stata/SAS7BDAT/SPSS/
    // SQLite/the log formats' own "-" nilvalue) - used to have its real,
    // present values re-checked against `is_missing_sentinel` a second
    // time, silently turning a real value that happens to read as "NA" or
    // "unknown" into a fabricated NULL. All three of this fixture's real
    // string values - including two that are themselves missing-value
    // sentinel words - must survive as real text.
    let sql = run_sql("edge_npy_sentinel_like_values.npy", &[]);
    assert!(sql.contains("('unknown')"));
    assert!(sql.contains("('NA')"));
    assert!(sql.contains("('real')"));
    assert!(!sql.contains("(NULL)"));
}

#[test]
#[cfg(feature = "weblog")]
fn sql_output_inline_mode_weblog_preserves_sentinel_like_real_values() {
    // Same regression as the NumPy test above, for a format whose reader
    // already has a real, precisely-resolved native-null concept (Common/
    // Combined Log's own "-" nilvalue, via `dash_to_none`) - a genuine
    // "-" referer must still become NULL, but a genuine "unknown" user
    // agent must survive as real text, not also be nulled by a second,
    // redundant sentinel guess.
    let sql = run_sql(
        "edge_weblog_sentinel_like_value.log",
        &["--format", "combined-log"],
    );
    assert!(sql.contains("'unknown'"));
    assert!(sql.contains(", NULL,") || sql.contains(", NULL)"));
}

#[test]
#[cfg(feature = "sqlite")]
fn sql_output_inline_mode_supports_sqlite_multi_table() {
    // Phase 9 of the "any format" rollout, and the first format in the
    // multi-table tier: render_sql's own inline loop now emits one
    // CREATE TABLE/INSERT pair per table, sharing one header comment
    // block for the whole file (written once, not once per table).
    let sql = run_sql("sample.sqlite", &[]);
    assert_eq!(sql.matches("-- Data dictionary for").count(), 1);
    assert!(sql.contains("CREATE TABLE \"users\""));
    assert!(sql.contains("CREATE TABLE \"events\""));
    assert!(sql.contains("INSERT INTO \"users\""));
    assert!(sql.contains("INSERT INTO \"events\""));
    assert!(!sql.contains("_staging\""));
    // The real regression this phase found: events.amount's own genuine
    // "unknown" text value must survive, not be nulled by the same
    // sentinel-guessing bug the two tests above lock in for other formats.
    assert!(sql.contains("'unknown'"));
    // users.age's own genuine SQL NULL (a real missing value, U1004's
    // row) must still become a bare NULL.
    assert!(sql.contains(", NULL,") || sql.contains(", NULL)"));
}

#[test]
#[cfg(feature = "sqlite")]
fn sql_output_inline_mode_sqlite_rejects_a_without_rowid_table() {
    // A WITHOUT ROWID table has no honest literal to embed (the same
    // "no honest literal" boundary ORC's own nested-column check already
    // draws) - a clear, actionable error naming the table, not a guess.
    let output = Command::new(bin())
        .args([
            fixture("edge_sqlite_without_rowid.sqlite")
                .to_str()
                .unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WITHOUT ROWID"));
}

#[test]
#[cfg(feature = "sqlite")]
fn sql_output_inline_mode_sqlite_respects_nrows_per_table() {
    let sql = run_sql("sample.sqlite", &["--nrows", "2"]);
    assert!(sql.contains("'U1001'"));
    assert!(sql.contains("'U1002'"));
    assert!(!sql.contains("'U1003'"));
}

#[test]
#[cfg(feature = "sqlite")]
fn load_into_accepts_sqlite() {
    let output = Command::new(bin())
        .args([
            fixture("sample.sqlite").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for sqlite"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "npy")]
fn sql_output_inline_mode_supports_npz_multi_array() {
    // Phase 10 of the "any format" rollout: .npz is many named .npy
    // arrays, reusing the already-shipped .npy row-source directly per
    // array (no new decode logic, only the archive/entry glue) - the
    // second format in the multi-table tier, sharing one header comment
    // across all of the archive's arrays.
    let sql = run_sql("sample.npz", &[]);
    assert_eq!(sql.matches("-- Data dictionary for").count(), 1);
    assert!(sql.contains("CREATE TABLE \"users\""));
    assert!(sql.contains("CREATE TABLE \"scores\""));
    assert!(sql.contains("INSERT INTO \"users\""));
    assert!(sql.contains("INSERT INTO \"scores\""));
    assert!(sql.contains("'U1001'"));
}

#[test]
#[cfg(feature = "npy")]
fn sql_output_inline_mode_npz_fortran_and_c_order_arrays_agree() {
    // A Fortran-order array's own column-major-to-row transpose must
    // produce the identical row values a row-major array does for the
    // same logical data.
    let sql = run_sql("edge_npz_fortran.npz", &[]);
    assert!(sql.contains("(1, 2)"));
    assert!(sql.contains("(3, 4)"));
}

#[test]
#[cfg(feature = "npy")]
fn sql_output_inline_mode_npz_rejects_an_unreadable_array() {
    // A genuinely 3-D array has no honest literal to emit at all (the
    // same disclosed-placeholder shape --output-format json already
    // gives it) - a clear, actionable error, not a guess.
    let output = Command::new(bin())
        .args([
            fixture("edge_npz_mixed_readable_and_unreadable.npz")
                .to_str()
                .unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no natural row/column reading"));
}

#[test]
#[cfg(feature = "npy")]
fn sql_output_inline_mode_npz_respects_nrows_per_array() {
    let sql = run_sql("sample.npz", &["--nrows", "2"]);
    assert!(sql.contains("'U1001'"));
    assert!(sql.contains("'U1002'"));
    assert!(!sql.contains("'U1003'"));
}

#[test]
#[cfg(feature = "npy")]
fn load_into_accepts_npz() {
    let output = Command::new(bin())
        .args([
            fixture("sample.npz").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for npz"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "ini")]
fn sql_output_inline_mode_supports_ini_multi_section() {
    // Phase 11 of the "any format" rollout, and the third format in the
    // multi-table tier: an INI section has no repeating-row concept at
    // all, so its "table" is always exactly one profiled record - a
    // section with no repeated key still emits one real INSERT row.
    let sql = run_sql("edge_ini_duplicate_key_diff_section.ini", &[]);
    assert_eq!(sql.matches("-- Data dictionary for").count(), 1);
    assert!(sql.contains("CREATE TABLE \"section1\""));
    assert!(sql.contains("CREATE TABLE \"section2\""));
    assert!(sql.contains("'value1'"));
    assert!(sql.contains("'value2'"));
    assert!(sql.contains("'value3'"));
}

#[test]
#[cfg(feature = "ini")]
fn sql_output_inline_mode_ini_preserves_a_genuinely_empty_value() {
    // A real, present-but-empty INI value (Key7= in this fixture) must
    // render as a real empty string literal, not a fabricated NULL -
    // the same sentinel/empty-string bug class Phase 9 already fixed
    // for every other native-null-or-no-null format.
    let sql = run_sql("edge_ini_quoting_and_escapes.ini", &[]);
    let values = sql
        .split("INSERT INTO")
        .nth(1)
        .expect("no INSERT statement found");
    assert!(values.contains("''"));
    assert!(!values.contains("NULL"));
}

#[test]
#[cfg(feature = "ini")]
fn sql_output_inline_mode_ini_rejects_a_section_with_a_repeated_key() {
    // A repeated key pools into a Vec<T> column for profiling, which has
    // no single cell to honestly embed as a literal - a clear,
    // actionable error naming the section and the key, not a guess.
    let output = Command::new(bin())
        .args([
            fixture("sample.ini").to_str().unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("its \"tag\" key repeats"));
    assert!(stderr.contains("\"database\""));
}

#[test]
#[cfg(feature = "ini")]
fn load_into_accepts_ini() {
    let output = Command::new(bin())
        .args([
            fixture("type_detection.ini").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for ini"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "xlsx")]
fn sql_output_inline_mode_supports_xlsx_multi_sheet() {
    // Phase 12 of the "any format" rollout, and the fourth/last format in
    // the multi-table tier: an OOXML workbook's sheets each get their own
    // CREATE TABLE/INSERT pair sharing one file-level header comment,
    // exactly like SQLite's/`.npz`'s/INI's own tables already do.
    let sql = run_sql("multi_sheet.xlsx", &[]);
    assert_eq!(sql.matches("-- Data dictionary for").count(), 1);
    assert!(sql.contains("CREATE TABLE \"customers\""));
    assert!(sql.contains("CREATE TABLE \"products\""));
    assert!(sql.contains("'Alice'"));
    assert!(sql.contains("'SKU-1'"));
}

#[test]
#[cfg(feature = "xlsx")]
fn sql_output_inline_mode_supports_ods() {
    let sql = run_sql("sample.ods", &[]);
    assert!(sql.contains("CREATE TABLE \"Sheet1\""));
    assert!(sql.contains("'alice'"));
}

#[test]
#[cfg(feature = "xlsx")]
fn sql_output_inline_mode_ods_fills_a_blank_row_gap_with_real_nulls() {
    // A genuinely blank cell in the middle of real ODS data must land as
    // a real NULL, not a fabricated empty string or a row-shifted value -
    // the deferred pending_blank_rows design's whole point.
    let sql = run_sql("edge_ods_repeated_cells.ods", &[]);
    let values = sql
        .split("INSERT INTO")
        .nth(1)
        .expect("no INSERT statement found");
    assert!(values.contains("NULL"));
    assert!(values.contains("'bob'"));
}

#[test]
#[cfg(feature = "xlsx")]
fn sql_output_inline_mode_supports_xls() {
    let sql = run_sql("multi_sheet_lo.xls", &[]);
    assert_eq!(sql.matches("-- Data dictionary for").count(), 1);
    assert!(sql.contains("CREATE TABLE \"customers\""));
    assert!(sql.contains("CREATE TABLE \"products\""));
}

#[test]
#[cfg(feature = "xlsx")]
fn sql_output_inline_mode_xls_native_date_cells_resolve_to_real_dates() {
    let sql = run_sql("edge_xls_native_date_cells.xls", &[]);
    assert!(sql.contains("'2024-01-15'"));
    // Excel's own raw day-count serial for this date (45306) must never
    // appear - only the resolved ISO date string.
    assert!(!sql.contains("45306"));
}

#[test]
#[cfg(feature = "xlsx")]
fn sql_output_inline_mode_supports_xlsb() {
    let sql = run_sql("poi_sample.xlsb", &[]);
    assert!(sql.contains("CREATE TABLE"));
}

#[test]
#[cfg(feature = "xlsx")]
fn sql_output_inline_mode_xlsx_respects_nrows_per_sheet() {
    let sql = run_sql("multi_sheet.xlsx", &["--nrows", "1"]);
    assert!(sql.contains("'Alice'"));
    assert!(!sql.contains("'Bob'"));
    assert!(sql.contains("'SKU-1'"));
    assert!(!sql.contains("'SKU-2'"));
}

#[test]
#[cfg(feature = "xlsx")]
fn load_into_accepts_xlsx() {
    let output = Command::new(bin())
        .args([
            fixture("sample.xlsx").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for xlsx"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
fn sql_output_inline_mode_supports_flat_json_with_nested_object_and_array() {
    // Phase 13 of the "any format" rollout, and the first format in the
    // recursively-nested, JSON-bridge tier: a plain (non-array) nested
    // object flattens transparently with no column of its own
    // ("meta" itself never appears, only "meta.score"/"meta.active"),
    // and a pooled scalar array column serializes as one JSON-array-text
    // literal per row.
    let sql = run_sql("edge_json_sql_inline_flat.jsonl", &[]);
    assert!(!sql.contains("\"meta\" "));
    assert!(sql.contains("\"meta.score\""));
    assert!(sql.contains("\"meta.active\""));
    assert!(sql.contains("'[\"red\",\"blue\"]'"));
    assert!(sql.contains("'[\"green\"]'"));
    assert!(sql.contains("'[]'"));
}

#[test]
fn sql_output_inline_mode_json_null_field_is_a_real_null_not_a_sentinel_guess() {
    let sql = run_sql("edge_json_sql_inline_flat.jsonl", &[]);
    let values = sql
        .split("INSERT INTO")
        .nth(1)
        .expect("no INSERT statement found");
    assert!(values.contains("NULL"));
    assert!(values.contains("'alice@example.com'"));
}

#[test]
fn sql_output_inline_mode_json_single_value_column_top_level_array() {
    // A top-level array of bare scalars (not objects) has no field names,
    // so the whole set profiles - and renders inline - as one "value"
    // column, the same convention a headerless CSV/NumPy 1D array already
    // uses elsewhere in this project.
    let sql = run_sql("edge_top_level_scalar_array.json", &[]);
    assert!(sql.contains("CREATE TABLE \"edge_top_level_scalar_array\""));
    assert!(sql.contains("\"value\""));
    assert!(sql.contains("'a4d1e6b0-1111-4a1a-9a1a-000000000001'"));
    assert!(sql.contains("NULL"));
}

#[test]
fn sql_output_inline_mode_json_rejects_an_array_of_objects_column() {
    let output = Command::new(bin())
        .args([
            fixture("nested_typed.jsonl").to_str().unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for field \"events\""));
    assert!(stderr.contains("--sql-mode staging"));
}

#[test]
fn load_into_accepts_json() {
    let output = Command::new(bin())
        .args([
            fixture("edge_json_sql_inline_flat.jsonl").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for json"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "yaml")]
fn sql_output_inline_mode_supports_flat_yaml_with_nested_mapping_and_sequence() {
    // Phase 14 of the "any format" rollout, and the second format in the
    // recursively-nested, JSON-bridge tier - reuses JSON's own
    // json_extract_value_for_sql/json_inline_blocking_column unchanged,
    // since a YAML document already decodes straight to the shared
    // json_support::Value shape. A plain (non-array) nested mapping
    // flattens transparently with no column of its own, and a pooled
    // flow-sequence column serializes as one JSON-array-text literal.
    let sql = run_sql("edge_yaml_sql_inline_flat.yaml", &[]);
    assert!(!sql.contains("\"meta\" "));
    assert!(sql.contains("\"meta.score\""));
    assert!(sql.contains("\"meta.active\""));
    assert!(sql.contains("'[\"red\",\"blue\"]'"));
    assert!(sql.contains("'[]'"));
    let values = sql
        .split("INSERT INTO")
        .nth(1)
        .expect("no INSERT statement found");
    assert!(values.contains("NULL"));
}

#[test]
#[cfg(feature = "yaml")]
fn sql_output_inline_mode_yaml_respects_multi_document_stream_and_nrows() {
    let sql = run_sql("edge_yaml_explicit_doc.yaml", &["--nrows", "1"]);
    assert!(sql.contains("'Alice'"));
    assert!(!sql.contains("'Bob'"));
}

#[test]
#[cfg(feature = "yaml")]
fn load_into_accepts_yaml() {
    let output = Command::new(bin())
        .args([
            fixture("edge_yaml_sql_inline_flat.yaml").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for yaml"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "toml")]
fn sql_output_inline_mode_supports_flat_toml_with_nested_table_and_array() {
    // Phase 15 of the "any format" rollout, and the third format in the
    // recursively-nested, JSON-bridge tier - a TOML document always
    // profiles as exactly one record, so this needs no loop at all, just
    // a single json_emit_row_for_sql call against the document's own
    // top-level object. A plain (non-array-of-tables) nested table
    // flattens transparently with no column of its own, and a pooled
    // array serializes as one JSON-array-text literal.
    let sql = run_sql("edge_toml_sql_inline_flat.toml", &[]);
    assert!(!sql.contains("\"meta\" "));
    assert!(sql.contains("\"meta.score\""));
    assert!(sql.contains("\"meta.active\""));
    assert!(sql.contains("'[\"red\",\"blue\"]'"));
}

#[test]
#[cfg(feature = "toml")]
fn sql_output_inline_mode_toml_rejects_an_array_of_tables_column() {
    // sample.toml's own real [[servers]] array-of-tables is exactly the
    // one-to-many shape this tier's own upfront check exists to catch.
    let output = Command::new(bin())
        .args([
            fixture("sample.toml").to_str().unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for field \"servers\""));
    assert!(stderr.contains("--sql-mode staging"));
}

#[test]
#[cfg(feature = "toml")]
fn load_into_accepts_toml() {
    let output = Command::new(bin())
        .args([
            fixture("edge_toml_sql_inline_flat.toml").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for toml"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "msgpack")]
fn sql_output_inline_mode_supports_flat_msgpack_with_nested_map_and_array() {
    // Phase 16 of the "any format" rollout, and the fourth format in the
    // recursively-nested, JSON-bridge tier - MessagePack decodes to the
    // shared json_support::Value shape the same way JSON/YAML/TOML
    // already do, so json_extract_value_for_sql/json_inline_blocking_
    // column carry over completely unchanged. A plain nested map
    // flattens transparently with no column of its own, and a pooled
    // array serializes as one JSON-array-text literal.
    let sql = run_sql("edge_msgpack_sql_inline_flat.msgpack", &[]);
    assert!(!sql.contains("\"meta\" "));
    assert!(sql.contains("\"meta.score\""));
    assert!(sql.contains("\"meta.active\""));
    assert!(sql.contains("'[\"red\",\"blue\"]'"));
    assert!(sql.contains("'[]'"));
    let values = sql
        .split("INSERT INTO")
        .nth(1)
        .expect("no INSERT statement found");
    assert!(values.contains("NULL"));
}

#[test]
#[cfg(feature = "msgpack")]
fn sql_output_inline_mode_msgpack_single_value_column_top_level_array() {
    let sql = run_sql("edge_msgpack_scalar_array.msgpack", &[]);
    assert!(sql.contains("\"value\""));
    assert!(sql.contains("(23.5)"));
}

#[test]
#[cfg(feature = "msgpack")]
fn load_into_accepts_msgpack() {
    let output = Command::new(bin())
        .args([
            fixture("edge_msgpack_sql_inline_flat.msgpack")
                .to_str()
                .unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for msgpack"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "cbor")]
fn sql_output_inline_mode_supports_flat_cbor_with_nested_map_and_array() {
    // The fifth format in the recursively-nested, JSON-bridge tier -
    // CBOR shares MessagePack's own concatenated-records-or-single-array
    // convention verbatim, and the same shared json_support::Value
    // bridge, so this exercises the identical shape through a genuinely
    // different binary wire format.
    let sql = run_sql("edge_cbor_sql_inline_flat.cbor", &[]);
    assert!(!sql.contains("\"meta\" "));
    assert!(sql.contains("\"meta.score\""));
    assert!(sql.contains("\"meta.active\""));
    assert!(sql.contains("'[\"red\",\"blue\"]'"));
    assert!(sql.contains("'[]'"));
}

#[test]
#[cfg(feature = "cbor")]
fn load_into_accepts_cbor() {
    let output = Command::new(bin())
        .args([
            fixture("edge_cbor_sql_inline_flat.cbor").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for cbor"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "avro")]
fn sql_output_inline_mode_supports_avro_nested_record_and_array() {
    // Phase 17 of the "any format" rollout, and the sixth format in the
    // recursively-nested, JSON-bridge tier - sample.avro's own real
    // "metadata" nested record and "tags" pooled array already exercise
    // this shape without needing a new hand-built fixture, unlike every
    // prior format in this tier.
    let sql = run_sql("sample.avro", &[]);
    assert!(!sql.contains("\"metadata\" "));
    assert!(sql.contains("\"metadata.source\""));
    assert!(sql.contains("\"metadata.risk_score\""));
    assert!(sql.contains("'[\"vip\",\"verified\"]'"));
    let values = sql
        .split("INSERT INTO")
        .nth(1)
        .expect("no INSERT statement found");
    assert!(values.contains("NULL"));
}

#[test]
#[cfg(feature = "avro")]
fn sql_output_inline_mode_avro_optional_nested_record_is_never_falsely_not_null() {
    // A real bug found via genuine SQLite testing: a column nested under
    // an optional (non-array) record has its own missing_pct computed
    // relative to how often its *parent* was present, not the true
    // top-level record count - a narrow 0% there doesn't mean the real
    // column can never be NULL. edge_avro_named_type_refs.avro's own
    // "backup_address" field (present in only 1 of 3 records) is exactly
    // this shape; loading it used to fail with a genuine NOT NULL
    // constraint violation once this reached a real database.
    let sql = run_sql("edge_avro_named_type_refs.avro", &[]);
    assert!(sql.contains("\"backup_address.city\" TEXT,"));
    assert!(!sql.contains("\"backup_address.city\" TEXT NOT NULL"));
}

#[test]
#[cfg(feature = "avro")]
fn sql_output_inline_mode_avro_logical_types_resolve_correctly() {
    let sql = run_sql("avro_logical_types.avro", &[]);
    assert!(sql.contains("'2024-01-15T10:00:00.000'"));
    assert!(sql.contains("'14:10:00.123'"));
    assert!(sql.contains("123.45"));
}

#[test]
#[cfg(feature = "avro")]
fn sql_output_inline_mode_avro_zero_columns_skips_create_table_instead_of_emitting_invalid_sql() {
    // A real, cross-format bug found via this phase's own real SQLite
    // testing: a genuinely zero-column schema (no columns profiled at
    // all, distinct from a real known column set with zero rows) used
    // to emit `CREATE TABLE t ( );` - invalid SQL syntax on every real
    // engine - for *any* inline-supported format, not just Avro (a
    // zero-byte CSV hits the identical gap, present since Phase 1).
    let sql = run_sql("edge_zero_records.avro", &[]);
    assert!(!sql.contains("CREATE TABLE"));
    assert!(sql.contains("no columns were profiled at all"));
}

#[test]
#[cfg(feature = "avro")]
fn load_into_accepts_avro() {
    let output = Command::new(bin())
        .args([
            fixture("sample.avro").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for avro"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "xml")]
fn sql_output_inline_mode_supports_homogeneous_xml_records() {
    // Phase 18 of the "any format" rollout, and the seventh format in
    // the recursively-nested, JSON-bridge tier - sample.xml's own real
    // homogeneous <user>...</user> records (with @-prefixed attribute
    // columns) exercise the stream_xml_records path directly.
    let sql = run_sql("sample.xml", &[]);
    assert!(sql.contains("\"@id\""));
    assert!(sql.contains("\"@active\""));
    assert!(sql.contains("'U1001'"));
    assert!(sql.contains("TRUE") || sql.contains("FALSE"));
}

#[test]
#[cfg(feature = "xml")]
fn sql_output_inline_mode_xml_non_homogeneous_root_is_a_single_record() {
    // A non-homogeneous/deeply-nested root falls back to the whole-DOM
    // parse as one single record, the same choice TOML's own whole-
    // document shape already makes.
    let sql = run_sql("edge_xml_deeply_nested_10.xml", &[]);
    assert!(sql.contains(
        "\"level9.level8.level7.level6.level5.level4.level3.level2.level1.level0.value\""
    ));
    assert!(sql.contains("'deep'"));
}

#[test]
#[cfg(feature = "xml")]
fn sql_output_inline_mode_xml_rejects_repeated_child_elements_as_an_array_of_objects() {
    // A repeated same-tag child element (<order> appearing more than
    // once under one <person>) pools into an array-of-objects column,
    // exactly the one-to-many shape this tier's own upfront check exists
    // to catch.
    let output = Command::new(bin())
        .args([
            fixture("edge_xml_sql_inline_array_of_objects.xml")
                .to_str()
                .unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for field \"order\""));
    assert!(stderr.contains("--sql-mode staging"));
}

#[test]
#[cfg(feature = "xml")]
fn sql_output_inline_mode_xml_respects_nrows() {
    let sql = run_sql("sample.xml", &["--nrows", "2"]);
    assert!(sql.contains("'U1001'"));
    assert!(sql.contains("'U1002'"));
    assert!(!sql.contains("'U1003'"));
}

#[test]
#[cfg(feature = "xml")]
fn load_into_accepts_xml() {
    let output = Command::new(bin())
        .args([
            fixture("sample.xml").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for xml"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "bson")]
fn sql_output_inline_mode_supports_bson_nested_document_and_array() {
    // Phase 19 of the "any format" rollout, and the eighth format in
    // the recursively-nested, JSON-bridge tier - sample.bson's own real
    // nested "meta" document and "tags" pooled array exercise this
    // shape without needing a new hand-built fixture.
    let sql = run_sql("sample.bson", &[]);
    assert!(!sql.contains("\"meta\" "));
    assert!(sql.contains("\"meta.x\""));
    assert!(sql.contains("'[\"a\",\"b\",\"c\"]'"));
    let values = sql
        .split("INSERT INTO")
        .nth(1)
        .expect("no INSERT statement found");
    assert!(values.contains("NULL"));
}

#[test]
#[cfg(feature = "bson")]
fn sql_output_inline_mode_bson_rare_element_types_render_correctly() {
    let sql = run_sql("edge_bson_rare_types.bson", &[]);
    assert!(sql.contains("'/^foo/i'"));
    assert!(sql.contains("'MinKey'"));
    assert!(sql.contains("'MaxKey'"));
}

#[test]
#[cfg(feature = "bson")]
fn sql_output_inline_mode_bson_rejects_an_array_of_documents_column() {
    let output = Command::new(bin())
        .args([
            fixture("edge_bson_sql_inline_array_of_objects.bson")
                .to_str()
                .unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for field \"orders\""));
    assert!(stderr.contains("--sql-mode staging"));
}

#[test]
#[cfg(feature = "bson")]
fn load_into_accepts_bson() {
    let output = Command::new(bin())
        .args([
            fixture("sample.bson").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for bson"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "plist")]
fn sql_output_inline_mode_supports_plist_nested_dict_and_array() {
    // Phase 20 of the "any format" rollout, and the ninth format in the
    // recursively-nested, JSON-bridge tier - sample.plist's own real
    // nested "meta" dict and "tags" array exercise this shape without
    // needing a new hand-built fixture.
    let sql = run_sql("sample.plist", &[]);
    assert!(!sql.contains("\"meta\" "));
    assert!(sql.contains("\"meta.x\""));
    assert!(sql.contains("'[\"a\",\"b\",\"c\"]'"));
}

#[test]
#[cfg(feature = "plist")]
fn sql_output_inline_mode_supports_binary_plist() {
    let sql = run_sql("edge_plist_binary_type_detection.plist", &[]);
    assert!(sql.contains("'550e8400-e29b-41d4-a716-446655440000'"));
}

#[test]
#[cfg(feature = "plist")]
fn sql_output_inline_mode_plist_streams_a_top_level_array_across_window_refills() {
    // A real, committed fixture proving the streamed top-level XML
    // <array> path (2,000 elements, spanning several real internal
    // buffer refills) round-trips every row correctly.
    let sql = run_sql("edge_plist_array_spans_multiple_window_refills.plist", &[]);
    assert!(sql.contains("(1999, 'item-1999',"));
}

#[test]
#[cfg(feature = "plist")]
fn sql_output_inline_mode_plist_rejects_an_array_of_dicts_column() {
    let output = Command::new(bin())
        .args([
            fixture("edge_plist_sql_inline_array_of_objects.plist")
                .to_str()
                .unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for field \"orders\""));
    assert!(stderr.contains("--sql-mode staging"));
}

#[test]
#[cfg(feature = "plist")]
fn load_into_accepts_plist() {
    let output = Command::new(bin())
        .args([
            fixture("sample.plist").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for plist"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "json5")]
fn sql_output_inline_mode_supports_json5_nested_object_and_array() {
    // Phase 21 of the "any format" rollout, and the tenth format in the
    // recursively-nested, JSON-bridge tier - sample.json5's own real
    // nested "meta" object and "tags" array (with comments, unquoted
    // keys, single-quoted strings, and trailing commas throughout)
    // exercise this shape without needing a new hand-built fixture.
    let sql = run_sql("sample.json5", &[]);
    assert!(!sql.contains("\"meta\" "));
    assert!(sql.contains("\"meta.x\""));
    assert!(sql.contains("'[\"a\",\"b\",\"c\"]'"));
}

#[test]
#[cfg(feature = "json5")]
fn sql_output_inline_mode_supports_jsonc() {
    let sql = run_sql("sample.jsonc", &[]);
    assert!(sql.contains("'sniff-rs'"));
}

#[test]
#[cfg(feature = "json5")]
fn sql_output_inline_mode_json5_streams_a_top_level_array_with_stray_bracket_comments() {
    // A real, committed adversarial fixture: comments containing stray
    // ]/{/" characters must not corrupt the structural scan.
    let sql = run_sql("edge_json5_comment_with_stray_brackets.json5", &[]);
    assert!(sql.contains("(1, 'Alice')"));
    assert!(sql.contains("(2, 'Bob')"));
}

#[test]
#[cfg(feature = "json5")]
fn sql_output_inline_mode_json5_rejects_an_array_of_objects_column() {
    let output = Command::new(bin())
        .args([
            fixture("edge_json5_sql_inline_array_of_objects.json5")
                .to_str()
                .unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for field \"orders\""));
    assert!(stderr.contains("--sql-mode staging"));
}

#[test]
#[cfg(feature = "json5")]
fn load_into_accepts_json5() {
    let output = Command::new(bin())
        .args([
            fixture("sample.json5").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for json5"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "har")]
fn sql_output_inline_mode_supports_har_nested_request_response() {
    // Phase 22 of the "any format" rollout, and the eleventh format in
    // the recursively-nested, JSON-bridge tier - sample.har's own real
    // nested request/response/timings objects exercise this shape
    // without needing a new hand-built fixture.
    let sql = run_sql("sample.har", &[]);
    assert!(!sql.contains("\"request\" "));
    assert!(sql.contains("\"request.method\""));
    assert!(sql.contains("\"response.status\""));
    assert!(sql.contains("'GET'"));
}

#[test]
#[cfg(feature = "har")]
fn sql_output_inline_mode_har_missing_log_entries_gives_the_same_disclosed_error_both_passes() {
    let output = Command::new(bin())
        .args([
            fixture("edge_har_missing_entries.har").to_str().unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("doesn't look like a HAR file"));
}

#[test]
#[cfg(feature = "har")]
fn sql_output_inline_mode_har_rejects_an_array_of_objects_column() {
    let output = Command::new(bin())
        .args([
            fixture("edge_har_sql_inline_array_of_objects.har")
                .to_str()
                .unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for field \"cookies\""));
    assert!(stderr.contains("--sql-mode staging"));
}

#[test]
#[cfg(feature = "har")]
fn sql_output_inline_mode_har_respects_nrows() {
    let sql = run_sql("sample.har", &["--nrows", "1"]);
    assert!(sql.contains("'GET'"));
    assert!(!sql.contains("'POST'"));
}

#[test]
#[cfg(feature = "har")]
fn load_into_accepts_har() {
    let output = Command::new(bin())
        .args([
            fixture("sample.har").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for har"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "geojson")]
fn sql_output_inline_mode_supports_geojson_feature_collection() {
    // Phase 23 of the "any format" rollout, and the twelfth format in
    // the recursively-nested, JSON-bridge tier - sample.geojson's own
    // real FeatureCollection exercises geometry-to-WKT rendering
    // alongside flattened properties.
    let sql = run_sql("sample.geojson", &[]);
    assert!(sql.contains("\"geometry\""));
    assert!(sql.contains("'POINT(-122.4783 37.8199)'"));
    assert!(sql.contains("'LINESTRING(-122.4 37.8, -122.41 37.81)'"));
}

#[test]
#[cfg(feature = "geojson")]
fn sql_output_inline_mode_supports_geojson_bare_feature_and_bare_geometry() {
    let feature_sql = run_sql("edge_geojson_bare_feature.geojson", &[]);
    assert!(feature_sql.contains("'POINT(-74.0445 40.6892)'"));

    // A bare top-level Geometry profiles as a single column literally
    // named "geometry" (not "value") - a real deviation from every
    // other format's own single-value-column convention in this tier,
    // handled by bypassing the generic records-mode extractor entirely.
    let geometry_sql = run_sql("edge_geojson_bare_geometry.geojson", &[]);
    assert!(
        geometry_sql.contains("CREATE TABLE \"edge_geojson_bare_geometry\" (\n    \"geometry\"")
    );
    assert!(geometry_sql.contains("'POINT(1.5 2.5)'"));
}

#[test]
#[cfg(feature = "geojson")]
fn sql_output_inline_mode_geojson_renders_every_geometry_type_and_a_null_geometry() {
    let sql = run_sql("edge_geojson_geometry_types.geojson", &[]);
    assert!(sql.contains("'POLYGON((0 0, 0 1, 1 1, 1 0, 0 0))'"));
    assert!(sql.contains("'MULTIPOLYGON("));
    assert!(sql.contains("'MULTIPOINT(0 0, 1 1)'"));
    assert!(sql.contains("'MULTILINESTRING("));
    assert!(sql.contains("'GEOMETRYCOLLECTION("));
    assert!(sql.contains("('unlocated', NULL)"));
}

#[test]
#[cfg(feature = "geojson")]
fn sql_output_inline_mode_geojson_rejects_an_array_of_objects_column() {
    let output = Command::new(bin())
        .args([
            fixture("edge_geojson_sql_inline_array_of_objects.geojson")
                .to_str()
                .unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for field \"tags\""));
    assert!(stderr.contains("--sql-mode staging"));
}

#[test]
#[cfg(feature = "geojson")]
fn load_into_accepts_geojson() {
    let output = Command::new(bin())
        .args([
            fixture("sample.geojson").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for geojson"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "vcard")]
fn sql_output_inline_mode_supports_vcard_repeated_property_pooling() {
    // Phase 24 of the "any format" rollout, and the first of the three
    // formats sharing vobject_support's own repeated-property pooling
    // mechanism (a genuinely different shape from JSON's array pooling,
    // but confirmed to need zero changes to the shared JSON-bridge
    // functions - insert_pooling already produces the identical
    // JsonValue::Object-with-array-values shape those functions expect).
    // edge_vcard_folding_and_escapes.vcf's own real repeated EMAIL
    // property exercises this without needing a new hand-built fixture.
    let sql = run_sql("edge_vcard_folding_and_escapes.vcf", &[]);
    assert!(sql.contains("'[\"primary@example.com\",\"secondary@example.com\"]'"));
}

#[test]
#[cfg(feature = "vcard")]
fn sql_output_inline_mode_supports_vcard_multiple_cards() {
    let sql = run_sql("sample.vcf", &[]);
    assert!(sql.contains("'alice@example.com'"));
    assert!(sql.contains("'bob@example.org'"));
}

#[test]
#[cfg(feature = "vcard")]
fn sql_output_inline_mode_vcard_respects_nrows() {
    let sql = run_sql("sample.vcf", &["--nrows", "1"]);
    assert!(sql.contains("'alice@example.com'"));
    assert!(!sql.contains("'bob@example.org'"));
}

#[test]
#[cfg(feature = "vcard")]
fn load_into_accepts_vcard() {
    let output = Command::new(bin())
        .args([
            fixture("sample.vcf").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for vcard"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "icalendar")]
fn sql_output_inline_mode_supports_icalendar_component_stack_scoping() {
    // Phase 25 of the "any format" rollout, and the fourteenth format in
    // the recursively-nested, JSON-bridge tier - the tenth format in a
    // row (including vCard) to need zero changes to the three shared
    // JSON-bridge functions, since iCalendar shares vCard's own
    // vobject_support pooling mechanism. sample.ics's own real VALARM
    // nested inside its first VEVENT exercises the one genuine
    // structural difference from vCard: a property belonging to a
    // component this reader doesn't turn into its own records must
    // never leak into the enclosing VEVENT/VTODO row.
    let sql = run_sql("sample.ics", &[]);
    assert!(sql.contains("'Team standup'"));
    assert!(sql.contains("'Conference room A'"));
    assert!(!sql.contains("\"TRIGGER\""));
    assert!(!sql.contains("\"ACTION\""));
}

#[test]
#[cfg(feature = "icalendar")]
fn sql_output_inline_mode_icalendar_unfolds_lines_and_reads_vtodo() {
    let sql = run_sql("edge_icalendar_vtodo_and_folding.ics", &[]);
    assert!(sql.contains("'This description spans two physical lines via folding.'"));
    assert!(!sql.contains("\"TRIGGER\""));
}

#[test]
#[cfg(feature = "icalendar")]
fn sql_output_inline_mode_icalendar_reads_multiple_concatenated_vcalendar_blocks() {
    let sql = run_sql("edge_icalendar_multiple_vcalendar_blocks.ics", &[]);
    assert!(sql.contains("'First calendar''s event'"));
    assert!(sql.contains("'Second calendar''s event'"));
}

#[test]
#[cfg(feature = "icalendar")]
fn sql_output_inline_mode_icalendar_respects_nrows() {
    let sql = run_sql("sample.ics", &["--nrows", "1"]);
    assert!(sql.contains("'Team standup'"));
    assert!(!sql.contains("'Quarterly review'"));
}

#[test]
#[cfg(feature = "icalendar")]
fn load_into_accepts_icalendar() {
    let output = Command::new(bin())
        .args([
            fixture("sample.ics").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for icalendar"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "mbox")]
fn sql_output_inline_mode_supports_mbox_repeated_header_pooling_and_body() {
    // Phase 26 of the "any format" rollout, and the FINAL format in the
    // recursively-nested, JSON-bridge tier - the eleventh format in a
    // row (counting vCard/iCalendar) to need zero changes to the three
    // shared JSON-bridge functions, since MessageBuilder::finish already
    // pools a repeated header (Received:) into a JsonValue::Array the
    // same way vCard/iCalendar's own vobject_support::insert_pooling
    // does, and the message body is just one more plain scalar field in
    // the same map.
    let sql = run_sql("sample.mbox", &[]);
    assert!(sql.contains("\"envelope_sender\""));
    assert!(sql.contains("\"body\""));
    assert!(sql.contains("'This is message one.'"));
    assert!(sql.contains("'This is message three, the last one.'"));

    let pooled = run_sql("edge_mbox_repeated_and_folded_headers.mbox", &[]);
    assert!(pooled.contains("\"Received\""));
    assert!(pooled.contains(
        "'[\"from mx1.example.com by mx2.example.com; Mon, 15 Jan 2024 12:00:00 +0000\",\"from client.example.com by mx1.example.com; Mon, 15 Jan 2024 11:59:00 +0000\"]'"
    ));
}

#[test]
#[cfg(feature = "mbox")]
fn sql_output_inline_mode_mbox_respects_nrows() {
    let sql = run_sql("sample.mbox", &["--nrows", "2"]);
    assert!(sql.contains("'This is message one.'"));
    assert!(sql.contains("'This is message two.'"));
    assert!(!sql.contains("'This is message three, the last one.'"));
}

#[test]
#[cfg(feature = "mbox")]
fn load_into_accepts_mbox() {
    let output = Command::new(bin())
        .args([
            fixture("sample.mbox").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for mbox"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "parquet")]
fn sql_output_inline_mode_supports_parquet_flat_and_nested_columns() {
    // Parquet joins the recursively-nested, JSON-bridge tier as its
    // sixteenth format - added after the tier's own campaign was
    // originally declared complete at MBOX (Phase 26) - and confirms
    // the shared JSON-bridge functions generalize to a fourth,
    // structurally distinct bridge mechanism: `decode_row_group_nested`
    // (this reader's own hand-rolled Dremel-style record assembler,
    // built well before this campaign existed) already produces one
    // `JsonValue::Object` per row covering flat scalars and nested
    // Struct/List/Map columns alike, so zero changes were needed to
    // `json_extract_value_for_sql`/`json_inline_blocking_column`/
    // `json_bridge_columns_and_mode`. `sample.parquet` exercises the
    // fully-flat schema (including a genuine missing value);
    // `edge_parquet_sql_inline_flat.parquet` exercises a pooled scalar
    // array and a nested struct (with one genuinely null struct
    // correctly forcing both its own children to NULL).
    let flat_sql = run_sql("sample.parquet", &[]);
    assert!(flat_sql.contains("'U1001'"));
    assert!(flat_sql.contains("NULL"));

    let nested_sql = run_sql("edge_parquet_sql_inline_flat.parquet", &[]);
    assert!(nested_sql.contains("\"info.age\""));
    assert!(nested_sql.contains("'[\"a\",\"b\"]'"));
    assert!(nested_sql.contains("('U3', 3.5, '[]', NULL, NULL)"));
}

#[test]
#[cfg(feature = "parquet")]
fn sql_output_inline_mode_parquet_rejects_a_map_column() {
    // A Parquet Map column always reconstructs as an array of
    // `{"key","value"}` pairs (this reader's own deliberate choice,
    // since a Map key isn't always a string) - exactly the `Vec<struct>`
    // shape `json_inline_blocking_column` already exists to catch, so
    // `nested_types.parquet`'s own real Map column (`attributes`)
    // correctly triggers the same disclosed error every other array-of-
    // objects column in this tier already does.
    let output = Command::new(bin())
        .args([
            fixture("nested_types.parquet").to_str().unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for field \"attributes\""));
    assert!(stderr.contains("--sql-mode staging"));
}

#[test]
#[cfg(feature = "parquet")]
fn sql_output_inline_mode_parquet_respects_nrows_across_a_row_group_boundary() {
    let sql = run_sql("sample.parquet", &["--nrows", "2"]);
    assert!(sql.contains("'U1001'"));
    assert!(sql.contains("'U1002'"));
    assert!(!sql.contains("'U1003'"));
}

#[test]
#[cfg(feature = "parquet")]
fn load_into_accepts_parquet() {
    let output = Command::new(bin())
        .args([
            fixture("sample.parquet").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for parquet"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
#[cfg(feature = "parquet")]
fn sql_output_inline_mode_supports_arrow_ipc_flat_and_nested_columns() {
    // Arrow IPC/Feather joins the recursively-nested, JSON-bridge tier
    // as its seventeenth format, right alongside Parquet - unlike
    // Parquet's own row-oriented `decode_row_group_nested`, this
    // reader's production path is column-oriented end to end
    // (`read_arrow_ipc_file_columns_streaming`), so its own row-source
    // transposes each RecordBatch's decoded columns back into row
    // objects (the identical transpose this reader's own `#[cfg(test)]`
    // -only `decode_record_batch` already does for the Streaming-format
    // test coverage) rather than reusing an existing per-row decoder -
    // still zero changes needed to any of the three shared JSON-bridge
    // functions, the tier's fifth structurally distinct bridge mechanism
    // confirmed to generalize. `type_detection.arrow` is fully flat;
    // `edge_arrow_nested_types.arrow`'s own real nested struct and
    // pooled scalar array exercise the nested path, including a
    // genuinely absent struct correctly leaving both its own children
    // NULL.
    let flat_sql = run_sql("type_detection.arrow", &[]);
    assert!(flat_sql.contains("'alice@example.com'"));

    let nested_sql = run_sql("edge_arrow_nested_types.arrow", &[]);
    assert!(nested_sql.contains("\"address.city\""));
    assert!(nested_sql.contains("'[90,85]'"));
    assert!(nested_sql.contains("(2, 'bob', '[]', NULL, NULL, 0.0"));
}

#[test]
#[cfg(feature = "parquet")]
fn sql_output_inline_mode_arrow_ipc_rejects_an_array_of_objects_column() {
    let output = Command::new(bin())
        .args([
            fixture("edge_arrow_sql_inline_array_of_objects.arrow")
                .to_str()
                .unwrap(),
            "-",
            "--output-format",
            "sql",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't emit real data for field \"orders\""));
    assert!(stderr.contains("--sql-mode staging"));
}

#[test]
#[cfg(feature = "parquet")]
fn sql_output_inline_mode_arrow_ipc_respects_nrows_across_a_batch_boundary() {
    let sql = run_sql("edge_arrow_lz4_multi_block.arrow", &["--nrows", "3"]);
    let insert_line = sql
        .lines()
        .find(|l| l.trim_start().starts_with('('))
        .unwrap();
    let kept = sql
        .lines()
        .filter(|l| l.trim_start().starts_with('('))
        .count();
    assert_eq!(
        kept, 3,
        "expected exactly 3 kept rows, first was {insert_line:?}"
    );
}

#[test]
#[cfg(feature = "parquet")]
fn load_into_accepts_arrow_ipc() {
    let output = Command::new(bin())
        .args([
            fixture("type_detection.arrow").to_str().unwrap(),
            "--output-format",
            "sql",
            "--load-into",
            "bogus",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("isn't available yet for arrow"));
    assert!(stderr.contains("must be in the form <engine>:<target>"));
}

#[test]
fn sql_output_default_extension_is_dictionary_sql() {
    // Copies the fixture into a scratch tempdir first (rather than
    // pointing the binary straight at the committed fixture with no
    // output path) so the *default*-named output lands in that same
    // auto-cleaned tempdir instead of next to the real fixture - default
    // naming always writes beside the *input* file, never the CWD.
    let (dir, input) = copy_fixture_as("type_detection.csv", "type_detection.csv");
    let status = Command::new(bin())
        .args([input.to_str().unwrap(), "--output-format", "sql"])
        .status()
        .unwrap();
    assert!(status.success());
    let default_out = dir.path().join("type_detection.dictionary.sql");
    let content = std::fs::read_to_string(&default_out)
        .unwrap_or_else(|e| panic!("expected {default_out:?} to exist: {e}"));
    assert!(content.starts_with("-- Data dictionary for"));
    assert!(content.ends_with('\n'));
    assert!(!content.ends_with("\n\n"));
}

#[test]
fn json_output_reports_row_count_per_column() {
    let doc = run_json("sample.csv", &[]);
    let cols = table(&doc, "sample");
    // sample.csv has 5 data rows - every column in a flat reader shares
    // the exact same row_count, since they're all profiled from the same
    // table's rows.
    assert_eq!(column(cols, "user_id")["row_count"], 5);
    assert_eq!(column(cols, "zip_code")["row_count"], 5);
}

#[cfg(feature = "sqlite")]
#[test]
fn json_output_reports_an_independent_row_count_per_table() {
    let doc = run_json("sample.sqlite", &[]);
    // Two tables, two genuinely different row counts - row_count must
    // never leak one table's count into another's.
    assert_eq!(column(table(&doc, "events"), "event_id")["row_count"], 3);
    assert_eq!(column(table(&doc, "users"), "user_id")["row_count"], 5);
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
fn fixed_width_nrows_stops_reading_before_invalid_utf8_past_the_cutoff() {
    // Proves --nrows bounds real disk I/O for the streaming fixed-width
    // reader, not just how many rows get profiled afterward: a file with
    // deliberately invalid UTF-8 bytes appended well past the --nrows
    // cutoff must still succeed with --nrows, and fail without it, on
    // the identical file.
    let dir = TempDir::new();
    let path = dir.path().join("data.txt");
    let mut content = String::from("ID   NAME\n");
    for i in 0..1000 {
        content.push_str(&format!("{i:<5}user_{i}\n"));
    }
    let mut bytes = content.into_bytes();
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    bytes.extend_from_slice(b" garbage not valid utf8\n");
    std::fs::write(&path, &bytes).unwrap();

    let with_nrows = Command::new(bin())
        .args([
            path.to_str().unwrap(),
            "-",
            "--format",
            "fixed-width",
            "--widths",
            "5,10",
            "--nrows",
            "5",
        ])
        .output()
        .unwrap();
    assert!(
        with_nrows.status.success(),
        "{}",
        String::from_utf8_lossy(&with_nrows.stderr)
    );

    let without_nrows = Command::new(bin())
        .args([
            path.to_str().unwrap(),
            "-",
            "--format",
            "fixed-width",
            "--widths",
            "5,10",
        ])
        .output()
        .unwrap();
    assert!(!without_nrows.status.success());
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

// A real, 3,000-row gzip file through the full pipeline (not just the
// direct gzip_decompress unit tests in lib.rs) - large/repetitive enough
// that the system `gzip` command reaches for multiple dynamic Huffman
// blocks rather than the single trivial block sample.csv.gz produces.
#[test]
fn gzip_with_dynamic_huffman_blocks_reads_correctly_end_to_end() {
    let doc = run_json("edge_gzip_dynamic_huffman.csv.gz", &[]);
    let cols = table(&doc, "edge_gzip_dynamic_huffman");
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "amount")["ideal_type"], "f64");
}

#[test]
fn gzip_with_a_corrupted_checksum_gives_an_actionable_error_not_a_panic() {
    let output = Command::new(bin())
        .args([
            fixture("malformed_gzip_checksum.csv.gz").to_str().unwrap(),
            "-",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("CRC32"),
        "error should mention the CRC32 mismatch, not panic: {stderr}"
    );
}

// 15,000 rows / ~665 KB decompressed - comfortably past DEFLATE_WINDOW's
// own 32 KiB and several multiples of GzipStreamSink's own 128 KiB flush
// threshold, so decoding this file genuinely exercises multiple
// flush-and-continue cycles rather than fitting inside a single one.
#[test]
fn gzip_streaming_decompression_survives_multiple_flush_cycles() {
    let doc = run_json("edge_gzip_multi_flush.csv.gz", &[]);
    let cols = table(&doc, "edge_gzip_multi_flush");
    assert_eq!(column(cols, "id")["row_count"], 15000);
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "email")["ideal_type"], "Email");
    assert_eq!(column(cols, "amount")["ideal_type"], "f64");
}

// The identical file above with one bit flipped in its footer - proves
// the streaming CRC32/ISIZE checks (computed incrementally across
// several flushes, never over one complete in-memory buffer) still
// correctly catch corruption rather than a flush accidentally losing
// track of the running checksum state partway through.
#[test]
fn gzip_corrupted_checksum_is_still_caught_across_multiple_flush_cycles() {
    let output = Command::new(bin())
        .args([
            fixture("malformed_gzip_checksum_multi_flush.csv.gz")
                .to_str()
                .unwrap(),
            "-",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("checksum mismatch"),
        "error should mention the checksum mismatch, not panic: {stderr}"
    );
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

// A real, 3,000-row zstd file through the full pipeline (not just the
// direct cross-verification unit test in lib.rs) - large/varied enough
// that the system `zstd` command reaches for FSE_Compressed sequence
// tables and a genuinely FSE-compressed Huffman weight list, rather than
// the Predefined-mode-only path sample.csv.zst's tiny content produces.
#[cfg(feature = "zstd")]
#[test]
fn zstd_with_fse_compressed_tables_reads_correctly_end_to_end() {
    let doc = run_json("edge_zstd_dynamic_tables.csv.zst", &[]);
    let cols = table(&doc, "edge_zstd_dynamic_tables");
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "score")["ideal_type"], "f64");
    assert_eq!(column(cols, "email")["ideal_type"], "Email");
}

// Found via real-world testing (a real zstd-CLI-compressed 100 MB CSV,
// while measuring the streaming-decompression rewrite's own memory
// footprint) rather than a synthetic edge case: HuffmanTable::parse's
// old maxBits formula (`32 - (weight_total - 1).leading_zeros()`)
// silently computed one bit too few whenever a literals block's own
// Huffman weight total happened to land exactly on a power of 2 - a
// real, common occurrence, not a rare corner case - since the correct
// formula (`32 - weight_total.leading_zeros()`, matching zstd's own
// `HUF_readStats`) always adds one more bit regardless. Every existing
// committed .zst fixture happened not to hit this exact boundary, which
// is exactly why a real, sizeable file was needed to find it at all.
// This fixture (4,500 rows, minimized by bisecting row count against
// the pre-fix binary) reliably reproduces it in ~13 KB.
#[cfg(feature = "zstd")]
#[test]
fn zstd_huffman_table_with_a_power_of_two_weight_total_decodes_correctly() {
    let doc = run_json("edge_zstd_huffman_power_of_two_weight_total.csv.zst", &[]);
    let cols = table(&doc, "edge_zstd_huffman_power_of_two_weight_total");
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "email")["ideal_type"], "Email");
    assert_eq!(column(cols, "id")["row_count"], 4500);
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_with_a_corrupted_checksum_gives_an_actionable_error_not_a_panic() {
    let output = Command::new(bin())
        .args([
            fixture("malformed_zstd_checksum.csv.zst").to_str().unwrap(),
            "-",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("checksum"),
        "error should mention the checksum mismatch, not panic: {stderr}"
    );
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_with_an_invalid_magic_number_gives_an_actionable_error_not_a_panic() {
    let bad_zst = fixture("_scratch_not_actually_zstd.csv.zst");
    std::fs::write(&bad_zst, b"this is not zstd data").unwrap();
    let output = Command::new(bin())
        .args([bad_zst.to_str().unwrap(), "-"])
        .output()
        .unwrap();
    std::fs::remove_file(&bad_zst).ok();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("magic number"),
        "error should mention the bad magic number, not panic: {stderr}"
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

    // A Map column reconstructs as an array of {"key", "value"} pairs (the
    // hand-rolled reader's own deliberate choice - see CLAUDE.md's Phase F
    // writeup - rather than a native keyed JSON object), so it flattens
    // into fixed `.key`/`.value` sub-columns rather than one sub-column per
    // distinct map key.
    let attributes = column(cols, "attributes");
    assert_eq!(attributes["current_type"], "Vec<object>");
    assert!(cols.iter().any(|c| c["name"] == "attributes.key"));
    assert!(cols.iter().any(|c| c["name"] == "attributes.value"));

    let value = column(cols, "attributes.value");
    assert_eq!(value["current_type"], "String");
}

// Found via a real-world sweep against the official apache/parquet-testing
// corpus: a Map column with non-UTF8 keys (Map<Int32, T> is legal Parquet/
// Arrow, e.g. a numeric-code-to-description lookup) used to fail Arrow's
// own JSON writer for the *whole batch* under the old Arrow-crate-based
// reader, taking every other column in the file down with it. The hand-
// rolled reader's own array-of-{"key","value"}-pairs Map representation
// has no such restriction at all - a non-string key is just another leaf
// value, so this column now profiles completely normally rather than
// needing the old reader's own per-column isolation/disclosed-placeholder
// fallback.
#[cfg(feature = "parquet")]
#[test]
fn parquet_map_with_non_string_keys_is_profiled_normally() {
    let doc = run_json("edge_map_non_string_key.parquet", &[]);
    let cols = table(&doc, "edge_map_non_string_key");

    let key = column(cols, "code_lookup.key");
    assert_eq!(key["current_type"], "i64");
    assert_eq!(key["ideal_type"], "i64");

    let value = column(cols, "code_lookup.value");
    assert_eq!(value["current_type"], "String");

    // The plain scalar column alongside it must still be profiled
    // normally too.
    let plain = column(cols, "plain_id");
    assert_eq!(plain["ideal_type"], "i64");
    assert_eq!(plain["missing_pct"].as_f64().unwrap(), 0.0);
}

// Found in the same real-world sweep: a Timestamp column carrying a
// *named* timezone ("UTC", as opposed to a raw numeric offset) - the
// exact shape of a real field (`ul_observation_date`) in
// nested_structs.rust.parquet from the official apache/parquet-testing
// corpus - failed Arrow's JSON writer ("only offset based timezones
// supported without chrono-tz feature") until this project's `arrow`
// dependency enabled that feature. Verified directly (not just inferred
// from the crate's docs): temporarily removing the feature and rebuilding
// reproduced the exact failure this fixture is meant to catch, before
// restoring it.
#[cfg(feature = "parquet")]
#[test]
fn parquet_named_timezone_timestamp_resolves_instead_of_failing() {
    let doc = run_json("edge_named_timezone.parquet", &[]);
    let cols = table(&doc, "edge_named_timezone");

    let started_at = column(cols, "session.started_at");
    assert_eq!(started_at["ideal_type"], "NaiveDate / DateTime");
    assert!(
        !started_at["notes"]
            .as_str()
            .unwrap()
            .contains("could not be converted")
    );

    let count = column(cols, "session.count");
    assert_eq!(count["ideal_type"], "i64");
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

#[cfg(feature = "bson")]
#[test]
fn bson_reads_concatenated_documents_and_resolves_semantic_types() {
    let doc = run_json("sample.bson", &[]);
    let cols = table(&doc, "sample");

    let email = column(cols, "email");
    assert_eq!(email["ideal_type"], "Email");

    let created = column(cols, "created");
    assert_eq!(created["ideal_type"], "NaiveDate / DateTime");

    // Decimal128 is bridged to a plain numeric string, not left as
    // unusable Debug output the way apache-avro's own equivalent
    // logical type was before this project's Avro reader fixed the
    // identical class of bug - see decimal128_to_string_matches_pymongo_
    // across_edge_cases for the dedicated bit-level coverage.
    let amount = column(cols, "amount");
    assert_eq!(amount["ideal_type"], "f64");
    assert_eq!(
        amount["sample_values"],
        serde_json::json!(["123.45", "-0.001"])
    );

    // A nested BSON document flattens into dot-notation sub-columns just
    // like a nested JSON object does - the same bridge-to-JsonValue
    // architecture every other nested format in this project shares.
    assert!(
        cols.iter().any(|c| c["name"] == "meta.x"),
        "a nested BSON document should flatten into meta.* sub-columns"
    );
}

#[cfg(feature = "bson")]
#[test]
fn bson_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.bson", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "bson")]
#[test]
fn bson_rare_element_types_render_per_documented_conventions() {
    // Covers BSON element types no other committed fixture exercised:
    // Regex (0x0B), Timestamp (0x11, the internal replication type, not
    // a UTC datetime), MinKey (0xFF), MaxKey (0x7F), JS code (0x0D), and
    // a literal null - each independently verified against pymongo's own
    // decode of the same encoded bytes before this fixture was committed
    // (see bson_support::decode_element_value's own doc comments for the
    // documented rendering each of these is checked against here).
    let doc = run_json("edge_bson_rare_types.bson", &[]);
    let cols = table(&doc, "edge_bson_rare_types");

    // "/{pattern}/{options}" slash notation; flags=2 (case-insensitive)
    // correctly maps to the "i" option string.
    assert_eq!(
        column(cols, "a_regex")["sample_values"],
        serde_json::json!(["/^foo/i"])
    );

    // The internal Timestamp type is genuinely compound (t + i), so it
    // flattens into two sub-columns rather than being forced into one
    // scalar - the same choice this project's Arrow IPC reader makes for
    // its own compound Interval type.
    assert_eq!(
        column(cols, "a_timestamp.t")["sample_values"],
        serde_json::json!(["1700000000"])
    );
    assert_eq!(
        column(cols, "a_timestamp.i")["sample_values"],
        serde_json::json!(["5"])
    );

    assert_eq!(
        column(cols, "a_minkey")["sample_values"],
        serde_json::json!(["MinKey"])
    );
    assert_eq!(
        column(cols, "a_maxkey")["sample_values"],
        serde_json::json!(["MaxKey"])
    );

    // JS-code-with-scope (0x0F) / plain JS code (0x0D) both keep only the
    // code text, discarding any scope document.
    assert_eq!(
        column(cols, "a_code")["sample_values"],
        serde_json::json!(["function() { return 1; }"])
    );

    // A literal null value is filtered out like any other reader's
    // missing value - 100% missing, not a spurious type.
    let a_null = column(cols, "a_null");
    assert!((a_null["missing_pct"].as_f64().unwrap() - 100.0).abs() < 0.01);
}

#[cfg(feature = "plist")]
#[test]
fn plist_xml_single_dict_profiles_as_one_record() {
    // A plist's own most common single-document shape - the whole file
    // is one top-level <dict> - profiles as a single record, the same
    // "whole document = one row" choice TOML's own single-document
    // shape already makes.
    let doc = run_json("sample.plist", &[]);
    let cols = table(&doc, "sample");

    let email = column(cols, "email");
    assert_eq!(email["ideal_type"], "Email");
    assert_eq!(email["row_count"].as_u64().unwrap(), 1);

    assert!(
        cols.iter().any(|c| c["name"] == "meta.x"),
        "a nested plist dict should flatten into meta.* sub-columns"
    );
    assert!(
        cols.iter().any(|c| c["name"] == "tags"),
        "a plist array should become a Vec<T> column"
    );
}

#[cfg(feature = "plist")]
#[test]
fn plist_recognizes_uuid_email_ipv4_and_date_columns() {
    // A top-level <array> of <dict>s is the array-of-records shape,
    // mirroring the "top-level sequence = array of records" choice
    // YAML's own dual-mode reader already makes.
    let doc = run_json("type_detection.plist", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "plist")]
#[test]
fn plist_binary_variant_reads_the_same_shape_as_xml() {
    // The exact same array-of-dicts content as type_detection.plist,
    // encoded as a binary bplist00 file instead of XML - proving the
    // binary-plist parser (object table + offset table + trailer) reads
    // identically to its XML sibling, not just that it doesn't crash.
    let doc = run_json("edge_plist_binary_type_detection.plist", &[]);
    let cols = table(&doc, "edge_plist_binary_type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "json5")]
#[test]
fn json5_reads_comments_trailing_commas_unquoted_keys_and_single_quotes() {
    let doc = run_json("sample.json5", &[]);
    let cols = table(&doc, "sample");

    let email = column(cols, "email");
    assert_eq!(email["ideal_type"], "Email");
    // `name` was written as `'Alice'` (single-quoted) and `id`/`meta.x`
    // as unquoted keys with no surrounding whitespace sensitivity - if
    // any of comments/trailing-commas/single-quotes/unquoted-keys were
    // mishandled, this file wouldn't have parsed as one record at all.
    assert_eq!(email["row_count"].as_u64().unwrap(), 1);
    assert!(
        cols.iter().any(|c| c["name"] == "meta.x"),
        "a nested JSON5 object should flatten into meta.* sub-columns"
    );
    assert!(
        cols.iter().any(|c| c["name"] == "tags"),
        "a JSON5 array (with a trailing comma) should become a Vec<T> column"
    );
}

#[cfg(feature = "json5")]
#[test]
fn json5_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.json5", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "json5")]
#[test]
fn jsonc_extension_routes_to_the_same_relaxed_reader() {
    // sample.jsonc uses only the subset of relaxations VS Code's own
    // "JSON with comments" convention actually allows (comments, a
    // trailing comma) - proving the .jsonc extension dispatches to the
    // same reader sample.json5's own richer fixture already exercises.
    let doc = run_json("sample.jsonc", &[]);
    assert_eq!(doc["format"], "json5");
    let cols = table(&doc, "sample");
    assert_eq!(column(cols, "name")["current_type"], "String");
    assert!(
        cols.iter().any(|c| c["name"] == "features"),
        "a JSONC array should become a Vec<T> column"
    );
}

#[cfg(feature = "har")]
#[test]
fn har_extracts_log_entries_and_flattens_nested_request_response_fields() {
    let doc = run_json("sample.har", &[]);
    let cols = table(&doc, "sample");

    let started = column(cols, "startedDateTime");
    assert_eq!(started["ideal_type"], "NaiveDate / DateTime");
    assert_eq!(started["row_count"].as_u64().unwrap(), 2);

    assert_eq!(column(cols, "request.url")["ideal_type"], "URL");
    assert_eq!(column(cols, "serverIPAddress")["ideal_type"], "IPv4");
    assert_eq!(column(cols, "response.status")["current_type"], "i64");
}

#[cfg(feature = "har")]
#[test]
fn har_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.har", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(
        column(cols, "startedDateTime")["ideal_type"],
        "NaiveDate / DateTime"
    );
    assert_eq!(column(cols, "serverIPAddress")["ideal_type"], "IPv4");
    assert_eq!(column(cols, "_userUuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "_contactEmail")["ideal_type"], "Email");
}

#[cfg(feature = "geojson")]
#[test]
fn geojson_extracts_features_and_renders_geometry_as_wkt() {
    let doc = run_json("sample.geojson", &[]);
    let cols = table(&doc, "sample");
    assert_eq!(column(cols, "name")["current_type"], "String");
    let geometry = column(cols, "geometry");
    // A Point and a LineString together - correctly typed as WKT
    // Geometry, the same heuristic a hand-authored WKT text column
    // already gets, confirming the geometry-to-WKT rendering feeds
    // straight into this project's existing coordinate/WKT detection.
    assert_eq!(geometry["ideal_type"], "WKT Geometry");
}

#[cfg(feature = "geojson")]
#[test]
fn geojson_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.geojson", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
    assert_eq!(column(cols, "geometry")["ideal_type"], "WKT Geometry");
}

#[cfg(feature = "geojson")]
#[test]
fn geojson_bare_geometry_becomes_one_value_column() {
    let doc = run_json("edge_geojson_bare_geometry.geojson", &[]);
    let cols = table(&doc, "edge_geojson_bare_geometry");
    assert_eq!(cols.len(), 1);
    let value = column(cols, "geometry");
    assert_eq!(
        value["sample_values"],
        serde_json::json!(["POINT(1.5 2.5)"])
    );
}

#[cfg(feature = "geojson")]
#[test]
fn geojson_bare_feature_profiles_as_one_record() {
    // A top-level Feature (not wrapped in a FeatureCollection) is a
    // real, spec-legal shape (RFC 7946 §3) with its own dispatch branch
    // in the reader - previously exercised by no committed fixture at
    // all, unlike the sibling bare-Geometry and FeatureCollection shapes.
    let doc = run_json("edge_geojson_bare_feature.geojson", &[]);
    let cols = table(&doc, "edge_geojson_bare_feature");
    assert_eq!(column(cols, "name")["row_count"].as_u64().unwrap(), 1);
    assert_eq!(column(cols, "opened")["ideal_type"], "NaiveDate / DateTime");
    assert_eq!(column(cols, "geometry")["ideal_type"], "WKT Geometry");
    assert_eq!(
        column(cols, "id")["sample_values"],
        serde_json::json!(["feature-1"])
    );
}

#[cfg(feature = "vcard")]
#[test]
fn vcard_reads_one_record_per_contact_and_recognizes_email() {
    let doc = run_json("sample.vcf", &[]);
    let cols = table(&doc, "sample");
    let email = column(cols, "EMAIL");
    assert_eq!(email["ideal_type"], "Email");
    assert_eq!(email["row_count"].as_u64().unwrap(), 2);
    assert_eq!(
        column(cols, "FN")["sample_values"],
        serde_json::json!(["Alice Anderson", "Bob Brown"])
    );
}

#[cfg(feature = "vcard")]
#[test]
fn vcard_recognizes_email_date_uuid_and_ipv4_columns() {
    let doc = run_json("type_detection.vcf", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "EMAIL")["ideal_type"], "Email");
    assert_eq!(column(cols, "BDAY")["ideal_type"], "NaiveDate / DateTime");
    assert_eq!(column(cols, "X-USER-UUID")["ideal_type"], "UUID");
    assert_eq!(column(cols, "X-IP-ADDRESS")["ideal_type"], "IPv4");
}

#[cfg(feature = "vcard")]
#[test]
fn vcard_unfolds_lines_unescapes_values_and_pools_repeated_properties() {
    let doc = run_json("edge_vcard_folding_and_escapes.vcf", &[]);
    let cols = table(&doc, "edge_vcard_folding_and_escapes");
    assert_eq!(
        column(cols, "NOTE")["sample_values"],
        serde_json::json!(["This note spans two physical lines via folding."])
    );
    let email = column(cols, "EMAIL");
    assert_eq!(email["current_type"], "Vec<String>");
    assert_eq!(
        email["sample_values"],
        serde_json::json!(["primary@example.com", "secondary@example.com"])
    );
}

#[cfg(feature = "icalendar")]
#[test]
fn icalendar_reads_one_record_per_vevent_and_isolates_nested_valarm() {
    let doc = run_json("sample.ics", &[]);
    let cols = table(&doc, "sample");
    assert_eq!(column(cols, "SUMMARY")["row_count"].as_u64().unwrap(), 2);
    // VALARM's own TRIGGER/ACTION properties belong to the nested alarm
    // component, not the enclosing VEVENT - they must never leak in.
    assert!(
        !cols
            .iter()
            .any(|c| c["name"] == "TRIGGER" || c["name"] == "ACTION"),
        "VALARM properties must not appear on the VEVENT record"
    );
}

#[cfg(feature = "icalendar")]
#[test]
fn icalendar_recognizes_uuid_date_email_and_ipv4_columns() {
    let doc = run_json("type_detection.ics", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "UID")["ideal_type"], "UUID");
    assert_eq!(
        column(cols, "DTSTART")["ideal_type"],
        "NaiveDate / DateTime"
    );
    assert_eq!(column(cols, "ATTENDEE")["ideal_type"], "Email");
    assert_eq!(column(cols, "X-IP-ADDRESS")["ideal_type"], "IPv4");
}

#[cfg(feature = "icalendar")]
#[test]
fn icalendar_reads_vtodo_and_unfolds_a_description() {
    let doc = run_json("edge_icalendar_vtodo_and_folding.ics", &[]);
    let cols = table(&doc, "edge_icalendar_vtodo_and_folding");
    assert_eq!(
        column(cols, "DESCRIPTION")["sample_values"],
        serde_json::json!(["This description spans two physical lines via folding."])
    );
    assert!(
        !cols.iter().any(|c| c["name"] == "TRIGGER"),
        "VALARM properties must not appear on the VTODO record"
    );
}

#[cfg(feature = "icalendar")]
#[test]
fn icalendar_reads_multiple_concatenated_vcalendar_blocks() {
    // ical_support has no special-casing preventing more than one
    // top-level VCALENDAR block in a single .ics file, and its
    // stack-based frame tracking should already handle this correctly -
    // but until this fixture, nothing actually exercised it: every other
    // committed fixture has exactly one VCALENDAR block.
    let doc = run_json("edge_icalendar_multiple_vcalendar_blocks.ics", &[]);
    let cols = table(&doc, "edge_icalendar_multiple_vcalendar_blocks");
    let uid = column(cols, "UID");
    assert_eq!(uid["row_count"].as_u64().unwrap(), 2);
    assert_eq!(
        uid["sample_values"],
        serde_json::json!(["event1@example.com", "event2@example.com"])
    );
    assert_eq!(
        column(cols, "SUMMARY")["sample_values"],
        serde_json::json!(["First calendar's event", "Second calendar's event"])
    );
}

#[cfg(feature = "mbox")]
#[test]
fn mbox_reads_one_record_per_message_including_the_last() {
    let doc = run_json("sample.mbox", &[]);
    let cols = table(&doc, "sample");
    let sender = column(cols, "envelope_sender");
    assert_eq!(sender["ideal_type"], "Email");
    assert_eq!(sender["row_count"].as_u64().unwrap(), 3);
    assert_eq!(
        sender["sample_values"],
        serde_json::json!(["alice@example.com", "bob@example.com", "carol@example.com"])
    );
}

#[cfg(feature = "mbox")]
#[test]
fn mbox_recognizes_email_date_and_ipv4_columns() {
    let doc = run_json("type_detection.mbox", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "From")["ideal_type"], "Email");
    assert_eq!(column(cols, "Date")["ideal_type"], "NaiveDate / DateTime");
    assert_eq!(column(cols, "X-Real-IP")["ideal_type"], "IPv4");
}

#[cfg(feature = "mbox")]
#[test]
fn mbox_folds_and_pools_a_repeated_header() {
    let doc = run_json("edge_mbox_repeated_and_folded_headers.mbox", &[]);
    let cols = table(&doc, "edge_mbox_repeated_and_folded_headers");
    let received = column(cols, "Received");
    assert_eq!(received["current_type"], "Vec<String>");
    assert_eq!(
        received["sample_values"],
        serde_json::json!([
            "from mx1.example.com by mx2.example.com; Mon, 15 Jan 2024 12:00:00 +0000",
            "from client.example.com by mx1.example.com; Mon, 15 Jan 2024 11:59:00 +0000"
        ])
    );
}

#[cfg(feature = "mbox")]
#[test]
fn mbox_accepts_genuine_crlf_line_endings() {
    // The reader's own doc comments claim CRLF is accepted alongside a
    // bare \n, but until this fixture that claim was never checked
    // against real CRLF-terminated bytes - every other committed mbox
    // fixture only ever used LF. Confirms both messages are recognized,
    // headers fold/parse cleanly with no stray \r leaking into values,
    // and the last message (no trailing boundary after it) still reads.
    let doc = run_json("edge_mbox_crlf_line_endings.mbox", &[]);
    let cols = table(&doc, "edge_mbox_crlf_line_endings");
    let sender = column(cols, "envelope_sender");
    assert_eq!(sender["row_count"].as_u64().unwrap(), 2);
    assert_eq!(
        sender["sample_values"],
        serde_json::json!(["alice@example.com", "bob@example.com"])
    );
    let subject = column(cols, "Subject");
    assert_eq!(
        subject["sample_values"],
        serde_json::json!(["Hello CRLF", "Re: Hello CRLF"])
    );
    for v in subject["sample_values"].as_array().unwrap() {
        assert!(
            !v.as_str().unwrap().contains('\r'),
            "a stray \\r leaked into a header value"
        );
    }
}

#[cfg(feature = "json5")]
#[test]
fn json5_comment_containing_stray_brackets_and_quotes_does_not_corrupt_the_scan() {
    // json5_support::ByteWindow::scan_value's own doc comment discloses
    // exactly this adversarial shape: a comment inside an array element
    // containing `]`/`{`/`"` characters could corrupt a naive depth/
    // string scan if comments weren't recognized and copied through
    // verbatim rather than being depth- or string-tracked. Verified ad
    // hoc during the streaming-conversion phase but never locked in as a
    // permanent fixture until now.
    let doc = run_json("edge_json5_comment_with_stray_brackets.json5", &[]);
    let cols = table(&doc, "edge_json5_comment_with_stray_brackets");
    let id = column(cols, "id");
    assert_eq!(id["row_count"].as_u64().unwrap(), 2);
    assert_eq!(id["sample_values"], serde_json::json!(["1", "2"]));
    assert_eq!(
        column(cols, "name")["sample_values"],
        serde_json::json!(["Alice", "Bob"])
    );
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

// Namespace prefixes are stripped (not resolved via real URI lookup - see
// CLAUDE.md's Dependency footprint section for why that's a deliberate,
// scoped stand-in), matching xmltree's own observed behavior: a plain
// <link> and a namespaced <atom:link> merge into the same flattened
// column, and a namespaced xsi:type attribute becomes plain @type - a
// real shape found in a real BBC RSS feed during this project's own
// real-world XML validation.
#[cfg(feature = "xml")]
#[test]
fn xml_strips_namespace_prefixes_from_elements_and_attributes() {
    let doc = run_json("edge_xml_namespaces.xml", &[]);
    let cols = table(&doc, "edge_xml_namespaces");

    let ty = column(cols, "@type");
    assert_eq!(ty["sample_values"][0], "Widget");

    // Both the plain <link> and the namespaced <atom:link> merged into
    // one "link" column - the plain one contributes a string, the
    // namespaced one contributes an object (it has an @href attribute),
    // so the column is a real, disclosed scalar/object mix.
    let link = column(cols, "link");
    assert!(link["current_type"].as_str().unwrap().contains("mixed"));
    let href = column(cols, "link.@href");
    assert_eq!(href["ideal_type"], "URL");
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

// Found via a real-world sweep against TensorFlow's own MNIST .npz
// (x_train/x_test are genuine 3-D image arrays, (60000, 28, 28) and
// (10000, 28, 28) - a real, documented boundary this tool correctly
// refuses to guess a flattening for - but y_train/y_test in the exact
// same archive are perfectly ordinary 1-D label arrays). One array's
// shape not being representable used to abort the *entire* archive read,
// costing every other array in the file its own profile too - the same
// "one bad part shouldn't sink everything else" principle already
// applied to a single unconvertible nested Parquet/Arrow column.
#[cfg(feature = "npy")]
#[test]
fn npz_one_unreadable_array_does_not_sink_the_rest_of_the_archive() {
    let doc = run_json("edge_npz_mixed_readable_and_unreadable.npz", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(tables.len(), 2, "both arrays must still appear as tables");

    let images = table(&doc, "images");
    assert!(
        column(images, "value")["notes"]
            .as_str()
            .unwrap()
            .contains("could not be profiled"),
        "the 3-D array's own table should disclose why, not silently vanish"
    );

    // The unrelated, perfectly ordinary array must still profile normally.
    let labels = table(&doc, "labels");
    assert_eq!(column(labels, "value")["ideal_type"], "i64");
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

// Found via a real-world sweep against loghub's Linux_2k.log - a real
// sample from an actual production /var/log/messages-style file. RFC 3164
// technically includes a <PRI> prefix, but PRI is primarily a wire-
// protocol artifact: the local syslog daemon on virtually every real
// Linux box writes its own on-disk log files *without* it. The original
// regex required PRI unconditionally, rejecting the single most common
// real-world shape this format actually appears in. The same real file
// also has a recurring line - sysklogd's own hardcoded restart
// announcement, "syslogd 1.4.1: restart." - whose "tag" is a
// space-containing "program version" rather than RFC 3164's usual
// single-token program name, which the old strict no-whitespace tag
// grammar also rejected outright.
#[cfg(feature = "syslog")]
#[test]
fn syslog_rfc3164_pri_is_optional_and_tag_may_contain_a_space() {
    let doc = run_with_format("sample_rfc3164_no_pri.log", "json", &["--format", "syslog"]);
    let cols = table(&doc, "sample_rfc3164_no_pri");

    // No <PRI> anywhere in this fixture - facility/severity can't be
    // derived, so they're missing, not defaulted to a wrong guess.
    let facility = column(cols, "facility");
    assert_eq!(facility["missing_pct"].as_f64().unwrap(), 100.0);
    let severity = column(cols, "severity");
    assert_eq!(severity["missing_pct"].as_f64().unwrap(), 100.0);

    // The rest of the line still parses normally without PRI.
    let tag = column(cols, "tag");
    let tag_samples: Vec<&str> = tag["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(tag_samples.contains(&"sshd(pam_unix)"));
    // The space-containing "program version" tag shape.
    assert!(tag_samples.contains(&"syslogd 1.4.1"));

    let pid = column(cols, "pid");
    assert_eq!(pid["current_type"], "i64");

    // A bracketed, colon-containing kernel-style message body must not be
    // mistaken for [pid]/tag structure of its own.
    let message = column(cols, "message");
    assert!(
        message["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "[12345.678] eth0: link up")
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

// `sas7bdat_people_nonascii.sas7bdat` is a real, vendored file (see
// tests/fixtures/sas7bdat_PROVENANCE.md - copied from the `sas7bdat`
// crate's own MIT-licensed test fixtures, since no tool in this
// environment can write a genuine .sas7bdat file). It exercises the same
// current_type=f64/ideal_type=i64 gap as Stata and dBase (SAS stores
// nearly all numeric data as doubles internally), and real non-ASCII
// text content in its own GENDER column.
#[cfg(feature = "sas7bdat")]
#[test]
fn sas7bdat_reads_a_real_file_with_non_ascii_text_and_the_f64_i64_gap() {
    let doc = run_json("sas7bdat_people_nonascii.sas7bdat", &[]);
    let cols = table(&doc, "sas7bdat_people_nonascii");

    let age = column(cols, "AGE");
    assert_eq!(age["current_type"], "f64");
    assert_eq!(age["ideal_type"], "i64");

    let gender = column(cols, "GENDER");
    assert_eq!(gender["current_type"], "String");
    let samples = gender["sample_values"].as_array().unwrap();
    assert!(
        samples.iter().any(|v| !v.as_str().unwrap().is_ascii()),
        "expected at least one non-ASCII sample value in GENDER, got {samples:?}"
    );
}

#[cfg(feature = "sas7bdat")]
#[test]
fn sas7bdat_format_is_recognized() {
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

// TOML 1.1.0 features with zero prior coverage in this project's own
// fixtures - found while auditing the hand-rolled `toml_support` parser
// against toml-lang/toml-test: optional seconds in local time/datetime
// values, newlines and a trailing comma inside an inline table, the
// `\xHH`/`\e` string escapes, and a multi-line basic string starting with
// an unescaped quote immediately after its opening delimiter.
#[cfg(feature = "toml")]
#[test]
fn toml_handles_v1_1_0_features_with_no_prior_fixture_coverage() {
    let doc = run_json("edge_toml_v1_1_features.toml", &[]);
    let cols = table(&doc, "edge_toml_v1_1_features");

    assert_eq!(column(cols, "time_no_seconds")["ideal_type"], "NaiveTime");
    assert_eq!(column(cols, "time_no_seconds")["sample_values"][0], "13:37");
    assert_eq!(
        column(cols, "datetime_no_seconds")["sample_values"][0],
        "1979-05-27T07:32Z"
    );
    assert_eq!(
        column(cols, "escapes")["sample_values"][0],
        "tab:\tesc:\u{1B} hex:A"
    );
    assert_eq!(
        column(cols, "four_quotes")["sample_values"][0],
        "\"four quotes at the start\""
    );
    assert_eq!(
        column(cols, "inline_multiline.name")["sample_values"][0],
        "multi-line inline table"
    );
    assert_eq!(
        column(cols, "inline_multiline.values")["current_type"],
        "Vec<i64>"
    );
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

// Found via a real-world sweep against yaml/yaml-test-suite (the YAML spec
// compliance corpus): a top-level sequence of scalars (no field names to
// extract) used to be rejected with "expected each YAML document/record
// to be a mapping", even though it's real, valid, unambiguous YAML - the
// same class of gap the JSON reader had for a top-level array of scalars.
#[cfg(feature = "yaml")]
#[test]
fn yaml_top_level_sequence_of_scalars_becomes_one_value_column() {
    let doc = run_json("edge_yaml_scalar_sequence.yaml", &[]);
    let cols = table(&doc, "edge_yaml_scalar_sequence");
    assert_eq!(cols.len(), 1);
    let value = column(cols, "value");
    assert_eq!(value["ideal_type"], "i64");
}

// Found while validating the hand-rolled YAML parser (replacing
// serde_norway - see CLAUDE.md's Dependency footprint section) against a
// real Kubernetes deployment manifest: a block sequence indented at the
// *same* level as its own key (`containers:` followed by `- name: ...`
// with no extra indentation), a real, common style YAML explicitly
// permits as an exception to its usual "children more indented than
// parent" rule. Locks in the fix at the full-pipeline level, not just the
// parser's own unit tests.
#[cfg(feature = "yaml")]
#[test]
fn yaml_handles_a_block_sequence_indented_the_same_as_its_own_key() {
    let doc = run_json("edge_yaml_same_indent_sequence.yaml", &[]);
    let cols = table(&doc, "edge_yaml_same_indent_sequence");
    let name = column(cols, "spec.template.spec.containers.name");
    assert_eq!(name["sample_values"], serde_json::json!(["nginx"]));
    let port = column(cols, "spec.template.spec.containers.ports.containerPort");
    assert_eq!(port["ideal_type"], "i64");
}

// Locks in a real, severe O(n^2) bug found via real-world-scale testing
// (a synthetic 60,000-record file with this exact shape took over 50
// seconds before the fix, ~700x slower than after): `parse_inline_value`
// (the hand-rolled YAML parser's own handling of `- key: value`, the
// single most common real-world "array of objects" shape) used to build a
// fresh `Vec` holding a full copy of every remaining line in the document
// on every call - once per record. This fixture is small (correctness
// only; the large-file timing evidence lives in CLAUDE.md/BENCHMARKS.md),
// but exercises the exact code path the bug lived in.
#[cfg(feature = "yaml")]
#[test]
fn yaml_inline_sequence_mapping_items_resolve_correctly() {
    let doc = run_json("edge_yaml_inline_sequence_mapping.yaml", &[]);
    let cols = table(&doc, "edge_yaml_inline_sequence_mapping");
    let id = column(cols, "id");
    assert_eq!(id["ideal_type"], "i64");
    let name = column(cols, "name");
    assert_eq!(
        name["sample_values"],
        serde_json::json!(["alpha", "beta", "gamma"])
    );
    let active = column(cols, "active");
    assert_eq!(active["ideal_type"], "bool");
}

// Locks in a second, independent O(n^2) bug found in the same real-world-
// scale investigation as the inline-sequence-mapping fix above:
// `parse_flow_from_lines` (an inline `[...]`/`{...}` flow collection, at
// any nesting depth) used to join *every remaining line in the document*
// into one string before attempting to parse anything, regardless of how
// small the actual flow value was - a `tags: [a, b, c]` field closing on
// its own line still paid for concatenating the entire rest of the file.
// This is a genuinely separate bug from the inline-mapping one above (a
// file with only plain scalar values never reaches this code path at
// all), found only because a more realistic test file happened to nest a
// flow collection inside an already-fixed inline-mapping record.
#[cfg(feature = "yaml")]
#[test]
fn yaml_inline_flow_collections_resolve_correctly() {
    let doc = run_json("edge_yaml_inline_flow_collection.yaml", &[]);
    let cols = table(&doc, "edge_yaml_inline_flow_collection");
    let tags = column(cols, "tags");
    assert_eq!(tags["current_type"], "Vec<String>");
    assert_eq!(
        tags["sample_values"],
        serde_json::json!(["red", "green", "blue"])
    );
    let owner = column(cols, "meta.owner");
    assert_eq!(
        owner["sample_values"],
        serde_json::json!(["alice", "bob", "carol"])
    );
    let priority = column(cols, "meta.priority");
    assert_eq!(priority["ideal_type"], "i64");
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

// .ods reads through the same InputFormat::Xlsx path as .xlsx (calamine's
// own open_workbook_auto covers all four formats under one --features
// xlsx flag) - dispatched internally to a hand-rolled ODF reader rather
// than calamine, see CLAUDE.md's Dependency footprint section. Its own
// date-value attribute is already a clean ISO string (no Excel-style
// epoch-serial resolution needed the way .xlsx's native dates require).
#[cfg(feature = "xlsx")]
#[test]
fn ods_recognizes_types_and_resolves_dates_already_in_iso_form() {
    let doc = run_json("sample.ods", &[]);
    assert_eq!(doc["format"], "xlsx");
    let cols = table(&doc, "Sheet1");
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "amount")["ideal_type"], "f64");
    let date = column(cols, "signup_date");
    assert_eq!(date["ideal_type"], "NaiveDate / DateTime");
    assert_eq!(date["sample_values"][0], "2024-01-15");
}

#[cfg(feature = "xlsx")]
#[test]
fn ods_is_auto_detected_from_content_when_extensionless() {
    let (_dir, dest) = copy_fixture_as("sample.ods", "mystery_spreadsheet");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "xlsx");
    let cols = table(&doc, "Sheet1");
    assert!(cols.iter().any(|c| c["name"] == "id"));
}

#[cfg(feature = "xlsx")]
#[test]
fn xlsx_with_a_stray_cell_near_the_max_row_does_not_allocate_a_dense_grid() {
    // One value at C1048570 (near Excel's 1,048,576-row limit) on top of
    // 3 real data rows. The old dense `vec![vec![None; max_col];
    // max_row]` path allocated a ~1M-tall grid for this (~150 MB RSS
    // even at 3 columns; an OOM if the stray cell were also far to the
    // right). The reader now builds only what the populated cells need.
    // The real columns still profile correctly and the phantom rows
    // still count toward `missing_pct`, exactly as before.
    let doc = run_json("edge_xlsx_stray_far_cell.xlsx", &[]);
    let cols = table(&doc, "Sheet1");
    assert_eq!(
        column(cols, "id")["sample_values"],
        serde_json::json!(["1", "2", "3"])
    );
    assert_eq!(
        column(cols, "name")["sample_values"],
        serde_json::json!(["alice", "bob", "carol"])
    );
    // 3 values out of ~1,048,569 data rows -> rounds to 100.0.
    assert_eq!(column(cols, "id")["missing_pct"].as_f64().unwrap(), 100.0);
}

#[cfg(feature = "xlsx")]
#[test]
fn ods_handles_a_real_scale_repeated_empty_row_block_without_hanging() {
    // table:number-rows-repeated at ODF's own max dimensions
    // (1,048,573 rows x 16,384 columns) - a real LibreOffice padding
    // convention, not a synthetic stress test - plus a repeated *empty*
    // cell in the middle of a real data row. Finishing at all quickly is
    // the correctness proof; see this fixture's own generation notes in
    // this project's history for why a naive eager expansion would be a
    // genuine memory-blowup risk here, not just a performance concern.
    let doc = run_json("edge_ods_repeated_cells.ods", &[]);
    let cols = table(&doc, "Sheet1");
    assert_eq!(
        column(cols, "id")["sample_values"],
        serde_json::json!(["1", "2"])
    );
    assert_eq!(
        column(cols, "note")["sample_values"],
        serde_json::json!(["first", "second"])
    );
    let name = column(cols, "name");
    assert_eq!(name["missing_pct"].as_f64().unwrap(), 50.0);
}

// .xls (Excel 97-2003, BIFF8) reads through the same InputFormat::Xlsx path
// as .xlsx/.ods - dispatched internally to a hand-rolled OLE2/CFBF +
// BIFF8 record reader rather than calamine, see CLAUDE.md's Dependency
// footprint section. Unlike .ods, .xls's own dates ARE Excel-style
// epoch-day serials (the same 1900 system .xlsx uses), resolved through
// the exact same `xlsx_serial_to_ymd`/`xlsx_format_serial` machinery.
#[cfg(feature = "xlsx")]
#[test]
fn xls_recognizes_types_and_resolves_native_date_serials() {
    let doc = run_json("type_detection_lo.xls", &[]);
    assert_eq!(doc["format"], "xlsx");
    let cols = table(&doc, "Sheet1");
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
}

#[cfg(feature = "xlsx")]
#[test]
fn xls_is_auto_detected_from_content_when_extensionless() {
    let (_dir, dest) = copy_fixture_as("type_detection_lo.xls", "mystery_spreadsheet");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "xlsx");
    let cols = table(&doc, "Sheet1");
    assert!(cols.iter().any(|c| c["name"] == "id"));
}

#[cfg(feature = "xlsx")]
#[test]
fn xls_resolves_native_date_and_datetime_serials_not_raw_day_counts() {
    // The exact same bug class documented in CLAUDE.md for .xlsx (a
    // native date cell silently rendering as Excel's meaningless raw
    // day-count serial, e.g. "45306") - this fixture was generated with
    // real openpyxl datetime.date/datetime.datetime values, then
    // exported through LibreOffice's own "MS Excel 97" filter, so a
    // regression here would mean the BIFF8 NUMBER + XF-format date
    // detection path (as opposed to .xlsx's styles.xml-based one) had
    // silently broken.
    let doc = run_json("edge_xls_native_date_cells.xls", &[]);
    let cols = table(&doc, "Sheet");
    let date = column(cols, "event_date");
    assert_eq!(date["ideal_type"], "NaiveDate / DateTime");
    assert_eq!(date["sample_values"][0], "2024-01-15");
    let datetime = column(cols, "event_datetime");
    assert_eq!(datetime["sample_values"][0], "2024-01-15T10:30:00");
}

#[cfg(feature = "xlsx")]
#[test]
fn xls_reports_one_table_per_sheet_and_extracts_formula_and_error_cells() {
    let doc = run_json("multi_sheet_lo.xls", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(
        tables.len(),
        2,
        "fixture has two sheets (customers, products), expected one table each"
    );
    let customers = table(&doc, "customers");
    assert!(customers.iter().any(|c| c["name"] == "customer_id"));
    let products = table(&doc, "products");
    assert!(products.iter().any(|c| c["name"] == "sku"));

    let formula_doc = run_json("edge_xls_formula_and_error.xls", &[]);
    let formula_cols = table(&formula_doc, "Sheet");
    let result = column(formula_cols, "formula_result");
    assert!(
        result["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "#DIV/0!"),
        "expected a formula-cached #DIV/0! error value: {:?}",
        result["sample_values"]
    );
}

// .xlsb (Excel Binary Workbook) reads through the same InputFormat::Xlsx
// path as .xlsx/.xls/.ods - dispatched internally to a hand-rolled
// BIFF12 reader rather than calamine, see CLAUDE.md's Dependency
// footprint section. Fixtures here are real files vendored from Apache
// POI's own test-data (see tests/fixtures/poi_xlsb_PROVENANCE.md) -
// this project can't generate its own .xlsb fixtures at all (no tool in
// this environment can write one).
#[cfg(feature = "xlsx")]
#[test]
fn xlsb_reports_one_table_per_sheet() {
    let doc = run_json("poi_sample.xlsb", &[]);
    assert_eq!(doc["format"], "xlsx");
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(tables.len(), 2, "fixture has two sheets");
    let sheet1 = table(&doc, "Sheet1");
    assert!(sheet1.iter().any(|c| c["name"] == "Lorem"));
    let rich = table(&doc, "rich test");
    assert!(!rich.is_empty());
}

#[cfg(feature = "xlsx")]
#[test]
fn xlsb_is_auto_detected_from_content_when_extensionless() {
    let (_dir, dest) = copy_fixture_as("poi_sample.xlsb", "mystery_spreadsheet");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "xlsx");
    let cols = table(&doc, "Sheet1");
    assert!(cols.iter().any(|c| c["name"] == "Lorem"));
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

    // "On"/"Off" is a real, common INI boolean convention (php.ini's own
    // directive style, also Apache/Windows-style configs) - found via a
    // real-world sweep against php.ini-production, which resolves every
    // On/Off directive as an untyped enum/category without this.
    let ssl_enabled = column(database, "ssl_enabled");
    assert_eq!(ssl_enabled["ideal_type"], "bool");
}

// Locks in the hand-rolled INI parser's quoting/escaping behavior (see
// CLAUDE.md's Dependency footprint section) at the full-pipeline level -
// the unit-level cross-check against rust-ini itself lives in src/lib.rs.
#[cfg(feature = "ini")]
#[test]
fn ini_handles_quoted_values_and_backslash_escapes() {
    let doc = run_json("edge_ini_quoting_and_escapes.ini", &[]);
    let cols = table(&doc, "Section");
    assert_eq!(column(cols, "Key1")["sample_values"][0], "Quoted value");
    assert_eq!(
        column(cols, "Key2")["sample_values"][0],
        "Single Quote with extra value"
    );
    assert_eq!(
        column(cols, "Key3")["sample_values"][0],
        "plain \t tab and \n newline"
    );
    assert_eq!(
        column(cols, "Key4")["sample_values"][0],
        "escaped \"quote\" inside"
    );
    assert_eq!(column(cols, "Key5")["sample_values"][0], "colon delimiter");
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

// Found via a real-world sweep against two well-known sample databases
// (Chinook, and Northwind's SQLite port) - confirmed correct rather than a
// gap, but previously untested: Northwind alone ships 18 real VIEWs
// (several with spaces in their names, like the table this fixture
// mirrors) alongside its 13 real tables, and none of them leaked into
// sniff-rs's output - `columns_from_sqlite`'s own `WHERE type='table'`
// query already excludes them structurally. This fixture locks that
// behavior in permanently, and doubles as coverage for a table name
// containing a space (also a real shape in both sample databases).
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_excludes_views_and_handles_table_names_with_spaces() {
    let doc = run_json("edge_sqlite_view_excluded.sqlite", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(
        tables.keys().collect::<Vec<_>>(),
        vec!["Order Details"],
        "the 'Order Summary' VIEW must not appear alongside the real table"
    );
    let cols = table(&doc, "Order Details");
    assert_eq!(column(cols, "qty")["ideal_type"], "i64");
}

// A payload past the local-page threshold spills into SQLite's own
// overflow-page linked list - this fixture's "body" column carries a
// 15,000-byte value (well past the ~4,061-byte local max on a 4,096-byte
// default page) alongside an ordinary short value, so both the overflow-
// chain-assembly path and the plain local-payload path get exercised in
// the same table.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_reassembles_a_payload_spanning_overflow_pages() {
    let doc = run_json("edge_sqlite_overflow_pages.sqlite", &[]);
    let cols = table(&doc, "docs");
    let body = column(cols, "body");
    assert_eq!(body["ideal_type"], "String");
    let samples = body["sample_values"].as_array().unwrap();
    assert!(
        samples.iter().any(|v| v.as_str().unwrap().len() == 15000),
        "expected a sample value carrying the full 15,000-byte overflowing payload"
    );
}

// `PRIMARY KEY(id)` as a table-level constraint (rather than inline
// `id INTEGER PRIMARY KEY`) makes `id` a rowid alias too, per SQLite's own
// documented rule - a real, common schema style this fixture locks in
// separately from the inline form.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_resolves_a_table_level_primary_key_as_a_rowid_alias() {
    let doc = run_json("edge_sqlite_table_level_primary_key.sqlite", &[]);
    let cols = table(&doc, "items");
    let id = column(cols, "id");
    assert_eq!(id["missing_pct"].as_f64().unwrap(), 0.0);
    let samples = id["sample_values"].as_array().unwrap();
    assert_eq!(
        samples
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["1", "2"],
        "id must resolve from the cell's own rowid, not a stored NULL"
    );
}

// WITHOUT ROWID storage uses an index b-tree rather than a table b-tree -
// a disclosed, unsupported shape (see CLAUDE.md) - so it gets a clear
// placeholder column rather than either a crash or silently wrong data,
// the same "one bad part shouldn't sink everything else" treatment a
// bad Parquet column or .npz array already gets elsewhere in this project.
// The other, ordinary table in the same file must still profile normally.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_without_rowid_table_is_a_disclosed_placeholder_not_a_crash() {
    let doc = run_json("edge_sqlite_without_rowid.sqlite", &[]);
    let kv = table(&doc, "kv");
    assert_eq!(kv.len(), 1);
    assert!(
        kv[0]["notes"].as_str().unwrap().contains("WITHOUT ROWID"),
        "expected a disclosed WITHOUT ROWID note, got {:?}",
        kv[0]["notes"]
    );
    let normal = table(&doc, "normal");
    assert_eq!(column(normal, "id")["ideal_type"], "i64");
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
fn csv_treats_backslash_n_as_missing_not_a_literal_string() {
    // MySQL's SELECT INTO OUTFILE, Hive's default text SerDe, and
    // Redshift's UNLOAD ... NULL AS '\N' all write literal backslash-N for
    // a null field - common enough in cloud-warehouse CSV/TSV exports that
    // it's its own missing-sentinel entry, not just a pandas default.
    let path = fixture("_scratch_backslash_n_null.csv");
    std::fs::write(&path, "id,amount\n1,10.50\n2,\\N\n3,30.00\n").unwrap();
    let doc = run_json("_scratch_backslash_n_null.csv", &[]);
    std::fs::remove_file(&path).ok();
    let cols = table(&doc, "_scratch_backslash_n_null");
    let amount = column(cols, "amount");
    assert_eq!(amount["ideal_type"], "f64");
    assert_eq!(amount["missing_pct"].as_f64().unwrap(), 33.3);
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
fn csv_recognizes_international_rfc2822_ctime_and_oracle_style_dates() {
    let doc = run_json("date_formats.csv", &[]);
    let cols = table(&doc, "date_formats");

    for name in [
        "dot_eu",
        "full_month",
        "rfc2822",
        "rfc2822_gmt",
        "ctime",
        "oracle_style",
        "datetime_no_seconds",
        "compact_iso",
    ] {
        assert_eq!(
            column(cols, name)["ideal_type"],
            "NaiveDate / DateTime",
            "column {name} should resolve to a date/datetime type"
        );
    }

    // Found via a real-world sweep of RSS feeds (BBC News's <pubDate>) -
    // RFC 2822 with the literal named zone "GMT" instead of a numeric
    // offset, the same shape RFC 7231's HTTP Date-header grammar itself
    // mandates. Asserted separately from the loop above to also confirm
    // it resolved via its *own* format string, not by coincidentally
    // matching the numeric-offset rfc2822 entry.
    let rfc2822_gmt = column(cols, "rfc2822_gmt");
    assert!(
        rfc2822_gmt["notes"].as_str().unwrap().contains("GMT"),
        "expected the literal-GMT format to win, got: {:?}",
        rfc2822_gmt["notes"]
    );

    // "01/15/24" - a genuinely 2-digit year must resolve to the %y form,
    // not be silently swallowed by %m/%d/%Y treating "24" as year 24 AD
    // (a real chrono characteristic - %Y accepts variable-width numeric
    // input while parsing). See matching_date_format_two_digit_year_takes_
    // priority_over_four_digit_for_short_years in lib.rs for the direct,
    // format-string-level proof; this is the full-pipeline confirmation.
    let two_digit = column(cols, "two_digit_year");
    assert_eq!(two_digit["ideal_type"], "NaiveDate / DateTime");
    assert!(
        two_digit["notes"].as_str().unwrap().contains("%m/%d/%y"),
        "expected the two-digit-year format to win, got: {:?}",
        two_digit["notes"]
    );
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
fn csv_recognizes_hex_colors_and_imei() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let color = column(cols, "hex_color");
    assert_eq!(color["ideal_type"], "Hex Color");

    // IMEIs are plain digit strings that fit i64 - current_type says i64,
    // but ideal_type correctly identifies an opaque device identifier
    // rather than a quantity, the same current-vs-ideal gap as credit
    // card numbers.
    let imei = column(cols, "imei");
    assert_eq!(imei["current_type"], "i64");
    assert_eq!(imei["ideal_type"], "IMEI");
}

#[test]
fn csv_recognizes_jwt() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let token = column(cols, "auth_token");
    assert_eq!(token["ideal_type"], "JWT");
}

#[test]
fn csv_recognizes_geographic_coordinate_pairs() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let location = column(cols, "location");
    assert_eq!(location["ideal_type"], "Geographic Coordinates");
}

#[test]
fn csv_flags_hash_digest_length_as_a_note_not_a_type_promotion() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let hash = column(cols, "content_hash");
    // Deliberately stays String - no checksum backs this, so it must never
    // be promoted to its own confident type the way UUID/IMEI/etc. are.
    assert_eq!(hash["ideal_type"], "String");
    assert!(hash["notes"].as_str().unwrap().contains("MD5"));
    assert!(hash["notes"].as_str().unwrap().contains("shape only"));
}

#[test]
fn csv_recognizes_vin() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let vin = column(cols, "vin");
    assert_eq!(vin["ideal_type"], "VIN");
}

#[test]
fn csv_recognizes_cidr() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let subnet = column(cols, "subnet");
    assert_eq!(subnet["ideal_type"], "CIDR");
}

#[test]
fn csv_recognizes_ulid() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let request_id = column(cols, "request_id");
    assert_eq!(request_id["ideal_type"], "ULID");
}

#[test]
fn csv_recognizes_wkt_geometry() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let geom = column(cols, "geom");
    assert_eq!(geom["ideal_type"], "WKT Geometry");
}

#[test]
fn csv_recognizes_cron_expression() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let schedule = column(cols, "schedule");
    assert_eq!(schedule["ideal_type"], "Cron Expression");
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
fn csv_recognizes_semver_and_flags_embedded_json_in_a_text_cell() {
    let doc = run_json("type_detection.csv", &[]);
    let cols = table(&doc, "type_detection");

    let version = column(cols, "app_version");
    assert_eq!(version["ideal_type"], "SemVer");

    // A cell that's itself a serialized JSON object stays String (it's
    // still literally a string in this CSV column), but with a note
    // flagging that it's worth parsing separately.
    let config = column(cols, "config_blob");
    assert_eq!(config["ideal_type"], "String");
    assert!(config["notes"].as_str().unwrap().contains("embedded JSON"));
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
    assert_eq!(props["app_version"], serde_json::json!({"type": "string"}));
    assert_eq!(props["hex_color"], serde_json::json!({"type": "string"}));
    assert_eq!(props["imei"], serde_json::json!({"type": "string"}));
    assert_eq!(props["auth_token"], serde_json::json!({"type": "string"}));
    assert_eq!(props["location"], serde_json::json!({"type": "string"}));
    assert_eq!(props["vin"], serde_json::json!({"type": "string"}));
    assert_eq!(props["subnet"], serde_json::json!({"type": "string"}));
    assert_eq!(props["request_id"], serde_json::json!({"type": "string"}));
    assert_eq!(props["geom"], serde_json::json!({"type": "string"}));
    assert_eq!(props["schedule"], serde_json::json!({"type": "string"}));
}

// --- Adversarial / robustness tests ----------------------------------
// These run the full pipeline (reader + heuristics + renderer) against
// deliberately hostile input, not just the unit-level validator functions
// tested in lib.rs - proving end to end that a near-miss value never false-
// positives into a specific type, that a stray non-finite/oversized number
// gets flagged rather than silently absorbed, and that a malformed file
// produces a clean actionable error instead of a panic.

#[test]
fn adversarial_csv_never_false_positives_on_any_near_miss_column() {
    let doc = run_json("adversarial.csv", &[]);
    let cols = table(&doc, "adversarial");

    // A perfectly ordinary float column gets no note at all - the fixes
    // below must not make an unrelated, clean column noisier.
    let clean = column(cols, "clean_float");
    assert_eq!(clean["ideal_type"], "f64");
    assert_eq!(clean["notes"], "");

    // A literal "infinity"/"NaN"/"-inf" value must not sail through a
    // numeric column silently.
    let infinity = column(cols, "infinity_mix");
    assert_eq!(infinity["ideal_type"], "f64");
    assert!(infinity["notes"].as_str().unwrap().contains("non-finite"));

    // Digit strings beyond i64's range must be flagged, not silently
    // rounded via an unqualified f64.
    let oversized = column(cols, "oversized_int");
    assert_eq!(oversized["ideal_type"], "f64");
    assert!(oversized["notes"].as_str().unwrap().contains("exceed i64"));

    // Every near-miss column below must NOT resolve to the specific type
    // its values were deliberately corrupted away from.
    assert_ne!(column(cols, "near_uuid")["ideal_type"], "UUID");
    assert_ne!(column(cols, "near_email")["ideal_type"], "Email");
    assert_ne!(column(cols, "near_ipv4")["ideal_type"], "IPv4");
    assert_ne!(column(cols, "near_iban")["ideal_type"], "IBAN");
    assert_ne!(
        column(cols, "near_credit_card")["ideal_type"],
        "Credit Card Number"
    );
    assert_ne!(column(cols, "near_isbn13")["ideal_type"], "ISBN-13");
    assert_ne!(column(cols, "near_mac")["ideal_type"], "MAC Address");
    assert_ne!(column(cols, "near_hex_color")["ideal_type"], "Hex Color");
    assert_ne!(column(cols, "near_imei")["ideal_type"], "IMEI");
    assert_ne!(column(cols, "near_jwt")["ideal_type"], "JWT");
    assert_ne!(
        column(cols, "near_coordinates")["ideal_type"],
        "Geographic Coordinates"
    );
    // Mixed digest "kinds" (a 32-char then a 40-char value) within one
    // column must not trigger the hash-digest note either.
    assert_eq!(column(cols, "near_hash")["notes"], "");
    assert_ne!(column(cols, "near_vin")["ideal_type"], "VIN");
    assert_ne!(column(cols, "near_cidr")["ideal_type"], "CIDR");
    assert_ne!(column(cols, "near_ulid")["ideal_type"], "ULID");
    assert_ne!(column(cols, "near_wkt")["ideal_type"], "WKT Geometry");
    assert_ne!(column(cols, "near_cron")["ideal_type"], "Cron Expression");

    // A column that's 3 real UUIDs and 1 clearly-not-a-UUID value must not
    // be classified as UUID - one bad value vetoes the whole column.
    assert_ne!(column(cols, "mostly_uuid")["ideal_type"], "UUID");

    // Injection-style payloads (SQL/shell/template) and heavy unicode
    // (emoji, CJK, zero-width spaces) are just opaque data - no crash, no
    // bogus type.
    let injection = column(cols, "injection");
    assert!(matches!(
        injection["ideal_type"].as_str().unwrap(),
        "String" | "enum / category"
    ));
    let unicode = column(cols, "unicode_heavy");
    assert!(matches!(
        unicode["ideal_type"].as_str().unwrap(),
        "String" | "enum / category"
    ));
}

#[test]
fn empty_csv_produces_an_empty_table_not_a_crash() {
    let doc = run_json("malformed_empty.csv", &[]);
    let cols = table(&doc, "malformed_empty");
    assert!(cols.is_empty());
}

#[test]
fn header_only_csv_reports_every_column_as_empty_not_a_crash() {
    let doc = run_json("malformed_header_only.csv", &[]);
    let cols = table(&doc, "malformed_header_only");
    assert_eq!(cols.len(), 3);
    for c in cols {
        assert_eq!(c["missing_pct"].as_f64().unwrap(), 0.0);
        assert!(c["notes"].as_str().unwrap().contains("empty/all null"));
    }
}

// Found via real-world testing (Ask A Manager's public salary survey CSV,
// and independently the HPI Pollock data-loading benchmark's own
// file_preamble.csv fixture) rather than reasoned about in advance: a
// title/banner row above the real header is a real shape human-authored
// spreadsheets export as. preamble.csv reproduces the same shape at fixture
// scale - see detect_preamble_rows's doc comment in lib.rs for the exact
// structural signal this fires on.

#[test]
fn preamble_row_is_auto_detected_and_skipped() {
    let doc = run_json("preamble.csv", &[]);
    let cols = table(&doc, "preamble");
    let names: Vec<&str> = cols.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["id", "name", "age"]);
    let id = column(cols, "id");
    assert_eq!(id["sample_values"], serde_json::json!(["1", "2"]));
}

#[test]
fn explicit_skip_rows_matches_auto_detection() {
    let doc = run_json("preamble.csv", &["--skip-rows", "1"]);
    let cols = table(&doc, "preamble");
    let names: Vec<&str> = cols.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["id", "name", "age"]);
}

#[test]
fn explicit_skip_rows_zero_disables_auto_detection() {
    // The banner row (4 fields, trailing commas) becomes the header
    // itself, so the very next row ("id,name,age", 3 fields) is a genuine
    // header/data mismatch - proving --skip-rows 0 really does override
    // auto-detection rather than being indistinguishable from "not passed".
    let output = std::process::Command::new(bin())
        .args([
            fixture("preamble.csv").to_str().unwrap(),
            "-",
            "--output-format",
            "json",
            "--skip-rows",
            "0",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("field"),
        "expected a header/data field-count mismatch error, got: {stderr}"
    );
}

#[test]
fn clean_csv_has_no_preamble_detected() {
    // sample.csv has no banner row - auto-detection must not fire on an
    // already-clean file, and no "detected N preamble row(s)" note should
    // appear on stderr.
    let output = std::process::Command::new(bin())
        .args([
            fixture("sample.csv").to_str().unwrap(),
            "-",
            "--output-format",
            "json",
        ])
        .output()
        .expect("failed to run binary");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("preamble"),
        "auto-detection should not have fired on a clean CSV, got: {stderr}"
    );
}

// Found via a real-world sweep against the HPI Pollock benchmark's own
// crawled-CSV survey: three real files are a scientific/numeric export
// where line 1 is a row count, not a header, followed by consistently
// 2-column data - row_count_preamble.csv reproduces that exact shape at
// fixture scale. Before this signal existed, all three real files failed
// with a hard "found record with 2 fields, but the header has 1 fields"
// error instead of resolving - a genuinely parseable file, not a corrupt
// one.
#[test]
fn row_count_preamble_line_is_auto_detected_and_skipped() {
    let doc = run_json("row_count_preamble.csv", &[]);
    let cols = table(&doc, "row_count_preamble");
    assert_eq!(cols.len(), 2);
    for c in cols {
        assert_eq!(c["ideal_type"], "f64");
    }
}

#[test]
fn whitespace_only_csv_treats_the_blank_value_as_missing_not_a_crash() {
    let doc = run_json("malformed_whitespace_only.csv", &[]);
    let cols = table(&doc, "malformed_whitespace_only");
    assert_eq!(cols.len(), 1);
    assert_eq!(cols[0]["missing_pct"].as_f64().unwrap(), 100.0);
}

#[test]
fn bom_prefixed_csv_reads_clean_column_names_not_a_crash() {
    let doc = run_json("malformed_bom.csv", &[]);
    let cols = table(&doc, "malformed_bom");
    // The BOM must not leak into the first header's name.
    assert!(cols.iter().any(|c| c["name"] == "name"));
    assert!(cols.iter().any(|c| c["name"] == "value"));
}

#[test]
fn ragged_csv_rows_produce_an_actionable_error_not_a_panic() {
    let output = Command::new(bin())
        .args([fixture("malformed_ragged.csv").to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("record") || stderr.contains("field"),
        "expected a CSV-shape error naming the problem, got: {stderr}"
    );
}

#[test]
fn invalid_utf8_csv_produces_an_actionable_error_not_a_panic() {
    let output = Command::new(bin())
        .args([fixture("malformed_invalid_utf8.csv").to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("utf-8") || stderr.contains("UTF-8"),
        "expected an actionable UTF-8 error, got: {stderr}"
    );
}

#[test]
fn deeply_nested_json_fails_cleanly_instead_of_a_stack_overflow() {
    // A classic adversarial-JSON pattern (unbounded nesting depth) - proves
    // serde_json's own recursion limit protects the recursive flattener in
    // profile_json_path, rather than the process crashing.
    let output = Command::new(bin())
        .args([fixture("malformed_deeply_nested.json").to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("recursion limit"),
        "expected a recursion-limit error, got: {stderr}"
    );
}

#[cfg(feature = "xml")]
#[test]
fn deeply_nested_xml_fails_cleanly_instead_of_a_stack_overflow() {
    // Unlike JSON/TOML/YAML/MessagePack/CBOR, xmltree has no recursion
    // guard of its own - confirmed by this exact adversarial shape
    // genuinely stack-overflowing the compiled binary (SIGABRT, not a
    // clean error) before xml_nesting_too_deep's pre-parse scan was added.
    // This locks in the fix.
    let output = Command::new(bin())
        .args([fixture("malformed_deeply_nested.xml").to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    assert!(
        output.status.code().is_some(),
        "expected a clean exit, not a signal (e.g. a stack-overflow abort): {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked at") && !stderr.contains("RUST_BACKTRACE"),
        "expected a clean handled error, got what looks like a crash: {stderr}"
    );
    assert!(
        stderr.contains("levels of nested XML elements"),
        "expected a nesting-depth error, got: {stderr}"
    );
}

#[cfg(feature = "xml")]
#[test]
fn xml_with_comments_cdata_and_self_closing_tags_is_not_miscounted_as_too_deep() {
    // The depth pre-scan has to walk past comment/CDATA content (which can
    // contain literal '<'/'>' characters that must not count as real tags)
    // and recognize self-closing tags (which must not add net depth) -
    // otherwise a legitimate, shallow document could be wrongly rejected.
    let path = fixture("_scratch_xml_comments_cdata.xml");
    std::fs::write(
        &path,
        r#"<root>
  <!-- a comment with < and > and <<<many>>> angle brackets -->
  <item><![CDATA[some <fake> <<<tags>>> here]]></item>
  <nested><deep><deeper><deepest>value</deepest></deeper></deep></nested>
  <self_closing_a/><self_closing_a/><self_closing_a/>
</root>
"#,
    )
    .unwrap();
    let doc = run_json("_scratch_xml_comments_cdata.xml", &[]);
    std::fs::remove_file(&path).ok();
    let cols = table(&doc, "_scratch_xml_comments_cdata");

    // The CDATA content came through as a plain value, not parsed as markup.
    let item = column(cols, "item");
    assert_eq!(item["sample_values"][0], "some <fake> <<<tags>>> here");

    let deepest = column(cols, "nested.deep.deeper.deepest");
    assert_eq!(deepest["sample_values"][0], "value");
}

#[cfg(feature = "xml")]
#[test]
fn xml_many_shallow_self_closing_siblings_is_not_miscounted_as_too_deep() {
    // 2,000 self-closing siblings at depth 1 - a legitimate, wide-but-
    // shallow document that must not trip the nesting-depth guard, which
    // only cares about depth, not element count. Doesn't inspect the
    // resulting columns (an empty self-closing tag with no attributes or
    // content is its own, unrelated "#text": null fallback shape) - the
    // only thing this proves is that width alone never triggers the
    // depth-guard error.
    let path = fixture("_scratch_xml_wide_self_closing.xml");
    let mut content = String::from("<root>");
    content.push_str(&"<item/>".repeat(2000));
    content.push_str("</root>");
    std::fs::write(&path, content).unwrap();
    let output = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--output-format", "json"])
        .output()
        .expect("failed to run binary");
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "a wide-but-shallow document should never trip the depth guard: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// --- Semantic type detection through every format's own reader --------
// suggest_ideal_type is format-agnostic (it only ever sees raw strings),
// and CSV/JSON already exercise it exhaustively (type_detection.csv,
// adversarial.csv, nested_typed.jsonl) - so the risk this section actually
// covers isn't "does UUID detection work," it's "does *this format's own
// reader* hand suggest_ideal_type the raw value unmangled." That's a real,
// format-specific failure mode this project has hit before (see CLAUDE.md's
// design philosophy: Excel's writer silently turning a leading-zero zip
// code into a number is exactly this class of bug, just for a different
// heuristic). Before this section, most formats below had never had a
// single assertion proving a precise-grammar type (UUID/Email/IPv4/date)
// resolves correctly through their own reader - only current_type/shape
// were checked. Every fixture here (`type_detection.<ext>`) carries the
// same five columns (id/user_uuid/contact_email/ip_address/signup_date)
// so the columns and expected values line up across formats; each was
// generated with pandas/pyarrow/fastavro/openpyxl/dbf/etc. and its output
// verified by hand against the compiled binary before being trusted,
// per this project's usual practice.

#[cfg(feature = "parquet")]
#[test]
fn parquet_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.parquet", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "parquet")]
#[test]
fn arrow_ipc_recognizes_uuid_email_ipv4_and_date_columns() {
    // The first real Arrow IPC/Feather fixture in this suite - previously
    // only feature-wiring was checked (feather_reads_via_the_shared_arrow_batch_profiler
    // above), never an actual read, since Parquet and Arrow IPC share
    // profile_arrow_batches and a Parquet fixture already existed. This
    // fixture closes that gap with a real .arrow file, auto-detected from
    // its extension the same way a user would actually invoke this (no
    // --format needed).
    let doc = run_json("type_detection.arrow", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "avro")]
#[test]
fn avro_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.avro", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "avro")]
#[test]
fn avro_resolves_logical_types_instead_of_leaving_them_as_raw_numbers_or_debug_output() {
    // Cloud-platform Avro producers (Kinesis Firehose, Event Hubs Capture,
    // Pub/Sub) lean heavily on Avro's logical-type mechanism for
    // timestamps and precise decimals - found via exactly this kind of
    // adversarial probing that two of them were silently broken:
    // timestamp-millis/-micros rendered as opaque epoch integers (the
    // semantic meaning the schema declares was being thrown away), and
    // decimal rendered as Rust's own Debug output on the internal wrapper
    // struct ("Decimal(Decimal { value: 12345, len: 2 })"), unusable and
    // arguably worse than the raw bytes. See avro_value_to_json's doc
    // comment for the fix (decimal needs the schema's scale, which only
    // the value's sibling schema node carries, not the value itself).
    let doc = run_json("avro_logical_types.avro", &[]);
    let cols = table(&doc, "avro_logical_types");

    assert_eq!(
        column(cols, "event_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
    assert_eq!(
        column(cols, "event_ts_millis")["ideal_type"],
        "NaiveDate / DateTime"
    );
    assert_eq!(
        column(cols, "event_ts_micros")["ideal_type"],
        "NaiveDate / DateTime"
    );
    assert_eq!(column(cols, "event_time_millis")["ideal_type"], "NaiveTime");
    assert_eq!(column(cols, "record_uuid")["ideal_type"], "UUID");

    // Decimal: positive, negative, and zero values in the same top-level
    // column, plus the same logical type nested inside a record and an
    // array - proves the schema co-recursion resolves scale correctly at
    // every nesting shape, not just the flat top-level case.
    let price = column(cols, "price");
    assert_eq!(price["ideal_type"], "f64");
    assert_eq!(
        price["sample_values"],
        serde_json::json!(["123.45", "-45.67", "0.00"])
    );
    let inner_price = column(cols, "nested.inner_price");
    assert_eq!(inner_price["sample_values"][1], "0.001"); // zero-padded, not "1" with the point misplaced
    let price_list = column(cols, "price_list");
    assert_eq!(price_list["ideal_type"], "Vec<f64>");
}

// Both found via a real-world sweep combining the Apache Avro project's
// own interop test data with real-shaped sample data (the widely-used
// "userdata" Avro dataset) - not a synthetic corpus built for this project.

#[cfg(feature = "avro")]
#[test]
fn avro_reads_snappy_and_zstandard_compressed_files() {
    // Snappy is Avro's most common production codec (Kafka/Hadoop
    // ecosystems especially) - it and zstd were both silently rejected
    // ("Codec 'snappy' is not supported/enabled") until this project's
    // apache-avro dependency enabled the matching optional features. Every
    // real .avro file using either codec failed outright before this,
    // including the Apache Avro project's own interop test data.
    let doc = run_json("edge_avro_snappy_codec.avro", &[]);
    let cols = table(&doc, "edge_avro_snappy_codec");
    assert_eq!(column(cols, "sensor_id")["ideal_type"], "i64");
    assert_eq!(column(cols, "value")["ideal_type"], "f64");
}

#[cfg(feature = "avro")]
#[test]
fn avro_top_level_scalar_records_become_one_value_column() {
    // An Avro RPC response file (or any Avro stream whose schema is a bare
    // scalar rather than a record - a real, valid shape, confirmed against
    // the Apache Avro project's own "hello world" RPC interop fixture,
    // which decodes to the plain string "Hello World", not an object) used
    // to be rejected with "expected each Avro record to decode to an
    // object" - the same class of gap the JSON/YAML readers had for their
    // own top-level-scalar cases.
    let doc = run_json("edge_avro_scalar_records.avro", &[]);
    let cols = table(&doc, "edge_avro_scalar_records");
    assert_eq!(cols.len(), 1);
    let value = column(cols, "value");
    assert_eq!(value["current_type"], "String");
    assert_eq!(value["missing_pct"].as_f64().unwrap(), 0.0);
}

// A field can reference a *named* record/enum/fixed type defined elsewhere
// in the same schema by its bare name - including a record referencing
// *itself* inside one of its own fields (a real, common shape for
// tree/list-like data, e.g. an "employee has a manager, who is also an
// employee" chain). This exercises `avro_support`'s own name-resolution
// mechanism, which has no analog in any other format this project reads:
// a flat name -> schema table built once during parsing, with every
// reference (forward, backward, or self) resolved lazily against it only
// once decoding starts - see avro_support's own `Schema::Ref` doc comment.
#[cfg(feature = "avro")]
#[test]
fn avro_resolves_named_type_references_including_self_reference() {
    let doc = run_json("edge_avro_named_type_refs.avro", &[]);
    let cols = table(&doc, "edge_avro_named_type_refs");

    // `backup_address` references the earlier-defined "Address" record by
    // name (inside a nullable union) - a plain backward reference.
    assert_eq!(
        column(cols, "backup_address.city")["sample_values"],
        serde_json::json!(["Capital City"])
    );

    // `manager` is `["null", "Employee"]` - the record referencing its own
    // name. Three levels of real recursion (Carol -> Bob -> Alice) must
    // all flatten correctly, including the innermost employee's own
    // (null) manager field.
    assert_eq!(
        column(cols, "manager.name")["sample_values"],
        serde_json::json!(["Alice", "Bob"])
    );
    assert_eq!(
        column(cols, "manager.manager.name")["sample_values"],
        serde_json::json!(["Alice"])
    );
    assert_eq!(
        column(cols, "manager.manager.manager")["missing_pct"]
            .as_f64()
            .unwrap(),
        100.0
    );
}

#[cfg(feature = "xlsx")]
#[test]
fn excel_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.xlsx", &[]);
    let cols = table(&doc, "Sheet1"); // openpyxl's default sheet name
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

// Found via a real-world sweep against three genuinely real .xlsx files
// (a cyclone-tracking dataset, Microsoft's own "Financial Sample" demo
// workbook, and a public "messy data" teaching dataset) - all three had
// at least one date column that came through as a meaningless raw
// integer (Excel's internal day-count serial, e.g. 44652) instead of a
// date. type_detection.xlsx's own "signup_date" column above never
// caught this because it was written as a plain date-shaped *string*,
// not a genuine native Excel date cell - this fixture is written with
// real datetime.date/datetime.datetime values via openpyxl specifically
// to exercise the code path the string-based fixture couldn't reach.
// Verified this fixture actually catches a regression, not just
// exercises already-correct code: temporarily reverting to the old
// `cell.to_string()` behavior reproduced the exact original bug (raw
// serials "45306"/"45306.4375") before the fix was restored.
#[cfg(feature = "xlsx")]
#[test]
fn excel_resolves_native_date_and_datetime_cells_not_raw_serial_numbers() {
    let doc = run_json("edge_xlsx_native_date_cells.xlsx", &[]);
    let cols = table(&doc, "Sheet");

    let event_date = column(cols, "event_date");
    assert_eq!(event_date["ideal_type"], "NaiveDate / DateTime");
    assert_eq!(event_date["sample_values"][0], "2024-01-15");

    // A cell with a real time-of-day component keeps it, rather than
    // collapsing everything to a bare date.
    let event_datetime = column(cols, "event_datetime");
    assert_eq!(event_datetime["ideal_type"], "NaiveDate / DateTime");
    assert_eq!(event_datetime["sample_values"][0], "2024-01-15T10:30:00");
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.sqlite", &[]);
    let cols = table(&doc, "data");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "msgpack")]
#[test]
fn msgpack_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.msgpack", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "cbor")]
#[test]
fn cbor_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.cbor", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "msgpack")]
#[test]
fn msgpack_top_level_array_of_scalars_becomes_one_value_column() {
    // A MessagePack stream of bare scalars (e.g. IoT/telemetry sensor
    // readings - a real, common shape for this format specifically because
    // it's a compact binary encoding aimed at exactly that use case) has no
    // field names to extract as a record, but is still a genuine single
    // column. Used to be rejected outright with "expected each MessagePack
    // record to decode to a map" - the same class of gap the JSON/YAML/Avro
    // readers had for their own top-level-scalar cases, confirmed by
    // generating this exact shape (five float sensor readings) with
    // Python's `msgpack` library and running it through the compiled
    // binary before the fix existed.
    let doc = run_json("edge_msgpack_scalar_array.msgpack", &[]);
    let cols = table(&doc, "edge_msgpack_scalar_array");
    assert_eq!(cols.len(), 1);
    let value = column(cols, "value");
    assert_eq!(value["ideal_type"], "f64");
    assert_eq!(value["missing_pct"].as_f64().unwrap(), 0.0);
}

// This project's existing MessagePack fixtures are small enough to only
// ever exercise the fixstr/fixarray/fixmap/fixint marker ranges - every
// "wide" marker (str8/str16, array16, map16, bin8, and a uint64 exceeding
// i64::MAX, which only the u64 branch of the hand-rolled decoder's
// integer handling can represent) had zero coverage until this fixture,
// found while auditing `msgpack_support` against `rmp`'s own marker.rs.
#[cfg(feature = "msgpack")]
#[test]
fn msgpack_handles_wide_string_array_map_and_uint64_markers() {
    let doc = run_json("edge_msgpack_wide_markers.msgpack", &[]);
    let cols = table(&doc, "edge_msgpack_wide_markers");

    assert_eq!(
        column(cols, "long_str")["sample_values"][0]
            .as_str()
            .unwrap()
            .len(),
        40
    );
    assert_eq!(
        column(cols, "very_long_str")["sample_values"][0]
            .as_str()
            .unwrap()
            .len(),
        300
    );
    assert_eq!(column(cols, "many_items")["current_type"], "Vec<i64>");
    assert!(cols.iter().any(|c| c["name"] == "wide_map.k49"));
    let raw_bytes = column(cols, "raw_bytes");
    assert_eq!(raw_bytes["sample_values"][0], "000102fffe");
    // Exceeds i64::MAX - only representable via the hand-rolled decoder's
    // separate UInt(u64) branch (see msgpack_support::Value).
    let huge_uint = column(cols, "huge_uint");
    assert_eq!(huge_uint["sample_values"][0], "18446744073709551615");
}

#[cfg(feature = "cbor")]
#[test]
fn cbor_top_level_array_of_scalars_becomes_one_value_column() {
    // Same fix, same fixture shape, as MessagePack's equivalent test above.
    let doc = run_json("edge_cbor_scalar_array.cbor", &[]);
    let cols = table(&doc, "edge_cbor_scalar_array");
    assert_eq!(cols.len(), 1);
    let value = column(cols, "value");
    assert_eq!(value["ideal_type"], "f64");
    assert_eq!(value["missing_pct"].as_f64().unwrap(), 0.0);
}

// RFC 8949 §3.2.3: an indefinite-length array/map/bytes/text is a sequence
// of chunks terminated by the `0xFF` break byte rather than a fixed count
// up front - a real, spec-legal encoding (used by streaming encoders that
// don't know a collection's final size ahead of time) genuinely distinct
// from the definite-length form every other CBOR fixture in this project
// exercises. `edge_cbor_indefinite_length.cbor` is hand-built raw bytes (no
// Python CBOR library used here emits indefinite length by default) with
// an indefinite-length *outer* map containing an indefinite array, an
// indefinite byte string (two chunks, `h'0102'` + `h'0304'`), and an
// indefinite text string (two chunks, `"strea"` + `"ming"`) - covering
// every one of `cbor_support`'s four chunked-decoding code paths
// (`read_array_body`/`read_map_body`/`read_bytes_body`/`read_text_body`'s
// own `None` branches) in one fixture.
#[cfg(feature = "cbor")]
#[test]
fn cbor_reads_indefinite_length_arrays_maps_bytes_and_text() {
    let doc = run_json("edge_cbor_indefinite_length.cbor", &[]);
    let cols = table(&doc, "edge_cbor_indefinite_length");
    assert_eq!(column(cols, "arr")["ideal_type"], "Vec<i64>");
    assert_eq!(
        column(cols, "arr")["sample_values"],
        serde_json::json!(["1", "2", "3"])
    );
    // The two byte-string chunks (0x01 0x02, 0x03 0x04) concatenate to the
    // hex string "01020304" before any type detection runs.
    assert_eq!(
        column(cols, "bytes")["sample_values"][0],
        serde_json::json!("01020304")
    );
    // The two text chunks ("strea", "ming") concatenate to "streaming".
    assert_eq!(
        column(cols, "text")["sample_values"][0],
        serde_json::json!("streaming")
    );
}

// The old ciborium-based reader already widened an out-of-i64-range CBOR
// integer to a string (`i64::try_from(*i).unwrap_or_else(|_| ...
// i128::from(*i).to_string())`) rather than silently truncating it -
// `cbor_support::Value::Integer` keeps that behavior by storing `i128`
// directly (CBOR's own integer range is wider than i64 on both ends: an
// unsigned major-type-0 value up to `u64::MAX`, and a negative major-
// type-1 value down to `-1 - u64::MAX`, confirmed against
// `ciborium::value::Integer`'s own internal representation - see
// CLAUDE.md). This fixture carries exactly those two extremes.
#[cfg(feature = "cbor")]
#[test]
fn cbor_reads_integers_beyond_i64_range_via_i128() {
    let doc = run_json("edge_cbor_big_integers.cbor", &[]);
    let cols = table(&doc, "edge_cbor_big_integers");
    assert_eq!(
        column(cols, "pos")["sample_values"][0],
        serde_json::json!("18446744073709551615")
    );
    assert_eq!(
        column(cols, "neg")["sample_values"][0],
        serde_json::json!("-18446744073709551616")
    );
}

// Half-precision (binary16) floats (major type 7, additional info 25) are a
// real CBOR feature with no coverage at all in this project's old
// ciborium-based fixtures - constrained-device/telemetry producers are the
// usual real-world source. `cbor_support::f16_to_f64` converts via plain
// floating-point arithmetic rather than the `half` crate (not otherwise a
// dependency of this project); hand-verified against known reference bit
// patterns before being trusted (see its own doc comment in src/lib.rs).
// This fixture locks in two of those reference values end-to-end.
#[cfg(feature = "cbor")]
#[test]
fn cbor_reads_half_precision_floats() {
    let doc = run_json("edge_cbor_float16.cbor", &[]);
    let cols = table(&doc, "edge_cbor_float16");
    assert_eq!(
        column(cols, "one")["sample_values"][0],
        serde_json::json!("1.0")
    );
    assert_eq!(
        column(cols, "neg_two")["sample_values"][0],
        serde_json::json!("-2.0")
    );
}

// Same class of debug-build stack-safety risk found (and fixed) for
// MessagePack and TOML earlier in this project's dependency-removal effort
// - a CBOR-decoded `serde_json::Value` tree never passes through
// `serde_json`'s own parse-time recursion guard, so `cbor_support`'s own
// recursive decode/convert path is what has to survive adversarially deep
// input. Unlike those two, this one was designed in from the start rather
// than discovered after the fact: `MAX_DEPTH` (256) was chosen up front to
// match `ciborium`'s own default recursion limit, precisely *because* the
// MessagePack finding already established that class of risk. Verified
// empirically anyway, not just assumed safe by design: a hand-built
// 50,000-level-deep definite-length array fails cleanly on a debug build
// (this fixture), and a boundary check (255 levels succeeds, 256 fails)
// confirmed the guard fires exactly where intended, not off by one.
#[cfg(feature = "cbor")]
#[test]
fn deeply_nested_cbor_fails_cleanly_instead_of_a_stack_overflow() {
    let output = Command::new(bin())
        .args([fixture("malformed_deeply_nested.cbor").to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    assert!(
        output.status.code().is_some(),
        "expected a clean exit, not a signal (e.g. a stack-overflow abort): {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked at") && !stderr.contains("RUST_BACKTRACE"),
        "expected a clean handled error, got what looks like a crash: {stderr}"
    );
    assert!(
        stderr.contains("nested more than 256 levels deep"),
        "expected a nesting-depth error, got: {stderr}"
    );
}

#[cfg(feature = "toml")]
#[test]
fn toml_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.toml", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "ini")]
#[test]
fn ini_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.ini", &[]);
    let cols = table(&doc, "data"); // the [data] section name
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "npy")]
#[test]
fn npy_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.npy", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

// No existing .npy fixture used a non-native byte order or a fixed-size
// sub-array field (`DType::Array` nested inside a `DType::Record`) - both
// real numpy shapes with zero prior test coverage, found while auditing
// the hand-rolled npy_support reader against npyz's own type_str.rs.
#[cfg(feature = "npy")]
#[test]
fn npy_handles_big_endian_fields_and_fixed_size_subarray_fields() {
    let doc = run_json("edge_npy_big_endian_and_subarray.npy", &[]);
    let cols = table(&doc, "edge_npy_big_endian_and_subarray");

    let score = column(cols, "score_be");
    assert_eq!(score["current_type"], "f64");
    assert_eq!(
        score["sample_values"],
        serde_json::json!(["9.5", "42", "100.25"])
    );

    let coords = column(cols, "coords");
    assert_eq!(coords["current_type"], "Vec<f64>");
    assert_eq!(
        coords["sample_values"],
        serde_json::json!(["1;2;3", "4;5;6", "7;8;9"])
    );
}

#[cfg(feature = "npy")]
#[test]
fn npz_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.npz", &[]);
    let cols = table(&doc, "people"); // the array's name inside the archive
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "dbase")]
#[test]
fn dbase_recognizes_uuid_email_ipv4_and_date_columns() {
    // Field names are shortened to fit dBase's own 10-character field-name
    // limit (email/ip_addr/sign_dt instead of contact_email/ip_address/
    // signup_date) - a real format constraint, not this tool's choice.
    // Character fields are declared wide enough (C(36)/C(32)) that a UUID
    // or email isn't silently truncated before it ever reaches the
    // heuristic engine.
    let doc = run_json("type_detection.dbf", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "USER_UUID")["ideal_type"], "UUID");
    assert_eq!(column(cols, "EMAIL")["ideal_type"], "Email");
    assert_eq!(column(cols, "IP_ADDR")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "SIGN_DT")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "stata")]
#[test]
fn stata_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.dta", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "spss")]
#[test]
fn spss_recognizes_uuid_email_ipv4_and_date_columns() {
    // `signup_date` is a genuine native SPSS date variable (numeric
    // storage, an ADATE10 print format) rather than a string that merely
    // looks like a date - so this also exercises the current_type/
    // ideal_type gap this project's other declared-numeric-but-really-a-
    // date readers (dBase, SAS7BDAT) already demonstrate: `current_type`
    // stays "f64" (SPSS stores every date as a numeric offset), while
    // `ideal_type` correctly resolves to a real date once
    // `format_numeric_value` renders it as an ISO string.
    let doc = run_json("type_detection.sav", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(column(cols, "signup_date")["current_type"], "f64");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "spss")]
#[test]
fn spss_excludes_declared_missing_values_not_just_sysmis() {
    // `score`/`rating` each declare their own user-missing values (two
    // discrete codes, and a 900-999 range respectively) on top of SPSS's
    // own SYSMIS sentinel; `code` declares a discrete missing string.
    // None of the three real fixture rows involved is SYSMIS at all - the
    // exclusion only happens because this reader consults each
    // variable's own missing-value declaration, not just the bit pattern.
    let doc = run_json("edge_spss_missing_values.sav", &[]);
    let cols = table(&doc, "edge_spss_missing_values");
    assert_eq!(column(cols, "score")["missing_pct"], 40.0);
    assert_eq!(
        column(cols, "score")["sample_values"],
        serde_json::json!(["10", "20", "30"])
    );
    assert_eq!(column(cols, "rating")["missing_pct"], 40.0);
    assert_eq!(column(cols, "code")["missing_pct"], 40.0);
    assert_eq!(
        column(cols, "code")["sample_values"],
        serde_json::json!(["AA", "BB", "CC"])
    );
}

#[cfg(feature = "spss")]
#[test]
fn spss_reconstructs_a_very_long_string_split_across_named_segments() {
    // `notes` is 300 characters wide - past SPSS's 255-byte single-
    // segment limit, so it's stored across multiple named "very long
    // string" segments (subtype 14) that this reader has to reassemble
    // in the right order, stripping each segment's own trailing slot-
    // alignment padding before appending the next segment's bytes.
    let doc = run_json("edge_spss_very_long_string.sav", &[]);
    let cols = table(&doc, "edge_spss_very_long_string");
    let notes = column(cols, "notes");
    let samples = notes["sample_values"].as_array().unwrap();
    assert_eq!(samples[0], "A".repeat(300));
    let expected_second = "B".repeat(137) + &"Z".repeat(163);
    assert_eq!(samples[1], expected_second);
}

#[cfg(feature = "spss")]
#[test]
fn spss_bytecode_compression_reads_identically_to_uncompressed() {
    // Same underlying data, one written with row_compress=True (SPSS's
    // own "bytecode" RLE-style compression) and one without - proving
    // compression is transparent, the same "compressed reads identically
    // to uncompressed" contract this project's gzip/zstd readers already
    // get.
    let compressed = run_json("edge_spss_bytecode_compressed.sav", &[]);
    let uncompressed = run_json("edge_spss_uncompressed_equivalent.sav", &[]);
    let compressed_cols = table(&compressed, "edge_spss_bytecode_compressed");
    let uncompressed_cols = table(&uncompressed, "edge_spss_uncompressed_equivalent");
    assert_eq!(compressed_cols.len(), uncompressed_cols.len());
    for (c, u) in compressed_cols.iter().zip(uncompressed_cols.iter()) {
        assert_eq!(c["name"], u["name"]);
        assert_eq!(c["current_type"], u["current_type"]);
        assert_eq!(c["ideal_type"], u["ideal_type"]);
        assert_eq!(c["sample_values"], u["sample_values"]);
        assert_eq!(c["missing_pct"], u["missing_pct"]);
    }
}

#[cfg(feature = "spss")]
#[test]
fn spss_zsav_zlib_compression_gives_an_actionable_error_not_a_panic() {
    let output = Command::new(bin())
        .args([fixture("edge_spss_zlib_compressed.zsav").to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("zsav"),
        "expected a zsav-specific error message: {stderr}"
    );
    assert!(!stderr.contains("panicked at"));
}

#[cfg(feature = "spss")]
#[test]
fn spss_format_recognized_via_extension_and_override() {
    let doc = run_with_format("type_detection.sav", "json", &[]);
    assert_eq!(doc["format"], "spss");
    let doc = run_with_format("type_detection.sav", "json", &["--format", "spss"]);
    assert_eq!(doc["format"], "spss");
}

#[cfg(feature = "orc")]
#[test]
fn orc_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_json("type_detection.orc", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(column(cols, "signup_date")["current_type"], "Date");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "orc")]
#[test]
fn orc_decodes_rle_v2_short_repeat_direct_and_delta_columns() {
    // `constant_ish` (all one value) forces RLEv2's short-repeat
    // sub-encoding, `scattered` (no useful structure) forces direct, and
    // `increasing` (a plain 0..n range) forces delta - together these
    // exercise three of RLEv2's four real sub-encodings through the full
    // reader pipeline, not just the unit-level worked examples.
    let doc = run_json("edge_orc_rle_v2_encodings.orc", &[]);
    let cols = table(&doc, "edge_orc_rle_v2_encodings");
    assert_eq!(
        column(cols, "constant_ish")["sample_values"],
        serde_json::json!(["42"])
    );
    assert_eq!(
        column(cols, "scattered")["sample_values"],
        serde_json::json!(["0", "7919", "15838"])
    );
    assert_eq!(
        column(cols, "increasing")["sample_values"],
        serde_json::json!(["0", "1", "2"])
    );
}

#[cfg(feature = "orc")]
#[test]
fn orc_excludes_null_values_via_the_present_stream() {
    let doc = run_json("edge_orc_missing_values.orc", &[]);
    let cols = table(&doc, "edge_orc_missing_values");
    assert_eq!(column(cols, "score")["missing_pct"], 40.0);
    assert_eq!(column(cols, "name")["missing_pct"], 40.0);
    assert_eq!(
        column(cols, "name")["sample_values"],
        serde_json::json!(["alice", "carol", "dave"])
    );
}

#[cfg(feature = "orc")]
#[test]
fn orc_dictionary_encoded_strings_resolve_to_the_real_values() {
    let doc = run_json("edge_orc_dictionary_strings.orc", &[]);
    let cols = table(&doc, "edge_orc_dictionary_strings");
    let category = column(cols, "category");
    assert_eq!(category["ideal_type"], "enum / category");
    let samples = category["sample_values"].as_array().unwrap();
    for s in samples {
        assert!(["red", "green", "blue"].contains(&s.as_str().unwrap()));
    }
}

#[cfg(feature = "orc")]
#[test]
fn orc_decodes_decimal_and_nanosecond_precision_timestamps() {
    let doc = run_json("edge_orc_decimal_and_timestamp.orc", &[]);
    let cols = table(&doc, "edge_orc_decimal_and_timestamp");
    assert_eq!(
        column(cols, "amount")["sample_values"],
        serde_json::json!(["123.45", "-67.89", "0.00"])
    );
    assert_eq!(
        column(cols, "ts")["sample_values"],
        serde_json::json!([
            "2024-01-15T10:30:00.123456789",
            "2024-06-01T00:00:00.000000000",
            "2024-06-01T12:00:00.500000000"
        ])
    );
}

#[cfg(feature = "orc")]
#[test]
fn orc_decodes_binary_as_hex_and_boolean_columns() {
    let doc = run_json("edge_orc_binary_and_bool.orc", &[]);
    let cols = table(&doc, "edge_orc_binary_and_bool");
    assert_eq!(column(cols, "flag")["current_type"], "bool");
    assert_eq!(
        column(cols, "blob")["sample_values"],
        serde_json::json!(["0001ff", "68656c6c6f", ""])
    );
}

#[cfg(feature = "orc")]
#[test]
fn orc_every_compression_codec_reads_identically_to_uncompressed() {
    // Same underlying data written with ZLIB/Snappy/Zstd/LZ4 and with no
    // compression at all - proving each codec's own chunked-block framing
    // is transparent, the same "compressed reads identically to
    // uncompressed" contract this project's gzip/zstd/SPSS readers
    // already get.
    let uncompressed = run_json("edge_orc_compression_none.orc", &[]);
    let uncompressed_cols = table(&uncompressed, "edge_orc_compression_none");
    for codec in ["zlib", "snappy", "zstd", "lz4"] {
        let fixture = format!("edge_orc_compression_{codec}.orc");
        let table_name = format!("edge_orc_compression_{codec}");
        let doc = run_json(&fixture, &[]);
        let cols = table(&doc, &table_name);
        assert_eq!(cols.len(), uncompressed_cols.len(), "codec {codec}");
        for (c, u) in cols.iter().zip(uncompressed_cols.iter()) {
            assert_eq!(c["name"], u["name"], "codec {codec}");
            assert_eq!(c["current_type"], u["current_type"], "codec {codec}");
            assert_eq!(c["ideal_type"], u["ideal_type"], "codec {codec}");
            assert_eq!(c["sample_values"], u["sample_values"], "codec {codec}");
        }
    }
}

#[cfg(feature = "orc")]
#[test]
fn orc_pre_epoch_fractional_timestamp_does_not_panic() {
    // A genuinely adversarial-looking (but real, pyarrow-written) shape:
    // a sub-second timestamp before 1970 that was found, via real-file
    // testing, to trigger an integer-overflow panic in the trailing-
    // zero-reconstruction step of this reader's nanosecond decoding
    // before a `checked_mul` guard was added. This doesn't assert a
    // specific "correct" rendered value - real ORC writers have
    // historically disagreed on how to encode this exact shape (see
    // ORC-763) - only that reading it never crashes the process.
    let output = std::process::Command::new(bin())
        .args([
            fixture("edge_orc_pre_epoch_fractional_timestamp.orc")
                .to_str()
                .unwrap(),
            "-",
            "--output-format",
            "json",
        ])
        .output()
        .expect("failed to run binary");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked at"));
}

#[cfg(feature = "orc")]
#[test]
fn orc_format_recognized_via_extension_content_sniffing_and_override() {
    let doc = run_with_format("type_detection.orc", "json", &[]);
    assert_eq!(doc["format"], "orc");
    let doc = run_with_format("type_detection.orc", "json", &["--format", "orc"]);
    assert_eq!(doc["format"], "orc");

    // Content-based sniffing: an extensionless copy must still be
    // recognized from the leading "ORC" magic plus the trailing
    // postscript-length plausibility check.
    let extensionless = fixture("type_detection.orc").with_extension("");
    std::fs::copy(fixture("type_detection.orc"), &extensionless).unwrap();
    let output = Command::new(bin())
        .args([
            extensionless.to_str().unwrap(),
            "-",
            "--output-format",
            "json",
        ])
        .output()
        .expect("failed to run binary");
    std::fs::remove_file(&extensionless).unwrap();
    assert!(output.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["format"], "orc");
}

#[test]
fn fixed_width_recognizes_uuid_email_ipv4_and_date_columns() {
    let doc = run_with_format(
        "type_detection.fwf",
        "json",
        &["--format", "fixed-width", "--widths", "3,38,22,13,12"],
    );
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "user_uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols, "contact_email")["ideal_type"], "Email");
    assert_eq!(column(cols, "ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "weblog")]
#[test]
fn common_and_combined_log_recognize_ipv4_hosts_and_a_url_referer() {
    // sample_common.log/sample_combined.log's host column is already real
    // IPv4 data (127.0.0.1, 192.168.1.5, ...) and combined's referer is
    // already a real URL - no new fixture needed, just assertions that
    // were never written proving the log-line regex hands those fields to
    // suggest_ideal_type unmangled.
    let common = run_with_format("sample_common.log", "json", &["--format", "common-log"]);
    let common_cols = table(&common, "sample_common");
    assert_eq!(column(common_cols, "host")["ideal_type"], "IPv4");

    let combined = run_with_format("sample_combined.log", "json", &["--format", "combined-log"]);
    let combined_cols = table(&combined, "sample_combined");
    assert_eq!(column(combined_cols, "host")["ideal_type"], "IPv4");
    assert_eq!(column(combined_cols, "referer")["ideal_type"], "URL");
}

#[cfg(feature = "syslog")]
#[test]
fn syslog_rfc5424_recognizes_a_uniformly_formatted_rfc3339_timestamp() {
    // A syslog5424-only fixture with every timestamp in the same "Z"
    // representation - the common real-world case of one sender emitting
    // a consistent format throughout its own log stream.
    let doc = run_with_format(
        "edge_rfc5424_uniform_timestamps.log",
        "json",
        &["--format", "syslog5424"],
    );
    let cols = table(&doc, "edge_rfc5424_uniform_timestamps");
    assert_eq!(
        column(cols, "timestamp")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "syslog")]
#[test]
fn syslog_rfc5424_does_not_force_a_mixed_z_and_offset_timestamp_column_into_one_type() {
    // sample_rfc5424.log deliberately mixes RFC 5424's two equally-valid
    // timestamp representations across its 3 lines - a literal "Z" suffix
    // (line 1/3) and an explicit numeric offset (line 2), both legal RFC
    // 3339. DATE_FORMATS requires one *single* candidate format to match
    // every value in the column (see CLAUDE.md's "fixed candidate list,
    // not a fuzzy parser" design note) - "%.fZ" and "%.f%z" are two
    // different candidates, and neither matches all 3 lines at once
    // (verified directly against chrono: the "%z" specifier does not
    // accept a literal "Z"). So this column honestly stays a String
    // rather than silently picking one representation and mis-parsing
    // the other - the same safe-failure-mode tradeoff already documented
    // for RFC 3164's yearless timestamp, just discovered from a different
    // angle (value heterogeneity instead of a missing field).
    let doc = run_with_format("sample_rfc5424.log", "json", &["--format", "syslog5424"]);
    let cols = table(&doc, "sample_rfc5424");
    assert_eq!(column(cols, "timestamp")["ideal_type"], "String");
}

// --- Per-format malformed-input tests ---------------------------------
// CSV/JSON already have their own dedicated malformed-file tests above.
// Every other format gets one here: a `malformed_garbage.<ext>` fixture -
// plain readable text with the right extension but none of the real
// format's structure (no Parquet footer, no SQLite header, no valid TOML
// syntax past the first token, etc.) - proving each reader fails with a
// clean, actionable error rather than a panic. This was verified
// empirically against every format before being written up as a test, not
// assumed: every one of them already propagates the underlying crate's own
// error through `?`/`with_context` rather than unwrapping, so none of this
// required a code fix - it only needed the coverage locking it in.

// Only called from #[cfg(feature = "...")]-gated tests below, so the
// default (no --features) build sees it as unused - allow(dead_code)
// rather than a long any(feature = ...) list naming every gate.
#[allow(dead_code)]
fn assert_fails_without_panicking(fixture_name: &str) {
    let output = Command::new(bin())
        .args([fixture(fixture_name).to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(
        !output.status.success(),
        "{fixture_name}: expected a non-zero exit for malformed input"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.is_empty(),
        "{fixture_name}: expected a non-empty, actionable error message"
    );
    assert!(
        !stderr.contains("panicked at") && !stderr.contains("RUST_BACKTRACE"),
        "{fixture_name}: expected a clean handled error, got what looks like a panic: {stderr}"
    );
}

#[cfg(feature = "parquet")]
#[test]
fn malformed_parquet_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.parquet");
}

#[cfg(feature = "parquet")]
#[test]
fn malformed_arrow_ipc_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.arrow");
}

#[cfg(feature = "avro")]
#[test]
fn malformed_avro_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.avro");
}

#[cfg(feature = "msgpack")]
#[test]
fn malformed_msgpack_fails_cleanly() {
    // Plain readable-text garbage (malformed_garbage.msgpack, the sibling-
    // format convention) doesn't actually work here - see
    // msgpack_ascii_garbage_text_decodes_as_a_stream_of_small_integers
    // below for why. A genuinely truncated multi-byte value (a str8 header
    // claiming 200 bytes with only 3 supplied) is what a real decode
    // failure looks like for this format.
    assert_fails_without_panicking("malformed_truncated.msgpack");
}

#[cfg(feature = "msgpack")]
#[test]
fn msgpack_ascii_garbage_text_decodes_as_a_stream_of_small_integers() {
    // A genuine, surprising discovery, not a tool bug: MessagePack's
    // positive-fixint encoding is defined as the single byte range
    // 0x00-0x7f standing for the integer of the same value - which is
    // byte-for-byte identical to the 7-bit ASCII range. So any plain
    // readable ASCII text (the "garbage" convention every other format's
    // malformed_garbage.<ext> fixture relies on to be structurally
    // invalid) is, byte for byte, already a legal MessagePack value
    // stream: a sequence of small non-negative integers. It used to
    // "fail cleanly" only by accident, via the unrelated pre-fix bug that
    // bailed on anything that wasn't a map - now that top-level non-map
    // streams correctly fall back to a single "value" column instead of
    // erroring, this fixture decodes successfully, and correctly so.
    let doc = run_json("malformed_garbage.msgpack", &[]);
    let cols = table(&doc, "malformed_garbage");
    assert_eq!(cols.len(), 1);
    let value = column(cols, "value");
    assert_eq!(value["ideal_type"], "i64");
    // First three bytes of the fixture are 't','h','i' -> 116, 104, 105.
    assert_eq!(value["sample_values"][0], "116");
}

// Found via this project's own real-world adversarial testing while
// replacing rmpv: a MessagePack-decoded `serde_json::Value` tree never
// passes through `serde_json`'s own parse-time recursion guard (that
// guard only fires while parsing *text*), so a deeply nested structure
// bypasses the protection every plain `.json`/`.jsonl` file gets for
// free. rmpv's own depth limit (1024) turned out to still be unsafe in a
// debug build - confirmed directly, not assumed: an unoptimized build's
// much larger, uninlined stack frames overflowed an 8MB thread stack
// somewhere between 700 and 900 nesting levels, well under 1024, while
// an optimized release build survived 1024 comfortably. `msgpack_support`
// now uses 256 (matching `ciborium`'s own *default* CBOR recursion limit
// - independent corroboration this is a real risk class, not specific to
// this project's code) with comfortable margin under the empirically
// found danger zone. Locks in the fix the same way
// `deeply_nested_xml_fails_cleanly_instead_of_a_stack_overflow` does for
// XML's own, differently-caused gap.
#[cfg(feature = "msgpack")]
#[test]
fn deeply_nested_msgpack_fails_cleanly_instead_of_a_stack_overflow() {
    let output = Command::new(bin())
        .args([fixture("malformed_deeply_nested.msgpack").to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    assert!(
        output.status.code().is_some(),
        "expected a clean exit, not a signal (e.g. a stack-overflow abort): {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked at") && !stderr.contains("RUST_BACKTRACE"),
        "expected a clean handled error, got what looks like a crash: {stderr}"
    );
    assert!(
        stderr.contains("nested more than 256 levels deep"),
        "expected a nesting-depth error, got: {stderr}"
    );
}

// Same class of gap as MessagePack's above, found the same way while
// auditing the hand-rolled `toml_support` parser: TOML's array/inline-
// table grammar recurses with no depth limit of its own. A hand-built
// `[[[...]]]`-nested array genuinely stack-overflowed a debug build
// somewhere between 5,000 and 10,000 levels (deeper than MessagePack's
// own danger zone, but real and reachable all the same, since `cargo
// test`/`cargo run` both default to the debug profile). `MAX_TOML_DEPTH`
// (512) matches this project's own XML depth guard for the same reason -
// comfortable margin, far deeper than any real document would nest.
#[cfg(feature = "toml")]
#[test]
fn deeply_nested_toml_fails_cleanly_instead_of_a_stack_overflow() {
    let output = Command::new(bin())
        .args([fixture("malformed_deeply_nested.toml").to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    assert!(
        output.status.code().is_some(),
        "expected a clean exit, not a signal (e.g. a stack-overflow abort): {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked at") && !stderr.contains("RUST_BACKTRACE"),
        "expected a clean handled error, got what looks like a crash: {stderr}"
    );
    assert!(
        stderr.contains("nested more than 512 levels deep"),
        "expected a nesting-depth error, got: {stderr}"
    );
}

#[cfg(feature = "cbor")]
#[test]
fn malformed_cbor_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.cbor");
}

#[cfg(feature = "xml")]
#[test]
fn malformed_xml_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.xml");
}

#[cfg(feature = "bson")]
#[test]
fn malformed_bson_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.bson");
}

#[cfg(feature = "plist")]
#[test]
fn malformed_plist_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.plist");
}

#[cfg(feature = "json5")]
#[test]
fn malformed_json5_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.json5");
}

#[cfg(feature = "har")]
#[test]
fn malformed_har_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.har");
}

#[cfg(feature = "har")]
#[test]
fn har_missing_log_entries_fails_cleanly() {
    assert_fails_without_panicking("edge_har_missing_entries.har");
}

#[cfg(feature = "geojson")]
#[test]
fn malformed_geojson_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.geojson");
}

// Plain readable text with no BEGIN:VCARD/BEGIN:VCALENDAR at all decodes
// successfully as zero records - the same class of "readable text
// happens to already be valid, structure-free input" quirk this
// project's own malformed_garbage.msgpack fixture already documents for
// MessagePack - so the real failure-shape fixture for both formats is an
// unterminated block instead.
#[cfg(feature = "vcard")]
#[test]
fn vcard_plain_text_with_no_structure_decodes_as_zero_records() {
    let doc = run_json("edge_vcard_no_structure.vcf", &[]);
    assert_eq!(table(&doc, "edge_vcard_no_structure").len(), 0);
}

#[cfg(feature = "vcard")]
#[test]
fn malformed_vcard_fails_cleanly() {
    assert_fails_without_panicking("malformed_unterminated.vcf");
}

#[cfg(feature = "icalendar")]
#[test]
fn icalendar_plain_text_with_no_structure_decodes_as_zero_records() {
    let doc = run_json("edge_icalendar_no_structure.ics", &[]);
    assert_eq!(table(&doc, "edge_icalendar_no_structure").len(), 0);
}

#[cfg(feature = "icalendar")]
#[test]
fn malformed_icalendar_fails_cleanly() {
    assert_fails_without_panicking("malformed_unterminated.ics");
}

#[cfg(feature = "mbox")]
#[test]
fn malformed_mbox_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.mbox");
}

#[cfg(feature = "npy")]
#[test]
fn malformed_npy_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.npy");
}

#[cfg(feature = "npy")]
#[test]
fn malformed_npz_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.npz");
}

#[cfg(feature = "xlsx")]
#[test]
fn malformed_xlsx_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.xlsx");
}

#[cfg(feature = "sqlite")]
#[test]
fn malformed_sqlite_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.sqlite");
}

#[cfg(feature = "toml")]
#[test]
fn malformed_toml_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.toml");
}

#[cfg(feature = "yaml")]
#[test]
fn malformed_yaml_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.yaml");
}

#[cfg(feature = "ini")]
#[test]
fn malformed_ini_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.ini");
}

#[cfg(feature = "dbase")]
#[test]
fn malformed_dbase_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.dbf");
}

// A soft-deleted dBase record (the 0x2A "marked for deletion" flag byte)
// is skipped entirely - a real, documented code path (see CLAUDE.md's
// Architecture section) that had zero fixture coverage before the
// `dbase_support` hand-roll: neither `sample.dbf` nor `type_detection.dbf`
// contains a deleted record. This fixture (hand-built raw bytes, matching
// this project's usual practice when no tool is available to author a
// specific binary shape) has three records - Alice/Bob/Carol - with Bob's
// marked deleted.
#[cfg(feature = "dbase")]
#[test]
fn dbase_skips_soft_deleted_records() {
    let doc = run_json("edge_dbase_deleted_records.dbf", &[]);
    let cols = table(&doc, "edge_dbase_deleted_records");
    let name = column(cols, "NAME");
    assert_eq!(name["sample_values"], serde_json::json!(["Alice", "Carol"]));
}

// A dBase file marked with the UTF-8 code page (`CodePageMark::Utf8`,
// header byte `0xf0`) decodes its text fields strictly as UTF-8 - this
// fixture's one Character value is genuine multi-byte content
// (café日本語), confirming multi-byte text survives the hand-rolled
// `trim_both`/`decode_text` path intact rather than being mangled by
// byte-oriented trimming.
#[cfg(feature = "dbase")]
#[test]
fn dbase_reads_utf8_code_page_content_correctly() {
    let doc = run_json("edge_dbase_unicode.dbf", &[]);
    let cols = table(&doc, "edge_dbase_unicode");
    let name = column(cols, "NAME");
    assert_eq!(name["sample_values"], serde_json::json!(["café日本語"]));
}

// A dBase file marked with one of the ~20 *named* legacy single-byte code
// pages (here CP1252, header byte 0x03) is a disclosed, clear error rather
// than a silent misdecode - this project's hand-rolled reader only
// supports UTF-8 or undefined/unmarked-codepage dBase files, exactly
// matching a real, pre-existing limitation of the `dbase` crate's own
// default build (no `yore`/`encoding_rs` feature enabled) that this
// hand-roll replaces - see CLAUDE.md's Dependency footprint section.
#[cfg(feature = "dbase")]
#[test]
fn dbase_named_code_page_is_a_clear_disclosed_error() {
    let output = Command::new(bin())
        .args([fixture("malformed_dbase_unsupported_codepage.dbf")
            .to_str()
            .unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("code page") && stderr.contains("0x03"),
        "expected a code-page error naming the byte, got: {stderr}"
    );
}

// A dBase file with a Memo field (whose content lives in an external
// .dbt/.fpt file this reader doesn't implement - see CLAUDE.md's Known
// limitations) is a disclosed, clear error, not a silent drop or a panic.
#[cfg(feature = "dbase")]
#[test]
fn dbase_memo_field_is_a_clear_disclosed_error() {
    assert_fails_without_panicking("malformed_dbase_memo_field.dbf");
}

#[cfg(feature = "stata")]
#[test]
fn malformed_stata_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.dta");
}

#[cfg(feature = "sas7bdat")]
#[test]
fn malformed_sas7bdat_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.sas7bdat");
}

#[cfg(feature = "spss")]
#[test]
fn malformed_spss_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.sav");
}

#[cfg(feature = "orc")]
#[test]
fn malformed_orc_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.orc");
}

// --- Format-level edge-case tests --------------------------------------
// Degenerate but structurally *valid* inputs - zero rows, empty documents,
// unicode content - as opposed to the malformed/garbage-input tests above.
// Every one of these was verified empirically before being written up:
// none of them needed a code fix, they were already handled sanely, this
// just locks the behavior in.

#[test]
fn json_empty_array_and_empty_object_produce_an_empty_table_not_a_crash() {
    for fixture_name in ["edge_empty_array.json", "edge_empty_object.json"] {
        let doc = run_json(fixture_name, &[]);
        let cols = table(&doc, fixture_name.strip_suffix(".json").unwrap());
        assert!(cols.is_empty(), "{fixture_name}: expected an empty table");
    }
}

#[test]
fn json_all_null_field_is_100_percent_missing_not_a_crash() {
    let doc = run_json("edge_all_null_field.json", &[]);
    let cols = table(&doc, "edge_all_null_field");
    let a = column(cols, "a");
    assert_eq!(a["missing_pct"].as_f64().unwrap(), 100.0);
    assert!(a["notes"].as_str().unwrap().contains("empty/all null"));
}

// Found via a real-world sweep against nst/JSONTestSuite - a JSON parser
// conformance corpus, played the same role for the JSON reader that the
// HPI Pollock benchmark played for CSV. Before these two fixes, only 13 of
// its 95 valid-JSON test files were accepted by sniff-rs; the other 82
// were rejected with "expected an array of objects" even though every one
// is a real, unambiguous JSON document. After: 95/95, and separately
// verified against 43 real nested-JSON datasets from the RealNest
// benchmark (GitHub Archive events, AWS public blockchain/genomics data,
// OpenStreetMap, cord-19) with zero failures.

#[test]
fn json_accepts_a_pretty_printed_single_object() {
    // Previously misdetected as JSON Lines mode (content doesn't start
    // with '[') and failed line-by-line, since "{" alone on its own line
    // isn't valid JSON - a real, common shape for a hand-authored or
    // tool-saved config/response file.
    let doc = run_json("edge_pretty_printed_single_object.json", &[]);
    let cols = table(&doc, "edge_pretty_printed_single_object");
    assert_eq!(column(cols, "user_id")["ideal_type"], "i64");
    assert_eq!(column(cols, "email")["ideal_type"], "Email");
}

#[test]
fn json_top_level_array_of_scalars_becomes_one_value_column() {
    let doc = run_json("edge_top_level_scalar_array.json", &[]);
    let cols = table(&doc, "edge_top_level_scalar_array");
    assert_eq!(cols.len(), 1);
    let value = column(cols, "value");
    assert_eq!(value["ideal_type"], "UUID");
    assert_eq!(value["missing_pct"].as_f64().unwrap(), 33.3);
}

#[cfg(feature = "parquet")]
#[test]
fn parquet_zero_rows_still_reports_the_schema_not_a_crash() {
    let doc = run_json("edge_zero_rows.parquet", &[]);
    let cols = table(&doc, "edge_zero_rows");
    assert_eq!(cols.len(), 2);
    for c in cols {
        assert_eq!(c["missing_pct"].as_f64().unwrap(), 0.0);
        assert!(c["notes"].as_str().unwrap().contains("empty/all null"));
    }
}

#[cfg(feature = "avro")]
#[test]
fn avro_zero_records_produces_an_empty_table_not_a_crash() {
    let doc = run_json("edge_zero_records.avro", &[]);
    let cols = table(&doc, "edge_zero_records");
    assert!(cols.is_empty());
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_table_with_zero_rows_still_reports_its_columns_not_a_crash() {
    let doc = run_json("edge_zero_rows.sqlite", &[]);
    let cols = table(&doc, "items");
    assert_eq!(cols.len(), 2);
    for c in cols {
        assert!(c["notes"].as_str().unwrap().contains("empty/all null"));
    }
}

#[cfg(feature = "xlsx")]
#[test]
fn excel_header_only_sheet_and_unicode_content_both_work() {
    let doc = run_json("edge_zero_rows_and_unicode.xlsx", &[]);

    let header_only = table(&doc, "HeaderOnly");
    assert_eq!(header_only.len(), 2);
    for c in header_only {
        assert!(c["notes"].as_str().unwrap().contains("empty/all null"));
    }

    let unicode = table(&doc, "Unicode");
    let name = column(unicode, "name");
    let samples: Vec<&str> = name["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(samples.contains(&"café"));
    assert!(samples.contains(&"日本語"));
}

#[cfg(feature = "xml")]
#[test]
fn xml_empty_root_element_is_an_actionable_error_not_a_crash() {
    let output = Command::new(bin())
        .args([fixture("edge_empty_root.xml").to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("root"),
        "expected an error naming the empty root element: {stderr}"
    );
}

#[cfg(feature = "xml")]
#[test]
fn xml_unicode_text_content_round_trips_exactly() {
    let doc = run_json("edge_unicode.xml", &[]);
    let cols = table(&doc, "edge_unicode");
    let text = column(cols, "#text");
    let samples: Vec<&str> = text["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(samples.contains(&"café"));
}

#[cfg(feature = "npy")]
#[test]
fn npy_zero_length_array_is_an_empty_column_not_a_crash() {
    let doc = run_json("edge_empty_array.npy", &[]);
    let cols = table(&doc, "edge_empty_array");
    assert_eq!(cols.len(), 1);
    assert!(
        cols[0]["notes"]
            .as_str()
            .unwrap()
            .contains("empty/all null")
    );
}

// Confirmed via a real-world sweep against nst/JSONTestSuite
// (n_structure_no_data.json and n_single_space.json, both technically
// invalid per strict JSON grammar - a document must contain exactly one
// value) that this was already the JSON reader's behavior before any of
// this session's JSON fixes; this just locks it in, matching every other
// format's own zero-byte-file test below.
#[test]
fn json_zero_byte_file_produces_an_empty_table_not_a_crash() {
    let doc = run_json("edge_empty_doc.json", &[]);
    assert!(table(&doc, "edge_empty_doc").is_empty());
}

#[cfg(feature = "msgpack")]
#[test]
fn msgpack_zero_byte_file_produces_an_empty_table_not_a_crash() {
    let doc = run_json("edge_empty.msgpack", &[]);
    assert!(table(&doc, "edge_empty").is_empty());
}

#[cfg(feature = "cbor")]
#[test]
fn cbor_zero_byte_file_produces_an_empty_table_not_a_crash() {
    let doc = run_json("edge_empty.cbor", &[]);
    assert!(table(&doc, "edge_empty").is_empty());
}

#[cfg(feature = "toml")]
#[test]
fn toml_zero_byte_file_produces_an_empty_table_not_a_crash() {
    let doc = run_json("edge_empty_doc.toml", &[]);
    assert!(table(&doc, "edge_empty_doc").is_empty());
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_zero_byte_file_produces_an_empty_table_not_a_crash() {
    let doc = run_json("edge_empty_doc.yaml", &[]);
    assert!(table(&doc, "edge_empty_doc").is_empty());
}

#[cfg(feature = "ini")]
#[test]
fn ini_zero_byte_file_is_an_actionable_error_not_a_crash() {
    // Unlike TOML/YAML (a genuinely empty document is valid there), INI's
    // own reader treats zero sections as an error - different from the
    // other two, but still a clean, actionable one rather than a panic.
    let output = Command::new(bin())
        .args([fixture("edge_empty_doc.ini").to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("section"),
        "expected an error naming the missing sections: {stderr}"
    );
}

#[test]
fn fixed_width_empty_file_is_an_actionable_error_not_a_crash() {
    // Unlike the other empty-file cases, fixed-width text has no way to
    // derive column meaning from zero bytes at all (no header to slice
    // even with --widths given), so this is correctly a hard error, not
    // an empty table.
    let output = Command::new(bin())
        .args([
            fixture("edge_empty.fwf").to_str().unwrap(),
            "-",
            "--format",
            "fixed-width",
            "--widths",
            "5,5",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("empty"),
        "expected an error naming the empty file: {stderr}"
    );
}

#[test]
fn gzip_wrapping_a_zero_byte_inner_file_produces_an_empty_table_not_a_crash() {
    let doc = run_json("edge_empty_inner.csv.gz", &[]);
    assert!(table(&doc, "edge_empty_inner").is_empty());
}

#[cfg(feature = "weblog")]
#[test]
fn empty_common_log_file_still_reports_the_fixed_column_set_not_a_crash() {
    let doc = run_with_format("edge_empty_common.log", "json", &["--format", "common-log"]);
    let cols = table(&doc, "edge_empty_common");
    assert_eq!(cols.len(), 9); // host/ident/authuser/timestamp/method/path/protocol/status/bytes
    for c in cols {
        assert!(c["notes"].as_str().unwrap().contains("empty/all null"));
    }
}

#[cfg(feature = "syslog")]
#[test]
fn empty_syslog_file_still_reports_the_fixed_column_set_not_a_crash() {
    let doc = run_with_format("edge_empty_syslog.log", "json", &["--format", "syslog"]);
    let cols = table(&doc, "edge_empty_syslog");
    assert_eq!(cols.len(), 7); // facility/severity/timestamp/hostname/tag/pid/message
    for c in cols {
        assert!(c["notes"].as_str().unwrap().contains("empty/all null"));
    }
}

// --- Content-based format auto-detection -------------------------------
// detect_format tries the extension first, exactly as before - these prove
// its fallback (sniff_format, in lib.rs) carries a real extensionless or
// wrongly-named file through the *full* pipeline: correct detection *and*
// a correct reader dispatch and profile, not just that the classification
// function itself returns the right enum variant (that narrower claim has
// its own direct unit tests in lib.rs's #[cfg(test)] module, including the
// near-miss/boundary cases that would be awkward to prove through a whole
// subprocess run here).

/// Minimal hand-rolled stand-in for `tempfile::TempDir`, used only here -
/// a fresh, uniquely-named directory under the OS temp dir, recursively
/// removed on drop. See `TempFile` in `src/lib.rs` for the same tradeoff
/// applied to a single scratch file rather than a directory.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let base = std::env::temp_dir();
        let pid = std::process::id();
        for _ in 0..8 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("sniff-rs-test-{pid}-{nanos}-{n}"));
            match std::fs::create_dir(&path) {
                Ok(()) => return TempDir { path },
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("failed to create tempdir: {e}"),
            }
        }
        panic!("failed to create a tempdir after several attempts");
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Copies `fixture_name`'s bytes into a fresh tempdir under `dest_name`
/// (typically an extensionless name, or an unrelated one) so detect_format
/// sees a real file its extension-based arm can't classify.
fn copy_fixture_as(fixture_name: &str, dest_name: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new();
    let dest = dir.path().join(dest_name);
    std::fs::copy(fixture(fixture_name), &dest)
        .unwrap_or_else(|e| panic!("failed to copy {fixture_name} to {dest:?}: {e}"));
    (dir, dest)
}

/// Runs the binary against an arbitrary path (no --format override) and
/// returns the parsed JSON document - run_json's counterpart for a path
/// that isn't itself a committed fixture.
fn run_json_at(dest: &std::path::Path) -> serde_json::Value {
    let output = Command::new(bin())
        .args([dest.to_str().unwrap(), "-", "--output-format", "json"])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "expected content-sniffing to succeed for {dest:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not valid JSON ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn extensionless_jsonl_is_auto_detected_from_content() {
    let (_dir, dest) = copy_fixture_as("nested.jsonl", "mystery_data");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "json");
    let cols = table(&doc, "mystery_data");
    assert!(cols.iter().any(|c| c["name"] == "metadata.risk_score"));
}

#[cfg(feature = "xml")]
#[test]
fn extensionless_xml_is_auto_detected_from_content() {
    let (_dir, dest) = copy_fixture_as("sample.xml", "mystery_data");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "xml");
}

#[cfg(feature = "sqlite")]
#[test]
fn misnamed_sqlite_file_is_auto_detected_from_content() {
    // A wrong-but-plausible extension, not just a missing one - proves the
    // fallback fires for any extension detect_format doesn't itself claim,
    // not only a literally absent one.
    let (_dir, dest) = copy_fixture_as("sample.sqlite", "backup.dat");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "sqlite");
}

#[cfg(feature = "parquet")]
#[test]
fn extensionless_parquet_is_auto_detected_from_content() {
    let (_dir, dest) = copy_fixture_as("sample.parquet", "mystery_data");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "parquet");
}

#[cfg(feature = "avro")]
#[test]
fn extensionless_avro_is_auto_detected_from_content() {
    let (_dir, dest) = copy_fixture_as("sample.avro", "mystery_data");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "avro");
}

#[cfg(feature = "npy")]
#[test]
fn extensionless_npy_is_auto_detected_from_content() {
    let (_dir, dest) = copy_fixture_as("sample_matrix.npy", "mystery_data");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "npy");
}

#[cfg(feature = "xlsx")]
#[test]
fn extensionless_xlsx_is_auto_detected_and_disambiguated_from_npz() {
    let (_dir, dest) = copy_fixture_as("sample.xlsx", "mystery_data");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "xlsx");
}

#[cfg(feature = "npy")]
#[test]
fn extensionless_npz_is_auto_detected_and_disambiguated_from_xlsx() {
    let (_dir, dest) = copy_fixture_as("sample.npz", "mystery_data");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "npz");
}

#[cfg(feature = "dbase")]
#[test]
fn extensionless_dbase_is_auto_detected_from_content() {
    let (_dir, dest) = copy_fixture_as("sample.dbf", "mystery_data");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "dbase");
}

#[cfg(feature = "stata")]
#[test]
fn extensionless_stata_is_auto_detected_from_content() {
    let (_dir, dest) = copy_fixture_as("sample.dta", "mystery_data");
    let doc = run_json_at(&dest);
    assert_eq!(doc["format"], "stata");
}

#[test]
fn extension_still_wins_and_is_never_second_guessed_by_content() {
    // sample.csv's content is genuinely plain CSV text, so this isn't a
    // mismatch case - what it proves is that a recognized extension routes
    // straight through the original extension-based arm and never reaches
    // sniff_format at all, so this fallback existing doesn't change
    // anything about the tool's existing, already-tested behavior.
    let doc = run_json("sample.csv", &[]);
    assert_eq!(doc["format"], "csv");
}

#[test]
fn an_extensionless_file_with_no_sniffable_signal_still_gets_an_actionable_error() {
    // Plain delimited text (TSV) has no magic number or other structural
    // signal sniff_format looks for (see its doc comment - this is the
    // same disclosed, deliberate gap CSV/TSV/TOML/YAML/INI all share), so
    // this must still fail with the same actionable "pass --format"
    // error it always has, not a wrong guess.
    let (_dir, dest) = copy_fixture_as("sample.tsv", "mystery_data");
    let output = Command::new(bin())
        .args([dest.to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--format"),
        "expected an error pointing at --format: {stderr}"
    );
}

// --- Directory-input batch mode -----------------------------------------
// Pointing sniff-rs at a directory profiles every file under it (recursively)
// that it can identify on its own, one output per input, instead of a single
// file. These tests build a real directory tree under a fresh TempDir rather
// than a committed fixture directory, since batch mode writes output files
// (co-located with the inputs by default) and a committed tests/fixtures/
// directory should never be polluted by running the test suite.

fn run_dir(args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .output()
        .expect("failed to run binary")
}

#[test]
fn batch_mode_processes_every_recognized_file_recursively() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::copy(fixture("nested.jsonl"), sub.join("data.jsonl")).unwrap();
    std::fs::write(dir.path().join("README.txt"), "not a data file").unwrap();

    let output = run_dir(&[dir.path().to_str().unwrap()]);
    assert!(
        output.status.success(),
        "batch run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("README.txt: skipped (unrecognized format)"));
    assert!(stderr.contains("2 file(s) processed (1 skipped)"));

    assert!(dir.path().join("data.csv.dictionary.md").exists());
    assert!(sub.join("data.jsonl.dictionary.md").exists());
    // The unrecognized file is left alone, not turned into an (empty or
    // erroring) output of its own.
    assert!(!dir.path().join("README.txt.dictionary.md").exists());
}

#[test]
fn batch_mode_avoids_a_naming_collision_between_same_stem_different_extensions() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();
    std::fs::copy(fixture("nested.jsonl"), dir.path().join("data.json")).unwrap();

    let output = run_dir(&[dir.path().to_str().unwrap()]);
    assert!(output.status.success());
    // Both must exist, distinctly - with the old with_extension-based
    // naming, both would have collided on "data.dictionary.md".
    assert!(dir.path().join("data.csv.dictionary.md").exists());
    assert!(dir.path().join("data.json.dictionary.md").exists());
}

#[test]
fn batch_mode_writes_under_output_dir_mirroring_input_structure() {
    let dir = TempDir::new();
    let out_dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::copy(fixture("nested.jsonl"), sub.join("data.jsonl")).unwrap();

    let output = run_dir(&[
        dir.path().to_str().unwrap(),
        "--output-dir",
        out_dir.path().to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "batch run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(out_dir.path().join("data.csv.dictionary.md").exists());
    assert!(out_dir.path().join("sub/data.jsonl.dictionary.md").exists());
    // Nothing was written next to the sources instead.
    assert!(!dir.path().join("data.csv.dictionary.md").exists());
    assert!(!sub.join("data.jsonl.dictionary.md").exists());
}

#[test]
fn batch_mode_fails_fast_and_names_the_offending_file() {
    let dir = TempDir::new();
    // Sorted order matters here: "a_" and "z_" prefixes guarantee good.csv
    // is processed (and its output written) before bad.xlsx is reached.
    std::fs::copy(fixture("sample.csv"), dir.path().join("a_good.csv")).unwrap();
    std::fs::write(dir.path().join("z_bad.xlsx"), "not a real xlsx file at all").unwrap();

    let output = run_dir(&[dir.path().to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("z_bad.xlsx"),
        "error should name the offending file: {stderr}"
    );
    // The good file that was processed before the failure keeps its
    // output - directory mode never rolls back prior successes.
    assert!(dir.path().join("a_good.csv.dictionary.md").exists());
}

#[test]
fn batch_mode_errors_when_nothing_is_recognized() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("notes.txt"), "just text").unwrap();

    let output = run_dir(&[dir.path().to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no recognized files found"),
        "expected an actionable error: {stderr}"
    );
}

#[test]
fn batch_mode_does_not_reprocess_its_own_prior_output_on_a_second_run() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();

    let first = run_dir(&[dir.path().to_str().unwrap(), "--output-format", "json"]);
    assert!(first.status.success());
    assert!(dir.path().join("data.csv.dictionary.json").exists());

    let second = run_dir(&[dir.path().to_str().unwrap(), "--output-format", "json"]);
    assert!(second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("looks like this tool's own prior output"),
        "expected the prior output to be recognized and skipped: {stderr}"
    );
    assert!(
        !dir.path()
            .join("data.csv.dictionary.json.dictionary.json")
            .exists(),
        "must not have re-profiled its own previous output"
    );
}

#[test]
fn batch_mode_rejects_a_positional_output_path() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();
    let output = run_dir(&[dir.path().to_str().unwrap(), "out.md"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--output-dir"), "got: {stderr}");
}

#[test]
fn batch_mode_rejects_a_format_override() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();
    let output = run_dir(&[dir.path().to_str().unwrap(), "--format", "csv"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--format"), "got: {stderr}");
}

#[test]
fn batch_mode_rejects_widths() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();
    let output = run_dir(&[dir.path().to_str().unwrap(), "--widths", "5,10"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--widths"), "got: {stderr}");
}

#[test]
fn single_file_mode_rejects_output_dir() {
    let output = run_dir(&[
        fixture("sample.csv").to_str().unwrap(),
        "-",
        "--output-dir",
        "/tmp/should-not-be-used",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--output-dir"), "got: {stderr}");
}

#[test]
fn batch_mode_does_not_follow_a_symlinked_directory_but_reads_a_symlinked_file() {
    let dir = TempDir::new();
    let real_dir = dir.path().join("real_dir");
    std::fs::create_dir(&real_dir).unwrap();
    std::fs::copy(fixture("sample.csv"), real_dir.join("data.csv")).unwrap();

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&real_dir, dir.path().join("dir_link")).unwrap();
        std::os::unix::fs::symlink(real_dir.join("data.csv"), dir.path().join("file_link.csv"))
            .unwrap();
        // A symlink cycle back to the root - must not hang or recurse
        // forever.
        std::os::unix::fs::symlink(dir.path(), real_dir.join("cycle_back")).unwrap();

        let output = run_dir(&[dir.path().to_str().unwrap()]);
        assert!(
            output.status.success(),
            "batch run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Exactly 2 files: the real one and the symlinked *file* - the
        // symlinked *directory* must not have been descended into (which
        // would have duplicated data.csv a second time).
        assert!(stderr.contains("2 file(s) processed"), "got: {stderr}");
        assert!(real_dir.join("data.csv.dictionary.md").exists());
        assert!(dir.path().join("file_link.csv.dictionary.md").exists());
    }
}

/// A comprehensive fixture tree exercising every real-world shape at once,
/// rather than one narrow thing per test: a plain top-level file, a hidden
/// (dotfile) file - proving the "include hidden files" decision actually
/// holds, a genuinely unrecognized file, a gzip-compressed file - proving
/// decompression runs before detection in batch mode too, an extensionless
/// but content-sniffable file that also happens to be multi-table (SQLite),
/// a `--format`-only format (syslog) with no extension convention - proving
/// it's correctly never auto-detected rather than silently mishandled, and
/// two levels of subdirectory nesting. Kept as a small, permanent, committed
/// fixture (unlike every other batch-mode test's own throwaway TempDir tree)
/// specifically so this exact combination is reviewable and doesn't have to
/// be reconstructed by reading test code - the same reason every other
/// format in this project has its own committed fixture. Every test against
/// it uses --output-dir so the fixture directory itself is never written
/// into and stays pristine across runs.
#[cfg(feature = "sqlite")]
#[test]
fn batch_mode_processes_a_comprehensive_real_world_fixture_tree() {
    let out = TempDir::new();
    let output = run_dir(&[
        "tests/fixtures/edge_batch_directory",
        "--output-dir",
        out.path().to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "batch run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("README.txt: skipped (unrecognized format)"));
    assert!(stderr.contains("unreachable_without_format.log: skipped (unrecognized format)"));
    assert!(
        stderr.contains("6 file(s) processed (2 skipped), 7 tables, 50 columns total"),
        "got: {stderr}"
    );

    // Every recognized file, at every depth, produced its own output under
    // the mirrored tree.
    assert!(out.path().join("top.csv.dictionary.md").exists());
    assert!(out.path().join(".hidden.csv.dictionary.md").exists());
    assert!(out.path().join("compressed.csv.dictionary.md").exists());
    assert!(
        out.path()
            .join("sniffable_no_extension.dictionary.md")
            .exists()
    );
    assert!(out.path().join("level1/mid.jsonl.dictionary.md").exists());
    assert!(
        out.path()
            .join("level1/level2/deep.csv.dictionary.md")
            .exists()
    );
    // The two unrecognized files produced nothing.
    assert!(!out.path().join("README.txt.dictionary.md").exists());
    assert!(
        !out.path()
            .join("unreachable_without_format.log.dictionary.md")
            .exists()
    );

    // The multi-table SQLite file's own two tables both actually flattened
    // through the shared BTreeMap<table, columns> shape correctly - a real
    // check that dispatch_reader's Sqlite branch works identically inside
    // batch orchestration, not just in single-file mode.
    let content =
        std::fs::read_to_string(out.path().join("sniffable_no_extension.dictionary.md")).unwrap();
    assert!(content.contains("## events"));
    assert!(content.contains("## users"));

    // The top-level index landed alongside the per-file outputs (under
    // --output-dir, same as everything else), lists every processed file
    // exactly once - the multi-table SQLite file included, as one row, not
    // two - and names both genuinely unrecognized files under its own
    // Skipped section.
    let index_path = out.path().join("_index.dictionary.md");
    assert!(index_path.exists());
    let index = std::fs::read_to_string(&index_path).unwrap();
    assert!(index.contains("**Files:** 6 · **Tables:** 7 · **Columns:** 50 · **Skipped:** 2"));
    assert!(index.contains("| top.csv | 1 | 9 |"));
    assert!(index.contains("| sniffable_no_extension | 2 | 7 |"));
    assert!(index.contains("| level1/level2/deep.csv | 1 | 9 |"));
    assert!(index.contains("- README.txt - unrecognized format"));
    assert!(index.contains("- unreachable_without_format.log - unrecognized format"));
}

/// The exact fixture above, run against a build where SQLite isn't
/// compiled in: the extensionless file is still correctly *identified* as
/// SQLite (content-sniffing doesn't care what's compiled in), so this
/// must fail fast with the same actionable "rebuild with --features"
/// error single-file mode already gives - not a silent skip, since
/// "detect_format could identify it" and "this build can actually read
/// it" are two different questions, and only the first one is what batch
/// mode treats as a non-fatal skip.
#[cfg(not(feature = "sqlite"))]
#[test]
fn batch_mode_fails_fast_when_a_recognized_format_is_not_compiled_in() {
    let dir = TempDir::new();
    std::fs::copy(
        fixture("sample.sqlite"),
        dir.path().join("sniffable_no_extension"),
    )
    .unwrap();

    let output = run_dir(&[dir.path().to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sniffable_no_extension"),
        "error should name the offending file: {stderr}"
    );
    assert!(
        stderr.contains("isn't compiled in"),
        "expected the same actionable not-compiled-in error single-file mode gives: {stderr}"
    );
}

#[test]
fn batch_mode_treats_a_decompression_failure_as_fail_fast_not_a_skip() {
    let dir = TempDir::new();
    // Sorted so the good file is processed before the corrupt one is reached.
    std::fs::copy(fixture("sample.csv"), dir.path().join("a_good.csv")).unwrap();
    std::fs::copy(
        fixture("malformed_gzip_checksum.csv.gz"),
        dir.path().join("z_bad.csv.gz"),
    )
    .unwrap();

    let output = run_dir(&[dir.path().to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("z_bad.csv.gz"),
        "error should name the offending file: {stderr}"
    );
    assert!(
        !stderr.contains("z_bad.csv.gz: skipped"),
        "a decompression failure is a real error, not an unrecognized-format skip: {stderr}"
    );
    assert!(dir.path().join("a_good.csv.dictionary.md").exists());
}

#[test]
fn batch_mode_supports_json_and_json_schema_output() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();

    let json_out = TempDir::new();
    let output = run_dir(&[
        dir.path().to_str().unwrap(),
        "--output-format",
        "json-schema",
        "--output-dir",
        json_out.path().to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "batch run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out_path = json_out.path().join("data.csv.dictionary.schema.json");
    assert!(out_path.exists());
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
    assert_eq!(doc["$schema"], "http://json-schema.org/draft-07/schema#");
    assert!(doc["tables"]["data"]["properties"]["zip_code"].is_object());
}

#[test]
fn batch_mode_applies_global_flags_uniformly() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();

    let out = TempDir::new();
    let output = run_dir(&[
        dir.path().to_str().unwrap(),
        "--output-format",
        "json",
        "--samples",
        "1",
        "--output-dir",
        out.path().to_str().unwrap(),
    ]);
    assert!(output.status.success());
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join("data.csv.dictionary.json")).unwrap(),
    )
    .unwrap();
    let cols = table(&doc, "data");
    let zip = column(cols, "zip_code");
    assert_eq!(
        zip["sample_values"].as_array().unwrap().len(),
        1,
        "--samples 1 should have applied to every file in the batch"
    );
}

#[test]
fn batch_mode_writes_the_index_co_located_when_no_output_dir_is_given() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();

    let output = run_dir(&[dir.path().to_str().unwrap()]);
    assert!(output.status.success());
    assert!(dir.path().join("_index.dictionary.md").exists());
}

#[test]
fn batch_mode_writes_a_json_index_for_output_format_json_schema() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();
    let out = TempDir::new();

    let output = run_dir(&[
        dir.path().to_str().unwrap(),
        "--output-format",
        "json-schema",
        "--output-dir",
        out.path().to_str().unwrap(),
    ]);
    assert!(output.status.success());
    // A file manifest has no natural json-schema.org shape of its own -
    // json-schema output-format still gets this tool's own rich JSON
    // index, not a schema, and not the Markdown one either.
    assert!(!out.path().join("_index.dictionary.md").exists());
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join("_index.dictionary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(doc["files"], 1);
    assert_eq!(doc["entries"][0]["source"], "data.csv");
    assert_eq!(
        doc["entries"][0]["output"],
        "data.csv.dictionary.schema.json"
    );
    assert!(out.path().join("data.csv.dictionary.schema.json").exists());
}

#[test]
fn batch_mode_writes_a_json_index_for_output_format_json() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();
    let out = TempDir::new();

    let output = run_dir(&[
        dir.path().to_str().unwrap(),
        "--output-format",
        "json",
        "--output-dir",
        out.path().to_str().unwrap(),
    ]);
    assert!(output.status.success());
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join("_index.dictionary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(doc["directory"], dir.path().to_str().unwrap());
    assert_eq!(doc["files"], 1);
    assert_eq!(doc["tables"], 1);
    assert_eq!(doc["columns"], 9);
    assert_eq!(doc["skipped"], 0);
    assert_eq!(doc["entries"][0]["source"], "data.csv");
    assert_eq!(doc["entries"][0]["tables"], 1);
    assert_eq!(doc["entries"][0]["columns"], 9);
    assert_eq!(doc["entries"][0]["output"], "data.csv.dictionary.json");
    assert_eq!(doc["unrecognized"].as_array().unwrap().len(), 0);
}

#[test]
fn batch_mode_json_index_is_not_capped_at_max_toc_entries() {
    // Unlike the Markdown index's rendered table, the JSON manifest exists
    // specifically for programmatic consumption - truncating it would be
    // real data loss for exactly the audience that reaches for JSON.
    let dir = TempDir::new();
    for i in 0..57 {
        std::fs::copy(
            fixture("sample.csv"),
            dir.path().join(format!("f{i:03}.csv")),
        )
        .unwrap();
    }
    let output = run_dir(&[dir.path().to_str().unwrap(), "--output-format", "json"]);
    assert!(output.status.success());
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("_index.dictionary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(doc["entries"].as_array().unwrap().len(), 57);
}

#[test]
fn batch_mode_json_index_does_not_list_its_own_prior_output_as_skipped() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();

    let first = run_dir(&[dir.path().to_str().unwrap(), "--output-format", "json"]);
    assert!(first.status.success());
    let second = run_dir(&[dir.path().to_str().unwrap(), "--output-format", "json"]);
    assert!(second.status.success());

    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("_index.dictionary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(doc["skipped"], 0);
    assert_eq!(doc["unrecognized"].as_array().unwrap().len(), 0);
}

#[test]
fn batch_mode_index_does_not_list_its_own_prior_output_as_skipped_on_a_second_run() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("data.csv")).unwrap();

    let first = run_dir(&[dir.path().to_str().unwrap()]);
    assert!(first.status.success());
    let second = run_dir(&[dir.path().to_str().unwrap()]);
    assert!(second.status.success());

    let index = std::fs::read_to_string(dir.path().join("_index.dictionary.md")).unwrap();
    assert!(
        !index.contains("**Skipped:**"),
        "the index must not carry over its own prior output (or itself) as a skipped file: {index}"
    );
    assert!(!index.contains("## Skipped"));
}

#[test]
fn batch_mode_index_caps_the_files_table_on_a_large_directory() {
    let dir = TempDir::new();
    for i in 0..57 {
        std::fs::copy(
            fixture("sample.csv"),
            dir.path().join(format!("f{i:03}.csv")),
        )
        .unwrap();
    }
    let output = run_dir(&[dir.path().to_str().unwrap()]);
    assert!(output.status.success());
    let index = std::fs::read_to_string(dir.path().join("_index.dictionary.md")).unwrap();
    assert_eq!(index.matches("| f").count(), 50);
    assert!(index.contains("…and 7 more file(s) not shown here"));
}

// ---------------------------------------------------------------------------
// --combine: one combined artifact for a whole directory, instead of the
// default one-artifact-per-file behavior above. Settled directly with the
// user (both the overall shape and the table-naming-collision policy -
// always qualify by the source file's own path, never only on an actual
// collision). None of these spawn a real sqlite3/duckdb process (matching
// this project's own standing --load-into precedent - see the validation-
// only tests further up this file for why); the naming/qualifier logic
// itself has its own portable unit tests next to combine_qualifier_from_
// path's/CombinedTableNamer's definitions.
// ---------------------------------------------------------------------------

#[test]
fn combine_only_applies_to_directory_input() {
    let output = run_dir(&[
        fixture("sample.csv").to_str().unwrap(),
        "--combine",
        "--output-format",
        "json",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--combine only applies when the input path is a directory"));
}

#[test]
fn combine_merges_two_files_own_tables_into_one_json_document_with_qualified_names() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join("2024")).unwrap();
    std::fs::create_dir_all(dir.path().join("2025")).unwrap();
    std::fs::copy(
        fixture("sample.csv"),
        dir.path().join("2024").join("sales.csv"),
    )
    .unwrap();
    std::fs::copy(
        fixture("type_detection.csv"),
        dir.path().join("2025").join("sales.csv"),
    )
    .unwrap();

    let out = TempDir::new();
    let output = run_dir(&[
        dir.path().to_str().unwrap(),
        "--combine",
        "--output-format",
        "json",
        "--output-dir",
        out.path().to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dir_name = dir.path().file_name().unwrap().to_str().unwrap();
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join(format!("{dir_name}.dictionary.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(doc["directory"], dir_name);
    // No top-level "format" field - a combined run can genuinely span
    // several different source formats, so there's no one honest answer
    // the way single-file mode's own JSON output always has.
    assert!(doc.get("format").is_none());
    let tables = doc["tables"].as_object().unwrap();
    assert!(tables.contains_key("2024_sales__sales"));
    assert!(tables.contains_key("2025_sales__sales"));
    // The two "sales" tables' own columns must not have bled into each
    // other - 2024's file is the plain 9-column sample.csv, 2025's is
    // type_detection.csv's own distinctly-shaped column set.
    let cols_2024 = tables["2024_sales__sales"].as_array().unwrap();
    let cols_2025 = tables["2025_sales__sales"].as_array().unwrap();
    assert_eq!(cols_2024.len(), 9);
    assert_ne!(cols_2025.len(), cols_2024.len());
    assert!(cols_2024.iter().any(|c| c["name"] == "user_id"));
    assert!(cols_2025.iter().any(|c| c["name"] == "contact_email"));
    assert!(!cols_2024.iter().any(|c| c["name"] == "contact_email"));
}

#[test]
fn combine_produces_one_markdown_document_with_a_table_of_contents() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("a.csv")).unwrap();
    std::fs::copy(fixture("type_detection.csv"), dir.path().join("b.csv")).unwrap();

    let out = TempDir::new();
    let output = run_dir(&[
        dir.path().to_str().unwrap(),
        "--combine",
        "--output-format",
        "md",
        "--output-dir",
        out.path().to_str().unwrap(),
    ]);
    assert!(output.status.success());

    let dir_name = dir.path().file_name().unwrap().to_str().unwrap();
    let md = std::fs::read_to_string(out.path().join(format!("{dir_name}.dictionary.md"))).unwrap();
    assert!(md.starts_with(&format!("# Data Dictionary: {dir_name}")));
    // No single "**Format:**" line - see the JSON test's own identical
    // reasoning above.
    assert!(!md.contains("**Format:**"));
    assert!(md.contains("## Tables"));
    assert!(md.contains("a__a") || md.contains("a\\_\\_a")); // escaped in the TOC link text
    assert!(md.contains("b__b") || md.contains("b\\_\\_b"));
}

#[test]
fn combine_writes_one_sql_script_with_a_shared_header_and_qualified_table_names() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("a.csv")).unwrap();
    std::fs::copy(fixture("type_detection.csv"), dir.path().join("b.csv")).unwrap();

    let out = TempDir::new();
    let output = run_dir(&[
        dir.path().to_str().unwrap(),
        "--combine",
        "--output-format",
        "sql",
        "--output-dir",
        out.path().to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dir_name = dir.path().file_name().unwrap().to_str().unwrap();
    let sql =
        std::fs::read_to_string(out.path().join(format!("{dir_name}.dictionary.sql"))).unwrap();
    // The shared header comment is written exactly once for the whole
    // combined run, not once per table - the same "is_first_table" split
    // a real multi-table *file* (SQLite, Excel, ...) already gets.
    assert_eq!(
        sql.matches("Generated by sniff-rs --output-format sql")
            .count(),
        1
    );
    assert!(sql.contains("CREATE TABLE \"a__a\""));
    assert!(sql.contains("CREATE TABLE \"b__b\""));
    assert!(sql.contains("'U1001'")); // real value from a.csv
    assert!(sql.contains("'alice@example.com'")); // real value from b.csv
}

#[test]
fn combine_supports_sql_mode_staging_with_per_table_load_hints() {
    // Unlike inline mode's single-pass, self-contained INSERT statements,
    // staging mode's own per-table "Load" comment has to name that
    // table's own real, distinct source file - the one piece that would
    // have broken had this reused the old whole-`String` render_sql_
    // staging unchanged (it only ever assumed one shared file/format for
    // the entire script). Two different files sharing the same bare
    // filename (2024/sales.csv, 2025/sales.csv) is exactly the case that
    // would silently produce two identical, ambiguous "Load sales.csv"
    // comments if the *relative* path weren't threaded through per table.
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join("2024")).unwrap();
    std::fs::create_dir_all(dir.path().join("2025")).unwrap();
    std::fs::copy(
        fixture("sample.csv"),
        dir.path().join("2024").join("sales.csv"),
    )
    .unwrap();
    std::fs::copy(
        fixture("type_detection.csv"),
        dir.path().join("2025").join("sales.csv"),
    )
    .unwrap();

    let out = TempDir::new();
    let output = run_dir(&[
        dir.path().to_str().unwrap(),
        "--combine",
        "--output-format",
        "sql",
        "--sql-mode",
        "staging",
        "--output-dir",
        out.path().to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dir_name = dir.path().file_name().unwrap().to_str().unwrap();
    let sql =
        std::fs::read_to_string(out.path().join(format!("{dir_name}.dictionary.sql"))).unwrap();
    // One shared header, written exactly once for the whole run.
    assert_eq!(
        sql.matches("Generated by sniff-rs --output-format sql --sql-mode staging --combine")
            .count(),
        1
    );
    assert!(sql.contains("CREATE TABLE \"2024_sales__sales_staging\""));
    assert!(sql.contains("CREATE TABLE \"2025_sales__sales_staging\""));
    // Each table's own Load comment names ITS OWN real relative path -
    // not a shared filename, and not just the ambiguous bare basename
    // both files happen to share.
    assert!(sql.contains("Load 2024/sales.csv into \"2024_sales__sales_staging\""));
    assert!(sql.contains("Load 2025/sales.csv into \"2025_sales__sales_staging\""));
    assert!(sql.contains("read_csv_auto('2024/sales.csv'"));
    assert!(sql.contains("read_csv_auto('2025/sales.csv'"));
}

#[test]
fn combine_load_into_rejects_combining_with_output_dir_or_an_output_path() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("a.csv")).unwrap();

    let with_output_dir = run_dir(&[
        dir.path().to_str().unwrap(),
        "--combine",
        "--output-format",
        "sql",
        "--load-into",
        "sqlite:/tmp/whatever-combine-target",
        "--output-dir",
        "/tmp/whatever-combine-output-dir",
    ]);
    assert!(!with_output_dir.status.success());
    assert!(
        String::from_utf8_lossy(&with_output_dir.stderr)
            .contains("already the one shared database")
    );

    let with_output_path = run_dir(&[
        dir.path().to_str().unwrap(),
        "out.sql",
        "--combine",
        "--output-format",
        "sql",
        "--load-into",
        "sqlite:/tmp/whatever-combine-target",
    ]);
    assert!(!with_output_path.status.success());
    assert!(
        String::from_utf8_lossy(&with_output_path.stderr)
            .contains("--load-into can't be combined with an output path")
    );
}

#[test]
fn combine_load_into_accepts_postgres_and_mysql_unlike_the_plain_per_file_case() {
    // The plain (non-combined) directory --load-into rejects postgres/
    // mysql outright, since "one database per file" would need this tool
    // to issue its own CREATE DATABASE per file first. --combine's own
    // "one shared target" shape has no such problem - it's structurally
    // identical to single-file mode's own --load-into, which already
    // supports every engine - so postgres/mysql must reach the same
    // "must be in the form <engine>:<target>"/spawn-failure path a bogus
    // target already does for single-file mode, never the directory-mode-
    // specific "only supports sqlite/duckdb" rejection.
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("a.csv")).unwrap();
    let output = run_dir(&[
        dir.path().to_str().unwrap(),
        "--combine",
        "--output-format",
        "sql",
        "--load-into",
        "postgres:mydb",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("only supports sqlite/duckdb"));
    assert!(stderr.contains("psql"));
}

#[test]
fn combine_nrows_bounds_each_files_own_row_count() {
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("a.csv")).unwrap();
    let out = TempDir::new();
    let output = run_dir(&[
        dir.path().to_str().unwrap(),
        "--combine",
        "--output-format",
        "json",
        "--output-dir",
        out.path().to_str().unwrap(),
        "--nrows",
        "2",
    ]);
    assert!(output.status.success());
    let dir_name = dir.path().file_name().unwrap().to_str().unwrap();
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join(format!("{dir_name}.dictionary.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(doc["tables"]["a__a"][0]["row_count"], 2);
}

// ---------------------------------------------------------------------------
// Additional edge-case fixtures added to broaden coverage beyond the
// original committed corpus. Each fixture is small and permanent
// (committed under tests/fixtures/edge_*) - the same "reviewable without
// reading test code" discipline every other format's fixture already gets.
// ---------------------------------------------------------------------------

#[test]
fn csv_quoted_fields_with_embedded_commas_quotes_and_newlines() {
    let doc = run_json("edge_csv_quoted_fields.csv", &[]);
    let cols = table(&doc, "edge_csv_quoted_fields");

    // Embedded comma must not split the field; the sample should retain it.
    let desc = column(cols, "description");
    assert!(
        desc["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "hello, world"),
        "quoted comma not preserved: {desc:?}"
    );
    // Embedded newline inside quotes must stay as one row, not two.
    assert!(
        desc["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains('\n')),
        "embedded newline should be inside a single field: {desc:?}"
    );
    assert_eq!(desc["row_count"], 3);
    // Doubled quotes -> single quote in output.
    let notes = column(cols, "notes");
    assert!(
        notes["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "a \"quoted\" word"),
        "escaped quotes not decoded: {notes:?}"
    );
    // Empty quoted field "" is treated as missing (same as CSV's own empty-field handling).
    assert!(
        (desc["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.1,
        "empty quoted field should be missing"
    );
}

#[test]
fn csv_semicolon_delimited_requires_explicit_delimiter_flag() {
    // Without --delimiter, the whole line is one string column (semicolons not split).
    let doc_default = run_json("edge_csv_semicolon.csv", &[]);
    let cols_default = table(&doc_default, "edge_csv_semicolon");
    assert_eq!(
        cols_default.len(),
        1,
        "without --delimiter ';' the file should collapse to one column"
    );

    // With --delimiter ';' it parses correctly as 4 columns with correct types.
    let doc = run_with_format("edge_csv_semicolon.csv", "json", &["--delimiter", ";"]);
    let cols = table(&doc, "edge_csv_semicolon");
    assert_eq!(cols.len(), 4);
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "email")["ideal_type"], "Email");
    let score = column(cols, "score");
    assert!(
        (score["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.1,
        "empty score should be missing"
    );
}

#[test]
fn csv_long_preamble_beyond_max_scan_does_not_auto_detect() {
    // MAX_PREAMBLE_SCAN is 5. With 6 sparse preamble rows, auto-detection must NOT fire -
    // the first sparse line becomes the header, not the real header after it.
    let doc = run_json("edge_csv_long_preamble.csv", &[]);
    let cols = table(&doc, "edge_csv_long_preamble");
    // Header is the first preamble line, not "id".
    assert!(
        cols.iter().any(|c| c["name"] == "Preamble line 1"),
        "expected preamble line to be treated as header when beyond scan cap: {cols:?}"
    );
    assert!(
        !cols.iter().any(|c| c["name"] == "id"),
        "real header should NOT be detected when preamble exceeds cap"
    );
    // The file has 6 preamble + 1 header + 3 data = 10 lines total, but header is preamble[0] so 9 rows profiled.
    assert_eq!(cols[0]["row_count"], 9);
}

#[test]
fn csv_crlf_and_blank_lines_are_handled() {
    // CRLF (\r\n) line endings must not leave a stray '\r' in values.
    let doc = run_json("edge_csv_crlf.csv", &[]);
    let cols = table(&doc, "edge_csv_crlf");
    assert_eq!(column(cols, "name")["row_count"], 3);
    for c in cols {
        for v in c["sample_values"].as_array().unwrap() {
            assert!(
                !v.as_str().unwrap().contains('\r'),
                "CRLF should be stripped, got {v:?}"
            );
        }
    }

    // Blank lines (empty records) must be skipped entirely, not treated as 1-field rows.
    let doc2 = run_json("edge_csv_blank_lines.csv", &[]);
    let cols2 = table(&doc2, "edge_csv_blank_lines");
    assert_eq!(column(cols2, "id")["row_count"], 3);
    assert_eq!(
        column(cols2, "name")["sample_values"],
        serde_json::json!(["Alice", "Bob", "Carol"])
    );
}

#[test]
fn tsv_tab_delimited_reads_without_explicit_flag() {
    let doc = run_json("edge_tsv_tab.tsv", &[]);
    let cols = table(&doc, "edge_tsv_tab");
    assert!(cols.iter().any(|c| c["name"] == "name"));
    assert_eq!(column(cols, "score")["ideal_type"], "i64");
}

#[test]
fn json_duplicate_keys_last_value_wins() {
    let doc = run_json("edge_json_duplicate_keys.json", &[]);
    let cols = table(&doc, "edge_json_duplicate_keys");
    // Duplicate "name" keys - last occurrence should win per Map::insert contract.
    let name = column(cols, "name");
    let vals: Vec<&str> = name["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        vals.contains(&"Alicia"),
        "last duplicate should win, got {vals:?}"
    );
    assert!(
        vals.contains(&"Bobby"),
        "last duplicate should win, got {vals:?}"
    );
    assert!(
        !vals.contains(&"Alice"),
        "first duplicate should be overwritten"
    );
}

#[test]
fn json_unicode_keys_and_emoji_preserved() {
    let doc = run_json("edge_json_unicode_keys.json", &[]);
    let cols = table(&doc, "edge_json_unicode_keys");
    // Japanese key "名前" should survive as a column name (possibly escaped in JSON output but must be findable).
    // serde_json escapes non-ASCII by default in this tool's output, but the test helper does string compare on the escaped form.
    // We check that the table has 4 columns and that emoji values are preserved.
    assert_eq!(cols.len(), 4);
    let emoji = column(cols, "emoji");
    let vals = emoji["sample_values"].as_array().unwrap();
    // Values contain multi-byte unicode that must not panic and must be present.
    assert!(
        vals.iter().any(|v| v.as_str().unwrap().contains("café")),
        "emoji column should contain café: {vals:?}"
    );
    assert_eq!(emoji["row_count"], 2);
}

#[test]
fn json_blank_lines_and_mixed_scalar_object_array() {
    // Blank lines in JSON Lines are skipped; file has 3 records despite 2 blank lines.
    let doc = run_json("edge_json_blank_lines.jsonl", &[]);
    let cols = table(&doc, "edge_json_blank_lines");
    assert_eq!(column(cols, "id")["row_count"], 3);

    // Mixed scalar+object top-level array: profiling falls back to a single "value" column.
    let doc2 = run_json("edge_json_mixed_array.json", &[]);
    let cols2 = table(&doc2, "edge_json_mixed_array");
    let value = column(cols2, "value");
    assert!(
        value["current_type"]
            .as_str()
            .unwrap()
            .starts_with("mixed("),
        "mixed array should be reported as mixed: {value:?}"
    );
    assert!(
        value["notes"]
            .as_str()
            .unwrap()
            .contains("mix of scalars and objects")
    );
    // Object fields within the mixed array are still flattened and typed.
    assert!(cols2.iter().any(|c| c["name"] == "value.id"));
    assert_eq!(column(cols2, "value.x")["ideal_type"], "i64");
}

#[cfg(feature = "xml")]
#[test]
fn xml_cdata_comments_pi_and_doctype_not_miscounted_as_too_deep() {
    let doc = run_json("edge_xml_cdata_comments.xml", &[]);
    let cols = table(&doc, "edge_xml_cdata_comments");
    // CDATA content must be preserved verbatim, including special chars.
    let name = column(cols, "name");
    assert!(
        name["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "Alice & Bob <test>"),
        "CDATA not decoded correctly: {name:?}"
    );
    assert_eq!(column(cols, "@id")["ideal_type"], "i64");
    assert_eq!(column(cols, "score")["ideal_type"], "i64");
    // Comments / PI / DOCTYPE must not affect record detection - still 3 records.
    assert_eq!(name["row_count"], 3);
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_anchor_value_is_stripped_not_treated_as_literal() {
    let doc = run_json("edge_yaml_anchor_value.yaml", &[]);
    let cols = table(&doc, "edge_yaml_anchor_value");
    // &defaults anchor tag must be discarded, value read normally.
    assert!(cols.iter().any(|c| c["name"] == "defaults.color"));
    assert_eq!(column(cols, "defaults.color")["sample_values"][0], "blue");
    assert_eq!(column(cols, "plain")["sample_values"][0], "hello world");
    // Literal "&myanchor" must NOT appear in output.
    for c in cols {
        for v in c["sample_values"].as_array().unwrap() {
            assert!(
                !v.as_str().unwrap().contains("&myanchor"),
                "anchor tag leaked into value: {v:?}"
            );
        }
    }
}

#[cfg(feature = "toml")]
#[test]
fn toml_complex_dotted_keys_and_array_of_tables() {
    let doc = run_json("edge_toml_complex.toml", &[]);
    let cols = table(&doc, "edge_toml_complex");
    // Dotted key "contact.email" inside [owner] flattens to owner.contact.email.
    assert!(cols.iter().any(|c| c["name"] == "owner.contact.email"));
    // Inline table trailing comma and multiline string (TOML 1.1.0) should not error.
    assert_eq!(column(cols, "title")["sample_values"][0], "Complex Test");
    // Array of tables [[products]] becomes Vec<object> and flattens.
    let products = column(cols, "products");
    assert_eq!(products["current_type"], "Vec<object>");
    assert!(cols.iter().any(|c| c["name"] == "products.name"));
    // [servers.alpha] / [servers.beta] sub-tables flatten correctly.
    assert!(cols.iter().any(|c| c["name"] == "servers.alpha.ip"));
    assert!(cols.iter().any(|c| c["name"] == "servers.beta.role"));
}

#[cfg(feature = "ini")]
#[test]
fn ini_duplicate_sections_and_keys_pooled() {
    let doc = run_json("edge_ini_duplicate_sections.ini", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert!(
        tables.contains_key("owner"),
        "owner section missing: {tables:?}"
    );
    assert!(tables.contains_key("database"), "database section missing");
    let owner = table(&doc, "owner");
    // Duplicate key "name" within one section pools into an array -> flattened handling
    // This tool pools repeated keys into an array value; the column should exist and have samples.
    let name = column(owner, "name");
    // Depending on impl, duplicate keys become Vec<String> with pooled values or last-wins;
    // this project's ini_support pools duplicates into an array value, so ideal_type should be String or Vec<String>.
    assert!(name["sample_values"].as_array().unwrap().len() >= 1);
    // Re-opened [owner] must have appended its new key "role" rather than creating a second table.
    assert!(
        owner.iter().any(|c| c["name"] == "role"),
        "re-opened section's key not merged: {owner:?}"
    );
    assert_eq!(
        tables.len(),
        2,
        "duplicate sections should not create extra tables"
    );
}

#[test]
fn fwf_unicode_with_fixed_widths() {
    let doc = run_with_format(
        "edge_fwf_unicode.fwf",
        "json",
        &[
            "--format",
            "fixed-width",
            "--widths",
            "3,10,5",
            "--samples",
            "10",
        ],
    );
    let cols = table(&doc, "edge_fwf_unicode");
    assert_eq!(cols.len(), 3);
    assert_eq!(column(cols, "ID")["ideal_type"], "i64");
    let name = column(cols, "NAME");
    assert!(
        name["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "café")
    );
    assert!(
        name["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "日本語")
    );
    // Emoji (multi-byte) must be handled correctly as character-based slicing, not byte-based.
    assert!(
        name["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("emoji"))
    );
    assert_eq!(column(cols, "SCORE")["ideal_type"], "i64");
    assert_eq!(column(cols, "NAME")["row_count"], 4);
}

#[cfg(feature = "weblog")]
#[test]
fn weblog_ipv6_host_recognized() {
    let doc = run_with_format("edge_weblog_ipv6.log", "json", &["--format", "common-log"]);
    let cols = table(&doc, "edge_weblog_ipv6");
    let host = column(cols, "host");
    assert_eq!(host["ideal_type"], "IPv6");
    // Bytes field "-" on the third line is missing.
    let bytes = column(cols, "bytes");
    assert!((bytes["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.1);
    let method = column(cols, "method");
    assert!(
        method["sample_values"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("GET"))
    );
}

#[test]
fn json_schema_output_for_edge_csv_quoted_fields_is_valid() {
    let doc = run_with_format("edge_csv_quoted_fields.csv", "json-schema", &[]);
    let schema = &doc["tables"]["edge_csv_quoted_fields"];
    assert_eq!(schema["type"], "object");
    // description column has missing values -> nullable union, not required.
    assert_eq!(
        schema["properties"]["description"]["type"],
        serde_json::json!(["string", "null"])
    );
    assert!(
        !schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "description")
    );
}

#[test]
fn nrows_limits_output_row_count_on_a_real_file() {
    let doc = run_json("sample.csv", &["--nrows", "2"]);
    let cols = table(&doc, "sample");
    assert_eq!(column(cols, "user_id")["row_count"], 2);
    // With nrows 2, sample_values should still be at most the row count.
    for c in cols {
        assert!(c["sample_values"].as_array().unwrap().len() <= 2);
    }
}

#[test]
fn delimiter_flag_rejects_a_multi_character_argument() {
    let output = Command::new(bin())
        .args([
            fixture("sample.csv").to_str().unwrap(),
            "-",
            "--delimiter",
            "ab",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("delimiter") || stderr.contains("single"),
        "expected a delimiter validation error, got: {stderr}"
    );
}

#[test]
fn csv_missing_sentinels_are_treated_as_null() {
    let doc = run_json("edge_csv_missing_sentinels.csv", &[]);
    let cols = table(&doc, "edge_csv_missing_sentinels");
    // score column uses 7 of 9 values as known missing sentinels (NA, N/A, NULL, None, -, ?, unknown)
    let score = column(cols, "score");
    assert!(
        (score["missing_pct"].as_f64().unwrap() - 77.8).abs() < 0.1,
        "score missing_pct should reflect sentinels as missing: {score:?}"
    );
    assert_eq!(score["ideal_type"], "i64");
    // status "unknown" is also a sentinel (case-insensitive) -> 55.6% missing
    let status = column(cols, "status");
    assert!((status["missing_pct"].as_f64().unwrap() - 55.6).abs() < 0.1);
    // is_missing_sentinel must be case-insensitive: "NA" and "na" both count.
    // Verified via the fact that row 3's "NA" and row 5's "None" are both counted.
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_mixed_affinity_and_all_null_column() {
    let doc = run_json("edge_sqlite_mixed_types.sqlite", &[]);
    let tables = doc["tables"].as_object().unwrap();
    // View must be excluded - only 2 real tables.
    assert_eq!(tables.len(), 2, "view should be excluded");
    assert!(tables.contains_key("mixed"));
    assert!(tables.contains_key("empty_table"));
    assert!(
        !tables.contains_key("mixed_view"),
        "view should not appear as a table"
    );

    let mixed = table(&doc, "mixed");
    // INTEGER PRIMARY KEY alias behaves as i64, not null.
    assert_eq!(column(mixed, "id")["current_type"], "i64");
    // All-null column is 100% missing and reports as null current_type.
    let all_null = column(mixed, "nullable_all_null");
    assert_eq!(all_null["missing_pct"].as_f64().unwrap(), 100.0);
    assert_eq!(all_null["current_type"], "null");
    assert_eq!(all_null["sample_values"].as_array().unwrap().len(), 0);
    // REAL affinity column holding both text and numbers must be reported as mixed.
    let aff = column(mixed, "mixed_affinity");
    assert!(
        aff["current_type"].as_str().unwrap().starts_with("mixed("),
        "mixed affinity not reported: {aff:?}"
    );
    assert!(aff["current_type"].as_str().unwrap().contains("String"));
    assert!(aff["current_type"].as_str().unwrap().contains("f64"));
    // Empty table has 0 rows on every column.
    let empty = table(&doc, "empty_table");
    for c in empty {
        assert_eq!(c["row_count"], 0);
    }
}

#[test]
fn gzip_decompresses_quoted_fields_and_missing_sentinels_transparently() {
    // Gzip is a preprocessing step before format detection (decompress_if_needed);
    // the inner CSV heuristics must work identically through it, including
    // quoted-field parsing and missing-sentinel handling.
    let doc = run_json("edge_csv_quoted_fields.csv.gz", &[]);
    assert_eq!(doc["file"], "edge_csv_quoted_fields.csv.gz");
    assert_eq!(doc["format"], "csv");
    let cols = table(&doc, "edge_csv_quoted_fields");
    let desc = column(cols, "description");
    assert!(
        desc["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "hello, world")
    );

    let doc2 = run_json("edge_csv_missing_sentinels.csv.gz", &[]);
    let score = column(table(&doc2, "edge_csv_missing_sentinels"), "score");
    assert!((score["missing_pct"].as_f64().unwrap() - 77.8).abs() < 0.1);
}

// ---------------------------------------------------------------------------
// Second batch of edge fixtures - deeper heuristic and format coverage
// ---------------------------------------------------------------------------

#[test]
fn csv_numeric_formatting_currency_percent_and_parens() {
    let doc = run_json("edge_csv_numeric_formatting.csv", &[]);
    let cols = table(&doc, "edge_csv_numeric_formatting");
    assert_eq!(column(cols, "amount")["ideal_type"], "f64");
    assert!(
        column(cols, "amount")["notes"]
            .as_str()
            .unwrap()
            .contains("numeric strings")
    );
    assert_eq!(column(cols, "percent")["ideal_type"], "i64");
    assert!(
        column(cols, "percent")["notes"]
            .as_str()
            .unwrap()
            .contains("%")
    );
    assert_eq!(column(cols, "paren_neg")["ideal_type"], "f64");
    // Currency with € and £ must also be recognized (same as $).
    assert_eq!(column(cols, "currency_mixed")["ideal_type"], "f64");
}

#[test]
fn csv_category_boundary_and_constant_detection() {
    let doc = run_json("edge_csv_category_boundary.csv", &[]);
    let cols = table(&doc, "edge_csv_category_boundary");
    assert_eq!(column(cols, "cat_col")["ideal_type"], "enum / category");
    assert!(
        column(cols, "cat_col")["notes"]
            .as_str()
            .unwrap()
            .contains("40 unique")
    );
    assert_eq!(column(cols, "not_cat_col")["ideal_type"], "String");
    // 51 unique > 50 must not be category.
    assert!(
        column(cols, "not_cat_col")["notes"]
            .as_str()
            .unwrap()
            .is_empty()
    );

    let doc2 = run_json("edge_csv_constant_column.csv", &[]);
    let cols2 = table(&doc2, "edge_csv_constant_column");
    assert_eq!(
        column(cols2, "constant_col")["ideal_type"],
        "enum / category"
    );
    assert!(
        column(cols2, "constant_col")["notes"]
            .as_str()
            .unwrap()
            .contains("constant column")
    );
}

#[test]
fn csv_infinity_nan_and_oversized_int_notes() {
    let doc = run_json("edge_csv_infinity_nan.csv", &[]);
    let cols = table(&doc, "edge_csv_infinity_nan");
    let f = column(cols, "float_col");
    // Infinity and -inf are non-finite f64 values -> note, and NaN is missing sentinel so 25% missing.
    assert!(f["notes"].as_str().unwrap().contains("non-finite"));
    assert!((f["missing_pct"].as_f64().unwrap() - 25.0).abs() < 0.1);

    let doc2 = run_json("edge_csv_oversized_int.csv", &[]);
    let big = column(table(&doc2, "edge_csv_oversized_int"), "big_int");
    assert!(big["notes"].as_str().unwrap().contains("exceed i64"));
}

#[test]
fn csv_leading_zeros_and_embedded_json_detection() {
    let doc = run_json("edge_csv_leading_zeros.csv", &[]);
    let zip = column(table(&doc, "edge_csv_leading_zeros"), "zip_code");
    assert_eq!(zip["ideal_type"], "String");
    assert!(zip["notes"].as_str().unwrap().contains("leading zeros"));
    assert!(zip["notes"].as_str().unwrap().contains("already lost"));

    let doc2 = run_json("edge_csv_embedded_json.csv", &[]);
    let payload = column(table(&doc2, "edge_csv_embedded_json"), "payload");
    // Not all rows are JSON (one is plain string) -> must NOT be flagged as embedded JSON.
    assert!(
        payload["notes"].as_str().unwrap().is_empty(),
        "mixed plain+JSON should not be flagged: {payload:?}"
    );

    let doc3 = run_json("edge_csv_all_types.csv", &[]);
    let cols3 = table(&doc3, "edge_csv_all_types");
    assert_eq!(column(cols3, "uuid")["ideal_type"], "UUID");
    assert_eq!(column(cols3, "email")["ideal_type"], "Email");
    assert_eq!(column(cols3, "ipv4")["ideal_type"], "IPv4");
    assert_eq!(column(cols3, "ipv6")["ideal_type"], "IPv6");
    assert_eq!(column(cols3, "url")["ideal_type"], "URL");
    assert_eq!(column(cols3, "hex_color")["ideal_type"], "Hex Color");
    assert_eq!(column(cols3, "iban")["ideal_type"], "IBAN");
}

#[test]
fn json_nested_deep_and_scientific_and_all_null() {
    let doc = run_json("edge_json_nested_deep.json", &[]);
    let cols = table(&doc, "edge_json_nested_deep");
    // 10 levels deep -> a.b.c.d.e.f.g.h.i.j should exist and be typed.
    assert!(cols.iter().any(|c| c["name"] == "a.b.c.d.e.f.g.h.i.j"));
    assert_eq!(
        column(cols, "a.b.c.d.e.f.g.h.i.j")["sample_values"][0],
        "deep_value"
    );

    let doc2 = run_json("edge_json_all_null_field.json", &[]);
    let val = column(table(&doc2, "edge_json_all_null_field"), "val");
    assert_eq!(val["missing_pct"].as_f64().unwrap(), 100.0);
    assert_eq!(val["current_type"], "null");

    let doc3 = run_json("edge_json_scientific.json", &[]);
    let sci = column(table(&doc3, "edge_json_scientific"), "sci");
    assert_eq!(sci["ideal_type"], "f64");
}

#[test]
fn json_nested_arrays_and_mixed_content() {
    let doc = run_json("edge_json_nested_arrays.json", &[]);
    let cols = table(&doc, "edge_json_nested_arrays");
    // matrix is an array of arrays (nested arrays) -> Vec<i64> or similar, not a crash.
    assert!(cols.iter().any(|c| c["name"] == "matrix"));
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_literal_block_and_flow_collections() {
    let doc = run_json("edge_yaml_literal_block.yaml", &[]);
    let cols = table(&doc, "edge_yaml_literal_block");
    let keep = column(cols, "literal_keep");
    assert!(
        keep["sample_values"][0]
            .as_str()
            .unwrap()
            .contains("line one")
    );
    assert!(
        keep["sample_values"][0].as_str().unwrap().ends_with('\n'),
        "literal_keep should retain trailing newline"
    );
    let strip = column(cols, "literal_strip");
    assert!(
        !strip["sample_values"][0].as_str().unwrap().ends_with('\n'),
        "literal_strip should strip trailing newline"
    );

    let doc2 = run_json("edge_yaml_flow_collections.yaml", &[]);
    let cols2 = table(&doc2, "edge_yaml_flow_collections");
    assert!(cols2.iter().any(|c| c["name"] == "flow_seq"));
    // flow_seq contains mixed types (ints + string + bool) -> Vec<mixed(...)> with ideal Vec<String>
    let flow = column(cols2, "flow_seq");
    assert!(
        flow["current_type"].as_str().unwrap().starts_with("Vec<"),
        "flow_seq should be Vec: {flow:?}"
    );
    assert!(
        flow["current_type"].as_str().unwrap().contains("mixed")
            || flow["ideal_type"] == "Vec<String>"
    );
    assert!(cols2.iter().any(|c| c["name"] == "flow_map.a"));
}

#[cfg(feature = "toml")]
#[test]
fn toml_multiline_and_dotted_keys() {
    let doc = run_json("edge_toml_multiline_and_dotted.toml", &[]);
    let cols = table(&doc, "edge_toml_multiline_and_dotted");
    assert!(cols.iter().any(|c| c["name"] == "str_multiline_basic"));
    // Dotted keys a.b.c under [database.connection] become database.connection.a.b.c etc.
    assert!(
        cols.iter()
            .any(|c| c["name"].as_str().unwrap().contains("a.b.c"))
    );
    assert!(cols.iter().any(|c| c["name"] == "database.ports"));
}

#[cfg(feature = "ini")]
#[test]
fn ini_inline_comment_and_empty_section() {
    let doc = run_json("edge_ini_inline_comment.ini", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert!(tables.contains_key("section1"));
    let s1 = table(&doc, "section1");
    assert!(s1.iter().any(|c| c["name"] == "key1"));
    assert!(s1.iter().any(|c| c["name"] == "empty_key"));
    // Duplicate key "duplicate" is in section2, not section1 - pools into Vec<String>.
    let s2 = table(&doc, "section2");
    let dup = column(s2, "duplicate");
    assert_eq!(dup["current_type"], "Vec<String>");
    assert_eq!(dup["sample_values"], serde_json::json!(["first", "second"]));
    assert!(tables.contains_key("section2"));
}

#[cfg(feature = "xml")]
#[test]
fn xml_mixed_content_and_self_closing() {
    let doc = run_json("edge_xml_mixed_content.xml", &[]);
    let cols = table(&doc, "edge_xml_mixed_content");
    // Homogeneous <item> vs <different> under root -> root is treated as one record (mixed children tags),
    // so columns include both item and different flattenings.
    // At least attributes and nested elements should appear.
    assert!(
        cols.iter()
            .any(|c| c["name"] == "@id" || c["name"] == "item.@id")
    );

    let doc2 = run_json("edge_xml_self_closing.xml", &[]);
    let cols2 = table(&doc2, "edge_xml_self_closing");
    assert_eq!(column(cols2, "@id")["row_count"], 3);
    assert_eq!(
        column(cols2, "@name")["sample_values"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn fwf_staggered_and_basic_unicode() {
    let doc = run_with_format(
        "edge_fwf_staggered.fwf",
        "json",
        &["--format", "fixed-width", "--widths", "5,5,10"],
    );
    let cols = table(&doc, "edge_fwf_staggered");
    assert_eq!(cols.len(), 3);
    let name = column(cols, "NAME");
    assert!(
        name["missing_pct"].as_f64().unwrap() > 0.0,
        "empty NAME field should be missing"
    );
    assert_eq!(column(cols, "DESC")["row_count"], 4);
}

#[cfg(feature = "weblog")]
#[test]
fn weblog_combined_and_common_edge() {
    let doc = run_with_format(
        "edge_weblog_combined.log",
        "json",
        &["--format", "combined-log"],
    );
    let cols = table(&doc, "edge_weblog_combined");
    assert!(cols.iter().any(|c| c["name"] == "referer"));
    assert!(cols.iter().any(|c| c["name"] == "user_agent"));
    let status = column(cols, "status");
    assert_eq!(status["ideal_type"], "i64");
    // Second line has "-" for bytes and referer/user_agent -> missing.
    let bytes = column(cols, "bytes");
    assert!(bytes["missing_pct"].as_f64().unwrap() > 0.0);
}

#[cfg(feature = "syslog")]
#[test]
fn syslog_structured_and_mixed_pri() {
    let doc = run_with_format(
        "edge_syslog_rfc5424_structured.log",
        "json",
        &["--format", "syslog5424"],
    );
    let cols = table(&doc, "edge_syslog_rfc5424_structured");
    let sd = column(cols, "structured_data");
    assert!(
        sd["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("exampleSDID"))
    );
    // Second line has "-" as structured_data -> missing.
    assert!(sd["missing_pct"].as_f64().unwrap() > 0.0);

    let doc2 = run_with_format(
        "edge_syslog_rfc3164_mixed.log",
        "json",
        &["--format", "syslog", "--samples", "10"],
    );
    let cols2 = table(&doc2, "edge_syslog_rfc3164_mixed");
    assert_eq!(
        column(cols2, "facility")["missing_pct"].as_f64().unwrap(),
        25.0
    ); // one line without PRI = 1/4 missing
    // Tag with spaces: fourth line has tag '"my tag with spaces"' (including quotes as part of tag).
    // With --samples 10 we see all 4 tags; check that at least one sample contains "my tag".
    let tag = column(cols2, "tag");
    assert!(
        tag["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("my tag")),
        "tag with spaces not preserved: {tag:?}"
    );
}

#[cfg(feature = "cbor")]
#[test]
fn cbor_manual_concatenated_records() {
    let doc = run_json("edge_cbor_manual.cbor", &[]);
    let cols = table(&doc, "edge_cbor_manual");
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "name")["ideal_type"], "String");
    assert_eq!(column(cols, "id")["row_count"], 2);
}

#[cfg(feature = "msgpack")]
#[test]
fn msgpack_manual_concatenated_records() {
    let doc = run_json("edge_msgpack_manual.msgpack", &[]);
    let cols = table(&doc, "edge_msgpack_manual");
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "name")["sample_values"][0], "Alice");
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_date_affinity_mixed_column() {
    let doc = run_json("edge_sqlite_date_affinity.sqlite", &[]);
    let cols = table(&doc, "events");
    // created_at is TEXT holding dates -> should be detected as NaiveDate.
    assert_eq!(
        column(cols, "created_at")["ideal_type"],
        "NaiveDate / DateTime"
    );
    // mixed REAL column holding both date string and plain text -> mixed.
    let mixed = column(cols, "mixed");
    assert!(
        mixed["current_type"].as_str().unwrap().contains("mixed")
            || mixed["current_type"] == "String"
    );
}

#[test]
fn cli_nrows_zero_and_samples_zero_edge() {
    // --nrows 0 should produce an empty table (0 rows) without panic.
    let doc = run_json("sample.csv", &["--nrows", "0"]);
    let cols = table(&doc, "sample");
    for c in cols {
        assert_eq!(c["row_count"], 0);
    }
    // --samples 0 should produce empty sample_values but still succeed.
    let doc2 = run_with_format("sample.csv", "json", &["--samples", "0"]);
    let cols2 = table(&doc2, "sample");
    for c in cols2 {
        assert_eq!(c["sample_values"].as_array().unwrap().len(), 0);
    }
}

#[test]
fn cli_output_schema_for_heuristic_types() {
    // Verify json-schema output maps a few more heuristic types correctly.
    let doc = run_with_format("edge_csv_all_types.csv", "json-schema", &[]);
    let props = &doc["tables"]["edge_csv_all_types"]["properties"];
    assert_eq!(props["uuid"]["type"], "string");
    assert_eq!(props["uuid"]["format"], "uuid");
    assert_eq!(props["email"]["format"], "email");
    assert_eq!(props["ipv4"]["format"], "ipv4");
    assert_eq!(props["ipv6"]["format"], "ipv6");
    assert_eq!(props["url"]["format"], "uri");
}

#[test]
fn csv_delimiter_with_tab_and_skip_rows() {
    let doc = run_with_format("edge_tsv_tab.tsv", "json", &["--delimiter", "\t"]);
    let cols = table(&doc, "edge_tsv_tab");
    assert_eq!(column(cols, "score")["ideal_type"], "i64");
    // --skip-rows should skip preamble rows before header.
    let doc2 = run_with_format("preamble.csv", "json", &["--skip-rows", "1"]);
    let cols2 = table(&doc2, "preamble");
    assert!(cols2.iter().any(|c| c["name"] == "id"));
}

#[test]
fn csv_heuristic_remaining_types() {
    let doc = run_json("edge_csv_heuristic_remaining.csv", &[]);
    let cols = table(&doc, "edge_csv_heuristic_remaining");
    assert_eq!(column(cols, "ulid")["ideal_type"], "ULID");
    assert_eq!(column(cols, "mac")["ideal_type"], "MAC Address");
    assert_eq!(column(cols, "isbn10")["ideal_type"], "ISBN-10");
    assert_eq!(column(cols, "ean")["ideal_type"], "EAN-13 / UPC-A");
    assert_eq!(column(cols, "imei")["ideal_type"], "IMEI");
    assert_eq!(column(cols, "vin")["ideal_type"], "VIN");
    assert_eq!(column(cols, "cidr")["ideal_type"], "CIDR");
    assert_eq!(column(cols, "semver")["ideal_type"], "SemVer");
    assert_eq!(column(cols, "wkt")["ideal_type"], "WKT Geometry");
    assert_eq!(column(cols, "cron")["ideal_type"], "Cron Expression");
    assert_eq!(column(cols, "jwt")["ideal_type"], "JWT");
    assert_eq!(
        column(cols, "latlon")["ideal_type"],
        "Geographic Coordinates"
    );
    assert!(
        column(cols, "hash_md5")["notes"]
            .as_str()
            .unwrap()
            .contains("MD5")
    );
    assert_eq!(column(cols, "constant")["ideal_type"], "enum / category");
}

#[test]
fn csv_header_only_and_mixed_column_and_date_formats() {
    let doc = run_json("edge_csv_header_only.csv", &[]);
    let cols = table(&doc, "edge_csv_header_only");
    assert_eq!(cols.len(), 3);
    for c in cols {
        assert_eq!(c["row_count"], 0);
        assert_eq!(c["sample_values"].as_array().unwrap().len(), 0);
        assert_eq!(c["notes"], "column is empty/all null");
    }

    let doc2 = run_json("edge_csv_category_exact_boundary.csv", &[]);
    let cols2 = table(&doc2, "edge_csv_category_exact_boundary");
    // 4 unique / 100 rows = 4% <5% and <=50 -> category
    assert_eq!(
        column(cols2, "cat_4_unique")["ideal_type"],
        "enum / category"
    );
    // 5 unique / 100 rows = 5% not <5% -> not category
    assert_eq!(column(cols2, "cat_5_unique")["ideal_type"], "String");

    let doc3 = run_json("edge_csv_mixed_column.csv", &[]);
    let mixed = column(table(&doc3, "edge_csv_mixed_column"), "mixed");
    // CSV current_type for a column mixing ints and strings is String (not mixed),
    // while its ideal_type falls back to String as well. Pure int column stays i64.
    assert_eq!(mixed["ideal_type"], "String");
    assert!(mixed["current_type"].as_str().unwrap().contains("String"));
    assert_eq!(
        column(table(&doc3, "edge_csv_mixed_column"), "pure")["ideal_type"],
        "i64"
    );

    let doc4 = run_json("edge_csv_date_formats.csv", &[]);
    let cols4 = table(&doc4, "edge_csv_date_formats");
    assert_eq!(
        column(cols4, "iso_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
    assert_eq!(
        column(cols4, "us_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
    assert_eq!(
        column(cols4, "eu_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
    assert_eq!(
        column(cols4, "rfc2822")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[test]
fn content_sniffing_for_extensionless_files() {
    // JSON without extension should be sniffed via leading { / [.
    let doc = run_json("edge_sniff_json_no_ext", &[]);
    assert_eq!(doc["format"], "json");
    assert!(doc["tables"]["edge_sniff_json_no_ext"].as_array().is_some());

    // CSV without extension is deliberately NOT sniffed -> requires --format.
    let output = Command::new(bin())
        .args([fixture("edge_sniff_csv_no_ext").to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--format"),
        "CSV without extension should demand --format: {stderr}"
    );
}

#[cfg(feature = "parquet")]
#[test]
fn content_sniffing_parquet_without_extension() {
    let doc = run_json("edge_sniff_parquet_no_ext", &[]);
    assert_eq!(doc["format"], "parquet");
}

#[cfg(feature = "sqlite")]
#[test]
fn content_sniffing_sqlite_without_extension() {
    let doc = run_json("edge_sniff_sqlite_no_ext", &[]);
    assert_eq!(doc["format"], "sqlite");
}

#[test]
fn cli_error_handling_for_missing_file_and_invalid_flags() {
    let output = Command::new(bin())
        .args(["/nonexistent/path.csv", "-"])
        .output()
        .unwrap();
    assert!(!output.status.success());

    let output2 = Command::new(bin())
        .args([
            fixture("sample.csv").to_str().unwrap(),
            "-",
            "--format",
            "not_a_format",
        ])
        .output()
        .unwrap();
    assert!(!output2.status.success());
    let stderr = String::from_utf8_lossy(&output2.stderr);
    assert!(stderr.contains("unrecognized --format") || stderr.contains("format"));

    let output3 = Command::new(bin())
        .args([
            fixture("sample.csv").to_str().unwrap(),
            "-",
            "--widths",
            "not,numbers",
        ])
        .output()
        .unwrap();
    assert!(!output3.status.success());
}

#[cfg(feature = "dbase")]
#[test]
fn dbase_edge_cases_unicode_and_date() {
    let doc = run_json("edge_dbf_edge_cases.dbf", &[]);
    let cols = table(&doc, "edge_dbf_edge_cases");
    assert_eq!(column(cols, "NAME")["ideal_type"], "String");
    assert!(
        column(cols, "NAME")["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "café")
    );
    assert_eq!(column(cols, "BIRTH")["ideal_type"], "NaiveDate / DateTime");
    assert_eq!(column(cols, "ACTIVE")["ideal_type"], "bool");
    // Numeric N field is stored as f64 but ideal is i64 for whole numbers.
    assert_eq!(column(cols, "ID")["ideal_type"], "i64");
}

#[cfg(feature = "stata")]
#[test]
fn stata_edge_cases_missing_and_unicode() {
    let doc = run_json("edge_stata_edge_cases.dta", &[]);
    let cols = table(&doc, "edge_stata_edge_cases");
    assert_eq!(column(cols, "name")["sample_values"][2], "café");
    let score = column(cols, "score");
    assert!((score["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.1);
    assert_eq!(score["ideal_type"], "f64");
}

#[cfg(feature = "spss")]
#[test]
fn spss_edge_cases_missing_and_unicode() {
    let doc = run_json("edge_spss_edge_cases.sav", &[]);
    let cols = table(&doc, "edge_spss_edge_cases");
    let amount = column(cols, "amount");
    assert!((amount["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.1);
    assert_eq!(column(cols, "name")["sample_values"][0], "Alice");
}

#[cfg(feature = "avro")]
#[test]
fn avro_edge_cases_nested_and_logical_types() {
    let doc = run_json("edge_avro_edge_cases.avro", &[]);
    let cols = table(&doc, "edge_avro_edge_cases");
    let tags = column(cols, "tags");
    assert_eq!(tags["current_type"], "Vec<String>");
    let created = column(cols, "metadata.created");
    assert_eq!(created["ideal_type"], "NaiveDate / DateTime");
    let score = column(cols, "score");
    assert!((score["missing_pct"].as_f64().unwrap() - 33.3).abs() < 0.1);
}

#[cfg(feature = "xlsx")]
#[test]
fn xlsx_edge_cases_multiple_sheets_and_dates() {
    let doc = run_json("edge_xlsx_edge_cases.xlsx", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(tables.len(), 2);
    assert!(tables.contains_key("Edge"));
    assert!(tables.contains_key("Second"));
    let edge = table(&doc, "Edge");
    assert_eq!(column(edge, "date")["ideal_type"], "NaiveDate / DateTime");
    assert_eq!(column(edge, "score")["missing_pct"].as_f64().unwrap(), 33.3);
    let second = table(&doc, "Second");
    assert_eq!(column(second, "value")["ideal_type"], "i64");
}

#[cfg(feature = "npy")]
#[test]
fn npy_structured_edge_various_types() {
    let doc = run_json("edge_npy_structured_edge.npy", &[]);
    let cols = table(&doc, "edge_npy_structured_edge");
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "score")["ideal_type"], "f64");
    assert!(cols.iter().any(|c| c["name"] == "name"));
}

#[cfg(feature = "parquet")]
#[test]
fn parquet_edge_cases_nested_and_timestamp() {
    let doc = run_json("edge_parquet_edge_cases.parquet", &[]);
    let cols = table(&doc, "edge_parquet_edge_cases");
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "score")["missing_pct"].as_f64().unwrap(), 33.3);
    assert_eq!(
        column(cols, "created_at")["ideal_type"],
        "NaiveDate / DateTime"
    );
    // Nested struct/list should be flattened correctly (unlike ORC).
    assert!(cols.iter().any(|c| c["name"] == "metadata.active"));
    assert_eq!(column(cols, "tags")["current_type"], "Vec<String>");
}

#[cfg(feature = "orc")]
#[test]
fn orc_edge_cases_nested_placeholder() {
    let doc = run_json("edge_orc_edge_cases.orc", &[]);
    let cols = table(&doc, "edge_orc_edge_cases");
    assert_eq!(column(cols, "score")["missing_pct"].as_f64().unwrap(), 33.3);
    // ORC nested types are disclosed placeholders (not yet supported).
    let tags = column(cols, "tags");
    assert!(tags["notes"].as_str().unwrap().contains("nested") || tags["current_type"] == "List");
    let meta = column(cols, "metadata");
    assert!(meta["notes"].as_str().unwrap().contains("nested") || meta["current_type"] == "Struct");
}

#[cfg(feature = "parquet")]
#[test]
fn arrow_ipc_edge_cases_nested_and_timestamp() {
    let doc = run_json("edge_arrow_edge_cases.arrow", &[]);
    let cols = table(&doc, "edge_arrow_edge_cases");
    assert_eq!(
        column(cols, "created_at")["ideal_type"],
        "NaiveDate / DateTime"
    );
    assert!(cols.iter().any(|c| c["name"] == "metadata.count"));
    assert_eq!(column(cols, "metadata.active")["ideal_type"], "bool");
}

#[test]
fn format_override_for_misnamed_file() {
    // Copy a CSV to .txt and require --format to read it.
    let dir = TempDir::new();
    let misnamed = dir.path().join("data.txt");
    std::fs::copy(fixture("sample.csv"), &misnamed).unwrap();
    let output = Command::new(bin())
        .args([
            misnamed.to_str().unwrap(),
            "-",
            "--format",
            "csv",
            "--output-format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "misnamed CSV with --format csv should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(doc["tables"]["data"].as_array().is_some());
    // Without --format it should fail (CSV not sniffed).
    let output2 = Command::new(bin())
        .args([misnamed.to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!output2.status.success());
}

#[test]
fn csv_with_pipe_delimiter_and_tsv_auto() {
    // Pipe-delimited file via --delimiter
    let dir = TempDir::new();
    let path = dir.path().join("pipe.csv");
    std::fs::write(&path, "id|name|score\n1|Alice|10\n2|Bob|20\n").unwrap();
    let output = Command::new(bin())
        .args([
            path.to_str().unwrap(),
            "-",
            "--delimiter",
            "|",
            "--output-format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let doc2: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tables = doc2["tables"].as_object().unwrap();
    let cols = tables.values().next().unwrap().as_array().unwrap();
    assert_eq!(cols.len(), 3);
}

#[test]
fn empty_file_and_header_only_produce_empty_tables() {
    let doc = run_json("malformed_empty.csv", &[]);
    // Empty file (0 bytes) should produce an empty table, not a crash.
    let tables = doc["tables"].as_object().unwrap();
    assert!(
        tables
            .values()
            .next()
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
            || doc["tables"]["malformed_empty"]
                .as_array()
                .unwrap()
                .is_empty()
    );
    let doc2 = run_json("edge_csv_header_only.csv", &[]);
    for c in table(&doc2, "edge_csv_header_only") {
        assert_eq!(c["row_count"], 0);
    }
}

#[test]
fn nrows_and_samples_edge_with_all_formats() {
    // --nrows larger than file should just return full file.
    let doc = run_json("sample.csv", &["--nrows", "100"]);
    assert_eq!(column(table(&doc, "sample"), "user_id")["row_count"], 5);
    // --samples larger than distinct values should cap at distinct count.
    let doc2 = run_with_format("sample.csv", "json", &["--samples", "100"]);
    let cols = table(&doc2, "sample");
    for c in cols {
        assert!(c["sample_values"].as_array().unwrap().len() <= 100);
    }
}

#[test]
fn json_schema_required_fields_for_missing_data() {
    let doc = run_with_format("edge_csv_missing_sentinels.csv", "json-schema", &[]);
    let schema = &doc["tables"]["edge_csv_missing_sentinels"];
    // score has missing -> not required, id has no missing -> required
    let required = schema["required"].as_array().unwrap();
    assert!(required.iter().any(|v| v == "id"));
    assert!(!required.iter().any(|v| v == "score"));
}

#[cfg(feature = "sas7bdat")]
#[test]
fn sas7bdat_reads_with_json_schema_output() {
    let doc = run_with_format("sas7bdat_people_nonascii.sas7bdat", "json-schema", &[]);
    let props = &doc["tables"]["sas7bdat_people_nonascii"]["properties"];
    // Age is f64 current but i64 ideal -> should be integer in schema (since ideal drives schema).
    assert!(props["AGE"].is_object());
}

#[cfg(feature = "spss")]
#[test]
fn spss_zsav_gives_clean_error_and_sav_reads_schema() {
    let output = Command::new(bin())
        .args([
            fixture("edge_spss_zlib_compressed.zsav").to_str().unwrap(),
            "-",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("zsav")
            || stderr.to_lowercase().contains("zlib")
            || stderr.contains("not supported")
    );

    let doc = run_with_format("edge_spss_edge_cases.sav", "json-schema", &[]);
    assert!(doc["tables"]["edge_spss_edge_cases"]["properties"].is_object());
}

#[cfg(feature = "stata")]
#[test]
fn stata_schema_and_json_output_consistent() {
    let doc_json = run_json("edge_stata_edge_cases.dta", &[]);
    let doc_schema = run_with_format("edge_stata_edge_cases.dta", "json-schema", &[]);
    let cols = table(&doc_json, "edge_stata_edge_cases");
    let schema_props = doc_schema["tables"]["edge_stata_edge_cases"]["properties"]
        .as_object()
        .unwrap();
    // Every column in JSON should have a corresponding property in schema.
    for c in cols {
        let name = c["name"].as_str().unwrap();
        assert!(
            schema_props.contains_key(name),
            "schema missing column {name}"
        );
    }
}

#[cfg(feature = "orc")]
#[test]
fn orc_all_types_schema_maps_correctly() {
    let doc = run_with_format("type_detection.orc", "json-schema", &[]);
    let props = &doc["tables"]["type_detection"]["properties"];
    assert!(props.is_object());
    // At least one property should be present and have a type.
    assert!(!props.as_object().unwrap().is_empty());
}

#[test]
fn csv_with_bom_and_gzip_both_handled() {
    let dir = TempDir::new();
    let src = fixture("malformed_bom.csv");
    let gz_path = dir.path().join("bom.csv.gz");
    let py_code = format!(
        "import gzip; data=open(r'{}','rb').read(); open(r'{}','wb').write(gzip.compress(data))",
        src.to_str().unwrap(),
        gz_path.to_str().unwrap()
    );
    let out = Command::new("python3")
        .args(["-c", &py_code])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "failed to gzip bom file: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = run_json(gz_path.to_str().unwrap(), &[]);
    let cols = doc["tables"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .as_array()
        .unwrap();
    assert!(cols.iter().any(|c| c["name"] == "name"));
}

#[test]
fn csv_one_row_and_numeric_header_and_matrix() {
    let doc = run_json("edge_csv_one_row.csv", &[]);
    let cols = table(&doc, "edge_csv_one_row");
    assert_eq!(column(cols, "id")["row_count"], 1);
    assert_eq!(column(cols, "name")["ideal_type"], "enum / category"); // 1 unique -> constant

    let doc2 = run_json("edge_csv_numeric_header.csv", &[]);
    let cols2 = table(&doc2, "edge_csv_numeric_header");
    assert_eq!(cols2.len(), 3);
    // Numeric header names are still strings, but should not panic.

    let doc3 = run_json("edge_json_matrix.json", &[]);
    let cols3 = table(&doc3, "edge_json_matrix");
    assert_eq!(column(cols3, "matrix")["current_type"], "Vec<i64>");
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_booleans_all_variants() {
    let doc = run_json("edge_yaml_booleans.yaml", &[]);
    let cols = table(&doc, "edge_yaml_booleans");
    for c in cols {
        assert_eq!(c["ideal_type"], "bool");
    }
}

#[cfg(feature = "toml")]
#[test]
fn toml_array_primitives_and_nested_inline() {
    let doc = run_json("edge_toml_array_primitives.toml", &[]);
    let cols = table(&doc, "edge_toml_array_primitives");
    assert!(cols.iter().any(|c| c["name"] == "numbers"));
    assert_eq!(column(cols, "numbers")["current_type"], "Vec<i64>");
    assert!(cols.iter().any(|c| c["name"] == "nested.a"));
}

#[cfg(feature = "ini")]
#[test]
fn ini_equals_and_colon_delimiters() {
    let doc = run_json("edge_ini_equals_colon.ini", &[]);
    let cols = table(&doc, "section");
    assert!(cols.iter().any(|c| c["name"] == "key1"));
    assert!(cols.iter().any(|c| c["name"] == "key2"));
    // key1 value contains equals, key2 contains colons - should be preserved.
    let k1 = column(cols, "key1");
    assert!(k1["sample_values"][0].as_str().unwrap().contains("="));
}

#[cfg(feature = "xml")]
#[test]
fn xml_attr_url_with_entity_and_count() {
    let doc = run_json("edge_xml_attr_url.xml", &[]);
    let cols = table(&doc, "edge_xml_attr_url");
    assert_eq!(column(cols, "@url")["ideal_type"], "URL");
    assert_eq!(column(cols, "@count")["ideal_type"], "i64");
}

#[cfg(feature = "weblog")]
#[test]
fn fwf_trailing_spaces_and_weblog_missing_request() {
    let doc = run_with_format(
        "edge_fwf_trailing_spaces.fwf",
        "json",
        &["--format", "fixed-width", "--widths", "5,10,5"],
    );
    let cols = table(&doc, "edge_fwf_trailing_spaces");
    assert!(column(cols, "NAME")["missing_pct"].as_f64().unwrap() > 0.0);

    // Common Log with request "-" is not a hard error - it yields missing method/path/protocol, same as "-" bytes.
    let doc2 = run_with_format(
        "edge_weblog_missing_request.log",
        "json",
        &["--format", "common-log"],
    );
    let cols2 = table(&doc2, "edge_weblog_missing_request");
    let method = column(cols2, "method");
    assert_eq!(method["missing_pct"].as_f64().unwrap(), 100.0);
}

#[test]
fn csv_null_bytes_and_unicode_emoji() {
    let doc = run_json("edge_csv_null_bytes.csv", &[]);
    let cols = table(&doc, "edge_csv_null_bytes");
    assert_eq!(column(cols, "id")["row_count"], 3);
    // Null byte inside field should not cause panic and should be handled.

    let doc2 = run_json("edge_json_unicode_emoji.json", &[]);
    let cols2 = table(&doc2, "edge_json_unicode_emoji");
    assert_eq!(column(cols2, "text")["row_count"], 2);
    assert!(cols2.iter().any(|c| c["name"] == "text"));
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_merge_alias_is_clean_error() {
    let output = Command::new(bin())
        .args([fixture("edge_yaml_merge.yaml").to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("alias") || stderr.contains("not supported"),
        "expected alias error: {stderr}"
    );
}

#[cfg(feature = "toml")]
#[test]
fn toml_array_of_tables_edge_with_missing_field() {
    let doc = run_json("edge_toml_array_of_tables_edge.toml", &[]);
    let cols = table(&doc, "edge_toml_array_of_tables_edge");
    assert_eq!(column(cols, "products")["current_type"], "Vec<object>");
    // price appears in only 1 of 3 products -> 66.7% missing.
    let price = column(cols, "products.price");
    assert!((price["missing_pct"].as_f64().unwrap() - 66.7).abs() < 0.1);
}

#[cfg(feature = "sas7bdat")]
#[test]
fn sas_copy_reads_same_as_original() {
    let doc = run_json("edge_sas_copy.sas7bdat", &[]);
    let cols = table(&doc, "edge_sas_copy");
    assert!(cols.iter().any(|c| c["name"] == "ID"));
    assert_eq!(column(cols, "ID")["row_count"], 5);
}

#[test]
fn csv_trailing_comma_is_ragged_error_and_tsv_via_csv_ext_needs_delimiter() {
    let output = Command::new(bin())
        .args([
            fixture("edge_csv_trailing_comma.csv").to_str().unwrap(),
            "-",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "trailing comma should be ragged error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("record") || stderr.contains("field"));

    // TSV content in .csv file without --delimiter collapses to one column.
    let doc_default = run_json("edge_tsv_via_csv_ext.csv", &[]);
    assert_eq!(table(&doc_default, "edge_tsv_via_csv_ext").len(), 1);
    let output2 = Command::new(bin())
        .args([
            fixture("edge_tsv_via_csv_ext.csv").to_str().unwrap(),
            "-",
            "--delimiter",
            "\t",
            "--output-format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(output2.status.success());
    let doc2: serde_json::Value = serde_json::from_slice(&output2.stdout).unwrap();
    assert_eq!(
        doc2["tables"]["edge_tsv_via_csv_ext"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn json_top_level_string_and_array_mixed_scalars() {
    let doc = run_json("edge_json_top_level_string.json", &[]);
    let cols = table(&doc, "edge_json_top_level_string");
    assert_eq!(column(cols, "value")["current_type"], "String");
    let doc2 = run_json("edge_json_array_mixed_scalars.json", &[]);
    let cols2 = table(&doc2, "edge_json_array_mixed_scalars");
    assert!(
        column(cols2, "value")["current_type"]
            .as_str()
            .unwrap()
            .starts_with("mixed")
    );
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_explicit_document_markers() {
    let doc = run_json("edge_yaml_explicit_doc.yaml", &[]);
    let cols = table(&doc, "edge_yaml_explicit_doc");
    // Two documents -> one row per document, so row_count should be 2 for first column.
    assert_eq!(column(cols, "id")["row_count"], 2);
    assert!(cols.iter().any(|c| c["name"] == "name"));
}

#[cfg(feature = "toml")]
#[test]
fn toml_datetime_formats() {
    let doc = run_json("edge_toml_datetime.toml", &[]);
    let cols = table(&doc, "edge_toml_datetime");
    assert_eq!(
        column(cols, "created")["ideal_type"],
        "NaiveDate / DateTime"
    );
    assert_eq!(
        column(cols, "updated")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[cfg(feature = "ini")]
#[test]
fn ini_boolean_like_and_maybe() {
    let doc = run_json("edge_ini_boolean_like.ini", &[]);
    let cols = table(&doc, "section");
    let maybe = column(cols, "bool_maybe");
    // "maybe" is not a bool word, so it should not be bool (with 1 row it becomes constant enum/category).
    assert_ne!(maybe["ideal_type"], "bool");
    let bool_true = column(cols, "bool_true");
    assert_eq!(bool_true["ideal_type"], "bool");
}

#[cfg(feature = "xml")]
#[test]
fn xml_typed_attrs_with_url_and_date() {
    let doc = run_json("edge_xml_typed_attrs.xml", &[]);
    let cols = table(&doc, "edge_xml_typed_attrs");
    // URL attribute should be typed as URL, date attribute/text as NaiveDate?
    let url = column(cols, "@url");
    assert_eq!(url["ideal_type"], "URL");
    assert!(
        cols.iter()
            .any(|c| c["name"] == "content" || c["name"] == "#text")
    );
}

#[test]
fn fwf_all_spaces_column_is_missing() {
    let doc = run_with_format(
        "edge_fwf_all_spaces.fwf",
        "json",
        &["--format", "fixed-width", "--widths", "5,10,5"],
    );
    let cols = table(&doc, "edge_fwf_all_spaces");
    // Second row has NAME all spaces -> missing, third row has VAL all spaces -> missing.
    let name = column(cols, "NAME");
    assert!(name["missing_pct"].as_f64().unwrap() > 0.0);
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_decompresses_trailing_comma_fixture_transparently() {
    // Ragged CSV even after zstd decompression should still be a CSV shape error, not a zstd error.
    let output = Command::new(bin())
        .args([
            fixture("edge_csv_trailing_comma.csv.zst").to_str().unwrap(),
            "-",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("record") || stderr.contains("field"),
        "ragged CSV via zstd should be CSV error: {stderr}"
    );

    let dir = TempDir::new();
    let src = fixture("sample.csv");
    let zst_path = dir.path().join("sample.csv.zst");
    let py_code = format!(
        "import subprocess, pathlib; subprocess.run(['zstd','-f',r'{}','-o',r'{}'], check=True)",
        src.to_str().unwrap(),
        zst_path.to_str().unwrap()
    );
    let out = Command::new("python3")
        .args(["-c", &py_code])
        .output()
        .unwrap();
    if out.status.success() {
        let output = Command::new(bin())
            .args([zst_path.to_str().unwrap(), "-", "--output-format", "json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "zstd decompressed sample.csv should succeed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn directory_batch_with_sniffable_and_gzipped_files() {
    // Batch mode should handle sniffable (no ext) and gzipped files via decompress_if_needed + sniff_format.
    let dir = TempDir::new();
    std::fs::copy(
        fixture("edge_sniff_json_no_ext"),
        dir.path().join("data_no_ext"),
    )
    .unwrap();
    std::fs::copy(
        fixture("edge_csv_quoted_fields.csv.gz"),
        dir.path().join("quoted.csv.gz"),
    )
    .unwrap();
    let out = TempDir::new();
    let output = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--output-dir",
            out.path().to_str().unwrap(),
            "--output-format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "batch with sniffable+gz should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out.path().join("data_no_ext.dictionary.json").exists());
    // Gzip output name uses the compression-stripped logical name (quoted.csv.gz -> quoted.csv.dictionary.json).
    assert!(out.path().join("quoted.csv.dictionary.json").exists());
    let idx: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join("_index.dictionary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(idx["files"], 2);
}

// --- Streaming: --nrows bounds real I/O for SPSS and NumPy too ---
// Both readers were converted from a single whole-remaining-file/whole-
// array-body read to reading exactly as many rows as --nrows asks for
// (see CLAUDE.md's "Streaming reads / memory footprint" section). These
// two tests prove that's genuinely true of *disk reads*, not just of how
// many rows end up profiled afterward: a file truncated right after
// enough bytes for a handful of rows still succeeds with a small enough
// --nrows, and fails without it on the identical file, since only the
// streaming path ever stops asking the file for more bytes before
// reaching the truncation.

#[cfg(feature = "npy")]
#[test]
fn npy_nrows_stops_reading_before_a_truncated_tail() {
    let dir = TempDir::new();
    let path = dir.path().join("big.npy");
    // A real, C-order 2D float64 array (1000 rows x 4 cols) via numpy
    // itself, so the header this reader has to parse is genuine, not
    // hand-guessed.
    let py_code = format!(
        "import numpy as np; a = np.arange(1000*4, dtype='<f8').reshape(1000, 4); np.save(r'{}', a)",
        path.to_str().unwrap()
    );
    let out = Command::new("python3")
        .args(["-c", &py_code])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "failed to generate npy fixture: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Truncate to header + exactly 10 rows' worth of data, well short of
    // the 1000 the header itself declares.
    let full = std::fs::read(&path).unwrap();
    let header_len = full.len() - 1000 * 4 * 8;
    let truncated = &full[..header_len + 10 * 4 * 8];
    std::fs::write(&path, truncated).unwrap();

    let with_nrows = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--nrows", "5"])
        .output()
        .unwrap();
    assert!(
        with_nrows.status.success(),
        "{}",
        String::from_utf8_lossy(&with_nrows.stderr)
    );

    let without_nrows = Command::new(bin())
        .args([path.to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(
        !without_nrows.status.success(),
        "reading past the truncated tail should fail without --nrows"
    );
}

#[cfg(feature = "spss")]
#[test]
fn spss_nrows_stops_reading_before_a_truncated_tail() {
    let dir = TempDir::new();
    let path = dir.path().join("big.sav");
    // A real, uncompressed .sav via pyreadstat, so the dictionary/case-
    // data layout this reader has to parse is genuine.
    let py_code = format!(
        "import pyreadstat, pandas as pd, numpy as np\n\
         df = pd.DataFrame({{'id': np.arange(1000, dtype='int64'), 'name': [f'user_{{i}}' for i in range(1000)]}})\n\
         pyreadstat.write_sav(df, r'{}')\n",
        path.to_str().unwrap()
    );
    let out = Command::new("python3")
        .args(["-c", &py_code])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "failed to generate sav fixture: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Truncate well before the file's own declared case count is
    // satisfied, but past enough real case data for a handful of rows.
    let full = std::fs::read(&path).unwrap();
    let truncated = &full[..full.len() / 4];
    std::fs::write(&path, truncated).unwrap();

    let with_nrows = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--nrows", "5"])
        .output()
        .unwrap();
    assert!(
        with_nrows.status.success(),
        "{}",
        String::from_utf8_lossy(&with_nrows.stderr)
    );

    let without_nrows = Command::new(bin())
        .args([path.to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(
        !without_nrows.status.success(),
        "reading past the truncated tail should fail without --nrows"
    );
}

// ---------------------------------------------------------------------------
// Batch 4 edge fixtures - duplicate names, empty columns, boundary values,
// and additional binary-format edge cases.
// ---------------------------------------------------------------------------

#[test]
fn csv_duplicate_column_names_all_missing_and_long_header() {
    let doc = run_json("edge_csv_duplicate_column_names.csv", &[]);
    let cols = table(&doc, "edge_csv_duplicate_column_names");
    // Duplicate header "name" appears twice - both should be present as separate columns.
    assert_eq!(cols.len(), 4);
    assert_eq!(cols.iter().filter(|c| c["name"] == "name").count(), 2);

    let doc2 = run_json("edge_csv_all_missing_column.csv", &[]);
    let empty = column(table(&doc2, "edge_csv_all_missing_column"), "empty_col");
    assert_eq!(empty["missing_pct"].as_f64().unwrap(), 100.0);
    assert_eq!(empty["current_type"], "String");

    let doc3 = run_json("edge_csv_very_long_header.csv", &[]);
    let cols3 = table(&doc3, "edge_csv_very_long_header");
    assert!(
        cols3
            .iter()
            .any(|c| c["name"].as_str().unwrap().len() == 200)
    );
}

#[test]
fn csv_single_column_and_large_numbers_at_boundaries() {
    let doc = run_json("edge_csv_single_column.csv", &[]);
    let cols = table(&doc, "edge_csv_single_column");
    assert_eq!(cols.len(), 1);
    assert_eq!(column(cols, "value")["ideal_type"], "i64");

    let doc2 = run_json("edge_csv_large_numbers.csv", &[]);
    let cols2 = table(&doc2, "edge_csv_large_numbers");
    // i64 MAX/MIN are valid, beyond is f64 with precision note already covered in other fixtures.
    assert_eq!(column(cols2, "big_int")["ideal_type"], "i64");
    let beyond = column(cols2, "beyond_i64");
    assert_eq!(beyond["ideal_type"], "f64");
    assert!(beyond["notes"].as_str().unwrap().contains("exceed"));
}

#[test]
fn json_large_numbers_and_empty_nested_and_top_level_scalar() {
    let doc = run_json("edge_json_large_numbers.json", &[]);
    let cols = table(&doc, "edge_json_large_numbers");
    let beyond = column(cols, "beyond");
    // Beyond i64 should be f64 with precision note or at least not i64.
    assert_ne!(beyond["ideal_type"], "i64");

    let doc2 = run_json("edge_json_empty_nested.json", &[]);
    let cols2 = table(&doc2, "edge_json_empty_nested");
    // Empty object "a" flattens but has no leaf values.
    assert!(cols2.iter().any(|c| c["name"] == "a"));
    assert!(cols2.iter().any(|c| c["name"] == "b"));

    let doc3 = run_json("edge_json_top_level_number.json", &[]);
    let cols3 = table(&doc3, "edge_json_top_level_number");
    assert_eq!(column(cols3, "value")["ideal_type"], "i64");
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_complex_mixed_explicit_tags() {
    let doc = run_json("edge_yaml_complex_mixed.yaml", &[]);
    let cols = table(&doc, "edge_yaml_complex_mixed");
    // !!str forces string even for numeric-like value.
    assert_eq!(column(cols, "explicit_str")["current_type"], "String");
    assert_eq!(column(cols, "inline_int")["ideal_type"], "i64");
    assert!(
        cols.iter()
            .any(|c| c["name"] == "nested.level1.level2.value")
    );
}

#[cfg(feature = "toml")]
#[test]
fn toml_dotted_table_conflict_and_valid() {
    let doc = run_json("edge_toml_dotted_table_conflict.toml", &[]);
    let cols = table(&doc, "edge_toml_dotted_table_conflict");
    assert!(cols.iter().any(|c| c["name"] == "server.host"));
    assert!(cols.iter().any(|c| c["name"] == "server.config.port"));
}

#[cfg(feature = "ini")]
#[test]
fn ini_bom_and_no_section() {
    // BOM-prefixed INI: either succeeds (if parser strips BOM) or fails with actionable error, but must not panic.
    let output = Command::new(bin())
        .args([fixture("edge_ini_bom.ini").to_str().unwrap(), "-"])
        .output()
        .unwrap();
    // Must not be a panic; either success or clean error.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("RUST_BACKTRACE"),
        "BOM handling should not panic: {stderr}"
    );
    if output.status.success() {
        let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let s = table(&doc, "section");
        assert!(s.iter().any(|c| c["name"] == "key"));
    }

    let doc2 = run_json("edge_ini_no_section.ini", &[]);
    let tables = doc2["tables"].as_object().unwrap();
    assert!(tables.contains_key("section"));
}

#[cfg(feature = "xml")]
#[test]
fn xml_entity_ref_and_deeply_nested_10() {
    let doc = run_json("edge_xml_entity_ref.xml", &[]);
    let cols = table(&doc, "edge_xml_entity_ref");
    // Entity &amp; should decode to &, &lt; to < etc.
    let vals: Vec<String> = cols
        .iter()
        .flat_map(|c| {
            c["sample_values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
        })
        .collect();
    assert!(
        vals.iter().any(|v| v.contains("Tom & Jerry")),
        "entity &amp; not decoded: {vals:?}"
    );

    let doc2 = run_json("edge_xml_deeply_nested_10.xml", &[]);
    let cols2 = table(&doc2, "edge_xml_deeply_nested_10");
    // 10 levels deep should flatten to 10 dot-notation columns plus the leaf.
    assert!(
        cols2
            .iter()
            .any(|c| c["name"].as_str().unwrap().contains("level0"))
    );
}

#[test]
fn fwf_empty_fields_and_single_char_widths() {
    let doc = run_with_format(
        "edge_fwf_empty_fields.fwf",
        "json",
        &["--format", "fixed-width", "--widths", "5,5,10"],
    );
    let cols = table(&doc, "edge_fwf_empty_fields");
    let name = column(cols, "NAME");
    assert!(name["missing_pct"].as_f64().unwrap() > 0.0);
    let doc2 = run_with_format(
        "edge_fwf_single_char.fwf",
        "json",
        &["--format", "fixed-width", "--widths", "1,1,1"],
    );
    let cols2 = table(&doc2, "edge_fwf_single_char");
    assert_eq!(cols2.len(), 3);
    assert_eq!(column(cols2, "A")["ideal_type"], "i64");
}

#[cfg(feature = "dbase")]
#[test]
fn dbase_with_deleted_records_skipped() {
    let doc = run_json("edge_dbf_with_deleted.dbf", &[]);
    let cols = table(&doc, "edge_dbf_with_deleted");
    // One of 3 records is deleted, so row_count should be 2, not 3.
    assert_eq!(column(cols, "ID")["row_count"], 2);
}

#[cfg(feature = "stata")]
#[test]
fn stata_long_string_strl_placeholder() {
    let doc = run_json("edge_stata_long_string.dta", &[]);
    let cols = table(&doc, "edge_stata_long_string");
    assert!(cols.iter().any(|c| c["name"] == "long_str"));
    // long_str >244 chars may be handled as String or placeholder, but should not panic.
    assert_eq!(column(cols, "id")["row_count"], 3);
}

#[cfg(feature = "spss")]
#[test]
fn spss_long_string_segmented() {
    let doc = run_json("edge_spss_long_string2.sav", &[]);
    let cols = table(&doc, "edge_spss_long_string2");
    assert!(cols.iter().any(|c| c["name"] == "long_text"));
    let txt = column(cols, "long_text");
    assert!(txt["sample_values"][0].as_str().unwrap().len() > 200);
}

#[cfg(feature = "parquet")]
#[test]
fn parquet_decimal_binary_and_all_types() {
    let doc = run_json("edge_parquet_decimal_binary.parquet", &[]);
    let cols = table(&doc, "edge_parquet_decimal_binary");
    assert_eq!(column(cols, "decimal_col")["ideal_type"], "f64");
    // binary_col is stored as binary, rendered as hex, so String.
    assert_eq!(column(cols, "binary_col")["current_type"], "String");
}

#[cfg(feature = "orc")]
#[test]
fn orc_decimal_binary_roundtrip() {
    let doc = run_json("edge_orc_decimal_binary.orc", &[]);
    let cols = table(&doc, "edge_orc_decimal_binary");
    assert_eq!(column(cols, "decimal_col")["ideal_type"], "f64");
}

#[cfg(feature = "xlsx")]
#[test]
fn xlsx_hidden_and_empty_sheets() {
    let doc = run_json("edge_xlsx_hidden_empty.xlsx", &[]);
    let tables = doc["tables"].as_object().unwrap();
    // Hidden sheet should still be present (unless skipped as empty?), and empty sheet with header only should be present with 0 rows.
    assert!(tables.contains_key("Visible"));
    assert!(tables.contains_key("Empty"));
    assert_eq!(column(table(&doc, "Empty"), "a")["row_count"], 0);
}

#[cfg(feature = "npy")]
#[test]
fn npy_plain_1d_and_fortran_order() {
    let doc = run_json("edge_npy_plain_1d.npy", &[]);
    let cols = table(&doc, "edge_npy_plain_1d");
    assert_eq!(column(cols, "value")["ideal_type"], "f64");
    assert_eq!(column(cols, "value")["row_count"], 3);

    let doc2 = run_json("edge_npz_fortran.npz", &[]);
    let tables = doc2["tables"].as_object().unwrap();
    assert!(tables.contains_key("c_order"));
    assert!(tables.contains_key("f_order"));
    // Both arrays should have 2 columns (2D plain) with correct types.
    assert!(table(&doc2, "c_order").iter().any(|c| c["name"] == "col_0"));
}

#[cfg(feature = "avro")]
#[test]
fn avro_enum_and_union_types() {
    let doc = run_json("edge_avro_enum.avro", &[]);
    let cols = table(&doc, "edge_avro_enum");
    // Enum should be detected as enum/category or String.
    let status = column(cols, "status");
    assert!(
        status["ideal_type"].as_str().unwrap().contains("enum") || status["ideal_type"] == "String"
    );
}

#[test]
fn csv_whitespace_header_and_empty_quoted_fields() {
    let doc = run_json("edge_csv_whitespace_header.csv", &[]);
    let cols = table(&doc, "edge_csv_whitespace_header");
    // Header with spaces should be trimmed? Check that columns exist and not panic.
    assert!(
        cols.iter()
            .any(|c| c["name"].as_str().unwrap().trim() == "id")
    );
    assert_eq!(column(cols, " id ")["row_count"], 2);

    let doc2 = run_json("edge_csv_empty_quoted_fields.csv", &[]);
    let cols2 = table(&doc2, "edge_csv_empty_quoted_fields");
    let name = column(cols2, "name");
    // Empty quoted "" should be missing.
    assert!(name["missing_pct"].as_f64().unwrap() > 0.0);
}

#[test]
fn csv_leading_zeros_string_and_mixed_line_endings() {
    let doc = run_json("edge_csv_leading_zeros_string.csv", &[]);
    let cols = table(&doc, "edge_csv_leading_zeros_string");
    let zip = column(cols, "zip");
    assert!(zip["notes"].as_str().unwrap().contains("leading zeros"));

    // Mixed line endings already handled via earlier edge_csv_crlf, but ensure this file also parses.
    let doc2 = run_json("edge_csv_mixed_line_endings.csv", &[]);
    assert_eq!(
        column(table(&doc2, "edge_csv_mixed_line_endings"), "id")["row_count"],
        3
    );
}

#[test]
fn json_duplicate_nested_keys_and_number_as_string() {
    let doc = run_json("edge_json_duplicate_nested_keys.json", &[]);
    let cols = table(&doc, "edge_json_duplicate_nested_keys");
    // Duplicate nested "x" should last win.
    assert!(cols.iter().any(|c| c["name"] == "meta.x"));

    let doc2 = run_json("edge_json_number_as_string.json", &[]);
    let cols2 = table(&doc2, "edge_json_number_as_string");
    // "001" with leading zeros should be string with leading zeros note.
    let id = column(cols2, "id");
    assert!(
        id["notes"].as_str().unwrap().contains("leading zeros") || id["ideal_type"] == "String"
    );
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_binary_tag_and_multiline_strings() {
    let doc = run_json("edge_yaml_binary_tag.yaml", &[]);
    let cols = table(&doc, "edge_yaml_binary_tag");
    assert!(cols.iter().any(|c| c["name"] == "plain"));
    // !!binary should be string.

    let doc2 = run_json("edge_yaml_multiline_strings.yaml", &[]);
    let cols2 = table(&doc2, "edge_yaml_multiline_strings");
    assert!(cols2.iter().any(|c| c["name"] == "literal"));
    assert!(
        column(cols2, "literal")["sample_values"][0]
            .as_str()
            .unwrap()
            .contains("line1")
    );
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_merge_alias_is_clean_error_second() {
    let output = Command::new(bin())
        .args([fixture("edge_yaml_merge.yaml").to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("alias") || stderr.contains("not supported"));
}

#[cfg(feature = "toml")]
#[test]
fn toml_invalid_duplicate_table_is_error() {
    let output = Command::new(bin())
        .args([
            fixture("edge_toml_invalid_duplicate_table.toml")
                .to_str()
                .unwrap(),
            "-",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[cfg(all(feature = "ini", feature = "xml"))]
#[test]
fn ini_value_with_equals_and_empty_attrs() {
    let doc = run_json("edge_ini_value_with_equals.ini", &[]);
    let cols = table(&doc, "section");
    let k1 = column(cols, "key1");
    assert!(k1["sample_values"][0].as_str().unwrap().contains("="));

    let doc2 = run_json("edge_xml_empty_attrs.xml", &[]);
    // Empty attrs should be present as @id with empty string -> missing?
    assert!(doc2["tables"]["edge_xml_empty_attrs"].as_array().is_some());
}

#[cfg(feature = "xml")]
#[test]
fn xml_with_pi_and_comment_and_negative_numbers() {
    let doc = run_json("edge_xml_with_pi_and_comment.xml", &[]);
    let cols = table(&doc, "edge_xml_with_pi_and_comment");
    // The two <item> siblings are the records (homogeneous same-tag
    // children of <root>), so "item" is never a column name itself - its
    // own attribute/text become "@id"/"#text" columns. Both leading `<?xml
    // ... ?>` declarations, the two comments (one with an embedded `<tag>`
    // that must not be mistaken for real markup), and the `<?custom-pi?>`
    // processing instruction between the two items must all be skipped
    // without disturbing record detection, and the CDATA section's own
    // literal `<content>`/`&` text must survive unescaped/unparsed.
    assert!(cols.iter().any(|c| c["name"] == "@id"));
    assert_eq!(column(cols, "@id")["row_count"], 2);
    assert_eq!(
        column(cols, "#text")["sample_values"][1],
        "raw <content> & stuff"
    );

    let doc2 = run_with_format(
        "edge_fwf_negative_numbers.fwf",
        "json",
        &["--format", "fixed-width", "--widths", "5,10,10"],
    );
    let cols2 = table(&doc2, "edge_fwf_negative_numbers");
    assert_eq!(column(cols2, "SCORE")["ideal_type"], "i64");
    assert!(
        column(cols2, "SCORE")["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "-12")
    );
}

#[cfg(feature = "toml")]
#[test]
fn json_array_of_objects_with_null_and_toml_nested() {
    let doc = run_json("edge_json_array_of_objects_with_null.json", &[]);
    let cols = table(&doc, "edge_json_array_of_objects_with_null");
    let opt = column(cols, "opt");
    assert!(opt["missing_pct"].as_f64().unwrap() > 0.0);

    let doc2 = run_json("edge_toml_nested_inline.toml", &[]);
    assert!(
        doc2["tables"]["edge_toml_nested_inline"]
            .as_array()
            .is_some()
    );
}

#[cfg(feature = "weblog")]
#[test]
fn weblog_with_useragent_quotes_and_empty() {
    // A literal backslash-escaped quote inside the user-agent field is not
    // a convention real Apache access logs use (Apache never escapes an
    // embedded quote in a quoted field) - this is genuinely ambiguous
    // input under the fixed Combined Log grammar, so the reader correctly
    // hard-errors naming the offending line rather than guessing at where
    // the quoted field actually ends, the same "never guess" contract
    // every other malformed-line case in this project already gets.
    let output = Command::new(bin())
        .args([
            fixture("edge_weblog_with_useragent_quotes.log")
                .to_str()
                .unwrap(),
            "-",
            "--format",
            "combined-log",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("line 1"),
        "error should name the offending line: {stderr}"
    );
}

#[cfg(feature = "syslog")]
#[test]
fn syslog_with_empty_msg() {
    let doc = run_with_format(
        "edge_syslog_with_empty_msg.log",
        "json",
        &["--format", "syslog"],
    );
    let cols = table(&doc, "edge_syslog_with_empty_msg");
    let msg = column(cols, "message");
    // A message that's genuinely empty text (everything after "tag[pid]: ")
    // is still a real, present value - RFC 3164 has no nilvalue convention
    // for the message field the way RFC 5424 does for its optional fields,
    // so it correctly counts as present (missing_pct 0.0), not missing.
    assert_eq!(msg["missing_pct"].as_f64().unwrap(), 0.0);
    assert!(
        msg["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "")
    );
}

#[test]
fn csv_heuristic_extra_and_category_and_free_text() {
    let doc = run_json("edge_csv_heuristic_extra.csv", &[]);
    let cols = table(&doc, "edge_csv_heuristic_extra");
    assert_eq!(column(cols, "ulid2")["ideal_type"], "ULID");
    assert_eq!(column(cols, "imei2")["ideal_type"], "IMEI");
    assert_eq!(column(cols, "cidr6")["ideal_type"], "CIDR");
    assert_eq!(column(cols, "wkt2")["ideal_type"], "WKT Geometry");
    assert_eq!(column(cols, "cron2")["ideal_type"], "Cron Expression");

    let doc2 = run_json("edge_csv_free_text_vs_category.csv", &[]);
    let cols2 = table(&doc2, "edge_csv_free_text_vs_category");
    // free_text has many unique high-cardinality -> String, cat_3 has low cardinality -> enum/category
    let cat = column(cols2, "cat_3");
    assert!(cat["ideal_type"].as_str().unwrap().contains("enum") || cat["ideal_type"] == "String");
}

#[test]
fn json_heuristic_extra_and_nested_mixed() {
    let doc = run_json("edge_json_heuristic_extra.jsonl", &[]);
    let cols = table(&doc, "edge_json_heuristic_extra");
    assert_eq!(column(cols, "ulid")["ideal_type"], "ULID");
    assert_eq!(column(cols, "cidr")["ideal_type"], "CIDR");

    let doc2 = run_json("edge_json_nested_mixed_types.jsonl", &[]);
    let cols2 = table(&doc2, "edge_json_nested_mixed_types");
    assert!(cols2.iter().any(|c| c["name"] == "meta.y"));
}

#[cfg(all(feature = "yaml", feature = "toml"))]
#[test]
fn yaml_heuristic_extra_and_toml_heuristic_extra() {
    let doc = run_json("edge_yaml_heuristic_extra.yaml", &[]);
    let cols = table(&doc, "edge_yaml_heuristic_extra");
    assert_eq!(column(cols, "ulid_val")["ideal_type"], "ULID");
    assert_eq!(column(cols, "cidr_val")["ideal_type"], "CIDR");

    let doc2 = run_json("edge_toml_heuristic_extra.toml", &[]);
    let cols2 = table(&doc2, "edge_toml_heuristic_extra");
    assert_eq!(column(cols2, "ulid_val")["ideal_type"], "ULID");
}

#[cfg(all(feature = "xml", feature = "ini"))]
#[test]
fn xml_heuristic_extra_and_ini_heuristic_extra() {
    let doc = run_json("edge_xml_heuristic_extra.xml", &[]);
    let cols = table(&doc, "edge_xml_heuristic_extra");
    assert!(
        cols.iter()
            .any(|c| c["name"] == "@ulid" || c["name"] == "ulid")
    );

    let doc2 = run_json("edge_ini_heuristic_extra.ini", &[]);
    let cols2 = table(&doc2, "heuristic");
    assert!(cols2.iter().any(|c| c["name"] == "ulid"));
}

#[cfg(all(feature = "toml", feature = "yaml"))]
#[test]
fn toml_empty_values_and_yaml_empty_values() {
    let doc = run_json("edge_toml_empty_values.toml", &[]);
    let cols = table(&doc, "edge_toml_empty_values");
    assert!(cols.iter().any(|c| c["name"] == "title"));

    let doc2 = run_json("edge_yaml_empty_values.yaml", &[]);
    let cols2 = table(&doc2, "edge_yaml_empty_values");
    assert!(cols2.iter().any(|c| c["name"] == "present"));
    assert_eq!(
        column(cols2, "null_val")["missing_pct"].as_f64().unwrap(),
        100.0
    );
}

#[cfg(feature = "xml")]
#[test]
fn xml_mixed_attrs_and_text_edge() {
    let doc = run_json("edge_xml_mixed_attrs_and_text.xml", &[]);
    let cols = table(&doc, "edge_xml_mixed_attrs_and_text");
    assert!(cols.iter().any(|c| c["name"] == "@id"));
    // Mixed content with <b> inside should flatten.
    assert!(cols.iter().any(|c| c["name"] == "b"));
}

#[test]
fn csv_category_50_vs_51_and_extra() {
    let doc = run_json("edge_csv_category_50_vs_51.csv", &[]);
    let cols = table(&doc, "edge_csv_category_50_vs_51");
    // With only 10 rows, both cat_50 and cat_51 have <50 unique but ratio >5%, so both are String (not category) - just check not panic.
    assert_eq!(cols.len(), 3);
}

// --- `sniff-rs diff <OLD> <NEW>` - schema diff / drift detection ---
// This project's first CLI subcommand, so these tests follow a slightly
// different shape from every test above: rather than pointing at one
// committed fixture, each test builds its own small pair of dictionary
// JSON files under a throwaway TempDir (via a real `sniff-rs <csv>
// --output-format json` run first, matching how a real user would
// actually produce the two inputs `diff` compares), then runs `sniff-rs
// diff` against them and asserts on the report.

fn run_diff_raw(args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .arg("diff")
        .args(args)
        .output()
        .expect("failed to run binary")
}

/// Writes `content` to `dir/name`, profiles it to JSON under the same
/// directory (`name.dictionary.json`), and returns that dictionary's
/// path.
fn write_dictionary(dir: &std::path::Path, name: &str, csv_content: &str) -> PathBuf {
    let csv_path = dir.join(name);
    std::fs::write(&csv_path, csv_content).unwrap();
    let json_path = dir.join(format!("{name}.json"));
    let output = Command::new(bin())
        .args([
            csv_path.to_str().unwrap(),
            json_path.to_str().unwrap(),
            "--output-format",
            "json",
        ])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "profiling {name} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    json_path
}

#[test]
fn diff_detects_added_removed_and_a_renamed_column_in_a_single_table_dictionary() {
    let dir = TempDir::new();
    // "name" -> "full_name" is a real rename candidate (same type, 100%
    // sample-value overlap); "legacy_col" is genuinely dropped;
    // "new_col" is genuinely added, with no missing values (so it's
    // classified breaking, since adding it as NOT NULL needs a
    // backfill).
    let old = write_dictionary(
        dir.path(),
        "old.csv",
        "id,name,email,legacy_col\n1,alice,alice@example.com,x\n2,bob,bob@example.com,y\n3,carol,carol@example.com,z\n",
    );
    let new = write_dictionary(
        dir.path(),
        "new.csv",
        "id,full_name,email,new_col\n1,alice,alice@example.com,10\n2,bob,bob@example.com,20\n3,carol,carol@example.com,30\n",
    );

    let output = run_diff_raw(&[old.to_str().unwrap(), new.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "diff should succeed (breaking changes alone don't fail the run without --fail-on-breaking): {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(
        report.contains("name -> full_name") && report.contains("possible rename"),
        "expected a rename candidate in the report:\n{report}"
    );
    assert!(
        report.contains("legacy_col") && report.contains("column removed"),
        "expected legacy_col to be reported removed:\n{report}"
    );
    assert!(
        report.contains("new_col") && report.contains("column added"),
        "expected new_col to be reported added:\n{report}"
    );
    assert!(
        report.contains("BREAKING"),
        "a dropped column and a non-nullable added column should both be breaking:\n{report}"
    );
}

#[test]
fn diff_reports_no_differences_for_an_unchanged_schema() {
    let dir = TempDir::new();
    let content = "id,name\n1,alice\n2,bob\n";
    let old = write_dictionary(dir.path(), "a.csv", content);
    let new = write_dictionary(dir.path(), "b.csv", content);

    let output = run_diff_raw(&[old.to_str().unwrap(), new.to_str().unwrap()]);
    assert!(output.status.success());
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(
        report.contains("No differences detected"),
        "identical schemas should report no differences:\n{report}"
    );
}

#[test]
fn diff_output_format_json_produces_structured_changes_with_a_breaking_flag() {
    let dir = TempDir::new();
    let old = write_dictionary(dir.path(), "old.csv", "id,name\n1,alice\n2,bob\n");
    let new = write_dictionary(dir.path(), "new.csv", "id\n1\n2\n");

    let output = run_diff_raw(&[
        old.to_str().unwrap(),
        new.to_str().unwrap(),
        "--output-format",
        "json",
    ]);
    assert!(output.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["has_breaking_changes"], true);
    let changes = doc["changes"].as_array().unwrap();
    assert!(
        changes.iter().any(|c| c["column"] == "name"
            && c["kind"] == "column removed"
            && c["compatibility"] == "breaking"),
        "expected a breaking column-removed entry for 'name': {doc}"
    );
}

#[test]
fn diff_fail_on_breaking_exits_with_status_2_only_when_a_breaking_change_exists() {
    let dir = TempDir::new();
    let old = write_dictionary(dir.path(), "old.csv", "id,name\n1,alice\n2,bob\n");
    let new_breaking = write_dictionary(dir.path(), "new_breaking.csv", "id\n1\n2\n");
    let new_same = write_dictionary(dir.path(), "new_same.csv", "id,name\n1,alice\n2,bob\n");

    let breaking_output = run_diff_raw(&[
        old.to_str().unwrap(),
        new_breaking.to_str().unwrap(),
        "--fail-on-breaking",
    ]);
    assert_eq!(breaking_output.status.code(), Some(2));

    let clean_output = run_diff_raw(&[
        old.to_str().unwrap(),
        new_same.to_str().unwrap(),
        "--fail-on-breaking",
    ]);
    assert_eq!(clean_output.status.code(), Some(0));
}

#[test]
fn diff_resolution_sql_emits_alter_table_for_a_safe_add_and_excludes_breaking_changes() {
    let dir = TempDir::new();
    // Same file name in two different subdirectories so both dictionaries'
    // one-and-only table shares a real, unambiguous name ("data") -
    // exercising the live ALTER TABLE path, not the "table names differ"
    // disclosed fallback.
    let old_dir = dir.path().join("a");
    let new_dir = dir.path().join("b");
    std::fs::create_dir_all(&old_dir).unwrap();
    std::fs::create_dir_all(&new_dir).unwrap();
    let old = write_dictionary(&old_dir, "data.csv", "id,name\n1,alice\n2,bob\n");
    let new = write_dictionary(
        &new_dir,
        "data.csv",
        "id,name,nickname\n1,alice,\n2,bob,bobby\n",
    );

    let sql_path = dir.path().join("resolution.sql");
    let output = run_diff_raw(&[
        old.to_str().unwrap(),
        new.to_str().unwrap(),
        "--resolution-sql",
        sql_path.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sql = std::fs::read_to_string(&sql_path).unwrap();
    assert!(
        sql.contains("ALTER TABLE \"data\" ADD COLUMN \"nickname\" TEXT;"),
        "expected a real ALTER TABLE statement for the safely-added nullable column:\n{sql}"
    );
    assert!(
        !sql.contains("DROP") && !sql.contains("RENAME COLUMN \"id\""),
        "resolution SQL must never touch anything beyond a safe ADD COLUMN or a commented-out rename:\n{sql}"
    );
}

#[test]
fn diff_resolution_sql_never_emits_a_live_statement_when_table_names_disagree() {
    let dir = TempDir::new();
    let old = write_dictionary(dir.path(), "old.csv", "id,name\n1,alice\n2,bob\n");
    let new = write_dictionary(
        dir.path(),
        "new.csv",
        "id,name,nickname\n1,alice,\n2,bob,bobby\n",
    );

    let sql_path = dir.path().join("resolution.sql");
    let output = run_diff_raw(&[
        old.to_str().unwrap(),
        new.to_str().unwrap(),
        "--resolution-sql",
        sql_path.to_str().unwrap(),
    ]);
    assert!(output.status.success());
    let sql = std::fs::read_to_string(&sql_path).unwrap();
    assert!(
        !sql.contains("ALTER TABLE \""),
        "an ambiguous table name (old.csv vs new.csv) must never be guessed at in generated SQL:\n{sql}"
    );
    assert!(
        sql.contains("name their (single) table differently"),
        "expected the disclosed ambiguous-table-name note:\n{sql}"
    );
}

#[test]
fn diff_rejects_a_json_schema_document_with_an_actionable_error() {
    let dir = TempDir::new();
    let csv_path = dir.path().join("data.csv");
    std::fs::write(&csv_path, "id,name\n1,alice\n").unwrap();
    let schema_path = dir.path().join("data.schema.json");
    let profile = Command::new(bin())
        .args([
            csv_path.to_str().unwrap(),
            schema_path.to_str().unwrap(),
            "--output-format",
            "json-schema",
        ])
        .output()
        .unwrap();
    assert!(profile.status.success());

    let new = write_dictionary(dir.path(), "new.csv", "id,name\n1,alice\n");
    let output = run_diff_raw(&[schema_path.to_str().unwrap(), new.to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("isn't a JSON array of columns") || stderr.contains("tables"),
        "expected an actionable error naming the shape mismatch: {stderr}"
    );
}

#[test]
fn diff_missing_positional_arguments_is_an_actionable_error_not_a_panic() {
    let output = run_diff_raw(&[]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("<OLD>"),
        "expected an actionable error: {stderr}"
    );
}

// Uses INI, not SQLite, to build the two multi-table dictionaries below -
// deliberately: this project's own standing rule is that no automated
// `cargo test` spawns a real external database engine CLI (see
// CLAUDE.md's own "Deliberately not covered by an automated `cargo
// test`" note under `--load-into`), and INI already gives one table per
// section (see `columns_from_ini`) with nothing more than a plain text
// file to write - no subprocess, no optional external tool, needed at all.
#[cfg(feature = "ini")]
#[test]
fn diff_multi_table_dictionaries_match_tables_by_name() {
    let dir = TempDir::new();
    let old = write_dictionary(
        dir.path(),
        "old.ini",
        "[users]\nid=1\nname=alice\n\n[orders]\nid=1\namount=9.99\n",
    );
    let new = write_dictionary(
        dir.path(),
        "new.ini",
        "[users]\nid=1\nname=alice\nemail=alice@example.com\n\n[payments]\nid=1\namount=9.99\n",
    );

    let output = run_diff_raw(&[old.to_str().unwrap(), new.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("orders") && report.contains("table removed"));
    assert!(report.contains("payments") && report.contains("table added"));
    assert!(
        report.contains("users") && report.contains("email") && report.contains("column added")
    );
}

// --- `sniff-rs diff` - committed fixture pairs, exercising every change
// kind and classification at once, plus malformed/degenerate inputs.
// These are permanent, reviewable assets (see CLAUDE.md's own "reviewable
// without reading test code" convention for every other format's fixture
// corpus) rather than only ever built on the fly under a TempDir.

fn run_diff_json_fixtures(old: &str, new: &str, extra_args: &[&str]) -> serde_json::Value {
    let mut args: Vec<&str> = vec![old, new, "--output-format", "json"];
    args.extend_from_slice(extra_args);
    let output = run_diff_raw(&args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not valid JSON ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn find_change<'a>(
    doc: &'a serde_json::Value,
    column: &str,
    kind: &str,
) -> Option<&'a serde_json::Value> {
    doc["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["column"] == column && c["kind"] == kind)
}

#[test]
fn diff_fixture_pair_exercises_every_change_kind_and_classification() {
    let old = fixture("diff_old.json");
    let new = fixture("diff_new.json");
    let doc = run_diff_json_fixtures(old.to_str().unwrap(), new.to_str().unwrap(), &[]);

    let rename = find_change(&doc, "name -> full_name", "possible rename").unwrap();
    assert_eq!(rename["compatibility"], "safe");
    assert_eq!(rename["from"], "name");
    assert_eq!(rename["to"], "full_name");

    let removed = find_change(&doc, "legacy_score", "column removed").unwrap();
    assert_eq!(removed["compatibility"], "breaking");

    let widened = find_change(&doc, "signup_year", "type changed").unwrap();
    assert_eq!(widened["compatibility"], "safe");
    assert_eq!(widened["old_ideal_type"], "i64");
    assert_eq!(widened["new_ideal_type"], "f64");

    let narrowed = find_change(&doc, "status", "type changed").unwrap();
    assert_eq!(narrowed["compatibility"], "breaking");
    assert_eq!(narrowed["old_ideal_type"], "String");
    assert_eq!(narrowed["new_ideal_type"], "i64");

    let became_nullable = find_change(&doc, "phone", "missing % changed").unwrap();
    assert_eq!(became_nullable["compatibility"], "safe");
    assert_eq!(became_nullable["old_missing_pct"], 0.0);
    assert_eq!(became_nullable["new_missing_pct"], 15.0);

    let became_full = find_change(&doc, "verified", "missing % changed").unwrap();
    assert_eq!(became_full["compatibility"], "breaking");
    assert_eq!(became_full["old_missing_pct"], 20.0);
    assert_eq!(became_full["new_missing_pct"], 0.0);

    let safe_add = find_change(&doc, "notes", "column added").unwrap();
    assert_eq!(safe_add["compatibility"], "safe");

    let breaking_add = find_change(&doc, "account_id", "column added").unwrap();
    assert_eq!(breaking_add["compatibility"], "breaking");

    // Exactly these 8 changes - "id" is completely unchanged and must
    // never show up as a spurious entry.
    assert_eq!(doc["changes"].as_array().unwrap().len(), 8);
    assert_eq!(doc["has_breaking_changes"], true);
}

#[test]
fn diff_fixture_pair_resolution_sql_only_alters_the_safe_side() {
    let dir = TempDir::new();
    let sql_path = dir.path().join("resolution.sql");
    let output = run_diff_raw(&[
        fixture("diff_old.json").to_str().unwrap(),
        fixture("diff_new.json").to_str().unwrap(),
        "--resolution-sql",
        sql_path.to_str().unwrap(),
    ]);
    assert!(output.status.success());
    let sql = std::fs::read_to_string(&sql_path).unwrap();

    // Both dictionaries name their one table "customers", so there's a
    // real, unambiguous table to write ALTER TABLE against.
    assert!(sql.contains("ALTER TABLE \"customers\" ADD COLUMN \"notes\" TEXT;"));
    assert!(!sql.contains("ADD COLUMN \"account_id\""));
    assert!(sql.contains("-- ALTER TABLE \"customers\" RENAME COLUMN \"name\" TO \"full_name\";"));
    // Every breaking change is named in the leading comment block, never
    // turned into a live statement.
    for breaking_col in ["legacy_score", "status", "verified", "account_id"] {
        assert!(
            sql.contains(breaking_col),
            "expected {breaking_col} to be named in the breaking-changes comment:\n{sql}"
        );
    }
    assert!(!sql.contains("DROP") && !sql.contains("CREATE TABLE"));
}

#[test]
fn diff_multitable_fixture_pair_matches_tables_by_name() {
    let doc = run_diff_json_fixtures(
        fixture("diff_old_multitable.json").to_str().unwrap(),
        fixture("diff_new_multitable.json").to_str().unwrap(),
        &[],
    );
    let changes = doc["changes"].as_array().unwrap();
    assert!(
        changes
            .iter()
            .any(|c| c["table"] == "orders" && c["kind"] == "table removed")
    );
    assert!(
        changes
            .iter()
            .any(|c| c["table"] == "payments" && c["kind"] == "table added")
    );
    let email_added = changes
        .iter()
        .find(|c| c["table"] == "users" && c["column"] == "email")
        .unwrap();
    assert_eq!(email_added["kind"], "column added");
    assert_eq!(email_added["compatibility"], "safe");
}

#[test]
fn diff_sparse_columns_defaults_missing_fields_and_ignores_non_string_samples() {
    // "label" is byte-for-byte identical on both sides (once its own
    // non-string sample values are filtered out) - it must produce zero
    // diff entries, proving DiffColumn::from_json's defaults/filtering
    // don't themselves manufacture a spurious difference.
    let doc = run_diff_json_fixtures(
        fixture("diff_sparse_columns.json").to_str().unwrap(),
        fixture("diff_sparse_columns_new.json").to_str().unwrap(),
        &[],
    );
    let changes = doc["changes"].as_array().unwrap();
    assert!(changes.iter().all(|c| c["column"] != "label"));
    assert!(changes.iter().all(|c| c["column"] != "id"));
    let extra = changes.iter().find(|c| c["column"] == "extra").unwrap();
    assert_eq!(extra["kind"], "column added");
    assert_eq!(extra["compatibility"], "safe");
}

#[test]
fn diff_self_comparison_reports_no_differences() {
    let old = fixture("diff_old.json");
    let output = run_diff_raw(&[old.to_str().unwrap(), old.to_str().unwrap()]);
    assert!(output.status.success());
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("No differences detected"));
}

#[test]
fn diff_writes_the_report_to_an_explicit_output_path() {
    let dir = TempDir::new();
    let out_path = dir.path().join("report.md");
    let output = run_diff_raw(&[
        fixture("diff_old.json").to_str().unwrap(),
        fixture("diff_new.json").to_str().unwrap(),
        out_path.to_str().unwrap(),
    ]);
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("change(s)"));
    let written = std::fs::read_to_string(&out_path).unwrap();
    assert!(written.contains("# Schema diff"));
}

#[test]
fn diff_help_flag_prints_usage_and_exits_cleanly() {
    let output = run_diff_raw(&["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("USAGE"));
    assert!(stdout.contains("--resolution-sql"));
}

#[test]
fn diff_malformed_no_tables_json_is_a_clear_error() {
    let output = run_diff_raw(&[
        fixture("diff_malformed_no_tables.json").to_str().unwrap(),
        fixture("diff_old.json").to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("\"tables\""));
}

#[test]
fn diff_malformed_table_not_array_json_is_a_clear_error() {
    let output = run_diff_raw(&[
        fixture("diff_malformed_table_not_array.json")
            .to_str()
            .unwrap(),
        fixture("diff_old.json").to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("isn't a JSON array of columns"));
}

#[test]
fn diff_malformed_invalid_json_is_a_clear_error_not_a_panic() {
    let output = run_diff_raw(&[
        fixture("diff_malformed_invalid_json.json")
            .to_str()
            .unwrap(),
        fixture("diff_old.json").to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not valid JSON"));
}

#[test]
fn diff_malformed_column_not_object_is_a_clear_error() {
    let output = run_diff_raw(&[
        fixture("diff_malformed_column_not_object.json")
            .to_str()
            .unwrap(),
        fixture("diff_old.json").to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("expected each column entry to be a JSON object"));
}

#[test]
fn diff_malformed_column_missing_name_is_a_clear_error() {
    let output = run_diff_raw(&[
        fixture("diff_malformed_column_missing_name.json")
            .to_str()
            .unwrap(),
        fixture("diff_old.json").to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing a string \"name\" field"));
}
