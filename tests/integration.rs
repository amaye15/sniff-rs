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

// Found via a real-world sweep against the official apache/parquet-testing
// corpus: a Map column with non-UTF8 keys (Map<Int32, T> is legal Parquet/
// Arrow, e.g. a numeric-code-to-description lookup) used to fail Arrow's
// own JSON writer for the *whole batch*, taking every other column in the
// file down with it - a single unsupported column shouldn't cost the rest
// of a file that's otherwise perfectly readable.
#[cfg(feature = "parquet")]
#[test]
fn parquet_map_with_non_string_keys_does_not_sink_the_rest_of_the_file() {
    let doc = run_json("edge_map_non_string_key.parquet", &[]);
    let cols = table(&doc, "edge_map_non_string_key");

    let map_col = column(cols, "code_lookup");
    assert_eq!(map_col["current_type"], "Map");
    assert!(
        map_col["notes"]
            .as_str()
            .unwrap()
            .contains("could not be converted")
    );

    // The plain scalar column alongside it must still be profiled
    // normally - this is the whole point of the isolation.
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
