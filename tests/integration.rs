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

/// A Python interpreter for the handful of tests that generate a fixture
/// on the fly (numpy/pyreadstat/zstd/gzip via `-c`). Tries `python3`
/// first, falling back to `python`: Windows never provides the `python3`
/// alias (python.org installer, Store, and setup-python all ship
/// `python.exe`), so hardcoding `python3` breaks every one of those tests
/// there. One probe process per call is negligible next to the fixture
/// generation itself.
fn python() -> Command {
    let probe = Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if probe {
        Command::new("python3")
    } else {
        Command::new("python")
    }
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

    // numeric_stats: a scalar top-level i64 column, a nested-object i64
    // column pooled across every record's own `events` array, and a
    // three-levels-deep i64 leaf - all real, verified min/max/mean, and
    // never present on a non-numeric column (`tags`, `Vec<UUID>`).
    assert_eq!(column(cols, "id")["numeric_stats"]["mean"], 2.0);
    let amount_stats = &amount["numeric_stats"];
    assert_eq!(amount_stats["count"], 4);
    assert_eq!(amount_stats["min"], 20.0);
    assert_eq!(amount_stats["max"], 99.0);
    assert_eq!(amount_stats["mean"], 61.0);
    assert_eq!(score["numeric_stats"]["mean"], 2.5);
    assert!(tags["numeric_stats"].is_null());
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
fn sql_output_inline_mode_json_writes_child_tables_for_arrays_of_objects() {
    // `events` is an array of objects and `deep.outer.inner_list` one
    // nested under a plain object: each is a child table of the records.
    // `mixed_list` holds scalars and objects, so it keeps a column of
    // JSON text and its objects get a table too.
    let sql = run_sql("nested_typed.jsonl", &["--sql-mode", "inline"]);
    assert_child_table(&sql, "nested_typed", "nested_typed__events");
    assert_child_table(&sql, "nested_typed", "nested_typed__deep_outer_inner_list");
    assert_child_table(&sql, "nested_typed", "nested_typed__mixed_list");
    assert_eq!(
        insert_rows(&sql, "nested_typed__events"),
        [
            "(1, 1, 'alice@example.com', 50, '2024-01-15')",
            "(1, 2, 'bob@example.com', 75, '2024-02-20')",
            "(2, 1, 'carol@example.com', 20, '2024-03-11')",
            "(3, 1, 'dave@example.com', 99, '2024-04-01')"
        ]
    );
    assert_eq!(
        insert_rows(&sql, "nested_typed__deep_outer_inner_list"),
        ["(1, 1, 1)", "(1, 2, 2)", "(2, 1, 3)", "(3, 1, 4)"]
    );
    assert_eq!(
        insert_rows(&sql, "nested_typed__mixed_list"),
        ["(1, 1, 1)", "(3, 1, 2)"]
    );
    assert!(sql.contains("'[1,{\"x\":1}]'"), "{sql}");
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
fn sql_output_inline_mode_rewrites_dates_as_iso_literals_using_the_columns_own_format() {
    // Each column resolved to its own format (`15/01/2024` is day-first
    // only because 20/02/2024 can't be a month), and every value is
    // rewritten as the ISO literal PostgreSQL, MySQL, DuckDB and SQLite
    // all read the same way - not the source text, which most of them
    // reject or misread.
    let sql = run_sql("edge_csv_date_formats.csv", &[]);
    assert!(
        sql.contains("('2024-01-15', '2024-01-15', '2024-01-15', '2024-01-15 10:00:00')"),
        "{sql}"
    );
    assert!(
        sql.contains("('2024-02-20', '2024-02-20', '2024-02-20', '2024-02-20 11:30:00')"),
        "{sql}"
    );
}

#[test]
fn sql_output_inline_mode_shortens_identifiers_past_the_engines_limit() {
    // PostgreSQL silently truncates a name past 63 bytes (two long names
    // with a shared prefix then collide) and MySQL rejects it, so the SQL
    // cuts it and appends a hash of the whole name instead.
    let sql = run_sql("edge_csv_very_long_header.csv", &[]);
    let long = "x".repeat(200);
    assert!(!sql.contains(&long));
    assert!(sql.contains("\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx_aecd5c45\""));
}

#[test]
fn load_into_directory_mode_takes_postgres_and_mysql_but_not_an_output_dir() {
    // A server engine creates a database per file on the server; nothing
    // is written to disk, so `--output-dir` has no meaning. (No test here
    // spawns `psql`/`mysql` - that needs a server - so this only checks
    // the validation that runs before any process is started.)
    let dir = TempDir::new();
    std::fs::copy(fixture("sample.csv"), dir.path().join("sample.csv")).unwrap();
    for target in ["postgres:mydb", "mysql:mydb"] {
        let output = Command::new(bin())
            .args([
                dir.path().to_str().unwrap(),
                "--output-format",
                "sql",
                "--load-into",
                target,
                "--output-dir",
                "/tmp/whatever-output-dir",
            ])
            .output()
            .expect("failed to run binary");
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("nothing is written to disk"),
            "{target}: {stderr}"
        );
    }
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
fn sql_output_inline_mode_orc_flattens_nested_columns() {
    // A Struct flattens into dotted columns and a List of scalars becomes
    // JSON-array text, the same as every other nested format's SQL.
    let sql = run_sql("edge_orc_edge_cases.orc", &[]);
    assert!(
        sql.contains("\"metadata.active\" BOOLEAN NOT NULL"),
        "{sql}"
    );
    assert!(
        sql.contains(
            "(1, 'Alice', 95.5, '2024-01-15 00:00:00.000000000', '[\"a\",\"b\"]', TRUE, 10)"
        ),
        "{sql}"
    );

    // A Map (a list of key/value entries) is a child table with one row
    // per entry, and so is a list of structs.
    let sql = run_sql("edge_orc_nested.orc", &["--sql-mode", "inline"]);
    assert_child_table(&sql, "edge_orc_nested", "edge_orc_nested__attrs");
    assert_eq!(
        insert_rows(&sql, "edge_orc_nested__attrs"),
        ["(1, 1, 1, 'a')", "(1, 2, 2, 'b')", "(4, 1, 3, NULL)"]
    );
    assert_child_table(&sql, "edge_orc_nested", "edge_orc_nested__events");
    assert_eq!(
        insert_rows(&sql, "edge_orc_nested__events"),
        ["(1, 1, 'x', 1)", "(3, 1, 'y', 2)", "(3, 2, NULL, 3)"]
    );
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
fn sql_output_inline_mode_sqlite_reads_a_without_rowid_table() {
    // A WITHOUT ROWID table is stored as an index b-tree (entries lead
    // with the primary key); its rows come out in declared column order.
    let sql = run_sql("edge_sqlite_without_rowid_multi.sqlite", &[]);
    // PRIMARY KEY (c DESC, a): stored as (c, a, b, d), read back as (a, b, c, d).
    assert_eq!(
        insert_rows(&sql, "comp"),
        [
            "(3, NULL, 9.25, 'r')",
            "(1, 'x', 2.5, 'p')",
            "(1, 'z', 0.5, 's')",
            "(2, 'y', 0.5, NULL)"
        ]
    );
    // A multi-level index b-tree, with one entry whose payload spills into
    // overflow pages.
    let big = insert_rows(&sql, "big");
    assert_eq!(big.len(), 1200);
    assert!(big[0].starts_with("(1, 'n1-n1-n1-', 0.25)"), "{}", big[0]);
    assert!(big[599].contains(&"L".repeat(5000)));
    assert!(big[1199].starts_with("(1200, "), "{}", big[1199]);
    assert_eq!(
        insert_rows(&sql, "quoted name"),
        ["('k1', 1)", "('k2', NULL)"]
    );
    assert_eq!(insert_rows(&sql, "plain"), ["(1, 'a')", "(2, 'b')"]);
    // --nrows bounds an index b-tree walk too.
    let sql = run_sql("edge_sqlite_without_rowid_multi.sqlite", &["--nrows", "3"]);
    assert_eq!(insert_rows(&sql, "big").len(), 3);
    assert_eq!(insert_rows(&sql, "comp").len(), 3);
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
    // An array of pickled Python objects has no honest literal to emit -
    // a clear, actionable error, not a guess.
    let output = Command::new(bin())
        .args([
            fixture("edge_npz_object_array_and_labels.npz")
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
    assert!(stderr.contains("pickled 'object' dtype"));
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
fn sql_output_inline_mode_ini_pools_a_repeated_key_into_json_array_text() {
    // A repeated key pools into a Vec<T> column for profiling; in SQL it
    // is one JSON-array text cell, like a repeated vCard/MBOX property.
    let sql = run_sql("sample.ini", &[]);
    let rows = insert_rows(&sql, "database");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("'[\""), "{rows:?}");
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
fn sql_output_inline_mode_toml_writes_a_child_table_for_an_array_of_tables() {
    let sql = run_sql("sample.toml", &["--sql-mode", "inline"]);
    assert_child_table(&sql, "sample", "sample__servers");
    assert_eq!(
        insert_rows(&sql, "sample"),
        ["(1, 'sample config', 3, TRUE, 'Alice', '02134')"]
    );
    assert_eq!(
        insert_rows(&sql, "sample__servers"),
        [
            "(1, 1, 'alpha', '10.0.0.1', TRUE)",
            "(1, 2, 'beta', '10.0.0.2', FALSE)"
        ]
    );
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
    // Dates are rewritten as ISO literals every engine reads the same way.
    assert!(sql.contains("'2024-01-15 10:00:00.000'"));
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
    // The full name is 76 bytes, past the 63 PostgreSQL keeps, so the SQL
    // cuts it and appends a hash of the whole name (the JSON output keeps
    // the real one).
    assert!(sql.contains("\"level9.level8.level7.level6.level5.level4.level3.level_f32830f7\""));
    assert!(sql.contains("'deep'"));
}

#[test]
#[cfg(feature = "xml")]
fn sql_output_inline_mode_xml_writes_a_child_table_for_repeated_child_elements() {
    let sql = run_sql(
        "edge_xml_sql_inline_array_of_objects.xml",
        &["--sql-mode", "inline"],
    );
    assert_child_table(
        &sql,
        "edge_xml_sql_inline_array_of_objects",
        "edge_xml_sql_inline_array_of_objects__order",
    );
    assert_eq!(
        insert_rows(&sql, "edge_xml_sql_inline_array_of_objects"),
        ["(1, 1, 'Alice')", "(2, 2, 'Bob')"]
    );
    assert_eq!(
        insert_rows(&sql, "edge_xml_sql_inline_array_of_objects__order"),
        ["(1, 1, 10)", "(1, 2, 20)", "(2, 1, 5)"]
    );
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
fn sql_output_inline_mode_bson_writes_a_child_table_for_an_array_of_documents() {
    let sql = run_sql(
        "edge_bson_sql_inline_array_of_objects.bson",
        &["--sql-mode", "inline"],
    );
    assert_child_table(
        &sql,
        "edge_bson_sql_inline_array_of_objects",
        "edge_bson_sql_inline_array_of_objects__orders",
    );
    assert_eq!(
        insert_rows(&sql, "edge_bson_sql_inline_array_of_objects"),
        ["(1, 1)"]
    );
    assert_eq!(
        insert_rows(&sql, "edge_bson_sql_inline_array_of_objects__orders"),
        ["(1, 1, 10)", "(1, 2, 20)"]
    );
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
fn sql_output_inline_mode_plist_writes_a_child_table_for_an_array_of_dicts() {
    let sql = run_sql(
        "edge_plist_sql_inline_array_of_objects.plist",
        &["--sql-mode", "inline"],
    );
    assert_child_table(
        &sql,
        "edge_plist_sql_inline_array_of_objects",
        "edge_plist_sql_inline_array_of_objects__orders",
    );
    assert_eq!(
        insert_rows(&sql, "edge_plist_sql_inline_array_of_objects"),
        ["(1, 1)"]
    );
    assert_eq!(
        insert_rows(&sql, "edge_plist_sql_inline_array_of_objects__orders"),
        ["(1, 1, 10)", "(1, 2, 20)"]
    );
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
fn sql_output_inline_mode_json5_writes_a_child_table_for_an_array_of_objects() {
    let sql = run_sql(
        "edge_json5_sql_inline_array_of_objects.json5",
        &["--sql-mode", "inline"],
    );
    assert_child_table(
        &sql,
        "edge_json5_sql_inline_array_of_objects",
        "edge_json5_sql_inline_array_of_objects__orders",
    );
    assert_eq!(
        insert_rows(&sql, "edge_json5_sql_inline_array_of_objects"),
        ["(1, 1)"]
    );
    assert_eq!(
        insert_rows(&sql, "edge_json5_sql_inline_array_of_objects__orders"),
        ["(1, 1, 10)", "(1, 2, 20)"]
    );
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
fn sql_output_inline_mode_har_writes_a_child_table_for_an_array_of_objects() {
    let sql = run_sql(
        "edge_har_sql_inline_array_of_objects.har",
        &["--sql-mode", "inline"],
    );
    assert_child_table(
        &sql,
        "edge_har_sql_inline_array_of_objects",
        "edge_har_sql_inline_array_of_objects__cookies",
    );
    assert_eq!(
        insert_rows(&sql, "edge_har_sql_inline_array_of_objects"),
        ["(1, 'https://example.com/x')"]
    );
    assert_eq!(
        insert_rows(&sql, "edge_har_sql_inline_array_of_objects__cookies"),
        ["(1, 1, 'a')", "(1, 2, 'b')"]
    );
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
fn sql_output_inline_mode_geojson_writes_a_child_table_for_an_array_of_objects() {
    let sql = run_sql(
        "edge_geojson_sql_inline_array_of_objects.geojson",
        &["--sql-mode", "inline"],
    );
    assert_child_table(
        &sql,
        "edge_geojson_sql_inline_array_of_objects",
        "edge_geojson_sql_inline_array_of_objects__tags",
    );
    assert_eq!(
        insert_rows(&sql, "edge_geojson_sql_inline_array_of_objects"),
        ["(1, 'POINT(0 0)')"]
    );
    assert_eq!(
        insert_rows(&sql, "edge_geojson_sql_inline_array_of_objects__tags"),
        ["(1, 1, 1)", "(1, 2, 2)"]
    );
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
fn sql_output_inline_mode_parquet_writes_a_child_table_for_a_map_column() {
    // A Map column reconstructs as an array of {"key","value"} pairs, so
    // `nested_types.parquet`'s own `attributes` map is a child table with
    // one row per entry.
    let sql = run_sql("nested_types.parquet", &["--sql-mode", "inline"]);
    assert_child_table(&sql, "nested_types", "nested_types__attributes");
    assert_eq!(
        insert_rows(&sql, "nested_types__attributes"),
        [
            "(1, 1, 'color', 'red')",
            "(1, 2, 'size', 'M')",
            "(2, 1, 'color', 'blue')",
            "(4, 1, 'color', 'green')",
            "(4, 2, 'size', 'L')"
        ]
    );
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
fn sql_output_inline_mode_arrow_ipc_writes_a_child_table_for_an_array_of_structs() {
    let sql = run_sql(
        "edge_arrow_sql_inline_array_of_objects.arrow",
        &["--sql-mode", "inline"],
    );
    assert_child_table(
        &sql,
        "edge_arrow_sql_inline_array_of_objects",
        "edge_arrow_sql_inline_array_of_objects__orders",
    );
    assert_eq!(
        insert_rows(&sql, "edge_arrow_sql_inline_array_of_objects"),
        ["(1, 1)"]
    );
    assert_eq!(
        insert_rows(&sql, "edge_arrow_sql_inline_array_of_objects__orders"),
        ["(1, 1, 1, 9.5)", "(1, 2, 2, 3.0)"]
    );
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
fn json_output_reports_numeric_stats_for_i64_and_f64_columns_only() {
    let doc = run_json("sample.csv", &[]);
    let cols = table(&doc, "sample");

    // A non-numeric column always carries a real, present `numeric_stats`
    // key - just `null` - never an omitted one.
    let user_id = column(cols, "user_id");
    assert_eq!(user_id["ideal_type"], "String");
    assert!(user_id["numeric_stats"].is_null());

    // `age` is i64 with one genuinely missing value (4 of 5 rows) -
    // min/max/mean checked against real, independently-computed values
    // (`pandas.Series.min/max/mean` on the same non-null values).
    let age = column(cols, "age");
    assert_eq!(age["ideal_type"], "i64");
    let age_stats = &age["numeric_stats"];
    assert_eq!(age_stats["count"], 4);
    assert_eq!(age_stats["min"], 29.0);
    assert_eq!(age_stats["max"], 52.0);
    assert_eq!(age_stats["mean"], 40.0);

    // `account_balance` is f64 and includes a thousands-separated value
    // ("5,120.75") - stats must reflect the *cleaned* number
    // (normalize_numeric_str's own output), not fail or truncate on the
    // comma, and must agree with the real value independently recomputed
    // by hand (1250.50, 340.00, 5120.75, 89.20, 12000.00).
    let balance = column(cols, "account_balance");
    assert_eq!(balance["ideal_type"], "f64");
    let balance_stats = &balance["numeric_stats"];
    assert_eq!(balance_stats["count"], 5);
    assert_eq!(balance_stats["min"], 89.2);
    assert_eq!(balance_stats["max"], 12000.0);
    assert!((balance_stats["mean"].as_f64().unwrap() - 3760.09).abs() < 1e-9);

    // median and percentiles: sample.csv's columns all have <= 5 numeric
    // values, so these must be *exact* (`numpy.percentile(..., method=
    // "linear")` on the same values), not just plausible-looking -
    // confirming the real bug this feature's own follow-up pass found
    // and fixed (a 5-value column silently reporting the median as
    // every percentile's own answer) stays fixed.
    assert_eq!(age_stats["median"], 39.5);
    assert_eq!(age_stats["percentiles"]["p25"], 32.75);
    assert_eq!(age_stats["percentiles"]["p75"], 46.75);

    let purchase = column(cols, "purchase_count");
    let purchase_stats = &purchase["numeric_stats"];
    assert_eq!(purchase_stats["median"], 3.0);
    assert_eq!(purchase_stats["percentiles"]["p25"], 1.0);
    assert_eq!(purchase_stats["percentiles"]["p75"], 7.0);
    assert_eq!(purchase_stats["percentiles"]["p90"], 10.0);
    assert_eq!(purchase_stats["percentiles"]["p95"], 11.0);
    assert_eq!(purchase_stats["percentiles"]["p99"], 11.8);
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

// A fully Zip64 archive: every entry's sizes and local-header offset sit
// behind 0xFFFFFFFF sentinels with their real values in the Zip64 extra
// field, and the classic end record's counts point at a Zip64 end record.
// One entry is stored, one deflated. Python's zipfile and NumPy both read
// it; the reader used to refuse any archive with a Zip64 record.
#[cfg(feature = "npy")]
#[test]
fn npz_reads_a_zip64_archive() {
    let doc = run_json("edge_npz_zip64.npz", &[]);
    let ids = column(table(&doc, "ids"), "value");
    assert_eq!(ids["ideal_type"], "i64");
    assert_eq!(ids["row_count"], 5);
    let scores = column(table(&doc, "scores"), "value");
    assert_eq!(scores["ideal_type"], "f64");
    assert_eq!(scores["sample_values"][0], "1.5");
}

// One array that can't be read used to abort the *entire* archive, costing
// every other array in the file its own profile too (found via a real-world
// sweep of TensorFlow's MNIST .npz). An array of pickled Python objects has
// no fixed byte layout; it gets a disclosed placeholder, and the unrelated,
// perfectly ordinary array next to it still profiles normally.
#[cfg(feature = "npy")]
#[test]
fn npz_one_unreadable_array_does_not_sink_the_rest_of_the_archive() {
    let doc = run_json("edge_npz_object_array_and_labels.npz", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(tables.len(), 2, "both arrays must still appear as tables");

    let bad = table(&doc, "bad");
    assert!(
        column(bad, "value")["notes"]
            .as_str()
            .unwrap()
            .contains("could not be profiled"),
        "the object array's own table should disclose why, not silently vanish"
    );
    let labels = table(&doc, "labels");
    assert_eq!(column(labels, "value")["ideal_type"], "i64");
}

// A plain array of 3 or more axes has no positional-column reading, so each
// slice along the first axis is one row: its index, and the slice's values.
// (MNIST's (60000, 28, 28) image arrays used to be refused outright.)
#[cfg(feature = "npy")]
#[test]
fn a_3d_array_is_one_row_per_slice_of_the_first_axis() {
    let doc = run_json("edge_npz_3d_images_and_labels.npz", &[]);
    let images = table(&doc, "images");
    assert_eq!(column(images, "index")["ideal_type"], "i64");
    assert_eq!(column(images, "index")["row_count"], 3);
    let value = column(images, "value");
    assert_eq!(value["ideal_type"], "Vec<i64>");
    assert!(
        value["description"]
            .as_str()
            .unwrap()
            .starts_with("NumPy array of shape (3, 4, 4)"),
        "{}",
        value["description"]
    );
    assert_eq!(column(table(&doc, "labels"), "value")["ideal_type"], "i64");

    // Row-major and column-major files holding the same logical array give
    // the same rows; a 4-D float array nests one level deeper.
    for (file, expect) in [
        ("edge_npy_3d.npy", "(0, '[0,1,2,3,4,5,6,7,8,9,10,11]')"),
        (
            "edge_npy_3d_fortran.npy",
            "(0, '[0,1,2,3,4,5,6,7,8,9,10,11]')",
        ),
    ] {
        let sql = run_sql(file, &[]);
        let name = file.trim_end_matches(".npy");
        let rows = insert_rows(&sql, name);
        assert_eq!(rows.len(), 2, "{file}");
        assert_eq!(rows[0], expect, "{file}");
        assert_eq!(
            rows[1], "(1, '[12,13,14,15,16,17,18,19,20,21,22,23]')",
            "{file}"
        );
        // The shape rides along as the column's comment.
        assert!(sql.contains("-- NumPy array of shape (2, 3, 4)"), "{sql}");
    }
    let sql = run_sql("edge_npy_4d_float.npy", &["--nrows", "2"]);
    assert_eq!(
        insert_rows(&sql, "edge_npy_4d_float"),
        [
            "(0, '[0.0,0.25,0.5,0.75,1.0,1.25,1.5,1.75]')",
            "(1, '[2.0,2.25,2.5,2.75,3.0,3.25,3.5,3.75]')"
        ]
    );
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

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_without_rowid_table_is_profiled_like_any_other() {
    let doc = run_json("edge_sqlite_without_rowid.sqlite", &[]);
    let kv = table(&doc, "kv");
    assert_eq!(
        kv.iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["k", "v"]
    );
    assert_eq!(column(kv, "k")["missing_pct"], 0.0);
    let normal = table(&doc, "normal");
    assert_eq!(column(normal, "id")["ideal_type"], "i64");
    // Key-first storage order doesn't leak: columns keep declared order.
    let doc = run_json("edge_sqlite_without_rowid_multi.sqlite", &[]);
    let comp = table(&doc, "comp");
    assert_eq!(
        comp.iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["a", "b", "c", "d"]
    );
    assert_eq!(column(comp, "c")["ideal_type"], "f64");
    assert_eq!(column(table(&doc, "big"), "id")["row_count"], 1200);
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
        .args([fixture("malformed_ragged.csv").to_str().unwrap(), "-"])
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
        .args([fixture("malformed_invalid_utf8.csv").to_str().unwrap(), "-"])
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
        .args([
            fixture("malformed_deeply_nested.json").to_str().unwrap(),
            "-",
        ])
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
        .args([
            fixture("malformed_deeply_nested.xml").to_str().unwrap(),
            "-",
        ])
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
        .args([
            fixture("malformed_deeply_nested.cbor").to_str().unwrap(),
            "-",
        ])
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
fn spss_zsav_zlib_compression_reads_correctly() {
    // .zsav support was added after this project's own hand-rolled zlib
    // block reader was verified byte-for-byte against the real `ambers`
    // crate (see spss_reader_matches_the_ambers_crate_output_exactly) -
    // this is the full-pipeline confirmation that the compiled binary
    // itself reads a real zlib-compressed file cleanly, not just the
    // in-process reader function.
    let doc = run_json("edge_spss_zlib_compressed.zsav", &[]);
    let cols = table(&doc, "edge_spss_zlib_compressed");
    assert_eq!(cols.len(), 2);
    let id = column(cols, "id");
    assert_eq!(id["ideal_type"], "i64");
    assert_eq!(id["sample_values"], serde_json::json!(["1", "2", "3"]));
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
        .args([fixture(fixture_name).to_str().unwrap(), "-"])
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
        .args([
            fixture("malformed_deeply_nested.msgpack").to_str().unwrap(),
            "-",
        ])
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
        .args([
            fixture("malformed_deeply_nested.toml").to_str().unwrap(),
            "-",
        ])
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

// A dBase file's header code page mark selects a legacy single-byte code
// page (see `resolve_text_mode`); text decodes through it rather than as
// UTF-8. Fixtures written by the `dbf` Python package in each code page.
#[cfg(feature = "dbase")]
#[test]
fn dbase_decodes_text_through_its_marked_code_page() {
    for (file, expected) in [
        ("edge_dbase_cp1252.dbf", vec!["café €5", "Straße", "naïve"]),
        ("edge_dbase_cp866.dbf", vec!["Привет", "Москва", "ёлка"]),
        ("edge_dbase_cp437.dbf", vec!["Müller", "Æble ½", "Ñandú"]),
    ] {
        let doc = run_json(file, &["--samples", "5"]);
        let stem = file.trim_end_matches(".dbf");
        let name = column(table(&doc, stem), "NAME");
        assert_eq!(name["sample_values"], serde_json::json!(expected), "{file}");
    }
    // Marked CP1252 but pure ASCII content: reads normally.
    let doc = run_json("edge_dbase_cp1252_marked_ascii.dbf", &[]);
    assert!(!table(&doc, "edge_dbase_cp1252_marked_ascii").is_empty());
}

// A double-byte East Asian code page (0x7B is Shift-JIS) reads with
// `--features cjk`, and is a disclosed error naming the feature without it.
#[cfg(all(feature = "dbase", feature = "cjk"))]
#[test]
fn dbase_double_byte_code_page_reads_with_the_cjk_feature() {
    let doc = run_json("malformed_dbase_double_byte_codepage.dbf", &[]);
    assert!(!table(&doc, "malformed_dbase_double_byte_codepage").is_empty());
}

#[cfg(all(feature = "dbase", not(feature = "cjk")))]
#[test]
fn dbase_double_byte_code_page_without_cjk_names_the_feature() {
    let output = Command::new(bin())
        .args([
            fixture("malformed_dbase_double_byte_codepage.dbf")
                .to_str()
                .unwrap(),
            "-",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--features cjk"), "got: {stderr}");
}

// A memo field whose .dbt/.fpt file is missing is a clear error naming
// the file it looked for, not a silent drop or a panic.
#[cfg(feature = "dbase")]
#[test]
fn dbase_memo_field_without_its_memo_file_is_a_clear_error() {
    let output = Command::new(bin())
        .args([
            fixture("malformed_dbase_memo_field.dbf").to_str().unwrap(),
            "-",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("malformed_dbase_memo_field.dbt"),
        "{stderr}"
    );
}

// Memo text read from each memo-file layout: dBase III (.dbt, text runs
// across 512-byte blocks to a 0x1A terminator), dBase IV (.dbt, a length
// header per block; hand-built from the published layout), FoxPro 2 and
// Visual FoxPro (.fpt, big-endian block header). The FoxPro 2 file also
// carries a 263-byte backlink without being Visual FoxPro. All but the
// dBase IV file were written by the `dbf` Python package.
#[cfg(feature = "dbase")]
#[test]
fn dbase_reads_memo_fields_from_every_memo_layout() {
    let long = "Lorem ipsum dolor sit amet. ".repeat(40);
    let long = long.trim_end();
    for stem in [
        "edge_dbase_memo_db3",
        "edge_dbase_memo_db4",
        "edge_dbase_memo_foxpro",
        "edge_dbase_memo_vfp",
    ] {
        let doc = run_json(&format!("{stem}.dbf"), &[]);
        let notes = column(table(&doc, stem), "NOTES");
        assert_eq!(
            notes["sample_values"],
            serde_json::json!(["Short note with café", long]),
            "{stem}"
        );
        assert_eq!(notes["missing_pct"], 33.3, "{stem}");
        assert_eq!(notes["current_type"], "String", "{stem}");
    }
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
        .args([fixture("edge_empty_root.xml").to_str().unwrap(), "-"])
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
        .args([fixture("edge_empty_doc.ini").to_str().unwrap(), "-"])
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
    // TOML has no magic number or other structural signal sniff_format
    // looks for (see its doc comment - the same disclosed, deliberate gap
    // TOML/YAML/INI share; delimited tables are recognized by content now),
    // so this must still fail with the same actionable "pass --format"
    // error it always has, not a wrong guess.
    let (_dir, dest) = copy_fixture_as("sample.toml", "mystery_data");
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
fn csv_semicolon_delimited_is_detected_and_the_flag_still_wins() {
    // The dialect detector finds the semicolons with no flag at all.
    let doc_default = run_json("edge_csv_semicolon.csv", &[]);
    let cols_default = table(&doc_default, "edge_csv_semicolon");
    assert_eq!(
        cols_default.len(),
        4,
        "the delimiter should be detected as ';'"
    );

    // Forcing the comma shows what --delimiter overrides: one column.
    let doc_comma = run_with_format("edge_csv_semicolon.csv", "json", &["--delimiter", ","]);
    assert_eq!(table(&doc_comma, "edge_csv_semicolon").len(), 1);

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

// --- CSV dialect detection (the data consistency measure) ---

fn column_names(doc: &serde_json::Value, tbl: &str) -> Vec<String> {
    table(doc, tbl)
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn csv_dialect_decimal_commas_do_not_hide_the_semicolons() {
    // 1,50 is a price, not two fields.
    let doc = run_json("edge_csv_dialect_semicolon_decimal_comma.csv", &[]);
    let tbl = "edge_csv_dialect_semicolon_decimal_comma";
    assert_eq!(column_names(&doc, tbl), ["item", "price", "qty", "shipped"]);
    assert_eq!(sample_names(&doc, tbl, "price"), ["1,50", "22,75", "0,99"]);
}

#[test]
fn csv_dialect_single_quotes_and_pipes_are_found() {
    let doc = run_json("edge_csv_dialect_pipe_single_quote.csv", &[]);
    let tbl = "edge_csv_dialect_pipe_single_quote";
    assert_eq!(column_names(&doc, tbl), ["id", "text", "note"]);
    // The pipes inside the quotes stay in the cell.
    assert_eq!(
        sample_names(&doc, tbl, "text"),
        ["hello | world", "plain", "a|b|c"]
    );
}

#[test]
fn csv_dialect_backslash_escapes_keep_the_comma() {
    let doc = run_json("edge_csv_dialect_backslash_escape.csv", &[]);
    let tbl = "edge_csv_dialect_backslash_escape";
    assert_eq!(column_names(&doc, tbl), ["id", "name", "city"]);
    assert_eq!(
        sample_names(&doc, tbl, "name"),
        ["Smith, John", "Doe, Jane", "Ng, Wei"]
    );
}

#[test]
fn csv_dialect_a_list_of_names_is_one_column_not_a_space_delimited_table() {
    let doc = run_json("edge_csv_dialect_single_column.csv", &[]);
    let tbl = "edge_csv_dialect_single_column";
    assert_eq!(column_names(&doc, tbl), ["Robert Plant"]);
}

#[test]
fn csv_dialect_the_delimiter_flag_overrides_detection() {
    let doc = run_with_format(
        "edge_csv_dialect_semicolon_decimal_comma.csv",
        "json",
        &["--delimiter", ","],
    );
    // Forced to commas, the decimal commas split the prices.
    assert!(column_names(&doc, "edge_csv_dialect_semicolon_decimal_comma").len() > 1);
    assert_ne!(
        column_names(&doc, "edge_csv_dialect_semicolon_decimal_comma"),
        ["item", "price", "qty", "shipped"]
    );
}

#[test]
fn delimited_text_under_other_extensions_is_recognized_by_content() {
    for (file, tbl, cols) in [
        (
            "edge_dialect_pipe_table.txt",
            "edge_dialect_pipe_table",
            vec!["id", "name", "score"],
        ),
        (
            "edge_dialect_semicolon_table.dat",
            "edge_dialect_semicolon_table",
            vec!["a", "b"],
        ),
        (
            "edge_dialect_tab_table.tab",
            "edge_dialect_tab_table",
            vec!["x", "y", "z"],
        ),
        (
            "edge_dialect_pipe_table.psv",
            "edge_dialect_pipe_table",
            vec!["k", "v"],
        ),
        // No extension at all.
        (
            "edge_dialect_pipe_table_noext",
            "edge_dialect_pipe_table_noext",
            vec!["id", "name", "score"],
        ),
    ] {
        let doc = run_json(file, &[]);
        assert_eq!(column_names(&doc, tbl), cols, "{file}");
    }
}

#[test]
fn prose_in_a_text_file_is_still_not_mistaken_for_a_table() {
    let out = Command::new(bin())
        .args([fixture("edge_dialect_prose.txt").to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("can't infer format"));
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
    assert!(!name["sample_values"].as_array().unwrap().is_empty());
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

    // CSV without an extension is recognized by its content: the same rows
    // split into the same columns under a common delimiter.
    let doc = run_json("edge_sniff_csv_no_ext", &[]);
    assert_eq!(doc["format"], "csv");
    assert!(doc["tables"]["edge_sniff_csv_no_ext"].as_array().is_some());
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
fn orc_edge_cases_flatten_nested_columns() {
    let doc = run_json("edge_orc_edge_cases.orc", &[]);
    let cols = table(&doc, "edge_orc_edge_cases");
    assert_eq!(column(cols, "score")["missing_pct"].as_f64().unwrap(), 33.3);
    assert_eq!(column(cols, "tags")["current_type"], "List");
    assert_eq!(column(cols, "tags")["ideal_type"], "Vec<String>");
    assert_eq!(column(cols, "metadata")["current_type"], "Struct");
    assert_eq!(column(cols, "metadata.active")["ideal_type"], "bool");
    assert_eq!(column(cols, "metadata.count")["ideal_type"], "i64");
}

// tests/fixtures/edge_orc_nested.orc (and its ZLIB twin) was written by
// pyarrow.orc: a nullable struct with null fields, a list of doubles with
// a null list and a null element, a list of lists, an int-keyed map with
// a null value, and a list of structs. Expected values are pyarrow's own
// read of the same file.
#[cfg(feature = "orc")]
#[test]
fn orc_nested_columns_match_pyarrow() {
    for name in ["edge_orc_nested", "edge_orc_nested_zlib"] {
        let doc = run_json(&format!("{name}.orc"), &[]);
        let cols = table(&doc, name);
        let addr = column(cols, "addr");
        assert_eq!(addr["current_type"], "Struct");
        assert_eq!(addr["missing_pct"].as_f64().unwrap(), 25.0);
        assert_eq!(
            column(cols, "addr.city")["sample_values"],
            serde_json::json!(["Paris", "Oslo"])
        );
        assert_eq!(column(cols, "addr.zip")["ideal_type"], "i64");
        assert_eq!(column(cols, "scores")["ideal_type"], "Vec<f64>");
        assert_eq!(
            column(cols, "grid")["sample_values"],
            serde_json::json!(["1", "2", "3"])
        );
        assert_eq!(column(cols, "attrs")["current_type"], "Map");
        assert_eq!(
            column(cols, "attrs.key")["sample_values"],
            serde_json::json!(["1", "2", "3"])
        );
        assert_eq!(
            column(cols, "attrs.value")["missing_pct"].as_f64().unwrap(),
            33.3
        );
        assert_eq!(
            column(cols, "events.kind")["sample_values"],
            serde_json::json!(["x", "y"])
        );
        assert_eq!(column(cols, "events.n")["ideal_type"], "i64");
    }
}

// tests/fixtures/edge_orc_union.orc: pyarrow.orc's write of a sparse
// union [1 (int), "b" (string), 3 (int)]. A union row is its variant's
// own value.
#[cfg(feature = "orc")]
#[test]
fn orc_union_column_reads_each_rows_variant() {
    let doc = run_json("edge_orc_union.orc", &[]);
    let u = column(table(&doc, "edge_orc_union"), "u");
    assert_eq!(u["current_type"], "Union");
    assert_eq!(u["sample_values"], serde_json::json!(["1", "b", "3"]));
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
    // Without --format a `.txt` that holds a consistent table is still read
    // as one (see `looks_like_delimited_table`); prose is not.
    let output2 = Command::new(bin())
        .args([misnamed.to_str().unwrap(), "-", "--output-format", "json"])
        .output()
        .unwrap();
    assert!(output2.status.success());
    let prose = dir.path().join("notes.txt");
    std::fs::write(&prose, "A line of prose, with a comma.\nAnother, longer line, with two commas.\nAnd a third.\nFin.\n").unwrap();
    let output3 = Command::new(bin())
        .args([prose.to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!output3.status.success());
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
fn spss_zsav_reads_with_json_schema_output_alongside_a_plain_sav() {
    let doc_zsav = run_with_format("edge_spss_zlib_compressed.zsav", "json-schema", &[]);
    assert!(doc_zsav["tables"]["edge_spss_zlib_compressed"]["properties"].is_object());

    let doc_sav = run_with_format("edge_spss_edge_cases.sav", "json-schema", &[]);
    assert!(doc_sav["tables"]["edge_spss_edge_cases"]["properties"].is_object());
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
    let out = python().args(["-c", &py_code]).output().unwrap();
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
fn yaml_merge_keys_resolve_through_aliases() {
    let doc = run_json("edge_yaml_merge.yaml", &[]);
    let cols = table(&doc, "edge_yaml_merge");
    assert_eq!(
        column(cols, "development.adapter")["sample_values"][0],
        "postgres"
    );
    assert_eq!(
        column(cols, "development.database")["sample_values"][0],
        "dev_db"
    );
    // An explicit key wins over the merged one.
    assert_eq!(
        column(cols, "production.host")["sample_values"][0],
        "prod.example.com"
    );
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_anchor_alone_on_its_line_names_the_block_below() {
    // PyYAML reads this as {name, base: {x, y}, copy: {x, y}}.
    let doc = run_json("edge_yaml_root_anchor.yaml", &[]);
    let cols = table(&doc, "edge_yaml_root_anchor");
    assert_eq!(column(cols, "name")["sample_values"][0], "top");
    assert_eq!(column(cols, "base.x")["sample_values"][0], "1");
    assert_eq!(column(cols, "copy.y")["sample_values"][0], "two");
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
fn csv_trailing_comma_is_ragged_error_and_tsv_via_csv_ext_is_detected() {
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

    // TSV content in a .csv file is detected as tab-delimited; forcing a comma
    // collapses it to one column.
    let doc_default = run_json("edge_tsv_via_csv_ext.csv", &[]);
    assert_eq!(table(&doc_default, "edge_tsv_via_csv_ext").len(), 3);
    let doc_comma = run_with_format("edge_tsv_via_csv_ext.csv", "json", &["--delimiter", ","]);
    assert_eq!(table(&doc_comma, "edge_tsv_via_csv_ext").len(), 1);
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
    let out = python().args(["-c", &py_code]).output().unwrap();
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
    let out = python().args(["-c", &py_code]).output().unwrap();
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
    let out = python().args(["-c", &py_code]).output().unwrap();
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
    // A leading UTF-8 BOM is stripped before the INI reader runs, so the
    // first section header still parses (it used to be a hard error).
    let doc = run_json("edge_ini_bom.ini", &[]);
    let s = table(&doc, "section");
    assert!(s.iter().any(|c| c["name"] == "key"));

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
    // "orders"/"payments" deliberately share no column names - a genuine
    // table rename (same columns, different name) is covered by its own
    // dedicated test/fixture below; this one stays a clean check of
    // plain by-name matching plus genuine add/remove for two unrelated
    // tables.
    let old = write_dictionary(
        dir.path(),
        "old.ini",
        "[users]\nid=1\nname=alice\n\n[orders]\norder_ref=A1\n",
    );
    let new = write_dictionary(
        dir.path(),
        "new.ini",
        "[users]\nid=1\nname=alice\nemail=alice@example.com\n\n[payments]\npayment_ref=P1\n",
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
    // "orders" -> "payments" share the exact same two columns (id,
    // amount) in these fixtures - a real table rename, not a genuine
    // drop+add, and table-rename detection now correctly reports it as
    // one suggestion instead of two unrelated table-level entries.
    let rename = changes
        .iter()
        .find(|c| c["kind"] == "possible table rename")
        .expect("expected a table rename to be detected between orders and payments");
    assert_eq!(rename["from"], "orders");
    assert_eq!(rename["to"], "payments");
    assert_eq!(rename["compatibility"], "safe");
    assert!(!changes.iter().any(|c| matches!(
        c["kind"].as_str(),
        Some("table removed") | Some("table added")
    )));
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
fn diff_json_with_no_tables_key_is_treated_as_raw_data_to_profile() {
    // `diff_malformed_no_tables.json` ({"foo": "bar", "not_a_dictionary":
    // true}) is well-formed JSON with no "tables" key - not a broken
    // dictionary, just an ordinary JSON data file. `diff` now profiles
    // it fresh (the same "diff accepts a raw data file" capability
    // proven directly below) instead of rejecting it.
    let output = run_diff_raw(&[
        fixture("diff_malformed_no_tables.json").to_str().unwrap(),
        fixture("diff_old.json").to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("foo") && report.contains("column"));
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

// --- `sniff-rs diff` accepting a raw data file directly, not just a
// pre-generated --output-format json dictionary ---

#[test]
fn diff_accepts_two_raw_csv_files_directly_with_no_pregenerated_json() {
    let dir = TempDir::new();
    let old = dir.path().join("old.csv");
    let new = dir.path().join("new.csv");
    std::fs::write(
        &old,
        "id,name,email\n1,alice,alice@example.com\n2,bob,bob@example.com\n",
    )
    .unwrap();
    std::fs::write(
        &new,
        "id,full_name,email\n1,alice,alice@example.com\n2,bob,bob@example.com\n",
    )
    .unwrap();

    let output = run_diff_raw(&[old.to_str().unwrap(), new.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(
        report.contains("name -> full_name") && report.contains("possible rename"),
        "expected a real rename detected straight from two raw CSV files:\n{report}"
    );
}

#[test]
fn diff_accepts_a_mix_of_a_dictionary_and_a_raw_file() {
    let dir = TempDir::new();
    let old_csv = dir.path().join("old.csv");
    let new_csv = dir.path().join("new.csv");
    std::fs::write(&old_csv, "id,name\n1,alice\n2,bob\n").unwrap();
    std::fs::write(&new_csv, "id,name,email\n1,alice,a@x.com\n2,bob,b@x.com\n").unwrap();
    let old_json = write_dictionary(dir.path(), "old_pre.csv", "id,name\n1,alice\n2,bob\n");

    let output = run_diff_raw(&[old_json.to_str().unwrap(), new_csv.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("email") && report.contains("column added"));
    let _ = old_csv; // only used to make the fixture pair explicit; not a diff input here
}

// A compressed (.json.gz) dictionary input is deliberately not covered
// by an automated test here - this project has no gzip *encoder*
// anywhere (only the hand-rolled decoder `--features zstd`/gzip
// support needs), and spawning a real `gzip` CLI to build one on the
// fly would add an external-tool dependency this test suite doesn't
// otherwise have (matching the same "don't assume a tool is on PATH"
// discipline already applied to `--load-into`'s own real-database-CLI
// tests). Verified manually instead: `sniff-rs diff old.json.gz new.csv`
// against a real gzip-compressed dictionary correctly decompressed it
// and diffed against the raw CSV - the exact case a real bug (this
// function's own `read_path` vs. `path` mixup, caught before shipping)
// would otherwise still be silently broken by.

#[test]
fn diff_compares_two_directories_and_a_directory_against_a_combine_dictionary() {
    let dir = TempDir::new();
    let (old, new) = (dir.path().join("old"), dir.path().join("new"));
    for d in [&old, &new] {
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/b.csv"), "k,v\n1,x\n").unwrap();
    }
    std::fs::write(old.join("a.csv"), "id,name\n1,a\n2,b\n").unwrap();
    std::fs::write(new.join("a.csv"), "id,name,extra\n1,a,q\n2,b,r\n").unwrap();
    std::fs::write(new.join("c.csv"), "z\n1\n").unwrap();

    let output = run_diff_raw(&[old.to_str().unwrap(), new.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    // Tables are named the way --combine names them.
    assert!(report.contains("extra") && report.contains("column added"));
    assert!(report.contains("table added") && report.contains("c\\_\\_c"));
    assert!(report.contains("sub_b\\_\\_b") && report.contains("unchanged"));

    // The same old snapshot saved as a --combine dictionary diffs identically.
    let saved = dir.path().join("old.json");
    let status = std::process::Command::new(bin())
        .args([
            old.to_str().unwrap(),
            "--combine",
            "--output-format",
            "json",
        ])
        .arg(&saved)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let against_saved = run_diff_raw(&[saved.to_str().unwrap(), new.to_str().unwrap()]);
    let saved_report = String::from_utf8(against_saved.stdout).unwrap();
    assert!(saved_report.contains("extra") && saved_report.contains("c\\_\\_c"));

    // A directory with nothing recognizable is an error, not an empty diff.
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let output = run_diff_raw(&[empty.to_str().unwrap(), new.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no recognized files"));
}

// --- Delta Lake table awareness (--features delta) ---
//
// tests/fixtures/edge_delta_table is a real, committed Delta table -
// two Hive-style partitions (category=a/category=b), one genuinely
// missing value each in `name`/`score` - generated with the real
// `deltalake` (delta-rs) Python package, the same "generate via a real
// tool, commit the fixture" convention every other format's own
// `type_detection.<ext>`/`sample.<ext>` fixtures already follow. Every
// expected value below was independently cross-checked against that
// same `deltalake` package's own `DeltaTable(...).to_pandas()` read
// (`id`: [1,2,3,4,5]; `name`: [alice,bob,carol,dave,None]; `score`:
// [10.5,20.0,30.25,None,50.75]; `category`: [a,a,b,b,b]) before being
// hardcoded here, not assumed correct from this reader's own output.

#[cfg(feature = "delta")]
#[test]
fn delta_table_resolves_schema_and_merges_rows_across_partitions() {
    let doc = run_json("edge_delta_table", &[]);
    assert_eq!(doc["format"], "delta");
    let cols = table(&doc, "edge_delta_table");

    let id = column(cols, "id");
    assert_eq!(id["current_type"], "long");
    assert_eq!(id["ideal_type"], "i64");
    assert_eq!(id["row_count"], 5);
    assert_eq!(id["missing_pct"], 0.0);
    let id_stats = &id["numeric_stats"];
    assert_eq!(id_stats["count"], 5);
    assert_eq!(id_stats["min"], 1.0);
    assert_eq!(id_stats["max"], 5.0);
    assert_eq!(id_stats["mean"], 3.0);

    // A genuinely missing (null) value in one partition - proves the
    // reader isn't just reading one file and calling it done.
    let name = column(cols, "name");
    assert_eq!(name["missing_pct"], 20.0);

    // `score` mixes a value from each of the two physical Parquet files
    // with one genuinely missing value - min/max/mean must reflect all
    // 4 real non-null values merged across both files, not just one.
    let score = column(cols, "score");
    assert_eq!(score["ideal_type"], "f64");
    let score_stats = &score["numeric_stats"];
    assert_eq!(score_stats["count"], 4);
    assert_eq!(score_stats["min"], 10.5);
    assert_eq!(score_stats["max"], 50.75);
    assert!((score_stats["mean"].as_f64().unwrap() - 27.875).abs() < 1e-9);

    // `category` is the partition column - its value never appears
    // inside either Parquet file's own content at all, only in the
    // transaction log's own `add.partitionValues` and the directory
    // name itself, so a correct answer here proves partition-value
    // resolution actually works, not just plain Parquet-content reading.
    let category = column(cols, "category");
    assert_eq!(category["missing_pct"], 0.0);
    let category_samples: Vec<&str> = category["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(category_samples, vec!["a", "b"]);
}

#[cfg(feature = "delta")]
#[test]
fn delta_table_nrows_bounds_the_total_row_count_across_every_file() {
    let doc = run_json("edge_delta_table", &["--nrows", "2"]);
    let cols = table(&doc, "edge_delta_table");
    for name in ["id", "name", "score", "category"] {
        assert_eq!(
            column(cols, name)["row_count"],
            2,
            "column {name} should be bounded to 2 rows"
        );
    }
}

#[cfg(feature = "delta")]
#[test]
fn delta_table_emits_inline_sql_and_rejects_combine() {
    let path = fixture("edge_delta_table");

    let sql_output = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--output-format", "sql"])
        .output()
        .expect("failed to run binary");
    assert!(
        sql_output.status.success(),
        "{}",
        String::from_utf8_lossy(&sql_output.stderr)
    );
    let sql = String::from_utf8_lossy(&sql_output.stdout);
    assert!(sql.contains("CREATE TABLE \"edge_delta_table\""), "{sql}");
    assert!(sql.contains("INSERT INTO \"edge_delta_table\""), "{sql}");

    let staging = Command::new(bin())
        .args([
            path.to_str().unwrap(),
            "-",
            "--output-format",
            "sql",
            "--sql-mode",
            "staging",
        ])
        .output()
        .expect("failed to run binary");
    assert!(staging.status.success());
    assert!(String::from_utf8_lossy(&staging.stdout).contains("delta_scan("));

    let combine_output = Command::new(bin())
        .args([
            path.to_str().unwrap(),
            "-",
            "--output-format",
            "json",
            "--combine",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!combine_output.status.success());
    assert!(String::from_utf8_lossy(&combine_output.stderr).contains("--combine"));
}

#[cfg(not(feature = "delta"))]
#[test]
fn delta_table_without_the_feature_gives_an_actionable_error() {
    let path = fixture("edge_delta_table");
    let output = Command::new(bin())
        .args([path.to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Delta") && stderr.contains("--features delta"));
}

// tests/fixtures/edge_delta_table_with_checkpoint is a real, committed
// Delta table exercising the checkpoint-replay gap this reader used to
// have: 6 real `deltalake`-appended commits (versions 0-5), a real
// checkpoint created at version 4 (`DeltaTable.create_checkpoint()`),
// and every commit *older* than the checkpoint (versions 0-3) deleted
// from the fixture entirely - the same shape a real, long-lived,
// log-cleaned production table has. Only the checkpoint's own Parquet
// rows plus the one remaining newer commit (version 5) are on disk at
// all, so this table can only be read correctly by actually replaying
// the checkpoint - a reader that only understood plain JSON commits (as
// this one used to) fails outright with "no metaData action found",
// confirmed directly against the pre-checkpoint-support binary before
// this fixture was committed. Expected values (`id`: [0,1,2,3,4,5],
// `name`: [row0..row5]) were cross-checked against `deltalake`'s own
// `DeltaTable(...).to_pandas()` read of the same table before this
// fixture's own commits were pruned.
#[cfg(feature = "delta")]
#[test]
fn delta_table_with_checkpoint_replays_checkpoint_plus_newer_commits() {
    let doc = run_json("edge_delta_table_with_checkpoint", &[]);
    let cols = table(&doc, "edge_delta_table_with_checkpoint");

    let id = column(cols, "id");
    assert_eq!(id["row_count"], 6);
    assert_eq!(id["missing_pct"], 0.0);
    let id_stats = &id["numeric_stats"];
    assert_eq!(id_stats["count"], 6);
    assert_eq!(id_stats["min"], 0.0);
    assert_eq!(id_stats["max"], 5.0);
    assert_eq!(id_stats["mean"], 2.5);

    let name = column(cols, "name");
    assert_eq!(name["row_count"], 6);
    assert_eq!(name["missing_pct"], 0.0);
}

/// A Delta table using `delta.columnMapping.mode = "name"` stores every
/// data file's own Parquet columns under a generated `col-<uuid>`
/// physical name, with the real logical name (and the mapping between
/// the two) carried only in the schema's own `metadata."delta.
/// columnMapping.physicalName"`. `tests/fixtures/edge_delta_table_column_mapping`
/// is a hand-built table exercising exactly this: a real Parquet file
/// whose own two columns are physically named
/// `col-11111111-...`/`col-22222222-...`, resolved back to their real
/// logical names (`id`/`label`) purely from the schema's own metadata -
/// a reader that ignored column mapping (looking data files up by
/// logical name directly) would find no matching column at all and
/// profile every row as 100% missing.
#[cfg(feature = "delta")]
#[test]
fn delta_table_resolves_column_mapped_physical_names() {
    let doc = run_json("edge_delta_table_column_mapping", &[]);
    let cols = table(&doc, "edge_delta_table_column_mapping");

    let id = column(cols, "id");
    assert_eq!(id["ideal_type"], "i64");
    assert_eq!(id["missing_pct"], 0.0);
    assert_eq!(id["row_count"], 3);

    let label = column(cols, "label");
    assert_eq!(label["ideal_type"], "String");
    assert_eq!(label["missing_pct"], 0.0);
    let samples: Vec<&str> = label["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(samples, vec!["a", "b", "c"]);
}

/// Deletion vectors are applied: `edge_delta_deletion_vectors` (hand-built
/// to the Delta protocol with pyroaring bitmaps) has two vectors stored in
/// one `deletion_vector_<uuid>.bin` file under a prefix directory and one
/// stored inline. delta-rs's own `deletion_vectors()` reads the same rows
/// as deleted: 3 and 7 of the first file, 5 and 9 of the second, 0, 2, 3
/// and 4 of the third - 17 of 25 rows left.
#[cfg(feature = "delta")]
#[test]
fn delta_table_applies_file_backed_and_inline_deletion_vectors() {
    let doc = run_json("edge_delta_deletion_vectors", &["--samples", "20"]);
    let id = column(table(&doc, "edge_delta_deletion_vectors"), "id");
    assert_eq!(id["row_count"], 17);
    assert_eq!(
        id["sample_values"],
        serde_json::json!([
            "0", "1", "2", "4", "5", "6", "8", "9", "10", "11", "12", "13", "14", "16", "17", "18",
            "21"
        ])
    );
    // --nrows counts rows left after deletion.
    let doc = run_json(
        "edge_delta_deletion_vectors",
        &["--nrows", "4", "--samples", "20"],
    );
    let id = column(table(&doc, "edge_delta_deletion_vectors"), "id");
    assert_eq!(id["sample_values"], serde_json::json!(["0", "1", "2", "4"]));
}

/// A corrupt deletion vector file fails its CRC-32 check with an error
/// naming the data file, rather than deleting the wrong rows.
#[cfg(feature = "delta")]
#[test]
fn delta_table_rejects_a_corrupt_deletion_vector() {
    let dir = TempDir::new();
    let src = fixture("edge_delta_deletion_vectors");
    let dest = dir.path().join("t");
    copy_dir_recursive(&src, &dest);
    let dv_dir = dest.join("ab");
    let dv_file = std::fs::read_dir(&dv_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = std::fs::read(&dv_file).unwrap();
    bytes[10] ^= 0xFF;
    std::fs::write(&dv_file, bytes).unwrap();
    let output = Command::new(bin())
        .args([dest.to_str().unwrap(), "-", "--output-format", "json"])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("deletion vector") && stderr.contains("part-a.parquet"),
        "{stderr}"
    );
}

/// A malformed deletion vector reference (`"abc"`, too short for a UUID)
/// is an error naming the file that carries it, not its clean sibling.
#[cfg(feature = "delta")]
#[test]
fn delta_table_names_the_correct_file_among_several_when_only_one_carries_a_deletion_vector() {
    let path = fixture("edge_delta_deletion_vector_among_multiple_files");
    let output = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--output-format", "json"])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("part-dv.parquet"));
    assert!(!stderr.contains("part-clean.parquet"));
}

// tests/fixtures/edge_delta_checkpoint_with_remove_tombstones is a real,
// committed Delta table exercising a real production shape: an
// `overwrite` write (via `deltalake`) that emits `remove` tombstones for
// the two files it replaced in the *same* commit that added the
// replacement file, followed by a real checkpoint created right after -
// `deltalake`'s own checkpoint writer genuinely retains those remove
// tombstones as rows in the checkpoint Parquet file (confirmed directly
// by inspecting the real checkpoint's own decoded rows, not assumed),
// rather than only ever emitting `add` rows for what's still live. This
// proves `apply_checkpoint_row`/`apply_action` correctly fold a
// checkpoint's own `remove` rows the same way a JSON commit's `remove`
// action already does - a checkpoint replay that only understood `add`
// rows would incorrectly resurrect both overwritten-away files' data
// (ids 1-5) alongside the real, current 2-row table (ids 10, 20).
#[cfg(feature = "delta")]
#[test]
fn delta_table_checkpoint_correctly_excludes_files_named_in_remove_tombstones() {
    let doc = run_json("edge_delta_checkpoint_with_remove_tombstones", &[]);
    let cols = table(&doc, "edge_delta_checkpoint_with_remove_tombstones");

    let id = column(cols, "id");
    assert_eq!(id["row_count"], 2);
    let id_stats = &id["numeric_stats"];
    assert_eq!(id_stats["min"], 10.0);
    assert_eq!(id_stats["max"], 20.0);

    let name = column(cols, "name");
    let samples: Vec<&str> = name["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    // Neither overwritten-away file's own data may leak through.
    assert!(
        !samples
            .iter()
            .any(|s| ["a", "b", "c", "d", "e"].contains(s))
    );
    assert!(samples.contains(&"x") || samples.contains(&"y"));
}

/// tests/fixtures/edge_delta_checkpoint_stale_last_checkpoint has a real
/// checkpoint at version 2, with its own `_last_checkpoint` sidecar
/// deliberately corrupted to name a nonexistent version (99) instead -
/// `find_checkpoint`'s own fallback (a plain directory scan for whatever
/// checkpoint is actually present, once the sidecar's own claim can't be
/// verified) must still resolve the real, valid checkpoint rather than
/// either erroring or silently reading zero rows.
#[cfg(feature = "delta")]
#[test]
fn delta_table_falls_back_to_a_directory_scan_when_last_checkpoint_names_a_missing_version() {
    let doc = run_json("edge_delta_checkpoint_stale_last_checkpoint", &[]);
    let cols = table(&doc, "edge_delta_checkpoint_stale_last_checkpoint");
    let id = column(cols, "id");
    assert_eq!(id["row_count"], 3);
    let id_stats = &id["numeric_stats"];
    assert_eq!(id_stats["min"], 0.0);
    assert_eq!(id_stats["max"], 2.0);
}

/// tests/fixtures/edge_delta_multipart_checkpoint is a real 4-row
/// `deltalake`-appended table whose single-part checkpoint was split by
/// hand (via `pyarrow`, since no tool in this environment can force
/// `deltalake` itself to write a genuine multi-part checkpoint at this
/// scale) into two real `<version>.checkpoint.<part>.<total>.parquet`
/// files, with `_last_checkpoint` updated to declare `"parts": 2` and
/// every commit older than the checkpoint deleted - the table can only
/// be read correctly by reading *both* parts.
#[cfg(feature = "delta")]
#[test]
fn delta_table_reads_every_part_of_a_multi_part_checkpoint() {
    let doc = run_json("edge_delta_multipart_checkpoint", &[]);
    let cols = table(&doc, "edge_delta_multipart_checkpoint");
    let id = column(cols, "id");
    assert_eq!(id["row_count"], 4);
    let id_stats = &id["numeric_stats"];
    assert_eq!(id_stats["min"], 0.0);
    assert_eq!(id_stats["max"], 3.0);
    assert_eq!(id_stats["mean"], 1.5);
}

/// tests/fixtures/edge_delta_column_mapping_with_partition combines
/// `delta.columnMapping.mode = "name"` with a real Hive-style partition
/// column in the *same* table - proving the two features compose
/// correctly: `id` must be resolved via its physical `col-<uuid>`
/// Parquet column name (column mapping), while `region` must be
/// resolved via `add.partitionValues` (Hive-style partitioning, which
/// never repeats a partition value inside the Parquet content at all,
/// column-mapped or not).
#[cfg(feature = "delta")]
#[test]
fn delta_table_resolves_column_mapping_and_partitioning_together() {
    let doc = run_json("edge_delta_column_mapping_with_partition", &[]);
    let cols = table(&doc, "edge_delta_column_mapping_with_partition");

    let id = column(cols, "id");
    assert_eq!(id["row_count"], 3);
    assert_eq!(id["missing_pct"], 0.0);
    let id_stats = &id["numeric_stats"];
    assert_eq!(id_stats["min"], 1.0);
    assert_eq!(id_stats["max"], 3.0);

    let region = column(cols, "region");
    assert_eq!(region["missing_pct"], 0.0);
    let mut samples: Vec<&str> = region["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    samples.sort_unstable();
    assert_eq!(samples, vec!["eu", "us"]);
}

/// A directory that merely happens to contain a `_delta_log` subdirectory
/// with no real commit files in it (no 20-digit-`.json` files) must still
/// be treated as an ordinary directory to batch-profile, never
/// misdetected as a genuine Delta table - `is_delta_table_dir`'s own
/// structural filename check exists specifically to rule this out.
#[test]
fn a_delta_log_directory_with_no_real_commit_files_is_not_misdetected() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join("_delta_log")).unwrap();
    std::fs::write(dir.path().join("_delta_log/readme.txt"), "not a commit").unwrap();
    std::fs::write(dir.path().join("data.csv"), "id\n1\n2\n").unwrap();
    let output_dir = dir.path().join("out");
    let output = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--output-dir",
            output_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Ordinary directory-batch mode ran (data.csv got profiled) rather
    // than either failing as an incomplete Delta table or silently doing
    // nothing.
    assert!(output_dir.join("data.csv.dictionary.md").exists());
}

// --- Apache Iceberg table awareness (--features iceberg) ---
//
// tests/fixtures/edge_iceberg_table is a real, committed Iceberg table -
// generated with the real `pyiceberg` (Apache Iceberg's own Python
// implementation) package, the same "generate via a real tool, commit
// the fixture" convention `edge_delta_table` already follows. Unlike
// Delta's own `add.path` (already relative to the table root), a real
// Iceberg writer's metadata/manifest-list/manifest files all bake in
// *absolute* `file://` URIs at write time - genuinely non-portable once
// committed to a repository that gets checked out somewhere else, found
// directly while building this fixture, not assumed. Every absolute
// path was rewritten to a path relative to the table's own root
// directory before committing (the metadata.json's own JSON text via a
// plain string replace; the manifest-list/manifest Avro files via
// `fastavro` - reading each with its own real schema and rewriting with
// the same schema, since a raw byte-level string replace would corrupt
// Avro's own length-prefixed string encoding whenever the replacement
// isn't byte-identical in length) - `resolve_file_uri`'s own relative-
// path fallback (join with the table's root) is exactly what makes a
// relocated fixture like this still resolve correctly. Every expected
// value below was independently cross-checked against `pyiceberg`'s own
// `table.scan().to_pandas()` read (on the *original*, not-yet-rewritten
// table, before rewriting could have introduced any doubt about
// intent) before being hardcoded here.

#[cfg(feature = "iceberg")]
#[test]
fn iceberg_table_resolves_schema_and_reads_the_current_snapshot() {
    let doc = run_json("edge_iceberg_table", &[]);
    assert_eq!(doc["format"], "iceberg");
    let cols = table(&doc, "edge_iceberg_table");

    let id = column(cols, "id");
    assert_eq!(id["current_type"], "long");
    assert_eq!(id["ideal_type"], "i64");
    assert_eq!(id["row_count"], 5);
    let id_stats = &id["numeric_stats"];
    assert_eq!(id_stats["count"], 5);
    assert_eq!(id_stats["min"], 1.0);
    assert_eq!(id_stats["max"], 5.0);
    assert_eq!(id_stats["mean"], 3.0);

    let name = column(cols, "name");
    assert_eq!(name["missing_pct"], 20.0);

    let score = column(cols, "score");
    assert_eq!(score["ideal_type"], "f64");
    let score_stats = &score["numeric_stats"];
    assert_eq!(score_stats["count"], 4);
    assert_eq!(score_stats["min"], 10.5);
    assert_eq!(score_stats["max"], 50.75);
    assert!((score_stats["mean"].as_f64().unwrap() - 27.875).abs() < 1e-9);

    // `category` isn't a partition column in this particular fixture,
    // but its own correct resolution (straight off the row's own
    // decoded Parquet content, exactly like every other column) is what
    // this reader relies on for a genuinely partitioned table too - see
    // this section's own header comment for why Iceberg needs no
    // partition-value special-casing at all, unlike Delta.
    let category = column(cols, "category");
    assert_eq!(category["missing_pct"], 0.0);
    let category_samples: Vec<&str> = category["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(category_samples, vec!["a", "b"]);
}

#[cfg(feature = "iceberg")]
#[test]
fn iceberg_table_nrows_bounds_the_total_row_count() {
    let doc = run_json("edge_iceberg_table", &["--nrows", "3"]);
    let cols = table(&doc, "edge_iceberg_table");
    for name in ["id", "name", "score", "category"] {
        assert_eq!(
            column(cols, name)["row_count"],
            3,
            "column {name} should be bounded to 3 rows"
        );
    }
}

#[cfg(feature = "iceberg")]
#[test]
fn iceberg_table_emits_inline_sql_and_rejects_combine() {
    let path = fixture("edge_iceberg_table");

    let sql_output = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--output-format", "sql"])
        .output()
        .expect("failed to run binary");
    assert!(
        sql_output.status.success(),
        "{}",
        String::from_utf8_lossy(&sql_output.stderr)
    );
    let sql = String::from_utf8_lossy(&sql_output.stdout);
    assert!(sql.contains("CREATE TABLE \"edge_iceberg_table\""), "{sql}");
    assert!(sql.contains("INSERT INTO \"edge_iceberg_table\""), "{sql}");

    let staging = Command::new(bin())
        .args([
            path.to_str().unwrap(),
            "-",
            "--output-format",
            "sql",
            "--sql-mode",
            "staging",
        ])
        .output()
        .expect("failed to run binary");
    assert!(staging.status.success());
    assert!(String::from_utf8_lossy(&staging.stdout).contains("iceberg_scan("));

    let combine_output = Command::new(bin())
        .args([
            path.to_str().unwrap(),
            "-",
            "--output-format",
            "json",
            "--combine",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!combine_output.status.success());
    assert!(String::from_utf8_lossy(&combine_output.stderr).contains("--combine"));
}

// tests/fixtures/edge_iceberg_position_delete is a real, committed
// Iceberg table exercising position-delete support - a 5-row table (a
// real `pyiceberg`-appended data file) plus one real position-delete
// file (`{"file_path", "pos"}` Parquet columns, `pos: 2` naming the
// third row - `id: 3`) and its own delete manifest, both hand-assembled
// via `fastavro` re-encoding the *real* manifest/manifest-list schema
// `pyiceberg` itself already wrote for the data file (not a guessed
// schema) - needed because `pyiceberg` 0.12's own write path can't yet
// produce a real position-delete file itself (confirmed directly: its
// own `table.delete()` warns "Merge on read is not yet supported,
// falling back to copy-on-write" even with `write.delete.mode` set to
// `merge-on-read`). The delete file's own row order and the resulting
// live-row set were independently verified via `pyarrow.parquet.
// read_table(...).to_pandas()` on the real data file (rows in on-disk
// order: id 1,2,3,4,5 at positions 0-4) and via `pyiceberg`'s own
// `table.scan().plan_files()` correctly recognizing the hand-built
// delete manifest as structurally valid and associating it with the
// right data file, before this fixture's paths were rewritten from
// their original absolute `/tmp/...` form to the portable relative form
// committed here (the same fix `edge_iceberg_table`'s own fixture
// already needed - see this file's own header comment above).
#[cfg(feature = "iceberg")]
#[test]
fn iceberg_table_excludes_rows_named_by_a_position_delete_file() {
    let doc = run_json("edge_iceberg_position_delete", &[]);
    let cols = table(&doc, "edge_iceberg_position_delete");

    let id = column(cols, "id");
    assert_eq!(id["row_count"], 4);
    let id_stats = &id["numeric_stats"];
    assert_eq!(id_stats["count"], 4);
    assert_eq!(id_stats["min"], 1.0);
    assert_eq!(id_stats["max"], 5.0);
    assert_eq!(id_stats["mean"], 3.0);

    let name = column(cols, "name");
    assert_eq!(name["row_count"], 4);
    let samples: Vec<&str> = name["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    // The deleted row's own value ("c", for id 3) must never appear.
    assert!(!samples.contains(&"c"));
}

/// Equality deletes (a hand-built delete file on field id 1 over a real
/// `pyiceberg` table): a delete with a higher sequence number than the
/// data file removes the matching row; one with an equal sequence number
/// doesn't reach it (spec, "Scan Planning"); and a delete file that isn't
/// on disk is an error naming it.
#[cfg(feature = "iceberg")]
#[test]
fn iceberg_table_applies_equality_deletes_by_sequence_number() {
    let doc = run_json("edge_iceberg_equality_delete_applied", &[]);
    let id = column(table(&doc, "edge_iceberg_equality_delete_applied"), "id");
    assert_eq!(id["row_count"], 2);
    assert_eq!(id["sample_values"], serde_json::json!(["1", "3"]));

    let doc = run_json("edge_iceberg_equality_delete_same_seq", &[]);
    let id = column(table(&doc, "edge_iceberg_equality_delete_same_seq"), "id");
    assert_eq!(id["row_count"], 3);

    let path = fixture("edge_iceberg_equality_delete");
    let output = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--output-format", "json"])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("equality-delete") && stderr.contains("fake-eq-delete.parquet"));
}

/// A struct column flattens into dot-notation sub-columns and a list into
/// a pooled `Vec<T>`; and a column renamed after the first append
/// (`name` -> `full_name`) still reads the older file's values, matched by
/// field id. Built with `pyiceberg`.
#[cfg(feature = "iceberg")]
#[test]
fn iceberg_table_flattens_nested_columns_and_follows_renames_by_field_id() {
    let doc = run_json("edge_iceberg_nested_renamed", &["--samples", "5"]);
    let cols = table(&doc, "edge_iceberg_nested_renamed");
    let full_name = column(cols, "full_name");
    assert_eq!(full_name["missing_pct"], 0.0);
    assert_eq!(full_name["row_count"], 3);
    assert_eq!(column(cols, "address")["current_type"], "struct");
    assert_eq!(
        column(cols, "address.city")["sample_values"],
        serde_json::json!(["Oslo", "Paris"])
    );
    assert_eq!(column(cols, "tags")["ideal_type"], "Vec<String>");
}

// tests/fixtures/edge_iceberg_position_delete_multi_file is a real,
// two-append `pyiceberg` table (two independent data files, 3 rows
// each) with two hand-assembled position-delete files attached: one
// deleting one row from *each* data file in a single delete file, and a
// second, separate delete file redundantly re-deleting the exact same
// row already deleted by the first (the same real position, in the same
// data file) - proving deletes spanning multiple data files resolve
// correctly together, and that a genuinely redundant delete recorded
// twice across two different delete files doesn't double-count or error,
// it just has no further effect the second time.
#[cfg(feature = "iceberg")]
#[test]
fn iceberg_table_applies_position_deletes_spanning_multiple_data_files_and_dedups_redundant_ones() {
    let doc = run_json("edge_iceberg_position_delete_multi_file", &[]);
    let cols = table(&doc, "edge_iceberg_position_delete_multi_file");

    // Data file 1 was [4,5,6]; data file 2 was [1,2,3]. Position 1 (0-
    // indexed) is deleted in each - id 5 from file 1 (twice, redundantly,
    // across the two delete files) and id 2 from file 2 - leaving
    // exactly [4,6,1,3].
    let id = column(cols, "id");
    assert_eq!(id["row_count"], 4);
    let id_stats = &id["numeric_stats"];
    assert_eq!(id_stats["count"], 4);
    assert_eq!(id_stats["min"], 1.0);
    assert_eq!(id_stats["max"], 6.0);
    assert_eq!(id_stats["mean"], 3.5);

    let name = column(cols, "name");
    let samples: Vec<&str> = name["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    // The two deleted rows' own values ("e" for id 5, "b" for id 2) must
    // never appear.
    assert!(!samples.contains(&"e"));
    assert!(!samples.contains(&"b"));
}

/// tests/fixtures/edge_iceberg_v1_table is a real Iceberg format-version
/// 1 table (`pyiceberg`, `properties={"format-version": "1"}`) - a
/// genuinely different manifest schema from every other committed
/// Iceberg fixture (a v1 manifest's own `data_file` struct has no
/// `content` field at all, confirmed directly by inspecting the real
/// file's own Avro schema, not assumed), proving `resolve_live_data_
/// files`'s `.unwrap_or(0)` default for a missing `content` field is
/// genuinely exercised, not just theoretically safe, and that schema
/// resolution correctly falls back to a v1 metadata.json's own top-level
/// `"schema"` object (v1 has no `"schemas"`/`"current-schema-id"`
/// concept at all). `pyiceberg` 0.12's own write path in this
/// environment doesn't actually persist a v1 table's snapshot pointer on
/// commit (confirmed directly - a fresh reload of the table it just
/// wrote to shows zero snapshots despite `to_pandas()` succeeding within
/// the same process/script that wrote it), so this fixture's own
/// metadata.json has that one missing pointer restored by hand (the
/// snapshot id and manifest-list path are both taken directly from the
/// real files `pyiceberg` itself already wrote alongside it) - verified
/// correct by loading the patched metadata.json through `pyiceberg`'s
/// own independent `StaticTable.from_metadata(...)` before trusting it,
/// not just by this reader's own output.
#[cfg(feature = "iceberg")]
#[test]
fn iceberg_table_reads_a_real_format_version_1_table() {
    let doc = run_json("edge_iceberg_v1_table", &[]);
    let cols = table(&doc, "edge_iceberg_v1_table");

    let id = column(cols, "id");
    assert_eq!(id["ideal_type"], "i64");
    assert_eq!(id["row_count"], 3);
    let id_stats = &id["numeric_stats"];
    assert_eq!(id_stats["min"], 1.0);
    assert_eq!(id_stats["max"], 3.0);

    let name = column(cols, "name");
    let mut samples: Vec<&str> = name["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    samples.sort_unstable();
    assert_eq!(samples, vec!["a", "b", "c"]);
}

#[cfg(not(feature = "iceberg"))]
#[test]
fn iceberg_table_without_the_feature_gives_an_actionable_error() {
    let path = fixture("edge_iceberg_table");
    let output = Command::new(bin())
        .args([path.to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Iceberg") && stderr.contains("--features iceberg"));
}

/// A directory that merely happens to contain a `metadata` subdirectory
/// with no real `*.metadata.json` file in it must still be treated as an
/// ordinary directory to batch-profile, never misdetected as a genuine
/// Iceberg table - `is_iceberg_table_dir`'s own structural filename check
/// exists specifically to rule this out.
#[test]
fn a_metadata_directory_with_no_real_metadata_json_file_is_not_misdetected() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
    std::fs::write(dir.path().join("metadata/readme.txt"), "not metadata").unwrap();
    std::fs::write(dir.path().join("data.csv"), "id\n1\n2\n").unwrap();
    let output_dir = dir.path().join("out");
    let output = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--output-dir",
            output_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output_dir.join("data.csv.dictionary.md").exists());
}

// --- Agent-friendly CLI surface: --list-formats and structured JSON
// errors, added directly in response to an "investigate and plan how to
// make this CLI as agent-friendly as possible" request. See
// FORMAT_CATALOG's own doc comment in src/lib.rs for the full design and
// why it exists (HELP_TEXT's own format list had drifted out of sync
// with this tool's real support - these two features are what an agent
// can rely on instead of parsing --help prose). ---

/// `--list-formats` alone (no INPUT_PATH at all) is a complete, valid
/// invocation - it never touches `input_path`, per `Args::parse_from`'s
/// own bypass.
#[test]
fn list_formats_succeeds_with_no_other_arguments() {
    let output = Command::new(bin())
        .args(["--list-formats"])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("FORMAT"));
    assert!(stdout.contains("csv"));
    // A format at or past its own column's width (e.g. "combined-log" is
    // exactly 12 characters) must never run straight into the next
    // column with no separator - see the fix in `print_format_catalog`.
    assert!(!stdout.contains("combined-logyes"));
}

/// The machine-readable half: `--output-format json` must produce a
/// single parseable JSON document naming every format this build knows
/// about, with real per-build `compiled_in` information (never a static
/// claim), not just a list of names.
#[test]
fn list_formats_json_reports_real_per_build_capability() {
    let output = Command::new(bin())
        .args(["--list-formats", "--output-format", "json"])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not valid JSON ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert!(doc["sniff_rs_version"].is_string());
    let formats = doc["formats"].as_array().expect("formats must be an array");
    let csv = formats
        .iter()
        .find(|f| f["name"] == "csv")
        .expect("csv must be listed");
    assert_eq!(csv["compiled_in"], true);
    assert_eq!(csv["feature"], serde_json::Value::Null);
    assert!(
        csv["extensions"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("csv"))
    );

    let parquet = formats
        .iter()
        .find(|f| f["name"] == "parquet")
        .expect("parquet must be listed");
    // Whether parquet is actually compiled in must track this build's
    // real feature flags, not a hardcoded claim - this binary is built
    // with whatever features `cargo test` (this file's own harness) used.
    assert_eq!(parquet["compiled_in"], cfg!(feature = "parquet"));

    let delta = formats
        .iter()
        .find(|f| f["name"] == "delta")
        .expect("delta must be listed even though it's never --format-selectable");
    assert_eq!(delta["auto_detected_from"], "directory-structure");
}

/// `--help` must actually mention the new flag - the same "don't let
/// this drift again" discipline as the format-list fix itself.
#[test]
fn help_text_mentions_list_formats() {
    let output = Command::new(bin())
        .args(["--help"])
        .output()
        .expect("failed to run binary");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--list-formats"));
}

/// A failing invocation that requested `--output-format json` gets a
/// structured, parseable JSON error on stderr instead of the default
/// human-readable `Error: ...`/`Caused by:` chain - the whole point being
/// that an agent piping JSON everywhere else doesn't have to fall back to
/// scraping prose for the one invocation that fails.
#[test]
fn a_failing_invocation_with_json_output_format_gets_a_structured_json_error() {
    let output = Command::new(bin())
        .args([
            "/this/path/does/not/exist.csv",
            "-",
            "--output-format",
            "json",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let doc: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|e| panic!("stderr was not valid JSON ({e}): {stderr}"));
    assert!(
        doc["error"]
            .as_str()
            .unwrap()
            .contains("does/not/exist.csv")
    );
    assert!(doc["caused_by"].as_array().unwrap().iter().any(|c| {
        // The OS message for a missing path is platform-specific (and
        // even case-specific on Windows: ERROR_FILE_NOT_FOUND says
        // "file", ERROR_PATH_NOT_FOUND says "path" - a path with several
        // missing components reports the latter). The stable contract is
        // the raw os error code, which Rust always appends untranslated:
        // 2 (ENOENT / NOT_FOUND) or 3 (Windows PATH_NOT_FOUND). Prose
        // kept as a fallback.
        let s = c.as_str().unwrap();
        s.contains("No such file")
            || s.contains("cannot find the")
            || s.contains("os error 2")
            || s.contains("os error 3")
    }));
}

/// The identical failure *without* `--output-format json` must keep
/// producing exactly the old human-readable shape - this feature only
/// ever changes behavior for an invocation that actually asked for JSON,
/// never the default.
#[test]
fn a_failing_invocation_without_json_output_format_keeps_the_human_readable_error() {
    let output = Command::new(bin())
        .args(["/this/path/does/not/exist.csv", "-"])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("Error:"));
    assert!(stderr.contains("Caused by:"));
    assert!(serde_json::from_str::<serde_json::Value>(stderr.trim()).is_err());
}

/// `--format bogus --output-format json` exercises the *other* error
/// site this pass touched (the format-override parser's own bail!, now
/// generated from `FORMAT_CATALOG`) through the same JSON-error path.
#[test]
fn unrecognized_format_with_json_output_format_gets_a_structured_json_error() {
    let path = fixture("sample.csv");
    let output = Command::new(bin())
        .args([
            path.to_str().unwrap(),
            "-",
            "--format",
            "bogus",
            "--output-format",
            "json",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let doc: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|e| panic!("stderr was not valid JSON ({e}): {stderr}"));
    assert!(doc["error"].as_str().unwrap().contains("bogus"));
    assert!(doc["error"].as_str().unwrap().contains("--list-formats"));
}

// --- Reading INPUT_PATH from stdin ("-") - added in the same
// agent-friendly-CLI pass as --list-formats/structured JSON errors, so
// an agent that already has data in memory doesn't have to write it to a
// real file itself just to hand it to this tool. See
// `resolve_stdin_input`'s own doc comment in src/lib.rs for the full
// design. ---

use std::io::Write as _;
use std::process::Stdio;

/// Runs the binary with `stdin_content` piped to its stdin, and the given
/// args - a small local helper, distinct from the rest of this file's own
/// `Command::new(bin()).args(...)` calls, since this is the only place
/// that needs to actually write to a child's stdin rather than leave it
/// inherited/empty.
fn run_with_stdin(stdin_content: &[u8], args: &[&str]) -> std::process::Output {
    let mut child = Command::new(bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn binary");
    // A child that rejects its args (e.g. `-` with no OUTPUT_PATH) can
    // exit before this write lands, closing the pipe - that early exit
    // is itself a legitimate outcome the caller asserts on below, so a
    // broken pipe here is ignored rather than panicking. Any other I/O
    // error still fails loudly. Whether the race fires is purely
    // scheduling-dependent, which is why this only flakes under load.
    match child
        .stdin
        .take()
        .expect("stdin was requested as piped")
        .write_all(stdin_content)
    {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => panic!("failed to write to child stdin: {e}"),
    }
    child.wait_with_output().expect("failed to wait on child")
}

/// The exact bug this feature shipped with, and was caught before it did:
/// the default single-table name must be a real, meaningful "stdin", not
/// `resolve_stdin_input`'s own randomly-named scratch temp file leaking
/// into user-visible output.
#[test]
fn stdin_input_names_its_table_stdin_not_the_internal_temp_file() {
    let content = std::fs::read(fixture("sample.csv")).unwrap();
    let output = run_with_stdin(
        &content,
        &["-", "-", "--format", "csv", "--output-format", "json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not valid JSON ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(doc["file"], "-");
    let tables = doc["tables"].as_object().expect("tables must be an object");
    assert!(tables.contains_key("stdin"), "tables were: {tables:?}");
    assert!(!tables.keys().any(|k| k.starts_with("sniff-rs-")));
}

/// A format with a fixed, unambiguous leading byte (JSON's own `{`/`[`)
/// still auto-detects correctly with no `--format` at all when read from
/// stdin - the same content-sniffing fallback an extensionless real file
/// already gets, since a piped `-` has no extension either.
#[test]
fn stdin_input_content_sniffs_json_with_no_format_flag() {
    let output = run_with_stdin(
        br#"[{"a": 1, "b": "x"}, {"a": 2, "b": "y"}]"#,
        &["-", "-", "--output-format", "json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["format"], "json");
    assert_eq!(doc["tables"]["stdin"][0]["name"], "a");
}

/// A format with no fixed leading byte (TOML) still needs `--format`
/// explicitly when piped through stdin, exactly as it already would for
/// any other extensionless input - not a stdin-specific limitation, just
/// the same rule applied consistently. (A delimited table is recognized
/// by its content, so piped CSV needs no flag.)
#[test]
fn stdin_input_without_format_still_needs_it_for_a_non_sniffable_format() {
    let content = std::fs::read(fixture("sample.toml")).unwrap();
    let output = run_with_stdin(&content, &["-", "-"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can't infer format from extension"));

    let csv = std::fs::read(fixture("sample.csv")).unwrap();
    let output = run_with_stdin(&csv, &["-", "-", "--output-format", "json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `sniff-rs -` alone (no OUTPUT_PATH) is a clear, actionable error - not
/// a guessed default-output filename derived from a scratch temp path -
/// since there's no real input filename to derive one from at all.
#[test]
fn stdin_input_without_an_output_path_is_an_actionable_error() {
    let content = std::fs::read(fixture("sample.csv")).unwrap();
    let output = run_with_stdin(&content, &["-", "--format", "csv"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires an explicit OUTPUT_PATH"));
}

/// `--output-format sql`'s own inline mode does a genuine *second* pass
/// over the same input to emit literal `INSERT` rows once column types
/// are known (see CLAUDE.md's own "SQL script output" section) - for
/// stdin input, that second pass has to re-read the same temporary file
/// the first pass already consumed, not stdin itself a second time
/// (which would already be exhausted). This proves that actually works,
/// not just that the header/schema comes out right.
#[test]
fn stdin_input_survives_sql_inline_modes_second_pass_reread() {
    let content = std::fs::read(fixture("sample.csv")).unwrap();
    let output = run_with_stdin(
        &content,
        &["-", "-", "--format", "csv", "--output-format", "sql"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sql = String::from_utf8_lossy(&output.stdout);
    assert!(sql.contains("CREATE TABLE \"stdin\""));
    // A real value from the fixture's own first data row, not just the
    // schema - proves the second pass actually re-read real content.
    assert!(sql.contains("U1001"));
}

// ---------------------------------------------------------------------------
// Cross-table relationships (join candidates): `detect_relationships`
// emits an undirected edge list under every rich-JSON document's own
// top-level "relationships" key - the "graphify for data" half of this
// tool (profiling already produces the nodes). Confidence follows
// Every edge carries a tier - `declared` (the schema states the key),
// `discovered` (the values show it: one column's values sit inside another's
// unique values), or `probable` (names and types make it more likely than
// not) - and a Fellegi-Sunter `probability`; bridges below 0.5 are dropped.
// ---------------------------------------------------------------------------

/// Finds the edge touching `column` in `table` (either endpoint), or panics
/// naming what was missing - the integration-level twin of the
/// `rel_edge` unit-test helper.
#[cfg(feature = "ini")]
fn find_edge<'a>(
    rels: &'a [serde_json::Value],
    table: &str,
    column: &str,
) -> &'a serde_json::Value {
    rels.iter()
        .find(|e| {
            (e["from_table"] == table || e["to_table"] == table)
                && (e["from_column"] == column || e["to_column"] == column)
        })
        .unwrap_or_else(|| panic!("no relationship edge for {table}.{column}"))
}

#[test]
#[cfg(feature = "ini")]
fn relationships_ini_reports_fk_and_identifier_domain_edges() {
    let doc = run_json("edge_relationships.ini", &[]);
    let rels = doc["relationships"].as_array().unwrap();
    // users.id <-> orders.user_id: foreign-key naming + UUID + a shared
    // sample - probable, and very likely. email <-> contact: shared Email
    // domain under different names - kept on the prior alone (the benchmark
    // had no such pairs to weigh). region_code <-> region_code is an exact
    // name neither table owns or leads with: a candidate, but one the
    // benchmark says is a real join only 6 times in 32, so it is dropped.
    // orders.order_id (i64) matches nothing and correctly yields no edge.
    assert_eq!(rels.len(), 2);
    let fk = find_edge(rels, "users", "id");
    assert_eq!(fk["confidence"], "probable");
    assert!(fk["probability"].as_f64().unwrap() > 0.9);
    assert!(find_edge(rels, "orders", "user_id").as_object() == fk.as_object());
    assert_eq!(fk["reference"]["referencing_table"], "orders");
    assert_eq!(fk["reference"]["referencing_column"], "user_id");
    assert_eq!(fk["reference"]["referenced_table"], "users");
    assert_eq!(fk["reference"]["referenced_column"], "id");
    assert!(rels.iter().all(|e| e["from_column"] != "region_code"));
    let domain = find_edge(rels, "users", "email");
    assert_eq!(domain["confidence"], "probable");
    let p = domain["probability"].as_f64().unwrap();
    assert!((0.5..0.9).contains(&p), "{p}");
    assert!(find_edge(rels, "orders", "contact").as_object() == domain.as_object());
    for e in rels {
        assert_eq!(e["kind"], "join_candidate");
        assert!(!e["evidence"].as_array().unwrap().is_empty());
        assert!(!e["reason"].as_str().unwrap().is_empty());
    }
    // Edges sorted by (from-table, from-column): both run orders -> users
    // here, contact < user_id.
    let cols: Vec<&str> = rels
        .iter()
        .map(|e| e["from_column"].as_str().unwrap())
        .collect();
    assert_eq!(cols, vec!["contact", "user_id"]);
}

#[test]
#[cfg(feature = "ini")]
fn relationships_need_key_evidence_and_resolve_roles_and_natural_keys() {
    let doc = run_json("edge_graph_key_evidence.ini", &[]);
    let rels = doc["relationships"].as_array().unwrap();
    // customer/staff share city and phone: attributes, never a bridge.
    for attr in ["city", "phone"] {
        assert!(
            rels.iter().all(|e| e["from_column"] != attr),
            "{attr} must not link: {rels:#?}"
        );
    }
    // store.manager_staff_id is a role-prefixed copy of staff's own key.
    let role = find_edge(rels, "store", "manager_staff_id");
    assert_eq!(role["reference"]["referenced_table"], "staff");
    assert_eq!(role["reference"]["referenced_column"], "staff_id");
    assert_eq!(role["context"], "bridge");
    // state owns state_name, so customer.state_name references it.
    let natural = find_edge(rels, "customer", "state_name");
    assert_eq!(natural["reference"]["referenced_table"], "state");
    // league_code sits in three tables and none owns it: shared, not a
    // clique of bridges.
    let league: Vec<&serde_json::Value> = rels
        .iter()
        .filter(|e| e["from_column"] == "league_code")
        .collect();
    assert_eq!(league.len(), 3);
    assert!(league.iter().all(|e| e["context"] == "shared_reference"));
    // Every edge carries its BM25-style score, then its probability as the
    // last field.
    for e in rels {
        let obj = e.as_object().unwrap();
        assert_eq!(
            obj.keys().next_back().map(String::as_str),
            Some("probability")
        );
        assert!(e["score"].as_f64().unwrap() >= 0.0);
        let p = e["probability"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&p));
    }
}

#[test]
fn relationships_combine_links_a_shared_column_across_files() {
    let dir = TempDir::new();
    std::fs::write(
        dir.path().join("customers.csv"),
        "customer_id,name\nC-001,Alice\nC-002,Bob\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("orders.csv"),
        "order_id,customer_id,total\n1,C-001,9.99\n2,C-002,4.50\n",
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
    let rels = doc["relationships"].as_array().unwrap();
    // `customer_id` (plain String in both files) is the one honest link:
    // order_id/total match nothing, and names must not bleed across the
    // qualified tables.
    assert_eq!(rels.len(), 1);
    let edge = &rels[0];
    assert_eq!(edge["from_column"], "customer_id");
    assert_eq!(edge["to_column"], "customer_id");
    assert_ne!(edge["from_table"], edge["to_table"]);
    // Both order values sit inside the customers' unique ids: the values
    // confirm what the names suggest.
    assert_eq!(edge["confidence"], "discovered");
    assert!(
        edge["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().starts_with("values match: 100%"))
    );
}

#[test]
#[cfg(feature = "sqlite")]
fn relationships_declared_sqlite_keys_are_declared_edges() {
    // Four declared foreign keys none of the name rules could find
    // (`opened_by -> staff.id`, `owner -> clients."client ref"`, and a
    // composite `(o, l) -> order_lines` resolved onto its primary key), a
    // self-reference, and one pointing at a table that doesn't exist.
    let doc = run_json("edge_graph_declared_keys.sqlite", &[]);
    let refs = |table: &str, column: &str| -> Vec<(String, String)> {
        doc["tables"][table]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == column)
            .unwrap()["references"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["table"].as_str().unwrap().to_string(),
                    r["column"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    assert_eq!(
        refs("tickets", "opened_by"),
        vec![("staff".into(), "id".into())]
    );
    assert_eq!(
        refs("tickets", "owner"),
        vec![("clients".into(), "client ref".into())]
    );
    assert_eq!(
        refs("shipments", "l"),
        vec![("order_lines".into(), "line_no".into())]
    );
    assert_eq!(
        refs("staff", "manager"),
        vec![("staff".into(), "id".into())]
    );
    assert!(
        refs("tickets", "legacy").is_empty(),
        "dangling target dropped"
    );
    assert!(refs("tickets", "ticket_no").is_empty());

    let rels = doc["relationships"].as_array().unwrap();
    assert_eq!(rels.len(), 4, "{rels:#?}");
    for e in rels {
        assert_eq!(e["confidence"], "declared");
        assert_eq!(e["probability"], 1.0);
        assert_eq!(e["context"], "bridge");
        assert!(e["evidence"][0].as_str().unwrap().starts_with("declared "));
    }
    let owner = rels.iter().find(|e| e["to_column"] == "owner").unwrap();
    assert_eq!(owner["reference"]["referenced_column"], "client ref");

    // The keys survive a round trip through a saved dictionary, which the
    // graph subcommands read back.
    let dir = TempDir::new();
    let dict = dir.path().join("keys.json");
    std::fs::write(&dict, serde_json::to_string(&doc).unwrap()).unwrap();
    let output = run_graph(&["path", dict.to_str().unwrap(), "shipments", "order_lines"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("(declared)"), "{stdout}");
}

#[test]
#[cfg(feature = "sqlite")]
fn relationships_combine_qualifies_declared_keys() {
    let dir = TempDir::new();
    std::fs::copy(
        fixture("edge_graph_declared_keys.sqlite"),
        dir.path().join("desk.sqlite"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("notes.csv"),
        "note_id,text
1,hi
",
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
    assert!(output.status.success());
    let dir_name = dir.path().file_name().unwrap().to_str().unwrap();
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join(format!("{dir_name}.dictionary.json"))).unwrap(),
    )
    .unwrap();
    let opened_by = doc["tables"]["desk__tickets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "opened_by")
        .unwrap();
    assert_eq!(opened_by["references"][0]["table"], "desk__staff");
    let declared = doc["relationships"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["confidence"] == "declared")
        .count();
    assert_eq!(declared, 4);
}

#[test]
fn relationships_values_discover_a_code_reference_across_files() {
    // `shipments.carrier` shares no name with `carriers.code`, but every
    // carrier value is one of the unique codes: discovered from the values.
    let dir = TempDir::new();
    std::fs::write(
        dir.path().join("carriers.csv"),
        "code,name
UPS,United Parcel
DHL,DHL Express
FDX,FedEx
TNT,TNT Express
",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("shipments.csv"),
        "shipment_no,carrier,weight
1,UPS,2.5
2,DHL,1.0
3,UPS,4.2
4,TNT,0.7
5,DHL,3.3
",
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
    assert!(output.status.success());
    let dir_name = dir.path().file_name().unwrap().to_str().unwrap();
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join(format!("{dir_name}.dictionary.json"))).unwrap(),
    )
    .unwrap();
    let rels = doc["relationships"].as_array().unwrap();
    let e = rels
        .iter()
        .find(|e| e["from_column"] == "carrier" || e["to_column"] == "carrier")
        .unwrap_or_else(|| panic!("no carrier edge: {rels:#?}"));
    assert_eq!(e["confidence"], "discovered");
    assert_eq!(e["reference"]["referenced_column"], "code");
    assert!(e["probability"].as_f64().unwrap() >= 0.5);
    // The sketch behind it is part of the column's own JSON.
    let code = &doc["tables"]["carriers__carriers"][0];
    assert_eq!(code["name"], "code");
    assert_eq!(code["value_sketch"]["count"], 4);
    assert_eq!(code["value_sketch"]["distinct"], 4);
    assert_eq!(code["value_sketch"]["hashes"].as_str().unwrap().len(), 32);
}

#[test]
fn relationships_single_table_reports_an_empty_array() {
    // The key is always present (never omitted), so consumers never
    // distinguish "none found" from "not computed".
    let doc = run_json("sample.csv", &[]);
    assert_eq!(doc["relationships"], serde_json::Value::Array(vec![]));
}

// ---------------------------------------------------------------------------
// Graph subcommands (explain / path / rank): query layer over the
// relationship graph above. `edge_graph_chain.ini` is a three-table chain
// (customers -> orders -> products) plus one isolated table (audit), so
// multi-hop paths, god-table ranking, communities, and the no-route error
// each have a permanent, reviewable shape - the same committed-fixture
// discipline every other feature in this file already follows.
// ---------------------------------------------------------------------------

fn run_graph(args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .output()
        .expect("failed to run binary")
}

#[test]
#[cfg(feature = "ini")]
fn graph_explain_reports_profile_and_incident_edges() {
    let output = run_graph(&[
        "explain",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "orders.customer_id",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("# orders.customer_id"));
    assert!(stdout.contains("customers"));
    assert!(stdout.contains("probable"));
    assert!(stdout.contains("Community: 0 (orders-centered)"));
}

#[test]
#[cfg(feature = "sqlite")]
fn graph_composite_foreign_keys_carry_the_whole_key_through_a_saved_dictionary() {
    // FOREIGN KEY (o, l) REFERENCES order_lines: each pair's reference names
    // the whole key, one-column keys keep their two-field shape, and the
    // edge built from a saved dictionary says to join on every pair.
    let doc = run_json("edge_graph_declared_keys.sqlite", &[]);
    let refs = |table: &str, column: &str| {
        doc["tables"][table]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == column)
            .unwrap()["references"]
            .clone()
    };
    let key = serde_json::json!([["o", "order_id"], ["l", "line_no"]]);
    assert_eq!(refs("shipments", "o")[0]["composite"], key);
    assert_eq!(refs("shipments", "l")[0]["composite"], key);
    assert_eq!(
        refs("tickets", "opened_by"),
        serde_json::json!([{"table": "staff", "column": "id"}])
    );

    let dir = TempDir::new();
    let saved = dir.path().join("dict.json");
    std::fs::write(&saved, serde_json::to_string(&doc).unwrap()).unwrap();
    let out = run_graph(&[
        "explain",
        saved.to_str().unwrap(),
        "shipments.l",
        "--output-format",
        "json",
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let explained: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rel = &explained["relationships"][0];
    assert_eq!(rel["confidence"], "declared");
    assert!(
        rel["evidence"][0]
            .as_str()
            .unwrap()
            .contains("composite foreign key: \"shipments\" (\"o\", \"l\") references \"order_lines\" (\"order_id\", \"line_no\")"),
        "{rel}"
    );
}

#[test]
#[cfg(feature = "sqlite")]
fn graph_explain_lists_declared_self_references_from_both_ends() {
    // staff.manager REFERENCES staff(id): no edge (the graph links tables),
    // but explain names the hierarchy on both columns and nowhere else.
    let path = fixture("edge_graph_declared_keys.sqlite");
    let md = |col: &str| {
        let out = run_graph(&["explain", path.to_str().unwrap(), col]);
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    assert!(md("staff.manager").contains("declared foreign key to `id` in this same table"));
    assert!(md("staff.id").contains("`manager` in this same table declares a foreign key"));
    assert!(!md("staff.name").contains("Self-reference"));
    let out = run_graph(&[
        "explain",
        path.to_str().unwrap(),
        "staff.id",
        "--output-format",
        "json",
    ]);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        doc["self_references"],
        serde_json::json!([{"column": "manager", "references": "id"}])
    );
}

#[test]
#[cfg(feature = "ini")]
fn graph_explain_json_shape_and_ambiguous_bare_name() {
    let output = run_graph(&[
        "explain",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "orders.customer_id",
        "--output-format",
        "json",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_eq!(doc["table"], "orders");
    assert_eq!(doc["column"], "customer_id");
    assert_eq!(doc["degree"], 1);
    assert_eq!(doc["neighbors"], serde_json::json!(["customers"]));
    assert_eq!(doc["community"], 0);
    assert_eq!(doc["community_label"], "orders-centered");
    assert_eq!(doc["relationships"].as_array().unwrap().len(), 1);

    // `customer_id` exists in two tables - the bare name must fail
    // actionably, naming both qualified candidates.
    let output = run_graph(&[
        "explain",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "customer_id",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("several tables"), "got: {stderr}");
    assert!(stderr.contains("customers.customer_id"), "got: {stderr}");
    assert!(stderr.contains("orders.customer_id"), "got: {stderr}");
}

#[test]
#[cfg(feature = "ini")]
fn graph_explain_rejects_unknown_table_and_column_actionably() {
    let output = run_graph(&[
        "explain",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "missing.id",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown table"));
    let output = run_graph(&[
        "explain",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "orders.missing",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no column"));
}

#[test]
#[cfg(feature = "ini")]
fn graph_explain_isolated_column_reports_no_relationships() {
    let output = run_graph(&[
        "explain",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "audit.note",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("No relationships"));
}

#[test]
#[cfg(feature = "ini")]
fn graph_path_reports_a_two_hop_chain_in_order() {
    let output = run_graph(&[
        "path",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "customers",
        "products",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 hops"), "got: {stdout}");
    let first = stdout.find("customers.customer_id").expect("first hop");
    let second = stdout.find("orders.product_id").expect("second hop");
    assert!(first < second, "hops out of order: {stdout}");

    let output = run_graph(&[
        "path",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "customers",
        "products",
        "--output-format",
        "json",
    ]);
    assert!(output.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    let hops = doc["hops"].as_array().unwrap();
    assert_eq!(hops.len(), 2);
    assert_eq!(hops[0]["from_table"], "customers");
    assert_eq!(hops[0]["to_table"], "orders");
    assert_eq!(hops[1]["from_table"], "orders");
    assert_eq!(hops[1]["to_table"], "products");
    assert_eq!(
        hops[0]["alternatives"],
        serde_json::Value::Array(vec![]),
        "single-edge hops have no alternatives"
    );
}

#[test]
#[cfg(feature = "ini")]
fn graph_path_lists_parallel_links_as_alternatives() {
    // users <-> orders share two links in edge_relationships.ini; the hop
    // reports the more probable one and names the other.
    let output = run_graph(&[
        "path",
        fixture("edge_relationships.ini").to_str().unwrap(),
        "users",
        "orders",
        "--output-format",
        "json",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    let hops = doc["hops"].as_array().unwrap();
    assert_eq!(hops.len(), 1);
    assert_eq!(hops[0]["confidence"], "probable");
    // The hop is the foreign key (user_id -> id, probability ~0.95); the
    // Email-domain link (~0.59) is the unchosen parallel one.
    assert_eq!(hops[0]["from_column"], "user_id");
    let alternatives = hops[0]["alternatives"].as_array().unwrap();
    assert_eq!(alternatives.len(), 1);
    let cols: Vec<&str> = alternatives
        .iter()
        .map(|e| e["from_column"].as_str().unwrap())
        .collect();
    assert_eq!(cols, vec!["contact"]);

    let output = run_graph(&[
        "path",
        fixture("edge_relationships.ini").to_str().unwrap(),
        "users",
        "orders",
    ]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("also via:"));
}

#[test]
#[cfg(feature = "ini")]
fn graph_path_disconnected_and_same_table_are_errors() {
    let output = run_graph(&[
        "path",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "customers",
        "audit",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no join path found"),
        "got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run_graph(&[
        "path",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "orders",
        "orders",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("same table"));
}

#[test]
#[cfg(feature = "ini")]
fn graph_path_reads_an_already_generated_dictionary() {
    // Every other graph test profiles the raw file; this one proves the
    // dictionary-input half of `load_graph_input` on the same chain.
    let dir = TempDir::new();
    let dict = dir.path().join("chain.dictionary.json");
    let output = Command::new(bin())
        .args([
            fixture("edge_graph_chain.ini").to_str().unwrap(),
            dict.to_str().unwrap(),
            "--output-format",
            "json",
        ])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run_graph(&["path", dict.to_str().unwrap(), "customers", "products"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("2 hops"));
}

#[test]
#[cfg(feature = "ini")]
fn graph_rank_lists_the_link_table_first_with_communities() {
    let output = run_graph(&["rank", fixture("edge_graph_chain.ini").to_str().unwrap()]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let orders = stdout
        .find("| orders | 1 | 2 | 0 | 2 | 0 |")
        .expect("orders row");
    let customers = stdout
        .find("| customers | 1 | 1 | 0 | 1 | 0 |")
        .expect("customers row");
    assert!(orders < customers, "god table must rank first: {stdout}");
    assert!(
        stdout.contains("Community 0 (orders-centered, 3 tables)"),
        "got: {stdout}"
    );
    assert!(
        stdout.contains("Community 1 (1 table): audit"),
        "got: {stdout}"
    );

    let output = run_graph(&[
        "rank",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "--output-format",
        "json",
    ]);
    assert!(output.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    let tables = doc["tables"].as_array().unwrap();
    assert_eq!(tables[0]["table"], "orders");
    assert_eq!(tables[0]["degree"], 2);
    assert_eq!(tables[0]["duplicate_degree"], 0);
    assert_eq!(tables[0]["rows"], 1);
    let communities = doc["communities"].as_array().unwrap();
    assert_eq!(communities.len(), 2);
    assert_eq!(communities[0]["members"].as_array().unwrap().len(), 3);
    assert_eq!(communities[0]["label"], "orders-centered");
    assert_eq!(communities[1]["members"], serde_json::json!(["audit"]));
    assert_eq!(communities[1]["label"], "audit");
}

#[test]
#[cfg(feature = "ini")]
fn graph_rank_reports_reference_rank_areas_and_cut_tables() {
    let output = run_graph(&[
        "rank",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "--output-format",
        "json",
    ]);
    assert!(output.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    let row = |name: &str| {
        doc["tables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["table"] == name)
            .unwrap()
            .clone()
    };
    // orders references both dimensions, so they outrank it; every join
    // between customers and products runs through orders.
    let rank = |name: &str| row(name)["reference_rank"].as_f64().unwrap();
    assert!(rank("customers") > rank("orders"));
    assert_eq!(rank("customers"), rank("products"));
    assert_eq!(row("orders")["articulation"], true);
    assert_eq!(row("customers")["articulation"], false);
    assert_eq!(row("orders")["area"], row("customers")["area"]);
    assert_ne!(row("audit")["area"], row("orders")["area"]);
    let areas = doc["areas"].as_array().unwrap();
    assert_eq!(areas.len(), 2);
    assert_eq!(areas[0]["size"], 3);
    assert_eq!(areas[0]["lead"], "orders");

    let output = run_graph(&["rank", fixture("edge_graph_chain.ini").to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("| Ref. rank | Area | Cut |"),
        "got: {stdout}"
    );
    // One community, one area: nothing to split, no section.
    assert!(!stdout.contains("## Subject areas"), "got: {stdout}");
}

#[test]
fn graph_rank_single_table_reports_zero_degree() {
    let output = run_graph(&["rank", fixture("sample.csv").to_str().unwrap()]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("| sample | 5 | 0 | 0 | 0 | 0 |"),
        "got: {stdout}"
    );
}

#[test]
#[cfg(feature = "sqlite")]
fn graph_samples_deepens_overlap_evidence_on_raw_files() {
    // `ref_code` leading two tables that don't own it is a mid-strength
    // name signal either way; its probability rises with overlap, which
    // only deeper samples can see. agents.ref_code holds v1..v10,
    // jobs.ref_code holds w1,w2,w3,v7,v8: the first three samples are
    // disjoint, the full columns share v7 and v8.
    let dir = TempDir::new();
    let db = dir.path().join("overlap.sqlite");
    let setup = format!(
        "import sqlite3; con = sqlite3.connect(r'{}'); \
         con.execute('CREATE TABLE agents (ref_code TEXT, name TEXT)'); \
         con.executemany('INSERT INTO agents VALUES (?, ?)', [(f'v{{i}}', f'n{{i}}') for i in range(1, 11)]); \
         con.execute('CREATE TABLE jobs (ref_code TEXT, title TEXT)'); \
         con.executemany('INSERT INTO jobs VALUES (?, ?)', [(f'w{{i}}', 't') for i in range(1, 4)] + [('v7', 't'), ('v8', 't')]); \
         con.commit(); con.close()",
        db.to_str().unwrap()
    );
    let out = python()
        .args(["-c", &setup])
        .output()
        .expect("failed to run python");
    assert!(
        out.status.success(),
        "failed to build sqlite fixture: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let output = run_graph(&[
        "explain",
        db.to_str().unwrap(),
        "agents.ref_code",
        "--output-format",
        "json",
        "--samples",
        "3",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_eq!(doc["relationships"].as_array().unwrap().len(), 1);
    assert_eq!(doc["relationships"][0]["confidence"], "probable");
    let shallow = doc["relationships"][0]["probability"].as_f64().unwrap();

    let output = run_graph(&[
        "explain",
        db.to_str().unwrap(),
        "agents.ref_code",
        "--output-format",
        "json",
        "--samples",
        "50",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    let rels = doc["relationships"].as_array().unwrap();
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0]["confidence"], "probable");
    // Seeing the shared values makes the same link more likely.
    assert!(rels[0]["probability"].as_f64().unwrap() > shallow + 0.1);
    assert!(
        rels[0]["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("v7"))
    );
}

#[test]
#[cfg(feature = "ini")]
fn graph_samples_with_a_dictionary_is_disclosed_not_silent() {
    let dir = TempDir::new();
    let dict = dir.path().join("chain.dictionary.json");
    let output = Command::new(bin())
        .args([
            fixture("edge_graph_chain.ini").to_str().unwrap(),
            dict.to_str().unwrap(),
            "--output-format",
            "json",
        ])
        .output()
        .expect("failed to run binary");
    assert!(output.status.success());
    let output = run_graph(&[
        "explain",
        dict.to_str().unwrap(),
        "orders.customer_id",
        "--samples",
        "50",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("already carries its own samples"),
        "got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(feature = "ini")]
fn diff_reports_relationship_drift_for_a_broken_join() {
    // users.id <-> orders.user_id links in old (both integers); the new
    // side stores user_id as text, which cannot join an integer. The
    // column entry already says "type changed" - the drift section adds
    // the consequence: a join queries may rely on is gone.
    let dir = TempDir::new();
    std::fs::write(
        dir.path().join("old.ini"),
        "[users]\nid = 1\n\n[orders]\norder_id = 10\nuser_id = 1\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("new.ini"),
        "[users]\nid = 1\n\n[orders]\norder_id = 10\nuser_id = C-001\n",
    )
    .unwrap();
    let old = dir.path().join("old.ini");
    let new = dir.path().join("new.ini");

    let output = Command::new(bin())
        .args(["diff", old.to_str().unwrap(), new.to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("## Relationship drift"), "got: {stdout}");
    assert!(stdout.contains("removed"), "got: {stdout}");

    let output = Command::new(bin())
        .args([
            "diff",
            old.to_str().unwrap(),
            new.to_str().unwrap(),
            "--output-format",
            "json",
        ])
        .output()
        .expect("failed to run binary");
    assert!(output.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert!(doc["has_breaking_changes"].as_bool().unwrap());
    let drift = doc["relationship_drift"].as_array().unwrap();
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0]["kind"], "removed");
    assert_eq!(drift[0]["compatibility"], "breaking");

    // A removed join counts as breaking for CI use...
    let output = Command::new(bin())
        .args([
            "diff",
            old.to_str().unwrap(),
            new.to_str().unwrap(),
            "--fail-on-breaking",
        ])
        .output()
        .expect("failed to run binary");
    assert_eq!(output.status.code(), Some(2));

    // ...and is named (never auto-resolved) in the resolution script.
    let sql_path = dir.path().join("resolution.sql");
    let output = Command::new(bin())
        .args([
            "diff",
            old.to_str().unwrap(),
            new.to_str().unwrap(),
            "--resolution-sql",
            sql_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sql = std::fs::read_to_string(&sql_path).unwrap();
    assert!(sql.contains("(relationship removed)"), "got: {sql}");
}

#[test]
fn graph_blank_headers_neither_link_nor_break_queries() {
    // A leading-comma header (pandas index column written nameless) in
    // two files: the blank columns must not link to each other, and the
    // dictionary built from them must still load for querying.
    let dir = TempDir::new();
    std::fs::write(dir.path().join("l.csv"), ",name\n0,Alice\n1,Bob\n").unwrap();
    std::fs::write(dir.path().join("r.csv"), ",city\n0,Paris\n1,Lyon\n").unwrap();
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
    let dict = out.path().join(format!("{dir_name}.dictionary.json"));
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dict).unwrap()).unwrap();
    let rels = doc["relationships"].as_array().unwrap();
    assert!(
        rels.iter()
            .all(|e| !e["from_column"].as_str().unwrap().is_empty()
                && !e["to_column"].as_str().unwrap().is_empty()),
        "no edge may touch a blank column: {rels:?}"
    );
    // And the dictionary itself loads for querying (the loader skips the
    // blank columns instead of refusing the whole file).
    let output = run_graph(&["rank", dict.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn graph_rank_reports_similar_tables_and_edge_context() {
    // v1/v2 are column-identical (similarity 1.0): their shared edges
    // read duplicate_schema. w shares one column with each (0.33) - below
    // the reporting bar entirely - and those edges stay bridges. The
    // shared column is `account_id`, a real reference key: a bare `id`
    // in each would be three tables' own surrogate keys, which never
    // bridge (see `graph_surrogate_ids_do_not_bridge_unrelated_tables`).
    let dir = TempDir::new();
    std::fs::write(
        dir.path().join("v1.csv"),
        "name,account_id\nAlice,1\nBob,2\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("v2.csv"),
        "name,account_id\nAlice,1\nBob,2\n",
    )
    .unwrap();
    // `w` is the only table leading with `account_id`, so it owns the key:
    // its two links are references into it, not a shared key.
    std::fs::write(dir.path().join("w.csv"), "account_id,city\n1,Paris\n").unwrap();
    let out = TempDir::new();
    let output = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--combine",
            "--output-format",
            "json",
            "--output-dir",
            out.path().to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dir_name = dir.path().file_name().unwrap().to_str().unwrap();
    let dict = out.path().join(format!("{dir_name}.dictionary.json"));

    let output = run_graph(&["rank", dict.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("## Similar tables"), "got: {stdout}");
    assert!(stdout.contains("likely duplicate"), "got: {stdout}");

    let output = run_graph(&["rank", dict.to_str().unwrap(), "--output-format", "json"]);
    assert!(output.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    let similar = doc["similar_tables"].as_array().unwrap();
    assert_eq!(similar.len(), 1);
    assert_eq!(similar[0]["similarity"], 1.0);
    assert_eq!(similar[0]["reading"], "likely_duplicate");

    // The real fix under test: `w` genuinely bridges v1 and v2 (it shares
    // `id` with each), while v1/v2 are merely two copies of one schema.
    // Ranking on raw incident-edge count would put v1/v2 (3 edges each -
    // 1 bridge to `w`, 2 duplicate-schema to each other) above `w` (2
    // edges, both bridges); ranking on `Bridge`-only degree - the actual
    // "god table" signal - must put `w` first instead, with its
    // duplicate-schema copies visible but demoted to their own column.
    let tables = doc["tables"].as_array().unwrap();
    let w = tables.iter().find(|t| t["table"] == "w__w").unwrap();
    let v1 = tables.iter().find(|t| t["table"] == "v1__v1").unwrap();
    let v2 = tables.iter().find(|t| t["table"] == "v2__v2").unwrap();
    assert_eq!(w["degree"], 2, "w bridges both v1 and v2: {tables:?}");
    assert_eq!(w["duplicate_degree"], 0);
    assert_eq!(v1["degree"], 1, "v1's only real bridge is to w: {tables:?}");
    assert_eq!(
        v1["duplicate_degree"], 2,
        "v1<->v2 share id and name: {tables:?}"
    );
    assert_eq!(v2["degree"], 1);
    assert_eq!(v2["duplicate_degree"], 2);
    assert_eq!(
        tables[0]["table"], "w__w",
        "the real bridge must rank first: {tables:?}"
    );
    // The community's hub is named off bridge degree too - `w`, not an
    // arbitrary v1/v2 tie-break inflated by their own duplicate edges.
    let communities = doc["communities"].as_array().unwrap();
    assert_eq!(communities[0]["label"], "w__w-centered");

    // Edge contexts straight from the combined dictionary itself.
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dict).unwrap()).unwrap();
    let rels = doc["relationships"].as_array().unwrap();
    assert!(!rels.is_empty());
    assert!(
        rels.iter().any(|e| e["context"] == "duplicate_schema"),
        "v1/v2 edges must be tagged"
    );
    assert!(
        rels.iter().any(|e| e["context"] == "bridge"),
        "w edges must stay bridges"
    );
}

#[test]
fn graph_explain_caps_duplicate_schema_noise_and_leads_with_the_real_bridge() {
    // The real-world shape this locks in: many near-identical tables
    // (hundreds of profiled PDFs, all sharing one fixed page_number/text
    // schema, is the motivating case) plus one genuinely different table
    // that bridges them all via a shared doc_id. Explaining one
    // duplicate's own doc_id column should surface the one real bridge
    // first, cap the wall of duplicate-schema copies at 50 rows, and
    // disclose exactly how many more of each kind exist beyond the cap.
    let dir = TempDir::new();
    for i in 0..55 {
        std::fs::write(
            dir.path().join(format!("t{i}.csv")),
            "text,doc_id\nhello,1\nworld,2\n",
        )
        .unwrap();
    }
    // `hub` is the only table leading with `doc_id`: it owns the key, so
    // its links are references into it rather than one more shared copy.
    std::fs::write(dir.path().join("hub.csv"), "doc_id\n1\n2\n").unwrap();
    let out = TempDir::new();
    let output = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--combine",
            "--output-format",
            "json",
            "--output-dir",
            out.path().to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dir_name = dir.path().file_name().unwrap().to_str().unwrap();
    let dict = out.path().join(format!("{dir_name}.dictionary.json"));

    // `t0`'s own `doc_id` column: 1 real bridge (to hub) + 54 duplicate-schema
    // links (to every other t*) = 55 edges, past the 50-row md cap.
    let output = run_graph(&["explain", dict.to_str().unwrap(), "t0__t0.doc_id"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("- Degree: 55 (1 bridge, 54 duplicate-schema)"),
        "got: {stdout}"
    );
    // The one real bridge (to hub) must appear before any duplicate-schema
    // row, not wherever alphabetical table-name order happens to place it.
    let hub_pos = stdout.find("hub__hub").expect("hub row present");
    let first_dup_pos = stdout
        .find("[duplicate-schema link]")
        .expect("a duplicate-schema row is present");
    assert!(
        hub_pos < first_dup_pos,
        "the real bridge must be listed before duplicate-schema noise: {stdout}"
    );
    assert!(
        stdout.contains("…and 5 more relationships not shown (0 bridge, 5 duplicate-schema)"),
        "got: {stdout}"
    );
    // Bounded, not unbounded: exactly 50 relationship rows in the table
    // (one header separator line plus 50 data rows).
    assert_eq!(
        stdout.matches("[duplicate-schema link]").count()
            + stdout.matches("hub__hub | doc_id | ").count(),
        50,
        "got: {stdout}"
    );

    // JSON stays uncapped (the machine-consumable form), per this
    // project's own established `MAX_TOC_ENTRIES` convention, and
    // carries the bridge/duplicate breakdown as real fields.
    let output = run_graph(&[
        "explain",
        dict.to_str().unwrap(),
        "t0__t0.doc_id",
        "--output-format",
        "json",
    ]);
    assert!(output.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_eq!(doc["degree"], 55);
    assert_eq!(doc["bridge_degree"], 1);
    assert_eq!(doc["duplicate_degree"], 54);
    assert_eq!(doc["relationships"].as_array().unwrap().len(), 55);

    // The community as a whole has a real bridge (hub) - its label should
    // name that hub, not an arbitrary near-duplicate.
    assert_eq!(doc["community_label"], "hub__hub-centered");
}

/// Profiles `files` as one `--combine` directory and returns the combined
/// dictionary's path (kept alive by the returned `TempDir`s).
fn combine_dictionary(files: &[(&str, &str)]) -> (TempDir, TempDir, std::path::PathBuf) {
    let dir = TempDir::new();
    for (name, body) in files {
        std::fs::write(dir.path().join(name), body).unwrap();
    }
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
    let dict = out.path().join(format!("{dir_name}.dictionary.json"));
    (dir, out, dict)
}

#[test]
fn graph_surrogate_ids_do_not_bridge_unrelated_tables() {
    // Every file has its own integer `id` (values 1, 2 in each) plus
    // shared attributes (`amount`, `active`): none of that is a join.
    // Only the real reference key, `vendors.id <- products.vendor_id`,
    // connects anything.
    let (_d, _o, dict) = combine_dictionary(&[
        (
            "vendors.csv",
            "id,name,active\n1,Acme,true\n2,Globex,false\n",
        ),
        (
            "products.csv",
            "id,vendor_id,amount,active\n1,1,9.5,true\n2,2,3.25,false\n",
        ),
        ("audits.csv", "id,amount,note\n1,9.5,ok\n2,3.25,late\n"),
    ]);
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dict).unwrap()).unwrap();
    let rels = doc["relationships"].as_array().unwrap();
    assert_eq!(rels.len(), 1, "got: {rels:?}");
    assert_eq!(rels[0]["reference"]["referencing_column"], "vendor_id");
    assert_eq!(rels[0]["reference"]["referenced_table"], "vendors__vendors");
    let output = run_graph(&["rank", dict.to_str().unwrap(), "--output-format", "json"]);
    assert!(output.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let communities = doc["communities"].as_array().unwrap();
    assert_eq!(
        communities.len(),
        2,
        "audits stays on its own: {communities:?}"
    );
    assert_eq!(
        communities[1]["members"],
        serde_json::json!(["audits__audits"])
    );
}

#[test]
fn graph_star_schema_ranks_the_hub_and_marks_sibling_links() {
    // customers owns `customer_id`; three fact tables carry it. The graph
    // should read as a star around customers, with the fact tables' direct
    // links to each other kept but labelled shared_reference.
    let (_d, _o, dict) = combine_dictionary(&[
        ("customers.csv", "customer_id,name\nC-1,Alice\nC-2,Bob\n"),
        ("orders.csv", "order_no,customer_id\n1,C-1\n2,C-2\n"),
        ("invoices.csv", "invoice_no,customer_id\n10,C-1\n11,C-2\n"),
        ("tickets.csv", "ticket_no,customer_id\n100,C-2\n101,C-1\n"),
    ]);
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dict).unwrap()).unwrap();
    let rels = doc["relationships"].as_array().unwrap();
    assert_eq!(rels.len(), 6);
    let spokes: Vec<_> = rels.iter().filter(|e| e["context"] == "bridge").collect();
    let siblings: Vec<_> = rels
        .iter()
        .filter(|e| e["context"] == "shared_reference")
        .collect();
    assert_eq!(spokes.len(), 3);
    assert_eq!(siblings.len(), 3);
    assert!(
        spokes
            .iter()
            .all(|e| e["reference"]["referenced_table"] == "customers__customers")
    );

    let output = run_graph(&["rank", dict.to_str().unwrap(), "--output-format", "json"]);
    assert!(output.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tables = doc["tables"].as_array().unwrap();
    assert_eq!(tables[0]["table"], "customers__customers");
    assert_eq!(tables[0]["degree"], 3);
    assert_eq!(tables[1]["degree"], 1);
    assert_eq!(tables[1]["shared_reference_degree"], 2);
    assert_eq!(
        doc["communities"][0]["label"],
        "customers__customers-centered"
    );

    let output = run_graph(&[
        "explain",
        dict.to_str().unwrap(),
        "orders__orders.customer_id",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("- Degree: 3 (1 bridge, 2 shared-reference, 0 duplicate-schema)"),
        "got: {stdout}"
    );
    let hub = stdout.find("customers__customers").expect("hub row");
    let sibling = stdout.find("[shared-reference link]").expect("sibling row");
    assert!(hub < sibling, "the spoke must lead: {stdout}");
}

#[test]
fn graph_subcommands_reject_bad_flags_and_explain_helps() {
    let output = run_graph(&[
        "explain",
        fixture("edge_graph_chain.ini").to_str().unwrap(),
        "orders.customer_id",
        "--bogus",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized flag"));
    let output = run_graph(&["explain", "--help"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("USAGE"));
}

// ---------------------------------------------------------------------------
// Directory robustness: --continue-on-error, --include/--exclude, and the
// empty-workbook skip. A ragged CSV (header 2 fields, a 3-field row) is
// the corrupt file, notes.xyz the unrecognized one - both committed inline
// since each is three lines, unlike the hand-built empty .xlsx below,
// which lives in fixtures because OOXML can't be written by hand inline.
// ---------------------------------------------------------------------------

/// A directory with one good CSV, one ragged CSV, and one unrecognized
/// file, returning (dir, out) TempDirs the caller runs against.
fn robustness_dir() -> (TempDir, TempDir) {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("good.csv"), "a,b\n1,2\n").unwrap();
    std::fs::write(dir.path().join("bad.csv"), "a,b\n1,2,3\n").unwrap();
    std::fs::write(dir.path().join("notes.xyz"), "just some text\n").unwrap();
    (dir, TempDir::new())
}

#[test]
fn directory_without_continue_on_error_still_fails_fast() {
    let (dir, out) = robustness_dir();
    let output = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--output-dir",
            out.path().to_str().unwrap(),
            "--output-format",
            "json",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("bad.csv"),
        "the failure must name the offending file"
    );
}

#[test]
fn directory_continue_on_error_records_failures_in_json_index() {
    let (dir, out) = robustness_dir();
    let output = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--output-dir",
            out.path().to_str().unwrap(),
            "--output-format",
            "json",
            "--continue-on-error",
        ])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out.path().join("good.csv.dictionary.json").exists());
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join("_index.dictionary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(doc["entries"].as_array().unwrap().len(), 1);
    let failed = doc["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["file"], "bad.csv");
    assert!(
        failed[0]["error"].as_str().unwrap().contains("3 fields"),
        "the recorded chain must carry the real cause"
    );
    assert_eq!(doc["unrecognized"], serde_json::json!(["notes.xyz"]));
    assert_eq!(doc["empty"], serde_json::Value::Array(vec![]));
}

#[test]
fn directory_continue_on_error_records_failures_in_markdown_index() {
    let (dir, out) = robustness_dir();
    let output = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--output-dir",
            out.path().to_str().unwrap(),
            "--continue-on-error",
        ])
        .output()
        .expect("failed to run binary");
    assert!(output.status.success());
    let index = std::fs::read_to_string(out.path().join("_index.dictionary.md")).unwrap();
    assert!(index.contains("## Failed"), "got: {index}");
    assert!(index.contains("bad.csv"), "got: {index}");
    assert!(index.contains("## Skipped"), "got: {index}");
}

#[test]
fn directory_combine_continue_on_error_merges_what_it_can() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("one.csv"), "id\n1\n").unwrap();
    std::fs::write(dir.path().join("two.csv"), "id\n2\n").unwrap();
    std::fs::write(dir.path().join("bad.csv"), "a,b\n1,2,3\n").unwrap();
    let out = TempDir::new();
    let output = Command::new(bin())
        .args([
            dir.path().to_str().unwrap(),
            "--combine",
            "--output-format",
            "json",
            "--output-dir",
            out.path().to_str().unwrap(),
            "--continue-on-error",
        ])
        .output()
        .expect("failed to run binary");
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
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(tables.len(), 2);
    let failed = doc["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["file"], "bad.csv");
}

#[test]
fn directory_include_and_exclude_select_files() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("a.csv"), "x\n1\n").unwrap();
    std::fs::write(dir.path().join("b.json"), "[{\"x\": 1}]").unwrap();
    std::fs::create_dir_all(dir.path().join("skipme")).unwrap();
    std::fs::write(dir.path().join("skipme").join("c.csv"), "x\n2\n").unwrap();

    // Returns this run's own output TempDir (kept alive by the caller)
    // plus every top-level output filename inside it.
    let run = |extra: &[&str]| {
        let out = TempDir::new();
        let mut args = vec![
            dir.path().to_str().unwrap().to_string(),
            "--output-dir".to_string(),
            out.path().to_str().unwrap().to_string(),
            "--output-format".to_string(),
            "json".to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        let output = Command::new(bin())
            .args(&args)
            .output()
            .expect("failed to run binary");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut names: Vec<String> = std::fs::read_dir(out.path())
            .unwrap()
            .filter_map(|e| {
                let e = e.unwrap();
                // Outputs only: a mirrored subdirectory (like skipme/)
                // holds outputs deeper down, it is not one itself.
                e.file_type()
                    .unwrap()
                    .is_file()
                    .then(|| e.file_name().to_string_lossy().into_owned())
            })
            .filter(|n| n != "_index.dictionary.json")
            .collect();
        names.sort();
        (out, names)
    };

    // Exclude by extension at any depth; the nested c.csv is still
    // processed (only *.json is excluded), landing mirrored one level
    // down - so the top level holds just a.csv's output.
    let (out, names) = run(&["--exclude", "*.json"]);
    assert_eq!(names, vec!["a.csv.dictionary.json"]);
    assert!(
        out.path()
            .join("skipme")
            .join("c.csv.dictionary.json")
            .exists()
    );
    // Exclude a whole subtree: nothing under skipme/ is even walked, so
    // its mirrored directory never appears either.
    let (out, names) = run(&["--exclude", "skipme/**"]);
    assert_eq!(
        names,
        vec!["a.csv.dictionary.json", "b.json.dictionary.json"]
    );
    assert!(!out.path().join("skipme").exists());
    // Include allow-lists; combined with exclude, both must agree.
    let (_out, names) = run(&["--include", "*.csv", "--exclude", "skipme/**"]);
    assert_eq!(names, vec!["a.csv.dictionary.json"]);
}

#[test]
fn directory_flags_are_rejected_for_single_file_input() {
    for flag in ["--continue-on-error", "--include", "--exclude"] {
        let arg = if flag == "--continue-on-error" {
            flag.to_string()
        } else {
            format!("{flag}=*.csv")
        };
        let output = Command::new(bin())
            .args([fixture("sample.csv").to_str().unwrap(), &arg])
            .output()
            .expect("failed to run binary");
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("when the input path is a directory"),
            "got: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[cfg(feature = "xlsx")]
fn directory_empty_workbook_skips_with_a_note() {
    let dir = TempDir::new();
    std::fs::copy(
        fixture("edge_xlsx_empty_sheets.xlsx"),
        dir.path().join("empty.xlsx"),
    )
    .unwrap();
    std::fs::write(dir.path().join("good.csv"), "a,b\n1,2\n").unwrap();
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
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join("_index.dictionary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(doc["entries"].as_array().unwrap().len(), 1);
    assert_eq!(doc["empty"], serde_json::json!(["empty.xlsx"]));
    assert_eq!(doc["failed"], serde_json::Value::Array(vec![]));
}

#[test]
#[cfg(feature = "xlsx")]
fn single_file_empty_workbook_keeps_its_clean_error() {
    let output = Command::new(bin())
        .args([
            fixture("edge_xlsx_empty_sheets.xlsx").to_str().unwrap(),
            "-",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no non-empty sheets found"),
        "got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// Jupyter notebooks (.ipynb, --features ipynb): standard JSON with a fixed
// top-level shape (nbformat v4), so the reader is plumbing over the
// always-on core JSON parser - one record per object in the top-level
// `cells` array, flattened exactly like any other array-of-objects JSON.
// A cell's own `source` (a list of line strings, not one joined string)
// pools into a Vec<String> column by the existing array convention.
// ---------------------------------------------------------------------------

#[test]
#[cfg(feature = "ipynb")]
fn ipynb_profiles_cells_as_records() {
    let doc = run_json("sample.ipynb", &[]);
    assert_eq!(doc["format"], "ipynb");
    let cols = table(&doc, "sample");
    // One row per cell: markdown + two code cells.
    assert_eq!(column(cols, "cell_type")["ideal_type"], "String");
    assert_eq!(column(cols, "source")["ideal_type"], "Vec<String>");
    // Only the two code cells carry an execution count - the markdown
    // cell's absence is a real missing value, not a zero.
    let count = column(cols, "execution_count");
    assert_eq!(count["ideal_type"], "i64");
    assert_eq!(count["missing_pct"], 33.3);
    // Nested outputs flatten like any other nested JSON object.
    assert_eq!(
        column(cols, "outputs.output_type")["ideal_type"],
        "enum / category"
    );
}

#[test]
#[cfg(feature = "ipynb")]
fn ipynb_recognizes_semantic_types_through_cells() {
    // Scalar leaves pooled across cells keep their precise type (a real
    // array literal would wrap as Vec<T> instead - see source above).
    let doc = run_json("type_detection.ipynb", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(column(cols, "metadata.user_uuid")["ideal_type"], "UUID");
    assert_eq!(
        column(cols, "metadata.contact_email")["ideal_type"],
        "Email"
    );
    assert_eq!(column(cols, "metadata.ip_address")["ideal_type"], "IPv4");
    assert_eq!(
        column(cols, "metadata.signup_date")["ideal_type"],
        "NaiveDate / DateTime"
    );
}

#[test]
#[cfg(feature = "ipynb")]
fn ipynb_without_cells_array_is_an_actionable_error() {
    let output = Command::new(bin())
        .args([fixture("edge_ipynb_no_cells.ipynb").to_str().unwrap(), "-"])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("doesn't look like a Jupyter notebook"),
        "got: {stderr}"
    );
    assert!(stderr.contains("`cells`"), "got: {stderr}");
}

#[test]
#[cfg(feature = "ipynb")]
fn ipynb_non_object_cell_is_an_actionable_error() {
    // Valid JSON, invalid notebook: the error must name the malformed
    // element, not mislabel the file as unparseable.
    let output = Command::new(bin())
        .args([
            fixture("edge_ipynb_scalar_cell.ipynb").to_str().unwrap(),
            "-",
        ])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not an object"), "got: {stderr}");
    assert!(!stderr.contains("failed to parse"), "got: {stderr}");
}

#[test]
#[cfg(feature = "ipynb")]
fn malformed_ipynb_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.ipynb");
}

// ---------------------------------------------------------------------------
// PDF text (.pdf, --features pdf): hand-rolled page-text reader - xref
// table/stream walk, FlateDecode/ASCII85/ASCIIHex/RunLength streams,
// WinAnsi/MacRoman/Differences/ToUnicode font decoding, one record per
// page (`page_number`, `text`). Fixtures are hand-built (byte-assembled
// with computed xref offsets, not written by any PDF library - none is
// installed here), each exercising one structural shape.
// ---------------------------------------------------------------------------

#[test]
#[cfg(feature = "pdf")]
fn pdf_profiles_pages_as_records() {
    let doc = run_json("sample.pdf", &[]);
    assert_eq!(doc["format"], "pdf");
    let cols = table(&doc, "sample");
    let num = column(cols, "page_number");
    assert_eq!(num["ideal_type"], "i64");
    assert_eq!(num["missing_pct"], 0.0);
    let text = column(cols, "text");
    assert_eq!(text["ideal_type"], "String");
    // Page 3 carries no /Contents: a real missing value, not an empty
    // string standing in for one.
    assert_eq!(text["missing_pct"], 33.3);
    // A TJ fragment pair joins without a space; separate shows don't.
    assert_eq!(
        text["sample_values"],
        serde_json::json!(["Hello, World!\nHello", "Cab"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_decodes_winansi_macroman_and_tounicode_faithfully() {
    // Byte 0x80 through WinAnsi is U+20AC; MacRoman 0xDE is the `fi`
    // ligature glyph (U+FB01 - expanded to `fi` in the extracted text,
    // Unicode's own NFKC mapping) and 0xDB is U+20AC (the iconv-oracle
    // fix); the CMap page maps custom codes to CJK. Any of these coming
    // back wrong means the font layer mangled real bytes, not a
    // heuristic disagreement.
    let doc = run_json("type_detection.pdf", &[]);
    let cols = table(&doc, "type_detection");
    assert_eq!(
        column(cols, "text")["sample_values"],
        serde_json::json!(["Price: €50, mail a@b.com", "fish €50", "中文"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_reads_flate_content_and_differences_fonts() {
    let doc = run_json("edge_pdf_flate_content.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_flate_content"), "text")["sample_values"],
        serde_json::json!(["Compressed hello"])
    );
    // `/Differences [65 /Aacute 66 /Beta 67 /uni2010 68 /Euro.069
    // 69 /AEacute]` over a WinAnsi base: AGL names, `uniXXXX` names,
    // subset-suffixed names, and precomposed AE-acute alike.
    let doc = run_json("edge_pdf_differences.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_differences"), "text")["sample_values"],
        serde_json::json!(["ÁΒ‐€Ǽ"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_accepts_differences_without_a_base_when_content_stays_mapped() {
    // No /BaseEncoding, no /ToUnicode: accepted exactly when every shown
    // code has an explicit mapping (the real subset-font shape).
    let doc = run_json("edge_pdf_differences_nobase.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_differences_nobase"), "text")["sample_values"],
        serde_json::json!(["Hi!"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_differences_without_a_base_apply_to_the_implicit_base_encoding() {
    // ISO 32000-1 Table 114: with no /BaseEncoding, an unembedded
    // nonsymbolic font's differences apply to StandardEncoding - so the
    // unlisted code 66 reads as `B`.
    let doc = run_json("edge_pdf_differences_unmapped.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_differences_unmapped"), "text")["sample_values"],
        serde_json::json!(["AB"])
    );
    // A symbolic font with no embedded program has no base to fall back
    // on: a shown code its differences don't cover reads as U+FFFD, and
    // the note names the code.
    let doc = run_json("edge_pdf_differences_unmapped_symbolic.pdf", &[]);
    let text = column(
        table(&doc, "edge_pdf_differences_unmapped_symbolic"),
        "text",
    );
    assert_eq!(text["sample_values"], serde_json::json!(["A\u{FFFD}"]));
    let notes = text["notes"].as_str().unwrap();
    assert!(
        notes.contains(
            "font /F1/page1 shows code 66 with no mapping (no base encoding, no ToUnicode)"
        ),
        "got: {notes}"
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_reads_a_symbolic_cff_fonts_own_built_in_encoding() {
    // A symbolic embedded CFF (/FontFile3 /Subtype /Type1C) font with no
    // /Encoding and no /ToUnicode: its own custom encoding maps 65/70/90/
    // 120 to Gamma/eacute/fi/angbracketleft (built with fontTools, which
    // reads the same table back); the `fi` glyph's U+FB01 ligature reads
    // as the letters `fi` in extracted text.
    let doc = run_json("edge_pdf_cff_builtin_encoding.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_cff_builtin_encoding"), "text")["sample_values"],
        serde_json::json!(["\u{0393}\u{00E9}fi\u{27E8}"])
    );
    // /Differences without /BaseEncoding apply on top of that same
    // program encoding: 70 is overridden to `A`, 90 still reads `fi`.
    let doc = run_json("edge_pdf_differences_over_program_base.pdf", &[]);
    assert_eq!(
        column(
            table(&doc, "edge_pdf_differences_over_program_base"),
            "text"
        )["sample_values"],
        serde_json::json!(["Afi"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_program_encoding_gaps_read_as_replacement_chars_and_say_why() {
    // Code 65 is `Gamma` in the CFF program's own encoding; the second
    // shown code is an unknown glyph (200) or one the program leaves
    // unassigned (66). Only that code reads as U+FFFD.
    for (name, needle) in [
        (
            "edge_pdf_cff_builtin_unknown_glyph",
            "font /F1/page1 maps code 200 to unknown glyph /zzzunknownglyph",
        ),
        (
            "edge_pdf_cff_builtin_unassigned_code",
            "font /F1/page1 shows code 66, which its embedded font program's built-in encoding doesn't assign",
        ),
    ] {
        let doc = run_json(&format!("{name}.pdf"), &[]);
        let text = column(table(&doc, name), "text");
        assert_eq!(
            text["sample_values"],
            serde_json::json!(["\u{0393}\u{FFFD}"]),
            "{name}"
        );
        let notes = text["notes"].as_str().unwrap();
        assert!(notes.contains(needle), "{name}: {notes}");
    }
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_reads_a_symbolic_type1_fonts_cleartext_encoding_vector() {
    // The Type 1 program's cleartext declares `dup 65 /Gamma put` etc.;
    // a decoy `/Encoding StandardEncoding def` inside a comment and inside
    // the /Notice string must not be mistaken for the real declaration.
    let doc = run_json("edge_pdf_type1_builtin_encoding.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_type1_builtin_encoding"), "text")["sample_values"],
        serde_json::json!(["\u{0393}\u{00E9}\u{22A2}"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_identity_h_codes_are_two_bytes_and_unmapped_ones_are_disclosed() {
    // `<0041><0042><004C>` through an Identity-H font whose ToUnicode maps
    // only the first two: the unmapped code is one U+FFFD (the old decoder
    // split it into NUL + `L`), and the `text` column says how many codes
    // read that way.
    let doc = run_json("edge_pdf_identity_h_partial_tounicode.pdf", &[]);
    let text = column(table(&doc, "edge_pdf_identity_h_partial_tounicode"), "text");
    assert_eq!(text["sample_values"], serde_json::json!(["is\u{FFFD}"]));
    let notes = text["notes"].as_str().unwrap();
    assert!(
        notes.contains(
            "1 character code(s) had no Unicode mapping in their font and read as U+FFFD"
        ),
        "got: {notes}"
    );
    // A ToUnicode that's present but maps nothing leaves the whole font
    // undecodable - each two-byte code is one U+FFFD, not two - and the
    // note names that shape rather than calling the ToUnicode missing.
    let doc = run_json("edge_pdf_identity_h_empty_tounicode.pdf", &[]);
    let text = column(table(&doc, "edge_pdf_identity_h_empty_tounicode"), "text");
    assert_eq!(
        text["sample_values"],
        serde_json::json!(["\u{FFFD}\u{FFFD}"])
    );
    let notes = text["notes"].as_str().unwrap();
    assert!(
        notes.contains("uses /Identity-H with a /ToUnicode that maps no codes"),
        "got: {notes}"
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_a_form_whose_text_cant_be_decoded_is_skipped_and_disclosed() {
    // The page's own text survives; the Form's content stream has a stray
    // `)` with more content after it - genuinely malformed, not a font
    // problem - so the Form is skipped, and the `text` column says so,
    // naming the Form and the root cause, instead of losing it silently.
    let doc = run_json("edge_pdf_form_skipped_is_disclosed.pdf", &[]);
    let text = column(table(&doc, "edge_pdf_form_skipped_is_disclosed"), "text");
    assert_eq!(text["sample_values"], serde_json::json!(["Kept"]));
    let notes = text["notes"].as_str().unwrap();
    assert!(
        notes.contains("text of 1 Form XObject(s) skipped because it couldn't be decoded (reason: Form /Fm0 on page 1: unexpected byte 0x29"),
        "got: {notes}"
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_form_xobject_fonts_are_scoped_per_page_not_just_per_invocation() {
    // Each page invokes its own Form, and each Form names a *different*
    // font `/F1` (page 2's maps every letter of "Plain" to Z). The font
    // cache used to key a Form's fonts by a counter that restarts on each
    // page, so page 2 reused page 1's `/F1` and read "Plain" again.
    let doc = run_json("edge_pdf_form_fonts_scoped_per_page.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_form_fonts_scoped_per_page"), "text")["sample_values"],
        serde_json::json!(["Plain", "ZZZZZ"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_restores_the_text_font_when_the_graphics_state_is_restored() {
    // `q BT /F2 Tf (x) Tj ET Q BT (After) Tj ET`: `After` is shown after
    // `Q` restored the graphics state, so it's in /F1 again. /F2 maps every
    // letter of "After" to `Z`; the old reader leaked /F2 past `Q` and
    // read "ZZZZZ". PDFium reads the same page as "Before ✓ After".
    let doc = run_json("edge_pdf_font_restored_after_q.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_font_restored_after_q"), "text")["sample_values"],
        serde_json::json!(["Before\n\u{2713}\nAfter"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_resolves_full_agl_tex_and_underscore_component_glyph_names() {
    // `/Differences [65 /G_tildecomb /cedilla /angbracketleft
    // /propersubset]`: an AGL-spec underscore ligature (G + U+0303), a
    // full-AGL name the old curated table lacked, a TeX-only name from
    // texglyphlist.txt, and a name the curated table used to map wrong
    // (U+228A instead of the AGL's U+2282).
    let doc = run_json("edge_pdf_agl_components.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_agl_components"), "text")["sample_values"],
        serde_json::json!(["G\u{0303}\u{00B8}\u{27E8}\u{2282}"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_an_unknown_glyph_name_costs_only_the_codes_a_page_shows() {
    // `/Differences [65 /A /zzznotaglyph]`: a font may name glyphs a page
    // never shows, and a ToUnicode entry covers one it does. Only a shown
    // code with neither reads as U+FFFD - the rest of the font still
    // decodes - and the note names the code and the glyph.
    let doc = run_json("edge_pdf_unknown_glyph_unshown.pdf", &[]);
    let text = column(table(&doc, "edge_pdf_unknown_glyph_unshown"), "text");
    assert_eq!(text["sample_values"], serde_json::json!(["A"]));
    assert!(!text["notes"].as_str().unwrap().contains("U+FFFD"));
    let doc = run_json("edge_pdf_unknown_glyph_tounicode.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_unknown_glyph_tounicode"), "text")["sample_values"],
        serde_json::json!(["A\u{263A}"])
    );
    let doc = run_json("edge_pdf_unknown_glyph_shown.pdf", &[]);
    let text = column(table(&doc, "edge_pdf_unknown_glyph_shown"), "text");
    assert_eq!(text["sample_values"], serde_json::json!(["A\u{FFFD}"]));
    let notes = text["notes"].as_str().unwrap();
    assert!(
        notes.contains("1 character code(s) had no Unicode mapping in their font and read as U+FFFD (first known reason: font /F1/page1 maps code 66 to unknown glyph /zzznotaglyph)"),
        "got: {notes}"
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_differences_running_past_code_255_degrades_that_font_not_a_panic() {
    // `/Differences [255 /a /b]`: `/b` would land on code 256. This used
    // to index past the 256-entry table and panic; now the malformed font
    // reads as U+FFFD and the note says why.
    let doc = run_json("edge_pdf_differences_overflow.pdf", &[]);
    let text = column(table(&doc, "edge_pdf_differences_overflow"), "text");
    assert_eq!(
        text["sample_values"],
        serde_json::json!(["\u{FFFD}\u{FFFD}"])
    );
    let notes = text["notes"].as_str().unwrap();
    assert!(
        notes.contains("has a /Differences entry past code 255"),
        "got: {notes}"
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_symbol_delimiter_pieces_read_as_their_unicode_characters() {
    // The AGL maps TeX/Symbol's extensible-delimiter pieces into the
    // Private Use Area; Unicode 3.2 gave them real characters.
    let doc = run_json("edge_pdf_symbol_delimiter_pieces.pdf", &[]);
    let text = column(table(&doc, "edge_pdf_symbol_delimiter_pieces"), "text");
    assert_eq!(
        text["sample_values"],
        serde_json::json!(["\u{23A1}\u{23A2}\u{23A3}\u{239E}\u{23AE}\u{23D0}"])
    );
    assert!(!text["notes"].as_str().unwrap().contains("U+FFFD"));
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_standard_symbol_and_zapfdingbats_fonts_use_their_built_in_encodings() {
    // Unembedded `/BaseFont /Symbol` and `/ZapfDingbats` with no usable
    // `/Encoding`: the standard fonts' published encodings apply (ISO
    // 32000-1 Annex D.5/D.6). The ZapfDingbats font's `/Differences`
    // name `a20` resolves through the ITC Zapf Dingbats Glyph List -
    // the AcroForm checkbox shape (code `4` is a check mark too).
    for (name, expected) in [
        (
            // The two strings are drawn back to back, so they join.
            "edge_pdf_standard_symbol_font",
            "\u{03B1}\u{03B2}\u{03B3}\u{03C0}\u{2192}\u{239B}",
        ),
        (
            "edge_pdf_standard_zapfdingbats_font",
            "\u{2714}\u{2714}\u{25CF}\u{25A0}",
        ),
    ] {
        let doc = run_json(&format!("{name}.pdf"), &[]);
        let text = column(table(&doc, name), "text");
        assert_eq!(
            text["sample_values"],
            serde_json::json!([expected]),
            "{name}"
        );
        assert!(
            !text["notes"].as_str().unwrap().contains("U+FFFD"),
            "{name}"
        );
    }
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_reads_xref_streams_and_object_streams() {
    // Modern writer shape: no `xref` table at all, object offsets from a
    // compressed xref stream, and the font dictionary itself packed into
    // an object stream (type-2 entries).
    let doc = run_json("edge_pdf_xref_stream.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_xref_stream"), "text")["sample_values"],
        serde_json::json!(["Streamed xref hello"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_follows_incremental_updates_to_the_newest_content() {
    // Two xref sections joined by /Prev; the update rewrites the page to
    // point at new content. Newest-first merge must win.
    let doc = run_json("edge_pdf_incremental.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_incremental"), "text")["sample_values"],
        serde_json::json!(["version two"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_encrypted_is_a_clean_refusal() {
    // A malformed `/Encrypt` dictionary (real `/Filter /Standard`, but
    // missing every field the Standard Security Handler actually needs
    // to derive a key) still fails cleanly, naming the missing field -
    // never a crash or a silent wrong-key decrypt.
    let output = Command::new(bin())
        .args([fixture("edge_pdf_encrypted.pdf").to_str().unwrap(), "-"])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("/Encrypt dictionary without /R"),
        "got: {stderr}"
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_decrypts_rc4_and_aes128_with_an_empty_user_password() {
    // Both fixtures are real, pikepdf-encrypted PDFs (RC4/R3 and
    // AES-128/R4 "AESV2" respectively) protected only by an owner
    // password - no user password at all, the overwhelming common
    // real-world shape ("restrict printing/editing", not "require a
    // password to even open it"). Both must decrypt to the exact same
    // plaintext the unencrypted source document had.
    for name in [
        "edge_pdf_encrypted_rc4.pdf",
        "edge_pdf_encrypted_aes128.pdf",
    ] {
        let doc = run_json(name, &[]);
        let table_name = name.trim_end_matches(".pdf");
        assert_eq!(
            column(table(&doc, table_name), "text")["sample_values"],
            serde_json::json!(["This PDF is encrypted but the user password is empty."]),
            "fixture {name} did not decrypt to the expected plaintext"
        );
    }
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_decrypts_aes256_r5_and_r6_with_an_empty_user_password() {
    // Both fixtures are real, pikepdf-encrypted PDFs (/V 5, AES-256
    // "AESV3") protected only by an owner password - no user password
    // at all, matching the RC4/AES-128 fixtures' own real-world shape.
    // /R 5 (Adobe's deprecated pre-ISO draft - a single SHA-256 key
    // derivation round) and /R 6 (the standardized ISO 32000-2 revision
    // - the full 64-round "hardened hash") use genuinely different key
    // derivation, so both get their own real fixture rather than
    // trusting one to stand in for the other.
    for name in ["edge_pdf_encrypted_r5.pdf", "edge_pdf_encrypted_aes256.pdf"] {
        let doc = run_json(name, &[]);
        let table_name = name.trim_end_matches(".pdf");
        assert_eq!(
            column(table(&doc, table_name), "text")["sample_values"],
            serde_json::json!(["This PDF is encrypted but the user password is empty."]),
            "fixture {name} did not decrypt to the expected plaintext"
        );
    }
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_with_a_real_user_password_refuses_distinctly_from_a_malformed_encrypt_dict() {
    // A real, pikepdf-encrypted file whose user password is genuinely
    // non-empty ("realuserpassword") - this must fail the Standard
    // Security Handler's own /U password check (Algorithm 5) and refuse
    // cleanly, distinctly from both the malformed-/Encrypt-dict case
    // above and a successful empty-password decrypt - never silently
    // decrypt every stream to garbage with the wrong key.
    let output = Command::new(bin())
        .args([fixture("edge_pdf_encrypted_real_password.pdf")
            .to_str()
            .unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("encrypted with a real user password"),
        "got: {stderr}"
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_lzw_stream_that_is_not_lzw_fails_cleanly() {
    // The stream says LZWDecode but holds plain text: decoding it yields
    // garbage, which the content parser rejects - a clean error, no panic.
    let output = Command::new(bin())
        .args([fixture("edge_pdf_lzw_garbage.pdf").to_str().unwrap(), "-"])
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "got: {stderr}");
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_falls_back_to_standard_encoding_for_a_nonsymbolic_font() {
    // A standard-14 font with neither /Encoding nor /ToUnicode - PDF's
    // own documented fallback (32000-1 9.6.6.2) for a *nonsymbolic* font
    // in exactly this shape is Adobe StandardEncoding, cross-checked
    // against pdfminer's own independent StandardEncoding table before
    // being hand-rolled here (see `STANDARD_ENCODING`'s own doc
    // comment) - no longer a disclosed refusal.
    let doc = run_json("edge_pdf_standard_encoding.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_standard_encoding"), "text")["sample_values"],
        serde_json::json!(["plain"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_a_symbolic_font_with_no_mapping_reads_as_replacement_chars_and_says_why() {
    // A symbolic font (FontDescriptor /Flags bit 3) that isn't one of the
    // standard 14, with no /Encoding, no /ToUnicode, and no program to read
    // an encoding from, can't be mapped - but it costs only its own text
    // (U+FFFD per code), and the `text` column says which font and why,
    // instead of refusing the whole document.
    let doc = run_json("edge_pdf_symbolic_no_encoding.pdf", &[]);
    let text = column(table(&doc, "edge_pdf_symbolic_no_encoding"), "text");
    assert_eq!(
        text["sample_values"],
        serde_json::json!(["\u{FFFD}\u{FFFD}"])
    );
    let notes = text["notes"].as_str().unwrap();
    assert!(
        notes.contains("1 font(s) couldn't be decoded at all, so their text reads as U+FFFD (reason: font /F1/page1 has no usable encoding"),
        "got: {notes}"
    );
}

#[test]
#[cfg(feature = "pdf")]
fn malformed_pdf_fails_cleanly() {
    assert_fails_without_panicking("malformed_garbage.pdf");
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_empty_pages_produces_an_empty_table() {
    let doc = run_json("edge_pdf_empty_pages.pdf", &[]);
    assert_eq!(
        doc["tables"]["edge_pdf_empty_pages"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_nrows_bounds_pages_decoded() {
    let doc = run_json("sample.pdf", &["--nrows", "2"]);
    let cols = table(&doc, "sample");
    assert_eq!(column(cols, "page_number")["row_count"], 2);
    // The blank third page falls outside the cutoff, so no missing %.
    assert_eq!(column(cols, "text")["missing_pct"], 0.0);
}

#[test]
#[cfg(feature = "pdf")]
fn content_sniffing_pdf_without_extension() {
    let doc = run_json("edge_sniff_pdf_no_ext", &[]);
    assert_eq!(doc["format"], "pdf");
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_tolerates_trailing_pad_after_a_zlib_stream() {
    // A real writer artifact (first found in a French university PDF):
    // one stray NUL after the zlib end. The Adler trailer is verified
    // where the DEFLATE stream actually ends, not at input end.
    let doc = run_json("edge_pdf_flate_trailing_pad.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_flate_trailing_pad"), "text")["sample_values"],
        serde_json::json!(["Trailing pad tolerated"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_inherits_resources_from_pages_ancestors() {
    // `/Resources` lives on the `/Pages` node, not the page itself -
    // the spec's inheritable-attributes rule, not a redundant copy.
    let doc = run_json("edge_pdf_inherited_resources.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_inherited_resources"), "text")["sample_values"],
        serde_json::json!(["Inherited resources work"])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_salvages_page_text_from_a_flate_stream_truncated_partway_through() {
    // A content stream's own FlateDecode payload cut off before its
    // fourth (and final) `Tj` operator ever closes - the shape a real,
    // interrupted download or write leaves behind. The three already-
    // complete text-showing operations before the cut are real, valid
    // text and must survive; only the incomplete fourth line is lost.
    // Each `0 -14 Td` starts a new line, and the text says so.
    let doc = run_json("edge_pdf_truncated_flate_salvage.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_truncated_flate_salvage"), "text")["sample_values"],
        serde_json::json!([
            "First recoverable line of real text.\nSecond recoverable line of real text.\nThird recoverable line of real text."
        ])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_word_and_line_breaks_come_from_where_glyphs_are_drawn() {
    // One page per case, each checked against PDFium's own text (which
    // agrees on all eight). The font is an unembedded Helvetica with no
    // `/Widths`, so every position also depends on its Core 14 metrics.
    // The build before this change got seven of the eight wrong.
    let doc = run_json("edge_pdf_glyph_positions.pdf", &["--samples", "20"]);
    assert_eq!(
        column(table(&doc, "edge_pdf_glyph_positions"), "text")["sample_values"],
        serde_json::json!([
            // `TJ`: a -250 gap is a word break, an 80 kern is not; `Td`
            // down is a new line.
            "Hello world\nWord",
            // Every glyph placed by its own `Td`, one space width apart
            // between the words.
            "to go",
            // PowerPoint's negative `Tc` won back by `TJ` gaps: measured
            // from each glyph's ink, not from where the next would go.
            "salaries rise",
            // A Form XObject's `/Matrix` puts its text on the page's line.
            "Before inside\nafter",
            // A tiny space glyph drawn over a word doesn't split it...
            "Graphing",
            // ...but a word's own trailing space always ends it (OCR
            // layers draw each word to its scanned box, overlapping).
            "Place of",
            // `aw ac string "` shows its string on the next line.
            "first\nsecond",
            // Letter spacing (`Tc`) never splits one string into letters.
            "SPACED",
        ])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_actual_text_stands_in_for_the_glyphs_it_covers() {
    // One page per case, each shaped like a real producer's output. ISO
    // 32000-1 14.9.4 is the reference, not PDFium: PDFium applies
    // ActualText only when the covered font can encode one of its
    // characters, and never across a `Do`, so it agrees on just the
    // second and fifth pages. The build before this change got every
    // page but the seventh wrong.
    let doc = run_json("edge_pdf_actual_text.pdf", &["--samples", "20"]);
    assert_eq!(
        column(table(&doc, "edge_pdf_actual_text"), "text")["sample_values"],
        serde_json::json!([
            // Skia: a ligature glyph whose ToUnicode is U+0000.
            "define",
            // A named property list, from `/Resources /Properties`.
            "AéB",
            // InDesign: a tab over a space glyph is the break itself, and
            // U+0007 is no text at all (the gap still breaks the word).
            "a\tb c",
            // Nested: the outer ActualText wins; a `BMC` inside it and a
            // stray `EMC` after it change nothing.
            "X e",
            // A Form's unclosed `BDC` ends with the Form.
            "Before Form\nafter",
            // ActualText around a `Do` replaces the Form's text.
            "Before Swap",
            // A tab leader: ActualText of U+0008 and U+FFFDs is no
            // ActualText, so the dots it covers stay.
            "Name ..",
            // InDesign: an unused hyphenation point drawn as a space glyph
            // is a soft hyphen inside the word, not a word break.
            "communi\u{AD}cations",
        ])
    );
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_recurses_into_a_form_xobject_but_skips_an_unreadable_image_xobject() {
    // A real, common shape this project's reader never handled at all
    // before: a page's own `/Do` operator invoking a Form XObject (its
    // own nested content stream, e.g. PowerPoint-to-PDF slide exports
    // and e-signature caption overlays both lean on this) - its text
    // must be recursed into and spliced in the right position relative
    // to the page's own direct text before/after it. The same page also
    // references a second XObject shaped like an Image using a codec
    // this reader doesn't implement (DCTDecode) - resolving it fails,
    // and that failure must never cost the rest of the page's real text.
    // The three are drawn on three different lines (the Form at its own
    // origin), so the text breaks between them.
    let doc = run_json("edge_pdf_form_xobject.pdf", &[]);
    assert_eq!(
        column(table(&doc, "edge_pdf_form_xobject"), "text")["sample_values"],
        serde_json::json!(["Direct text before.\nText from inside the form.\nDirect text after."])
    );
}

// --- Knowledge graph (`sniff-rs graph`) ---

#[cfg(all(feature = "ipynb", feature = "pdf"))]
fn kg_link<'a>(
    doc: &'a serde_json::Value,
    relation: &str,
    source: &str,
    target: &str,
) -> Option<&'a serde_json::Value> {
    doc["links"].as_array().unwrap().iter().find(|l| {
        l["relation"] == relation
            && ((l["source"] == source && l["target"] == target)
                || (l["source"] == target && l["target"] == source))
    })
}

#[test]
#[cfg(all(feature = "ipynb", feature = "pdf"))]
fn knowledge_graph_links_every_data_type_in_a_folder() {
    let tmp = TempDir::new();
    let out = tmp.path().join("kg");
    let output = run_graph(&[
        "graph",
        fixture("edge_knowledge_graph").to_str().unwrap(),
        out.to_str().unwrap(),
        "--obsidian",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("graph.json")).unwrap()).unwrap();
    assert!(
        doc["graph"]["generator"]
            .as_str()
            .unwrap()
            .starts_with("sniff-rs")
    );
    assert_eq!(doc["directed"], false);
    let ids: Vec<&str> = doc["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    for file in [
        "analysis/analysis.ipynb",
        "analysis/plot.png",
        "crm/customers.csv",
        "crm/orders.json",
        "docs/notes.md",
        "docs/report.docx",
        "docs/report.pdf",
        "lectures/lecture1.txt",
        "lectures/lecture2.txt",
        "lectures/recipes.txt",
        "sales/sales_2024.csv",
        "sales/sales_2025.csv",
    ] {
        assert!(ids.contains(&file), "missing node {file}: {ids:?}");
    }
    assert!(
        !ids.iter().any(|id| id.contains(".DS_Store")
            || id.contains("__pycache__")
            || id.contains(".ipynb_checkpoints")),
        "clutter must be skipped: {ids:?}"
    );

    let join = kg_link(&doc, "joins", "crm/customers.csv", "crm/orders.json").expect("join");
    assert_eq!(join["confidence"], "EXTRACTED");
    assert!(
        join["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("foreign-key naming"))
    );

    for (from, to) in [
        ("analysis/analysis.ipynb", "crm/customers.csv"),
        ("analysis/analysis.ipynb", "analysis/plot.png"),
        ("docs/notes.md", "docs/report.pdf"),
    ] {
        let link =
            kg_link(&doc, "references", from, to).unwrap_or_else(|| panic!("{from} -> {to}"));
        assert_eq!(
            link["source"], from,
            "a reference points from the naming file"
        );
        assert_eq!(link["confidence"], "EXTRACTED");
    }

    for (entity, files) in [
        (
            "email:alice@acme-corp.com",
            &["crm/customers.csv", "crm/orders.json", "docs/report.pdf"][..],
        ),
        (
            "doi:10.1000/xyz123",
            &["analysis/analysis.ipynb", "docs/report.pdf"][..],
        ),
        (
            "isbn:9780306406157",
            &["docs/notes.md", "docs/report.pdf"][..],
        ),
        ("code:SIT720", &["docs/notes.md", "docs/report.pdf"][..]),
    ] {
        for file in files {
            assert!(
                kg_link(&doc, "mentions", file, entity).is_some(),
                "{file} should mention {entity}"
            );
        }
    }
    assert_eq!(
        kg_link(&doc, "mentions", "docs/report.pdf", "code:SIT720").unwrap()["confidence"],
        "INFERRED",
        "a course code is a shape heuristic"
    );

    let schemas: Vec<&serde_json::Value> = doc["links"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["relation"] == "has_schema")
        .collect();
    assert_eq!(schemas.len(), 2);
    assert_eq!(schemas[0]["target"], schemas[1]["target"]);
    assert!(
        kg_link(
            &doc,
            "similar_to",
            "lectures/lecture1.txt",
            "lectures/lecture2.txt"
        )
        .is_some()
    );
    assert!(kg_link(&doc, "same_name", "docs/report.docx", "docs/report.pdf").is_some());
    assert!(
        !doc["links"].as_array().unwrap().iter().any(|l| l["source"]
            == "lectures/recipes.txt"
            || l["target"] == "lectures/recipes.txt"),
        "an unrelated file links to nothing"
    );

    let report = std::fs::read_to_string(out.join("GRAPH_REPORT.md")).unwrap();
    assert!(report.contains("## Connections across data types"));
    assert!(
        report.contains("`lectures/recipes.txt` (txt)"),
        "isolated file listed: {report}"
    );

    let vault = out.join("obsidian");
    let note = std::fs::read_to_string(vault.join("files/docs/report.pdf.md")).unwrap();
    assert!(note.starts_with("---\ntype: file\n"), "{note}");
    assert!(
        note.contains("[[entities/email/alice@acme-corp.com|alice@acme-corp.com]]"),
        "{note}"
    );
    assert!(
        note.contains("referenced by [[files/docs/notes.md|notes.md]]"),
        "{note}"
    );
    assert!(vault.join("entities/doi/10.1000_xyz123.md").exists());
    assert!(vault.join("index.md").exists());
    assert!(vault.join(".obsidian/graph.json").exists());

    // A re-run replaces the graph in place; a folder this tool didn't
    // write is refused rather than written into.
    let again = run_graph(&[
        "graph",
        fixture("edge_knowledge_graph").to_str().unwrap(),
        out.to_str().unwrap(),
    ]);
    assert!(again.status.success());
    let foreign = tmp.path().join("mine");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("keep.txt"), "mine").unwrap();
    let refused = run_graph(&[
        "graph",
        fixture("edge_knowledge_graph").to_str().unwrap(),
        foreign.to_str().unwrap(),
    ]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("wasn't written by sniff-rs graph"));
    assert_eq!(
        std::fs::read_to_string(foreign.join("keep.txt")).unwrap(),
        "mine"
    );
}

#[test]
#[cfg(all(feature = "ipynb", feature = "pdf"))]
fn graph_queries_accept_graph_json_and_directories() {
    let tmp = TempDir::new();
    let out = tmp.path().join("kg");
    let dir = fixture("edge_knowledge_graph");
    assert!(
        run_graph(&["graph", dir.to_str().unwrap(), out.to_str().unwrap()])
            .status
            .success()
    );
    let graph_json = out.join("graph.json");

    let explained = run_graph(&["explain", graph_json.to_str().unwrap(), "customers.csv"]);
    assert!(
        explained.status.success(),
        "{}",
        String::from_utf8_lossy(&explained.stderr)
    );
    let text = String::from_utf8_lossy(&explained.stdout);
    assert!(text.starts_with("# customers.csv"), "{text}");
    assert!(text.contains("### joins (1)"), "{text}");
    assert!(
        text.contains("referenced by `analysis/analysis.ipynb`"),
        "{text}"
    );

    let path = run_graph(&[
        "path",
        graph_json.to_str().unwrap(),
        "notes.md",
        "customers.csv",
        "--output-format",
        "json",
    ]);
    assert!(
        path.status.success(),
        "{}",
        String::from_utf8_lossy(&path.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&path.stdout).unwrap();
    assert_eq!(doc["from"], "docs/notes.md");
    assert_eq!(doc["to"], "crm/customers.csv");
    let hops = doc["hops"].as_array().unwrap();
    assert_eq!(hops.len(), 3, "{doc}");
    assert_eq!(hops[0]["relation"], "references");

    // A directory is graphed in memory, with the same answer.
    let from_dir = run_graph(&[
        "path",
        dir.to_str().unwrap(),
        "notes.md",
        "customers.csv",
        "--output-format",
        "json",
    ]);
    assert!(from_dir.status.success());
    let doc2: serde_json::Value = serde_json::from_slice(&from_dir.stdout).unwrap();
    assert_eq!(doc, doc2);

    let none = run_graph(&[
        "path",
        graph_json.to_str().unwrap(),
        "lecture1.txt",
        "recipes.txt",
    ]);
    assert!(!none.status.success());
    assert!(String::from_utf8_lossy(&none.stderr).contains("no path between"));

    let rank = run_graph(&[
        "rank",
        graph_json.to_str().unwrap(),
        "--output-format",
        "json",
    ]);
    assert!(rank.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&rank.stdout).unwrap();
    assert_eq!(doc["nodes"][0]["id"], "crm/customers.csv");
    assert!(!doc["communities"].as_array().unwrap().is_empty());

    let ambiguous = run_graph(&["explain", graph_json.to_str().unwrap(), "report"]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("matches several nodes"));
}

#[test]
fn graph_writes_to_stdout_and_generated_folders_are_never_reprofiled() {
    let tmp = TempDir::new();
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(
        data.join("a.csv"),
        "id,email\n1,x@example.org\n2,y@example.org\n",
    )
    .unwrap();
    std::fs::write(data.join("b.txt"), "Contact x@example.org about a.csv\n").unwrap();

    let json = run_graph(&["graph", data.to_str().unwrap(), "-"]);
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(kg_link_any(&doc, "references", "b.txt", "a.csv"));
    let md = run_graph(&[
        "graph",
        data.to_str().unwrap(),
        "-",
        "--output-format",
        "md",
    ]);
    assert!(String::from_utf8_lossy(&md.stdout).starts_with("# Knowledge graph: data"));

    // A graph written inside the folder it describes is skipped by later
    // walks - neither the graph nor directory mode profiles graph.json.
    let inside = data.join("graph");
    assert!(
        run_graph(&["graph", data.to_str().unwrap(), inside.to_str().unwrap()])
            .status
            .success()
    );
    let again = run_graph(&["graph", data.to_str().unwrap(), "-"]);
    let doc: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert!(
        !doc["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"].as_str().unwrap().starts_with("graph/"))
    );
    let dict = tmp.path().join("dict");
    let batch = Command::new(bin())
        .args([
            data.to_str().unwrap(),
            "--output-dir",
            dict.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        batch.status.success(),
        "{}",
        String::from_utf8_lossy(&batch.stderr)
    );
    assert!(!dict.join("graph").exists());
}

fn kg_link_any(doc: &serde_json::Value, relation: &str, source: &str, target: &str) -> bool {
    doc["links"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["relation"] == relation && l["source"] == source && l["target"] == target)
}

// --- Compressed input recognized by content, and `diff` reading stdin ---

/// A gzip-compressed CSV with no extension at all is decompressed from
/// its own magic bytes; CSV itself still can't be sniffed, so `--format`
/// is still required for the decompressed content.
#[test]
fn extensionless_gzip_csv_is_decompressed_by_its_magic_bytes() {
    let doc = run_json("edge_gzip_no_extension", &["--format", "csv"]);
    assert_eq!(doc["format"], "csv");
    let cols = doc["tables"]["edge_gzip_no_extension"].as_array().unwrap();
    assert_eq!(cols[0]["name"], "user_id");
}

/// Extensionless gzip whose decompressed content is sniffable (JSON Lines)
/// needs no flags at all.
#[test]
fn extensionless_gzip_json_is_decompressed_then_sniffed() {
    let doc = run_json("edge_gzip_json_no_extension", &[]);
    assert_eq!(doc["format"], "json");
    let cols = doc["tables"]["edge_gzip_json_no_extension"]
        .as_array()
        .unwrap();
    assert_eq!(cols[0]["name"], "id");
}

/// Piped gzip on stdin used to reach the readers still compressed.
#[test]
fn stdin_gzip_input_is_decompressed() {
    let gz = std::fs::read(fixture("edge_gzip_no_extension")).unwrap();
    let output = run_with_stdin(
        &gz,
        &["-", "-", "--format", "csv", "--output-format", "json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["tables"]["stdin"][0]["name"], "user_id");
}

/// `diff` accepts `-` for one side; identical data reports no changes.
#[test]
fn diff_reads_one_side_from_stdin() {
    let csv = std::fs::read(fixture("sample.csv")).unwrap();
    let old = fixture("sample.csv");
    let output = run_with_stdin(
        &csv,
        &[
            "diff",
            old.to_str().unwrap(),
            "-",
            "--format",
            "csv",
            "--output-format",
            "json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["changes"].as_array().unwrap().len(), 0);
}

#[test]
fn diff_rejects_stdin_for_both_sides_and_a_stray_format_flag() {
    let both = run_with_stdin(b"", &["diff", "-", "-"]);
    assert!(!both.status.success());
    assert!(String::from_utf8_lossy(&both.stderr).contains("only one of"));
    let a = fixture("sample.csv");
    let stray = Command::new(bin())
        .args([
            "diff",
            a.to_str().unwrap(),
            a.to_str().unwrap(),
            "--format",
            "csv",
        ])
        .output()
        .unwrap();
    assert!(!stray.status.success());
    assert!(String::from_utf8_lossy(&stray.stderr).contains("applies only to"));
}

/// A SAS7BDAT file declaring WINDOWS-1251 (pandas' own datetime fixture)
/// used to be refused over its encoding; it now reads.
#[cfg(feature = "sas7bdat")]
#[test]
fn sas7bdat_reads_a_windows_1251_file() {
    let doc = run_json("sas7bdat_pandas_windows1251.sas7bdat", &[]);
    let cols = table(&doc, "sas7bdat_pandas_windows1251");
    assert_eq!(column(cols, "Date1")["ideal_type"], "NaiveDate / DateTime");
}

/// A strL cell resolves through the `<strls>` table at every release that
/// has one; files written by pandas (`convert_strl`).
#[cfg(feature = "stata")]
#[test]
fn stata_strl_values_resolve_to_their_text() {
    for (release, accented) in [
        (117, "cafÃ© naÃ¯ve"),
        (118, "café naïve"),
        (119, "café naïve"),
    ] {
        let stem = format!("edge_stata_strl_{release}");
        let doc = run_json(&format!("{stem}.dta"), &["--samples", "5"]);
        let note = column(table(&doc, &stem), "note");
        let samples: Vec<&str> = note["sample_values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(samples[0], "short strL", "{release}");
        assert_eq!(samples[1], "x".repeat(3000), "{release}");
        assert_eq!(samples[2], accented, "{release}");
        assert_eq!(note["missing_pct"], 25.0, "{release}");
    }
}

/// MIME decoding, checked against Python's `email` package on the same
/// file: RFC 2047 header words, quoted-printable/base64/8-bit bodies in
/// UTF-8 and ISO-8859-1, and a multipart/mixed message whose text comes
/// from its plain part while its PDF lands in `attachments`.
#[cfg(feature = "mbox")]
#[test]
fn mbox_decodes_mime_headers_bodies_and_attachments() {
    let doc = run_json("edge_mbox_mime.mbox", &["--samples", "5"]);
    let cols = table(&doc, "edge_mbox_mime");
    assert_eq!(
        column(cols, "Subject")["sample_values"][0],
        "Réunion café ☕"
    );
    assert_eq!(
        column(cols, "body")["sample_values"],
        serde_json::json!([
            "Bonjour, à demain.\n",
            "Plain text part: see attachment.\n",
            "Straße naïve\n",
            "Base64 bodied ✓\n"
        ])
    );
    assert_eq!(
        column(cols, "attachments")["sample_values"],
        serde_json::json!(["résumé.pdf"])
    );
}

/// Struct and array columns in a Delta table flatten the way nested
/// Parquet does. Written with `deltalake`.
#[cfg(feature = "delta")]
#[test]
fn delta_table_flattens_nested_columns() {
    let doc = run_json("edge_delta_nested", &[]);
    let cols = table(&doc, "edge_delta_nested");
    assert_eq!(column(cols, "address")["current_type"], "struct");
    assert_eq!(column(cols, "address")["missing_pct"], 33.3);
    assert_eq!(
        column(cols, "address.city")["sample_values"],
        serde_json::json!(["Paris", "Oslo"])
    );
    assert_eq!(column(cols, "tags")["ideal_type"], "Vec<String>");
}

fn copy_dir_recursive(src: &std::path::Path, dest: &std::path::Path) {
    std::fs::create_dir_all(dest).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), to).unwrap();
        }
    }
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_form_fields_and_annotations_are_surfaced() {
    // Checked against pikepdf and pypdf reading the same files. The
    // encrypted copy (AES-128, empty user password) stores every string
    // encrypted, so it also proves strings are decrypted with their
    // object's key.
    for (fixture_name, stem) in [
        (
            "edge_pdf_form_and_annotations.pdf",
            "edge_pdf_form_and_annotations",
        ),
        (
            "edge_pdf_form_and_annotations_encrypted.pdf",
            "edge_pdf_form_and_annotations_encrypted",
        ),
    ] {
        let doc = run_json(fixture_name, &[]);
        let pages = table(&doc, stem);
        assert_eq!(
            column(pages, "annotations")["sample_values"],
            serde_json::json!(["Please review section 2", "Signed off — ok"]),
            "{fixture_name}"
        );
        let form = table(&doc, &format!("{stem}_form"));
        let names: Vec<&str> = form.iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "name",
                "address.city",
                "address.zip",
                "agree",
                "subscribe",
                "langs",
                "notes",
                "email"
            ],
            "{fixture_name}"
        );
        for (name, value) in [
            ("name", serde_json::json!(["José Álvarez"])),
            ("address.city", serde_json::json!(["Paris"])),
            ("address.zip", serde_json::json!(["75001"])),
            ("agree", serde_json::json!(["Yes"])),
            ("subscribe", serde_json::json!(["Off"])),
            ("langs", serde_json::json!(["en", "fr"])),
            ("email", serde_json::json!(["jose@example.com"])),
        ] {
            assert_eq!(
                column(form, name)["sample_values"],
                value,
                "{fixture_name} {name}"
            );
        }
        assert_eq!(
            column(form, "notes")["missing_pct"],
            100.0,
            "{fixture_name}"
        );
    }
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_without_a_form_or_annotations_keeps_its_old_shape() {
    let doc = run_json("sample.pdf", &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(tables.len(), 1);
    let pages = table(&doc, "sample");
    assert!(pages.iter().all(|c| c["name"] != "annotations"));
}

#[test]
#[cfg(feature = "pdf")]
fn sql_output_inline_mode_pdf_emits_pages_and_the_form() {
    let sql = run_sql("edge_pdf_form_and_annotations_encrypted.pdf", &[]);
    assert!(sql.contains("--sql-mode inline"), "{sql}");
    assert!(sql.contains("CREATE TABLE \"edge_pdf_form_and_annotations_encrypted\" ("));
    assert!(sql.contains("CREATE TABLE \"edge_pdf_form_and_annotations_encrypted_form\" ("));
    assert!(sql.contains("'Form page'"), "{sql}");
    assert!(sql.contains("'[\"Signed off — ok\"]'"), "{sql}");
    // The form's dotted field names nest, so every value lands.
    assert!(
        sql.contains("'José Álvarez', 'Paris', 75001, TRUE, FALSE"),
        "{sql}"
    );
}

#[test]
#[cfg(feature = "ipynb")]
fn sql_output_inline_mode_ipynb_emits_one_row_per_cell() {
    let sql = run_sql("type_detection.ipynb", &[]);
    assert!(sql.contains("--sql-mode inline"), "{sql}");
    assert!(sql.contains("'alice@example.com'"), "{sql}");
}

#[test]
#[cfg(feature = "ipynb")]
fn sql_output_default_mode_writes_child_tables_for_an_array_of_objects() {
    // sample.ipynb's `outputs` holds result objects: one child table,
    // keyed to the cell it belongs to - no staging fallback, no note.
    let path = fixture("sample.ipynb");
    let output = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--output-format", "sql"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let sql = String::from_utf8(output.stdout).unwrap();
    assert!(!sql.contains("_staging"), "{sql}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("staging"));
    assert_child_table(&sql, "sample", "sample__outputs");
    // Only the third cell has an output.
    assert_eq!(
        insert_rows(&sql, "sample__outputs"),
        ["(3, 1, '[\"   a  b\"]', 2, 'execute_result')"]
    );
}

/// Reads every file under `root` into a sorted (relative path, bytes) list.
fn read_tree(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn batch_mode_output_is_the_same_with_one_job_or_many() {
    let mut runs = Vec::new();
    for jobs in ["1", "8"] {
        let out = TempDir::new();
        let output = run_dir(&[
            "tests/fixtures/edge_batch_directory",
            "--output-dir",
            out.path().to_str().unwrap(),
            "--output-format",
            "json",
            "--continue-on-error",
            "--jobs",
            jobs,
        ]);
        let stderr =
            String::from_utf8_lossy(&output.stderr).replace(out.path().to_str().unwrap(), "OUT");
        runs.push((output.status.code(), stderr, read_tree(out.path())));
    }
    assert_eq!(runs[0], runs[1]);
}

#[test]
fn batch_mode_fails_fast_on_the_first_failing_file_in_walk_order() {
    // Many good files, then two bad ones: with several workers, the error
    // reported is still the earlier bad file, never the later one.
    let dir = TempDir::new();
    for i in 0..20 {
        std::fs::write(dir.path().join(format!("a{i:02}.csv")), "x,y\n1,2\n").unwrap();
    }
    std::fs::write(dir.path().join("b_bad.csv"), "x,y\n1,2,3\n").unwrap();
    std::fs::write(dir.path().join("c_bad.csv"), "x,y\n1,2,3\n").unwrap();
    let out = TempDir::new();
    for _ in 0..5 {
        let output = run_dir(&[
            dir.path().to_str().unwrap(),
            "--output-dir",
            out.path().to_str().unwrap(),
            "--jobs",
            "8",
        ]);
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("b_bad.csv"), "{stderr}");
        assert!(!stderr.contains("c_bad.csv"), "{stderr}");
    }
}

#[test]
fn jobs_flag_is_validated() {
    let output = run_dir(&["tests/fixtures/edge_batch_directory", "--jobs", "0"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--jobs must be a positive integer"));
    let output = Command::new(bin())
        .args([fixture("sample.csv").to_str().unwrap(), "-", "--jobs", "2"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--jobs only applies"));
}

#[test]
#[cfg(feature = "xlsx")]
fn xls_biff5_reads_sheets_codepage_text_and_both_kinds_of_date_format() {
    // Checked against xlrd. Format 165 is an explicit custom `m/d/y`;
    // XF 25 uses built-in format 14, which BIFF5 never writes out.
    let doc = run_json(
        "edge_xls_biff5_dates_and_codepage.xls",
        &["--samples", "20"],
    );
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(tables.keys().collect::<Vec<_>>(), ["Feuil1"]);
    let sheet = table(&doc, "Feuil1");
    let values = |i: usize| sheet[i]["sample_values"].as_array().unwrap().clone();
    assert!(values(1).contains(&serde_json::json!("Nümber")));
    let formatted = values(4);
    for v in [
        "1900-01-01",
        "1900-01-01T13:12:00",
        "1900-06-17",
        "1905-05-13",
        "1903-06-06T19:40:48",
    ] {
        assert!(
            formatted.contains(&serde_json::json!(v)),
            "{v} not in {formatted:?}"
        );
    }
}

#[test]
#[cfg(feature = "xlsx")]
fn xls_bare_biff3_and_biff4_worksheets_are_read() {
    let doc = run_json("poi_biff3.xls", &[]);
    let sheet = table(&doc, "Sheet1");
    assert_eq!(sheet.len(), 10);
    assert_eq!(sheet[0]["row_count"], 34);
    // BIFF4: format indices are positional, so XF 102 -> format 18 is
    // `m/d/yy`. Serial 12 is 1900-01-12.
    let doc = run_json("edge_xls_biff4_dates.xls", &["--samples", "1000"]);
    let sheet = table(&doc, "Sheet1");
    assert_eq!(sheet.len(), 12);
    assert!(
        sheet[1]["sample_values"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("1900-01-12"))
    );
}

#[test]
#[cfg(feature = "xlsx")]
fn xls_biff5_empty_sheets_are_skipped() {
    let doc = run_json("poi_biff5_excel95.xls", &[]);
    assert_eq!(doc["tables"].as_object().unwrap().len(), 1);
}

#[test]
#[cfg(feature = "xlsx")]
fn xls_bare_biff2_is_refused_clearly() {
    let dir = TempDir::new();
    let path = dir.path().join("old.xls");
    // BOF (BIFF2, worksheet) then EOF.
    std::fs::write(
        &path,
        [
            0x09, 0x00, 0x04, 0x00, 0x02, 0x00, 0x10, 0x00, 0x0A, 0x00, 0x00, 0x00,
        ],
    )
    .unwrap();
    let output = Command::new(bin())
        .args([path.to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("BIFF2"));
}

/// The page text of a one-page PDF fixture.
#[cfg(feature = "pdf")]
fn pdf_page_text(fixture_name: &str) -> String {
    let doc = run_json(fixture_name, &["--samples", "50"]);
    let stem = fixture_name.trim_end_matches(".pdf");
    column(table(&doc, stem), "text")["sample_values"][0]
        .as_str()
        .unwrap()
        .to_string()
}

/// The source text both RTL fixtures were printed from (LibreOffice from
/// a flat ODT, Chrome from HTML), one paragraph per line.
#[cfg(feature = "pdf")]
const RTL_SOURCE: [&str; 11] = [
    "שלום עולם זה מבחן",
    "مرحبا بالعالم هذا اختبار",
    "המחיר הוא 250 שקלים עבור Apple",
    "שלום!",
    "התאריך 12/05/2024 והסכום 1,234.50 ש״ח.",
    "السعر ١٢٣ ريال فقط",
    "הטלפון (03) 555-1234 זמין",
    "צרו קשר: info@example.co.il",
    "The word שלום means peace.",
    "Order 42 shipped to תל אביב today",
    "זוהי פסקה ארוכה מאוד שנועדה לבדוק גלישה של שורות בתוך אותה פסקה כאשר הטקסט ממשיך הלאה והלאה ללא הפסקה עד שהוא עובר לשורה הבאה בעמוד",
];

#[test]
#[cfg(feature = "pdf")]
fn pdf_right_to_left_text_reads_in_logical_order() {
    // LibreOffice draws a line's runs in reading order, which settles
    // each line's direction: every line comes out exactly as typed (the
    // last paragraph wraps, so it's two lines).
    let text = pdf_page_text("edge_pdf_rtl_libreoffice.pdf");
    assert_eq!(text.replace('\n', " "), RTL_SOURCE.join(" "));
    // Chrome draws in visual order and gives a lam-alef ligature's text as
    // ActualText in reading order. Every line matches but one: a
    // right-to-left paragraph ending in Latin text looks, drawn left to
    // right, exactly like a left-to-right paragraph ending in Hebrew, and
    // with no other signal it reads as the latter.
    let text = pdf_page_text("edge_pdf_rtl_chrome.pdf");
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(lines[..7], RTL_SOURCE[..7]);
    assert_eq!(lines[7], "info@example.co.il :צרו קשר");
    assert_eq!(lines[8..10], RTL_SOURCE[8..10]);
    assert_eq!(lines[10..].join(" "), RTL_SOURCE[10]);
}

#[test]
#[cfg(feature = "pdf")]
fn pdf_vertical_text_reads_down_its_columns() {
    // LibreOffice sets vertical Japanese one horizontal glyph per position;
    // a true Identity-V font advances down the page itself.
    assert_eq!(
        pdf_page_text("edge_pdf_vertical_libreoffice.pdf"),
        "日本語の縦書きテストです。\n二行目の文章。"
    );
    assert_eq!(
        pdf_page_text("edge_pdf_identity_v.pdf"),
        "縦書きの例です\n二列目"
    );
}

// Variable labels become `description`, value labels a note - values below
// are what pyreadstat wrote into each file (and read back from it).

#[test]
#[cfg(feature = "stata")]
fn stata_variable_and_value_labels_are_surfaced_for_every_layout() {
    // XML releases 117/118 and binary 113/114 keep their labels in
    // different places; all four must read the same.
    for release in ["118", "117", "114", "113"] {
        let doc = run_json(&format!("edge_stata_labels_{release}.dta"), &[]);
        let cols = table(&doc, &format!("edge_stata_labels_{release}"));
        let sex = column(cols, "sex");
        assert_eq!(sex["description"], "Respondent sex", "release {release}");
        assert!(
            sex["notes"]
                .as_str()
                .unwrap()
                .contains("value labels: 1 = male; 2 = female; 3 = other"),
            "release {release}: {}",
            sex["notes"]
        );
        assert_eq!(
            column(cols, "income")["description"],
            "Household income, last year (USD)"
        );
        // A variable with a label but no value labels gets only the label.
        let name = column(cols, "name");
        assert_eq!(name["description"], "Free-text name");
        assert!(!name["notes"].as_str().unwrap().contains("value labels"));
    }
}

#[test]
#[cfg(feature = "stata")]
fn stata_value_labels_are_capped_and_keep_non_ascii_text() {
    let doc = run_json("edge_stata_labels_edge_cases.dta", &[]);
    let cols = table(&doc, "edge_stata_labels_edge_cases");
    let code = column(cols, "code")["notes"].as_str().unwrap().to_string();
    assert!(code.contains("20 = occupation 20; ... 5 more"), "{code}");
    assert!(!code.contains("21 = occupation 21"), "{code}");
    let cafe = column(cols, "cafe")["notes"].as_str().unwrap().to_string();
    assert!(cafe.contains("1 = café au lait ☕"), "{cafe}");
    // A 100-character label is cut, with an ellipsis.
    assert!(cafe.contains("past t..."), "{cafe}");
    // No label at all leaves description empty.
    assert_eq!(column(cols, "unlabeled")["description"], "");
}

#[test]
#[cfg(feature = "sas7bdat")]
fn sas7bdat_column_labels_become_descriptions() {
    let doc = run_json("sas7bdat_pandas_airline.sas7bdat", &[]);
    let cols = table(&doc, "sas7bdat_pandas_airline");
    assert_eq!(column(cols, "Y")["description"], "level of output");
    assert_eq!(column(cols, "YEAR")["description"], "year");
    let doc = run_json("sas7bdat_pandas_cars.sas7bdat", &[]);
    let cols = table(&doc, "sas7bdat_pandas_cars");
    assert_eq!(column(cols, "MPG")["description"], "miles per gallon");
    assert_eq!(column(cols, "CYL")["description"], "number of cylinders");
    // A file with no labels leaves every description empty.
    let doc = run_json("sas7bdat_people_nonascii.sas7bdat", &[]);
    assert!(
        table(&doc, "sas7bdat_people_nonascii")
            .iter()
            .all(|c| c["description"] == "")
    );
}

#[test]
#[cfg(feature = "spss")]
fn spss_variable_and_value_labels_are_surfaced_in_plain_and_zlib_files() {
    for (file, tbl) in [
        ("edge_spss_labels.sav", "edge_spss_labels"),
        (
            "edge_spss_labels_compressed.zsav",
            "edge_spss_labels_compressed",
        ),
    ] {
        let doc = run_json(file, &[]);
        let cols = table(&doc, tbl);
        let sex = column(cols, "sex");
        assert_eq!(sex["description"], "Respondent sex", "{file}");
        assert!(
            sex["notes"]
                .as_str()
                .unwrap()
                .contains("value labels: 1 = male; 2 = female; 3 = other"),
            "{file}: {}",
            sex["notes"]
        );
        // A short string variable's value labels are text.
        let grp = column(cols, "grp");
        assert!(
            grp["notes"]
                .as_str()
                .unwrap()
                .contains("value labels: m = Male; f = Female"),
            "{file}: {}",
            grp["notes"]
        );
    }
}

// --- Text encodings and byte-order marks ---

fn sample_names(doc: &serde_json::Value, tbl: &str, col: &str) -> Vec<String> {
    column(table(doc, tbl), col)["sample_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn utf16_and_utf32_csvs_with_a_bom_read_with_no_flags() {
    for (file, tbl) in [
        ("edge_encoding_utf16le_bom.csv", "edge_encoding_utf16le_bom"),
        ("edge_encoding_utf16be_bom.csv", "edge_encoding_utf16be_bom"),
        ("edge_encoding_utf32le_bom.csv", "edge_encoding_utf32le_bom"),
    ] {
        let doc = run_json(file, &["--samples", "3"]);
        let cols = table(&doc, tbl);
        assert_eq!(
            cols.iter()
                .map(|c| c["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["id", "name", "city"],
            "{file}"
        );
        assert_eq!(
            sample_names(&doc, tbl, "name"),
            ["Zoë", "Søren", "Ann"],
            "{file}"
        );
        assert_eq!(
            sample_names(&doc, tbl, "city"),
            ["Köln", "Łódź", "Paris"],
            "{file}"
        );
    }
}

#[test]
fn utf16_surrogate_pairs_survive_transcoding() {
    let doc = run_json("edge_encoding_utf16le_emoji.csv", &[]);
    assert_eq!(
        sample_names(&doc, "edge_encoding_utf16le_emoji", "tag"),
        ["😀", "ok"]
    );
}

#[test]
fn a_utf16_file_with_no_bom_needs_an_explicit_encoding() {
    let out = Command::new(bin())
        .args([
            fixture("edge_encoding_utf16le_nobom.csv").to_str().unwrap(),
            "-",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("invalid UTF-8") && err.contains("--encoding"),
        "{err}"
    );

    let doc = run_json(
        "edge_encoding_utf16le_nobom.csv",
        &["--encoding", "utf-16le"],
    );
    assert_eq!(
        sample_names(&doc, "edge_encoding_utf16le_nobom", "city"),
        ["Köln", "Łódź", "Paris"]
    );
}

#[test]
fn single_byte_encodings_decode_through_the_code_page_tables() {
    let doc = run_json(
        "edge_encoding_windows1252.csv",
        &["--encoding", "windows-1252"],
    );
    assert_eq!(
        sample_names(&doc, "edge_encoding_windows1252", "city"),
        ["Köln", "Málaga", "Paris"]
    );
    // latin1 is the WHATWG alias for windows-1252.
    let doc = run_json("edge_encoding_windows1252.csv", &["--encoding", "latin1"]);
    assert_eq!(
        sample_names(&doc, "edge_encoding_windows1252", "name")[0],
        "Zoë"
    );
    let doc = run_json("edge_encoding_cp866.csv", &["--encoding", "cp866"]);
    assert_eq!(
        sample_names(&doc, "edge_encoding_cp866", "word"),
        ["Привет", "мир"]
    );
}

// Expected text from Python's own codecs (the files were written with them
// and round-trip there); the decoder itself is differentially tested
// against encoding_rs in `cjk_support`.
#[cfg(feature = "cjk")]
#[test]
fn east_asian_encodings_decode_with_the_encoding_flag() {
    let cases: [(&str, &str, &str, [&str; 3], [&str; 3]); 6] = [
        (
            "shift_jis",
            "shift_jis",
            "edge_encoding_shift_jis",
            ["山田太郎", "ﾖｼﾀﾞ", "佐藤花子"],
            ["東京", "大阪", "①京都"],
        ),
        (
            "euc_jp",
            "euc-jp",
            "edge_encoding_euc_jp",
            ["山田太郎", "ﾖｼﾀﾞ", "佐藤花子"],
            ["東京", "大阪", "京都"],
        ),
        (
            "euc_kr",
            "cp949",
            "edge_encoding_euc_kr",
            ["김철수", "이영희", "박민수"],
            ["서울", "부산", "똠방각하"],
        ),
        (
            "gbk",
            "gb2312",
            "edge_encoding_gbk",
            ["张伟", "王芳", "李娜"],
            ["北京", "上海", "广州"],
        ),
        (
            "gb18030",
            "gb18030",
            "edge_encoding_gb18030",
            ["张伟", "𠀀𠀁", "€"],
            ["北京", "上海", "😀"],
        ),
        (
            "big5",
            "big5",
            "edge_encoding_big5",
            ["陳大文", "林小明", "黃美玲"],
            ["台北", "高雄", "台中"],
        ),
    ];
    for (file, label, tbl, names, cities) in cases {
        let doc = run_json(&format!("edge_encoding_{file}.csv"), &["--encoding", label]);
        assert_eq!(sample_names(&doc, tbl, "name"), names, "{label}");
        assert_eq!(sample_names(&doc, tbl, "city"), cities, "{label}");
    }
    // Without the flag these aren't UTF-8, and the error says what to do.
    let out = Command::new(bin())
        .args([
            fixture("edge_encoding_shift_jis.csv").to_str().unwrap(),
            "-",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("shift_jis"));
}

#[cfg(all(feature = "cjk", feature = "dbase"))]
#[test]
fn dbase_decodes_the_east_asian_code_pages() {
    for (file, names) in [
        ("cp932", ["山田太郎", "ﾖｼﾀﾞ"]),
        ("cp936", ["张伟", "王芳"]),
        ("cp949", ["김철수", "이영희"]),
        ("cp950", ["陳大文", "林小明"]),
    ] {
        let doc = run_json(&format!("edge_dbase_{file}.dbf"), &[]);
        assert_eq!(
            sample_names(&doc, &format!("edge_dbase_{file}"), "NAME"),
            names,
            "{file}"
        );
    }
}

#[cfg(all(feature = "cjk", feature = "mbox"))]
#[test]
fn mbox_decodes_east_asian_charsets_in_headers_and_bodies() {
    let doc = run_json("edge_mbox_cjk_charsets.mbox", &["--samples", "5"]);
    let tbl = "edge_mbox_cjk_charsets";
    // gb2312, iso-2022-jp, euc-kr, big5, and a Shift_JIS message.
    assert_eq!(
        sample_names(&doc, tbl, "Subject"),
        [
            "你好，世界",
            "日本語の件名",
            "한국어 제목",
            "繁體中文標題",
            "東京の天気"
        ]
    );
    assert_eq!(
        sample_names(&doc, tbl, "body"),
        [
            "这是一封测试邮件。\n",
            "こんにちは、世界。\n",
            "안녕하세요 세계\n",
            "你好，世界。這是測試。\n",
            "今日は晴れです。ﾊﾛｰ\n"
        ]
    );
}

#[cfg(not(feature = "cjk"))]
#[test]
fn east_asian_encoding_without_the_cjk_feature_names_it() {
    let out = Command::new(bin())
        .args([
            fixture("edge_encoding_shift_jis.csv").to_str().unwrap(),
            "-",
            "--encoding",
            "shift_jis",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--features cjk"));
}

#[test]
fn encoding_flag_errors_are_specific() {
    let run = |args: &[&str]| {
        let out = Command::new(bin()).args(args).output().unwrap();
        assert!(!out.status.success());
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let csv = fixture("edge_encoding_windows1252.csv");
    let csv = csv.to_str().unwrap();
    assert!(run(&[csv, "-", "--encoding", "klingon"]).contains("unrecognized --encoding"));
    // utf-16 without a BOM can't pick a byte order.
    assert!(run(&[csv, "-", "--encoding", "utf-16"]).contains("needs a byte-order mark"));
    // A BOM that contradicts the flag is refused rather than guessed at.
    let bom = fixture("edge_encoding_utf16le_bom.csv");
    assert!(
        run(&[bom.to_str().unwrap(), "-", "--encoding", "utf-16be"])
            .contains("contradicts --encoding")
    );
}

// A binary format has no text encoding.
#[cfg(feature = "sqlite")]
#[test]
fn encoding_is_refused_for_a_binary_format() {
    let sqlite = fixture("sample.sqlite");
    let out = Command::new(bin())
        .args([sqlite.to_str().unwrap(), "-", "--encoding", "utf-8"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("only applies to text formats"), "{err}");
}

fn assert_bom_ignored(file: &str, tbl: &str, col: &str) {
    let doc = run_json(file, &[]);
    let names: Vec<&str> = table(&doc, tbl)
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&col), "{file}: {names:?}");
    assert!(
        names.iter().all(|n| !n.contains('\u{feff}')),
        "{file}: {names:?}"
    );
}

// Before, YAML put U+FEFF inside the first key and JSON/JSONL/XML/INI/
// vCard refused the file outright.
#[test]
fn a_utf8_bom_is_ignored_by_the_json_readers() {
    assert_bom_ignored("edge_bom_json.json", "edge_bom_json", "name");
    assert_bom_ignored("edge_bom_jsonl.jsonl", "edge_bom_jsonl", "name");
}

#[cfg(feature = "yaml")]
#[test]
fn a_utf8_bom_is_ignored_by_the_yaml_reader() {
    assert_bom_ignored("edge_bom_yaml.yaml", "edge_bom_yaml", "name");
}

#[cfg(feature = "xml")]
#[test]
fn a_utf8_bom_is_ignored_by_the_xml_reader() {
    assert_bom_ignored("edge_bom_xml.xml", "edge_bom_xml", "name");
}

#[cfg(feature = "ini")]
#[test]
fn a_utf8_bom_is_ignored_by_the_ini_reader() {
    assert_bom_ignored("edge_bom_ini.ini", "server", "host");
}

#[cfg(feature = "vcard")]
#[test]
fn a_utf8_bom_is_ignored_by_the_vcard_reader() {
    assert_bom_ignored("edge_bom_vcard.vcf", "edge_bom_vcard", "FN");
}

#[test]
fn a_bom_precedes_content_sniffing_for_an_extensionless_file() {
    let doc = run_json("edge_bom_utf16_json_noext", &[]);
    assert_eq!(doc["format"], "json");
    assert_eq!(
        sample_names(&doc, "edge_bom_utf16_json_noext", "name"),
        ["é"]
    );
}

#[test]
fn encoding_applies_through_stdin_and_inline_sql() {
    let bytes = std::fs::read(fixture("edge_encoding_windows1252.csv")).unwrap();
    let out = run_with_stdin(
        &bytes,
        &[
            "-",
            "-",
            "--format",
            "csv",
            "--encoding",
            "latin1",
            "--output-format",
            "sql",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sql = String::from_utf8(out.stdout).unwrap();
    // The inline second pass re-reads the transcoded copy, not the raw bytes.
    assert!(sql.contains("Köln") && sql.contains("Málaga"), "{sql}");
}

#[test]
fn encoding_applies_across_a_directory() {
    let dir = std::env::temp_dir().join(format!("sniff-enc-dir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fixture("edge_encoding_utf16le_bom.csv"), dir.join("a.csv")).unwrap();
    std::fs::copy(fixture("edge_bom_json.json"), dir.join("b.json")).unwrap();
    let out_dir = dir.join("out");
    let out = Command::new(bin())
        .args([
            dir.to_str().unwrap(),
            "--output-dir",
            out_dir.to_str().unwrap(),
            "--output-format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let a = std::fs::read_to_string(out_dir.join("a.csv.dictionary.json")).unwrap();
    assert!(a.contains("Zoë"), "{a}");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Labels in every output ---

#[cfg(feature = "stata")]
#[test]
fn labels_reach_json_schema_and_sql() {
    // json-schema carries the variable label and value labels in the
    // standard `description` keyword.
    let doc = run_with_format("edge_stata_labels_118.dta", "json-schema", &[]);
    let props = &doc["tables"]["edge_stata_labels_118"]["properties"];
    assert_eq!(
        props["sex"]["description"],
        "Respondent sex (value labels: 1 = male; 2 = female; 3 = other)"
    );
    assert_eq!(
        props["income"]["description"],
        "Household income, last year (USD)"
    );
    // A column the file never described has no description at all.
    let plain = run_with_format("sample.csv", "json-schema", &[]);
    assert!(
        plain["tables"]["sample"]["properties"]
            .as_object()
            .unwrap()
            .iter()
            .all(|(_, p)| p.get("description").is_none())
    );

    // SQL keeps them as `--` comments after each column's comma, which
    // every engine accepts; the last column has no comma at all.
    let sql = run_sql("edge_stata_labels_118.dta", &[]);
    assert!(
        sql.contains("\"sex\" BIGINT NOT NULL, -- Respondent sex (value labels: 1 = male; 2 = female; 3 = other)"),
        "{sql}"
    );
    assert!(
        sql.contains("\"name\" TEXT NOT NULL -- Free-text name\n);"),
        "{sql}"
    );
    let staging = run_sql("edge_stata_labels_118.dta", &["--sql-mode", "staging"]);
    assert!(
        staging.contains("-- Household income, last year (USD)"),
        "{staging}"
    );
}

#[cfg(feature = "stata")]
#[test]
fn diff_reports_a_changed_label_and_recoded_value_labels() {
    let dir = std::env::temp_dir().join(format!("sniff-label-diff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let old = dir.join("old.json");
    let out = Command::new(bin())
        .args([
            fixture("edge_stata_labels_118.dta").to_str().unwrap(),
            old.to_str().unwrap(),
            "--output-format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    // Recode a value label and reword a variable label, keep the data.
    let text = std::fs::read_to_string(&old).unwrap();
    assert!(text.contains("2 = female") && text.contains("Household income, last year (USD)"));
    let new = dir.join("new.json");
    std::fs::write(
        &new,
        text.replace("2 = female", "2 = nonbinary").replace(
            "Household income, last year (USD)",
            "Household income (USD)",
        ),
    )
    .unwrap();

    let out = Command::new(bin())
        .args([
            "diff",
            old.to_str().unwrap(),
            new.to_str().unwrap(),
            "-",
            "--output-format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let changes = report["changes"].as_array().unwrap();
    let labelled: Vec<&serde_json::Value> = changes
        .iter()
        .filter(|c| c["kind"] == "labels changed")
        .collect();
    assert_eq!(labelled.len(), 2, "{report}");
    let sex = labelled.iter().find(|c| c["column"] == "sex").unwrap();
    assert_eq!(sex["compatibility"], "safe");
    assert!(
        sex["new_labels"]
            .as_str()
            .unwrap()
            .contains("2 = nonbinary")
    );
    assert!(sex["old_labels"].as_str().unwrap().contains("2 = female"));
    // Not breaking: a label never changes what's stored.
    assert_eq!(report["has_breaking_changes"], false);

    // Identical dictionaries still report nothing.
    let out = Command::new(bin())
        .args([
            "diff",
            old.to_str().unwrap(),
            old.to_str().unwrap(),
            "-",
            "--output-format",
            "json",
        ])
        .output()
        .unwrap();
    let same: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(same["changes"].as_array().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Lakehouse tables nested in a directory ---

/// A warehouse folder holding a Delta table, an Iceberg table and a CSV.
#[cfg(all(feature = "delta", feature = "iceberg"))]
fn nested_lakehouse_tree(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("sniff-lake-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let wh = root.join("warehouse");
    copy_dir_recursive(&fixture("edge_delta_table"), &wh.join("sales_delta"));
    copy_dir_recursive(&fixture("edge_iceberg_table"), &wh.join("events_iceberg"));
    std::fs::copy(fixture("sample.csv"), wh.join("sample.csv")).unwrap();
    root
}

#[cfg(all(feature = "delta", feature = "iceberg"))]
#[test]
fn nested_delta_and_iceberg_tables_are_one_table_each_in_directory_mode() {
    let root = nested_lakehouse_tree("dir");
    let out_dir = root.join("out");
    let out = Command::new(bin())
        .args([
            root.join("warehouse").to_str().unwrap(),
            "--output-dir",
            out_dir.to_str().unwrap(),
            "--output-format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // One output per table - not one per Parquet data file and log commit.
    let mut names: Vec<String> = std::fs::read_dir(&out_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "_index.dictionary.json",
            "events_iceberg.dictionary.json",
            "sales_delta.dictionary.json",
            "sample.csv.dictionary.json",
        ]
    );
    let delta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out_dir.join("sales_delta.dictionary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(delta["format"], "delta");
    let id = column(table(&delta, "sales_delta"), "id");
    assert_eq!(id["row_count"], 5);
    // The partition column only exists in the transaction log.
    assert!(
        table(&delta, "sales_delta")
            .iter()
            .any(|c| c["name"] == "category")
    );
    let iceberg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out_dir.join("events_iceberg.dictionary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(iceberg["format"], "iceberg");
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(all(feature = "delta", feature = "iceberg"))]
#[test]
fn nested_lakehouse_tables_combine_and_load_as_one_table_each() {
    let root = nested_lakehouse_tree("combine");
    let wh = root.join("warehouse");
    let out = Command::new(bin())
        .args([
            wh.to_str().unwrap(),
            "--combine",
            "--output-format",
            "json",
            "-",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&String> = doc["tables"].as_object().unwrap().keys().collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(
        names
            .iter()
            .any(|n| n.ends_with("sales_delta__sales_delta")),
        "{names:?}"
    );
    assert!(
        names
            .iter()
            .any(|n| n.ends_with("events_iceberg__events_iceberg")),
        "{names:?}"
    );

    // Inline SQL replays each table's live rows, not loose files.
    let out = Command::new(bin())
        .args([
            wh.to_str().unwrap(),
            "--combine",
            "--output-format",
            "sql",
            "-",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sql = String::from_utf8(out.stdout).unwrap();
    assert!(
        sql.contains("CREATE TABLE \"sales_delta__sales_delta\""),
        "{sql}"
    );
    assert_eq!(
        sql.matches("INSERT INTO \"sales_delta__sales_delta\"")
            .count(),
        1,
        "{sql}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(all(feature = "delta", feature = "iceberg"))]
#[test]
fn nested_lakehouse_tables_are_single_nodes_in_the_graph() {
    let root = nested_lakehouse_tree("graph");
    let out_dir = root.join("graph_out");
    let out = Command::new(bin())
        .args([
            "graph",
            root.join("warehouse").to_str().unwrap(),
            out_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let graph: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out_dir.join("graph.json")).unwrap())
            .unwrap();
    let files: Vec<&serde_json::Value> = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["type"] == "file")
        .collect();
    let labels: Vec<&str> = files.iter().map(|n| n["label"].as_str().unwrap()).collect();
    // Exactly three file nodes - the two tables and the CSV - and none of
    // the Parquet data files or commit JSON inside them.
    assert_eq!(files.len(), 3, "{labels:?}");
    assert!(
        labels.contains(&"sales_delta") && labels.contains(&"events_iceberg"),
        "{labels:?}"
    );
    let file_type =
        |label: &str| files.iter().find(|n| n["label"] == label).unwrap()["file_type"].clone();
    assert_eq!(file_type("sales_delta"), "delta");
    assert_eq!(file_type("events_iceberg"), "iceberg");
    let _ = std::fs::remove_dir_all(&root);
}

// A build without the feature still recognizes the table and says what to
// rebuild with, instead of profiling its log and data files as loose files.
#[cfg(not(feature = "delta"))]
#[test]
fn nested_delta_table_without_the_feature_fails_with_a_rebuild_hint() {
    let root = std::env::temp_dir().join(format!("sniff-lake-nofeat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_dir_recursive(&fixture("edge_delta_table"), &root.join("wh").join("t"));
    let out = Command::new(bin())
        .args([
            root.join("wh").to_str().unwrap(),
            "--output-dir",
            root.join("o").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--features delta"));
    let _ = std::fs::remove_dir_all(&root);
}

// --- Compression and archive wrappers ---

fn container_table_columns(fixture_name: &str) -> (String, Vec<String>, u64) {
    let doc = run_json(fixture_name, &[]);
    let tables = doc["tables"].as_object().unwrap();
    assert_eq!(tables.len(), 1, "{fixture_name}: {tables:?}");
    let (name, cols) = tables.iter().next().unwrap();
    let cols = cols.as_array().unwrap();
    (
        name.clone(),
        cols.iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect(),
        cols[0]["row_count"].as_u64().unwrap(),
    )
}

#[test]
fn a_single_file_archive_is_read_as_the_file_inside_it() {
    // The table is named for the archive member, not the archive.
    for (file, member) in [
        ("edge_container_single.zip", "data"),
        ("edge_container_zip_macos_junk.zip", "people"),
        ("edge_container_single.tar", "data"),
        ("edge_container_single.tar.gz", "people"),
        ("edge_container_single.tgz", "people"),
        ("edge_container_long_name.tar", "deep"),
        ("edge_container_gnu_long.tar", "deep"),
        // gzip around a zip: peeled layer by layer.
        ("edge_container_zip_in_gzip.zip.gz", "data"),
        // No extension at all: gzip, then tar, both recognized by magic.
        ("edge_container_tar_gz_noext", "people"),
        ("edge_container_tar_xz_noext", "people"),
        // bzip2 and xz, alone and around a tar archive.
        ("edge_container_single.csv.bz2", "edge_container_single"),
        ("edge_container_single.csv.xz", "edge_container_single"),
        ("edge_container_single.tar.bz2", "people"),
        ("edge_container_single.tbz2", "people"),
        ("edge_container_single.tar.xz", "people"),
        ("edge_container_single.txz", "people"),
    ] {
        let (name, columns, rows) = container_table_columns(file);
        assert_eq!(name, member, "{file}");
        assert_eq!(columns[..2], ["id", "name"], "{file}");
        assert_eq!(rows, 3, "{file}");
    }
}

#[test]
fn an_archive_of_several_files_is_profiled_as_one_combined_dictionary() {
    for file in [
        "edge_container_two_files.zip",
        "edge_container_two_files.tar",
    ] {
        let out = Command::new(bin())
            .args([
                fixture(file).to_str().unwrap(),
                "-",
                "--output-format",
                "json",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{file}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        // Named for the archive, tables qualified like --combine's.
        assert_eq!(doc["directory"], "edge_container_two_files", "{file}");
        let tables = doc["tables"].as_object().unwrap();
        let mut names: Vec<&str> = tables.keys().map(String::as_str).collect();
        names.sort();
        assert_eq!(names, ["a__a", "b__b"], "{file}");
    }
}

#[test]
fn a_piped_multi_file_archive_is_named_stdin() {
    use std::io::Write;
    let mut child = Command::new(bin())
        .args(["-", "-", "--output-format", "json"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&std::fs::read(fixture("edge_container_two_files.tar")).unwrap())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["directory"], "stdin");
    assert!(doc["tables"]["a__a"].is_array());
}

#[test]
fn a_multi_file_archive_writes_its_dictionary_next_to_the_archive() {
    let dir = TempDir::new();
    let archive = dir.path().join("bundle.zip");
    std::fs::copy(fixture("edge_container_two_files.zip"), &archive).unwrap();
    let out = Command::new(bin())
        .args([archive.to_str().unwrap(), "--output-format", "json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let written = std::fs::read_to_string(dir.path().join("bundle.dictionary.json")).unwrap();
    assert!(written.contains("\"a__a\"") && written.contains("\"b__b\""));
    // A dictionary left beside the archive is never mistaken for input.
    let again = Command::new(bin())
        .args([archive.to_str().unwrap(), "--output-format", "json"])
        .output()
        .unwrap();
    assert!(again.status.success());
}

#[test]
fn archive_members_that_would_escape_the_extraction_directory_are_skipped() {
    let dir = TempDir::new();
    let nested = dir.path().join("inner");
    std::fs::create_dir_all(&nested).unwrap();
    let archive = nested.join("slip.zip");
    std::fs::copy(fixture("edge_container_zip_slip.zip"), &archive).unwrap();
    let out = Command::new(bin())
        .args([archive.to_str().unwrap(), "-", "--output-format", "json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut names: Vec<&str> = doc["tables"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    names.sort();
    // Only the two honest members (and not the macOS resource fork).
    assert_eq!(names, ["a__a", "sub_b__b"]);
    // Nothing was written outside the scratch directory.
    assert!(!dir.path().join("evil.csv").exists());
    assert!(!nested.join("evil.csv").exists());
}

#[test]
fn an_empty_archive_is_still_refused() {
    let dir = TempDir::new();
    let archive = dir.path().join("empty.zip");
    // An end-of-central-directory record with zero entries.
    let mut eocd = vec![0x50, 0x4B, 0x05, 0x06];
    eocd.extend_from_slice(&[0u8; 18]);
    std::fs::write(&archive, eocd).unwrap();
    let out = Command::new(bin())
        .args([archive.to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("empty zip archive"));
}

#[test]
fn a_directory_walk_reads_archives_and_skips_multi_file_ones() {
    let dir = std::env::temp_dir().join(format!("sniff-archives-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in [
        "edge_container_single.zip",
        "edge_container_single.tar.gz",
        "edge_container_two_files.zip",
        "edge_container_two_files.tar",
    ] {
        std::fs::copy(fixture(f), dir.join(f)).unwrap();
    }
    let out_dir = dir.join("out");
    let out = Command::new(bin())
        .args([
            dir.to_str().unwrap(),
            "--output-dir",
            out_dir.to_str().unwrap(),
            "--output-format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut names: Vec<String> = std::fs::read_dir(&out_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    // The two readable archives each get an output, named for their
    // member; the multi-file ones are skipped like any unrecognized file.
    assert_eq!(
        names,
        [
            "_index.dictionary.json",
            "data.csv.dictionary.json",
            "people.csv.dictionary.json"
        ]
    );
    let index = std::fs::read_to_string(out_dir.join("_index.dictionary.json")).unwrap();
    assert!(index.contains("edge_container_two_files.zip"), "{index}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_piped_archive_is_unwrapped_by_its_magic_bytes() {
    let bytes = std::fs::read(fixture("edge_container_single.tar.gz")).unwrap();
    let out = run_with_stdin(
        &bytes,
        &["-", "-", "--format", "csv", "--output-format", "json"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(table(&doc, "stdin")[0]["row_count"], 3);
}

#[test]
fn a_corrupt_tar_is_an_error_not_a_guess() {
    let mut bytes = std::fs::read(fixture("edge_container_single.tar")).unwrap();
    bytes[10] ^= 0xFF; // a byte inside the first header, which breaks its checksum
    let dir = std::env::temp_dir().join(format!("sniff-badtar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bad.tar");
    std::fs::write(&path, bytes).unwrap();
    let out = Command::new(bin())
        .args([path.to_str().unwrap(), "-"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("isn't a valid tar archive"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "parquet")]
#[test]
fn brotli_and_lz4_files_are_unwrapped() {
    for (file, rows) in [
        ("edge_container_single.csv.br", 3),
        ("edge_container_brotli_large.csv.br", 6000),
        ("edge_container_single.csv.lz4", 3),
        // 6,000 rows across two linked 64 KiB blocks: the second block's
        // matches reach back into the first.
        ("edge_container_lz4_multi_block.csv.lz4", 6000),
        // Two concatenated frames, which `cat a.lz4 b.lz4` produces.
        ("edge_container_lz4_two_frames.csv.lz4", 6),
    ] {
        let (name, columns, got) = container_table_columns(file);
        assert_eq!(name, file.split('.').next().unwrap(), "{file}");
        assert_eq!(columns[..2], ["id", "name"], "{file}");
        assert_eq!(got, rows, "{file}");
    }
}

#[cfg(not(feature = "parquet"))]
#[test]
fn brotli_without_its_feature_says_what_to_rebuild_with() {
    let out = Command::new(bin())
        .args([
            fixture("edge_container_single.csv.br").to_str().unwrap(),
            "-",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--features parquet"));
}

#[test]
fn list_formats_names_the_wrappers() {
    let out = Command::new(bin())
        .args(["--list-formats", "--output-format", "json"])
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let containers = doc["containers"].as_array().unwrap();
    let names: Vec<&str> = containers
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["gzip", "zstd", "zip", "tar", "brotli", "lz4", "bzip2", "xz"]
    );
    let zip = containers.iter().find(|c| c["name"] == "zip").unwrap();
    assert_eq!(zip["compiled_in"], true);
    assert!(
        zip["extensions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "zip")
    );
}

// --- Arrow IPC streams (.arrows) ---

#[cfg(feature = "parquet")]
#[test]
fn an_arrow_ipc_stream_file_profiles_like_a_feather_file() {
    let doc = run_json("edge_arrow_stream.arrows", &[]);
    let cols = table(&doc, "edge_arrow_stream");
    assert_eq!(column(cols, "id")["ideal_type"], "i64");
    assert_eq!(column(cols, "id")["row_count"], 3);
    // A dictionary-encoded column resolves to its values.
    assert_eq!(
        column(cols, "tag")["sample_values"],
        serde_json::json!(["a", "b"])
    );
    // Dictionary batches that grow between record batches accumulate.
    let doc = run_json("edge_arrow_stream_delta_dict.arrows", &[]);
    let cols = table(&doc, "edge_arrow_stream_delta_dict");
    assert_eq!(column(cols, "tag")["row_count"], 5);
    assert_eq!(
        column(cols, "tag")["sample_values"],
        serde_json::json!(["a", "b", "c"])
    );
    // --nrows stops after the batch that reaches the limit.
    let doc = run_json("edge_arrow_stream_delta_dict.arrows", &["--nrows", "2"]);
    assert_eq!(
        column(table(&doc, "edge_arrow_stream_delta_dict"), "tag")["row_count"],
        2
    );
}

#[cfg(feature = "parquet")]
#[test]
fn an_extensionless_arrow_stream_is_recognized_by_content_and_loads_as_sql() {
    let dir = std::env::temp_dir().join(format!("sniff-arrows-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stream_no_ext");
    std::fs::copy(fixture("edge_arrow_stream.arrows"), &path).unwrap();
    let out = Command::new(bin())
        .args([path.to_str().unwrap(), "-", "--output-format", "sql"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sql = String::from_utf8(out.stdout).unwrap();
    assert!(sql.contains("INSERT INTO \"stream_no_ext\""), "{sql}");
    assert!(
        sql.contains("(10, 'a')") && sql.contains("(30, 'a')"),
        "{sql}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The value tuples of every INSERT statement for `table` in an inline
/// script (a big table is written as several batched statements), one
/// trimmed string per row (`(1, 'a')`).
fn insert_rows(sql: &str, table: &str) -> Vec<String> {
    let header = format!("INSERT INTO \"{table}\" (");
    let mut rows = Vec::new();
    let mut from = 0;
    while let Some(at) = sql[from..].find(&header) {
        let rest = &sql[from + at..];
        let values = rest.find("VALUES\n").expect("an INSERT has a VALUES list") + "VALUES\n".len();
        let end = rest[values..].find("\n;").expect("an INSERT ends with ;");
        rows.extend(
            rest[values..values + end]
                .lines()
                .map(|l| l.trim().trim_end_matches(',').to_string()),
        );
        from += at + values + end;
    }
    rows
}

/// `child` is declared as a child table of `parent`: it carries the
/// `_parent_row_id`/`_index` keys and a foreign key to `parent`'s
/// `_row_id`, which is `parent`'s primary key.
fn assert_child_table(sql: &str, parent: &str, child: &str) {
    assert!(
        sql.contains(&format!(
            "CREATE TABLE \"{parent}\" (\n    \"_row_id\" BIGINT NOT NULL,"
        )),
        "{sql}"
    );
    assert!(sql.contains("PRIMARY KEY (\"_row_id\")"), "{sql}");
    let create = sql
        .find(&format!("CREATE TABLE \"{child}\" ("))
        .unwrap_or_else(|| panic!("no child table {child}: {sql}"));
    let block = &sql[create..create + sql[create..].find(");").unwrap()];
    assert!(
        block.contains("\"_parent_row_id\" BIGINT NOT NULL"),
        "{block}"
    );
    assert!(block.contains("\"_index\" BIGINT NOT NULL"), "{block}");
    assert!(
        block.contains(&format!(
            "FOREIGN KEY (\"_parent_row_id\") REFERENCES \"{parent}\" (\"_row_id\")"
        )),
        "{block}"
    );
}

// Several blocks/streams: a bzip2 file holds 100 kB blocks and may be
// several streams back to back; an xz file holds blocks and streams with
// padding between them. Each yields every row, in order.
#[test]
fn bzip2_and_xz_files_with_several_blocks_and_streams_are_read_whole() {
    for (file, rows) in [
        ("edge_container_bzip2_multi_block.csv.bz2", 9000),
        ("edge_container_xz_multi_block.csv.xz", 9000),
        ("edge_container_bzip2_two_streams.csv.bz2", 4),
        ("edge_container_xz_two_streams.csv.xz", 4),
    ] {
        let (name, columns, got) = container_table_columns(file);
        assert_eq!(name, file.split('.').next().unwrap(), "{file}");
        assert_eq!(columns[..2], ["id", "name"], "{file}");
        assert_eq!(got, rows, "{file}");
    }
}

#[test]
fn a_corrupt_bzip2_or_xz_file_is_an_error_not_garbage() {
    let dir = std::env::temp_dir().join(format!("sniff-badcomp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (file, tail) in [
        ("edge_container_single.csv.bz2", "bz2"),
        ("edge_container_single.csv.xz", "xz"),
    ] {
        let mut bytes = std::fs::read(fixture(file)).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        let path = dir.join(format!("bad.csv.{tail}"));
        std::fs::write(&path, &bytes).unwrap();
        let out = Command::new(bin())
            .args([path.to_str().unwrap(), "-"])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{file}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("corrupt") || err.contains("mismatch"),
            "{file}: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// A WAL-mode database that was copied while a connection was open keeps its
// newest data only in the `-wal` file. The main file here holds 20 `people`
// rows and nothing else; the log adds 12 more, an update to row 1, a whole
// new table, and an unfinished transaction (150 rows that spilled into the
// log without a commit frame) that must not show up.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_reads_committed_rows_from_a_write_ahead_log() {
    let doc = run_json("edge_sqlite_wal_pending.db", &[]);
    assert_eq!(column(table(&doc, "people"), "id")["row_count"], 32);
    let extra = table(&doc, "extra");
    assert_eq!(column(extra, "k")["row_count"], 5);
    assert_eq!(column(extra, "v")["ideal_type"], "f64");
    let sql = run_sql("edge_sqlite_wal_pending.db", &[]);
    let people = insert_rows(&sql, "people");
    assert_eq!(people.len(), 32);
    assert!(
        people.iter().any(|r| r.contains("'CHANGED'")),
        "the update in the log is applied"
    );
    assert!(
        !people.iter().any(|r| r.contains("uncommitted")),
        "an unfinished transaction is ignored"
    );
    assert!(
        !people.iter().any(|r| r.contains("'p0'")),
        "row 1 was renamed in the log"
    );
}

// An empty main file plus a log is a complete database (a new database that
// was never checkpointed), including its header.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_reads_a_database_that_exists_only_in_its_log() {
    let doc = run_json("edge_sqlite_wal_logonly.db", &[]);
    assert_eq!(column(table(&doc, "people"), "id")["row_count"], 12);
    assert_eq!(column(table(&doc, "extra"), "k")["row_count"], 5);
    assert!(
        insert_rows(&run_sql("edge_sqlite_wal_logonly.db", &[]), "people")
            .iter()
            .any(|r| r.contains("'CHANGED'"))
    );
}

// A log whose frames are cut short or whose checksums stop matching ends the
// valid log there - what came before still counts, what comes after doesn't.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_ignores_a_truncated_or_corrupt_write_ahead_log_tail() {
    let dir = TempDir::new();
    let db = dir.path().join("app.db");
    std::fs::copy(fixture("edge_sqlite_wal_pending.db"), &db).unwrap();
    let wal = std::fs::read(fixture("edge_sqlite_wal_pending.db-wal")).unwrap();
    // Cut mid-frame near the end: the last complete commit survives.
    std::fs::write(dir.path().join("app.db-wal"), &wal[..wal.len() - 5000]).unwrap();
    let doc = run_json_at(&db);
    assert_eq!(column(table(&doc, "people"), "id")["row_count"], 32);
    // Flip a byte in the middle of the first frame's page: that frame's
    // checksum fails, so no log frame is valid and only the main file shows.
    let mut bad = wal.clone();
    bad[32 + 24 + 100] ^= 0xff;
    std::fs::write(dir.path().join("app.db-wal"), &bad).unwrap();
    let doc = run_json_at(&db);
    assert_eq!(column(table(&doc, "people"), "id")["row_count"], 20);
}

// LZWDecode, with both /EarlyChange settings. Each fixture's content stream
// is ~60 KB of text LZW-compressed to ~36 KB by an independent encoder (and
// checked to decode identically in qpdf), long enough to cross every code
// width (9 to 12 bits) and to hit the table-full clear code several times.
#[cfg(feature = "pdf")]
#[test]
fn pdf_lzw_streams_decode_with_either_early_change_setting() {
    for name in ["edge_pdf_lzw.pdf", "edge_pdf_lzw_early0.pdf"] {
        let sql = run_sql(name, &[]);
        for line in [
            "gfeyczzug euana cfomrienr upzdk ugmxmga sjr yindoy sbqgbcn oikuhp",
            "aburlt seh tfl shekou zdqppye fbilb dnpwo he mkthm",
            "qkpqsg mb xqr nbwb imlselk luo emubcrd kwjes rrgxcbxn",
        ] {
            assert!(sql.contains(line), "{name} lost the line {line:?}");
        }
        assert!(
            !sql.contains('\u{FFFD}'),
            "{name} decoded to replacement characters"
        );
    }
}

// An Avro `duration` (months, days, milliseconds in 12 bytes) reads as an
// ISO 8601 duration instead of a debug-formatted struct.
#[cfg(feature = "avro")]
#[test]
fn avro_duration_logical_type_renders_as_an_iso_8601_duration() {
    let sql = run_sql("edge_avro_duration.avro", &[]);
    let rows = insert_rows(&sql, "edge_avro_duration");
    assert_eq!(
        rows,
        [
            "(1, 'P1M2DT3.5S')",
            "(2, 'P0D')",
            "(3, 'P14M')",
            "(4, 'PT0.25S')",
            "(5, 'P30DT86400S')"
        ]
    );
}
