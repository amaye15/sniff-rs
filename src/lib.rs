use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{NaiveDate, NaiveDateTime};
use clap::Parser;
use serde_json::Value as JsonValue;
use serde_json::json;

/// Generate a data dictionary from a CSV, TSV, JSON, JSON Lines, Parquet,
/// Arrow IPC/Feather, Avro, Excel, SQLite, MessagePack, TOML, YAML, CBOR, or
/// INI file: one row per column, with a current type, a heuristic "ideal" type
/// suggestion, missing %, sample values, and a blank Description field to
/// fill in by hand. Output is Markdown tables (default), this tool's own
/// rich JSON (--output-format json), or json-schema.org-vocabulary JSON
/// (--output-format json-schema); any of the three can be written to
/// stdout by passing "-" as the output path. SQLite files (one table per
/// database table), Excel workbooks (one table per sheet), and INI files
/// (one table per section) can produce multiple tables; every other format
/// always produces exactly one implicit table - all of it renders through
/// the same path. Format is inferred from the file extension unless
/// --format is given. Every
/// optional format needs its own --features flag (see the Supported
/// formats table in CLAUDE.md), or use --features full for everything.
#[derive(Parser)]
#[command(name = "sniff-rs")]
struct Args {
    /// Path to the input file (.csv, .tsv, .json, .jsonl/.ndjson, .parquet,
    /// .arrow/.feather, .avro, .xlsx/.xls/.xlsb/.ods, .db/.sqlite/.sqlite3,
    /// .msgpack/.mp, .toml, .yaml/.yml, .cbor, .ini)
    input_path: PathBuf,
    /// Output path (default: <input>.dictionary.md or .json). Pass "-" to write to stdout.
    output_path: Option<PathBuf>,
    /// Number of sample values to show per column
    #[arg(long, default_value_t = 3)]
    samples: usize,
    /// Only read the first N rows/records (for large files)
    #[arg(long)]
    nrows: Option<usize>,
    /// Override format detection: csv, tsv, json (covers json/jsonl/ndjson), parquet, arrow, avro, xlsx, sqlite, msgpack, toml, yaml, cbor, or ini
    #[arg(long)]
    format: Option<String>,
    /// Override the field delimiter for csv/tsv (single character)
    #[arg(long)]
    delimiter: Option<char>,
    /// Output format: md (markdown tables), json (this tool's own rich shape), or
    /// json-schema (json-schema.org vocabulary, for schema-consuming tools)
    #[arg(long, default_value = "md")]
    output_format: String,
}

const DATE_FORMATS: &[&str] = &[
    "%Y-%m-%d",
    "%Y/%m/%d",
    "%m/%d/%Y",
    "%d/%m/%Y",
    "%d-%m-%Y",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%d %H:%M:%S",
    "%m-%d-%Y",
    "%d %b %Y",
    "%b %d, %Y",
];

// --- Shared intermediate representation, produced by each format's reader ---

struct ColumnInput {
    name: String,
    current_type: String,
    raw_values: Vec<String>, // non-null/non-missing values only
    total: usize,            // total rows/records, for missing % calc
    skip_heuristics: bool,   // true for nested JSON (array/object) columns
}

#[derive(serde::Serialize)]
struct ColumnProfile {
    name: String,
    current_type: String,
    ideal_type: String,
    description: String, // always empty - intentionally left for manual fill-in
    missing_pct: f64,
    sample_values: Vec<String>,
    notes: String,
}

// --- Shared heuristic engine (format-agnostic, works on stringified values) ---

fn has_leading_zero(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0] == b'0' && b[1].is_ascii_digit()
}

fn is_bool_word(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "true" | "false" | "yes" | "no" | "y" | "n"
    )
}

fn matching_date_format(values: &[&str]) -> Option<&'static str> {
    DATE_FORMATS.iter().copied().find(|fmt| {
        values.iter().all(|v| {
            NaiveDate::parse_from_str(v, fmt).is_ok()
                || NaiveDateTime::parse_from_str(v, fmt).is_ok()
        })
    })
}

fn suggest_ideal_type(values: &[&str], current: &str) -> (String, String) {
    if values.iter().any(|v| has_leading_zero(v)) {
        let mut note = "leading zeros in raw values (likely an ID/code)".to_string();
        if current == "i64" || current == "f64" {
            note.push_str(" - a naive numeric parse already lost them");
        }
        return ("String".to_string(), note);
    }

    if values.iter().all(|v| is_bool_word(v)) {
        return (
            "bool".to_string(),
            "values are yes/no/true/false".to_string(),
        );
    }

    if let Some(fmt) = matching_date_format(values) {
        return (
            "NaiveDate / DateTime".to_string(),
            format!("all values match date format \"{fmt}\""),
        );
    }

    let cleaned: Vec<String> = values.iter().map(|v| v.replace([',', '$'], "")).collect();
    let cleaned_refs: Vec<&str> = cleaned.iter().map(|s| s.as_str()).collect();

    if cleaned_refs.iter().all(|v| v.parse::<i64>().is_ok()) {
        let note = if current == "i64" {
            String::new()
        } else {
            "numeric strings".to_string()
        };
        return ("i64".to_string(), note);
    }
    if cleaned_refs.iter().all(|v| v.parse::<f64>().is_ok()) {
        let note = if current == "f64" {
            String::new()
        } else {
            "numeric strings".to_string()
        };
        return ("f64".to_string(), note);
    }

    let unique: HashSet<&str> = values.iter().copied().collect();
    let ratio = unique.len() as f64 / values.len() as f64;
    if unique.len() <= 50 && ratio < 0.05 {
        return (
            "enum / category".to_string(),
            format!("low cardinality ({} unique values)", unique.len()),
        );
    }

    ("String".to_string(), String::new())
}

fn escape_md(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Round to 1 decimal place so JSON output shows 66.7, not 66.66666666666667.
fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

// --- CSV / TSV reader ---

fn naive_current_type(values: &[&str]) -> &'static str {
    if values
        .iter()
        .all(|v| v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("false"))
    {
        "bool"
    } else if values.iter().all(|v| v.parse::<i64>().is_ok()) {
        "i64"
    } else if values.iter().all(|v| v.parse::<f64>().is_ok()) {
        "f64"
    } else {
        "String"
    }
}

fn columns_from_csv(path: &Path, nrows: Option<usize>, delimiter: u8) -> Result<Vec<ColumnInput>> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(path)
        .with_context(|| format!("failed to open {path:?}"))?;
    let headers: Vec<String> = reader.headers()?.iter().map(|s| s.to_string()).collect();

    let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); headers.len()];
    for (i, result) in reader.records().enumerate() {
        if nrows.is_some_and(|limit| i >= limit) {
            break;
        }
        let record = result?;
        for (col_idx, field) in record.iter().enumerate() {
            let value = if field.trim().is_empty() {
                None
            } else {
                Some(field.to_string())
            };
            raw[col_idx].push(value);
        }
    }

    let mut columns = Vec::new();
    for (col_idx, name) in headers.into_iter().enumerate() {
        let total = raw[col_idx].len();
        let non_null: Vec<String> = raw[col_idx].iter().filter_map(|v| v.clone()).collect();
        let current_type = if non_null.is_empty() {
            "String".to_string()
        } else {
            let refs: Vec<&str> = non_null.iter().map(|s| s.as_str()).collect();
            naive_current_type(&refs).to_string()
        };
        columns.push(ColumnInput {
            name,
            current_type,
            raw_values: non_null,
            total,
            skip_heuristics: false,
        });
    }
    Ok(columns)
}

// --- JSON / JSON Lines reader ---
// Nesting is never force-fit into one opaque row: objects are flattened into
// dot-notation sub-columns (recursively), and array elements are pooled
// (unwrapping nested arrays) so the pool's real type(s) get reported and,
// if the pool holds objects, those get flattened too.

#[derive(PartialEq, Eq, Hash, Clone, Copy)]
enum JsonKind {
    Integer,
    Float,
    Str,
    Bool,
    Object,
}

fn kind_label(k: JsonKind) -> &'static str {
    match k {
        JsonKind::Integer => "i64",
        JsonKind::Float => "f64",
        JsonKind::Str => "String",
        JsonKind::Bool => "bool",
        JsonKind::Object => "object",
    }
}

/// current_type label for a pool of observed kinds: the single kind if
/// consistent, otherwise every observed kind with its count, e.g.
/// "mixed(String: 1, i64: 2)" - so an inconsistency is never just flagged,
/// it's fully enumerated.
fn describe_kinds(counts: &HashMap<JsonKind, usize>) -> String {
    if counts.len() == 1 {
        return kind_label(*counts.keys().next().unwrap()).to_string();
    }
    let mut parts: Vec<(String, usize)> = counts
        .iter()
        .map(|(k, c)| (kind_label(*k).to_string(), *c))
        .collect();
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    let inner = parts
        .iter()
        .map(|(label, count)| format!("{label}: {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("mixed({inner})")
}

/// Recursively unwraps arrays of any depth into a flat pool of non-array,
/// non-null leaf values (scalars and/or objects), noting whether an array
/// was seen anywhere along the way.
fn unwrap_arrays<'a>(values: &[&'a JsonValue]) -> (Vec<&'a JsonValue>, bool) {
    fn walk<'a>(v: &'a JsonValue, pool: &mut Vec<&'a JsonValue>, saw_array: &mut bool) {
        match v {
            JsonValue::Array(items) => {
                *saw_array = true;
                for item in items {
                    if !item.is_null() {
                        walk(item, pool, saw_array);
                    }
                }
            }
            _ => pool.push(v),
        }
    }
    let mut pool = Vec::new();
    let mut saw_array = false;
    for v in values {
        walk(v, &mut pool, &mut saw_array);
    }
    (pool, saw_array)
}

/// Reads either a top-level JSON array of objects, or JSON Lines (one object
/// per non-empty line) - detected by whether the trimmed content starts with '['.
fn read_json_records(path: &Path) -> Result<Vec<serde_json::Map<String, JsonValue>>> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    let trimmed = content.trim_start();

    if trimmed.starts_with('[') {
        let values: Vec<JsonValue> = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {path:?} as a JSON array"))?;
        values
            .into_iter()
            .map(|v| match v {
                JsonValue::Object(m) => Ok(m),
                _ => bail!("expected an array of objects in {path:?}"),
            })
            .collect()
    } else {
        trimmed
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                let v: JsonValue = serde_json::from_str(line)
                    .with_context(|| format!("failed to parse a line of {path:?} as JSON"))?;
                match v {
                    JsonValue::Object(m) => Ok(m),
                    _ => bail!("expected one JSON object per line in {path:?}"),
                }
            })
            .collect()
    }
}

/// Profiles one JSON "path". `total` is how many parent slots could have held
/// a value here (for missing %); `values` are the non-null values actually
/// found there. Returns this path's own row followed by every descendant row
/// (dot-notation), in order, so nested content always ends up reported, not
/// just labelled.
fn profile_json_path(
    name: String,
    total: usize,
    values: Vec<&JsonValue>,
    n_samples: usize,
) -> Vec<ColumnProfile> {
    let missing = total.saturating_sub(values.len());
    let missing_pct = round1(if total > 0 {
        missing as f64 / total as f64 * 100.0
    } else {
        0.0
    });

    if values.is_empty() {
        return vec![ColumnProfile {
            name,
            current_type: "null".to_string(),
            ideal_type: "String".to_string(),
            description: String::new(),
            missing_pct,
            sample_values: Vec::new(),
            notes: "column is empty/all null".to_string(),
        }];
    }

    let (pool, saw_array) = unwrap_arrays(&values);
    let wrap = |s: &str| {
        if saw_array {
            format!("Vec<{s}>")
        } else {
            s.to_string()
        }
    };

    let mut kind_counts: HashMap<JsonKind, usize> = HashMap::new();
    let mut scalar_raw: Vec<String> = Vec::new();
    let mut object_maps: Vec<&serde_json::Map<String, JsonValue>> = Vec::new();

    for v in &pool {
        match v {
            JsonValue::Object(m) => {
                *kind_counts.entry(JsonKind::Object).or_insert(0) += 1;
                object_maps.push(m);
            }
            JsonValue::Bool(b) => {
                *kind_counts.entry(JsonKind::Bool).or_insert(0) += 1;
                scalar_raw.push(b.to_string());
            }
            JsonValue::Number(n) => {
                let k = if n.is_i64() || n.is_u64() {
                    JsonKind::Integer
                } else {
                    JsonKind::Float
                };
                *kind_counts.entry(k).or_insert(0) += 1;
                scalar_raw.push(n.to_string());
            }
            JsonValue::String(s) => {
                *kind_counts.entry(JsonKind::Str).or_insert(0) += 1;
                scalar_raw.push(s.clone());
            }
            JsonValue::Null | JsonValue::Array(_) => {
                unreachable!("unwrap_arrays already removed these")
            }
        }
    }

    let (current_type, ideal_type, mut notes) = if pool.is_empty() {
        // saw_array must be true here: values was non-empty but every array found was empty.
        (
            wrap("empty"),
            wrap("empty"),
            "array is always empty - can't infer an element type".to_string(),
        )
    } else if !object_maps.is_empty() && scalar_raw.is_empty() {
        (
            wrap("object"),
            wrap("struct"),
            format!("flattened into {name}.* below"),
        )
    } else if !scalar_raw.is_empty() && object_maps.is_empty() {
        let base_current = describe_kinds(&kind_counts);
        let refs: Vec<&str> = scalar_raw.iter().map(|s| s.as_str()).collect();
        let (ideal, note) = suggest_ideal_type(&refs, &base_current);
        (wrap(&base_current), wrap(&ideal), note)
    } else {
        let base_current = describe_kinds(&kind_counts);
        let note =
            format!("mix of scalars and objects - object fields listed separately under {name}.*");
        (wrap(&base_current), wrap("String"), note)
    };

    if missing_pct > 0.0 {
        let extra = "has missing values -> wrap in Option<T> / handle nulls";
        notes = if notes.is_empty() {
            extra.to_string()
        } else {
            format!("{notes}; {extra}")
        };
    }

    let sample_pool: Vec<String> = if !scalar_raw.is_empty() {
        scalar_raw.clone()
    } else {
        object_maps
            .iter()
            .map(|m| JsonValue::Object((*m).clone()).to_string())
            .collect()
    };
    let mut seen = HashSet::new();
    let mut samples = Vec::new();
    for v in &sample_pool {
        if seen.insert(v.clone()) {
            samples.push(v.clone());
            if samples.len() >= n_samples {
                break;
            }
        }
    }

    let mut result = vec![ColumnProfile {
        name: name.clone(),
        current_type,
        ideal_type,
        description: String::new(),
        missing_pct,
        sample_values: samples,
        notes,
    }];

    if !object_maps.is_empty() {
        let mut order: Vec<String> = Vec::new();
        let mut seen_keys = HashSet::new();
        for m in &object_maps {
            for k in m.keys() {
                if seen_keys.insert(k.clone()) {
                    order.push(k.clone());
                }
            }
        }
        let child_total = object_maps.len();
        for key in order {
            let child_values: Vec<&JsonValue> = object_maps
                .iter()
                .filter_map(|m| m.get(&key))
                .filter(|v| !v.is_null())
                .collect();
            result.extend(profile_json_path(
                format!("{name}.{key}"),
                child_total,
                child_values,
                n_samples,
            ));
        }
    }

    result
}

/// Shared by any format that decodes to a list of named-field records
/// (JSON files today, Avro below) - extracts top-level columns in
/// first-seen order and profiles each, recursing into nested content.
fn profile_json_records(
    records: &[serde_json::Map<String, JsonValue>],
    n_samples: usize,
) -> Vec<ColumnProfile> {
    let total = records.len();
    let mut order: Vec<String> = Vec::new();
    let mut seen_keys = HashSet::new();
    for rec in records {
        for k in rec.keys() {
            if seen_keys.insert(k.clone()) {
                order.push(k.clone());
            }
        }
    }

    let mut out = Vec::new();
    for name in order {
        let values: Vec<&JsonValue> = records
            .iter()
            .filter_map(|r| r.get(&name))
            .filter(|v| !v.is_null())
            .collect();
        out.extend(profile_json_path(name, total, values, n_samples));
    }
    out
}

fn columns_from_json(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    let mut records = read_json_records(path)?;
    if let Some(n) = nrows {
        records.truncate(n);
    }
    Ok(profile_json_records(&records, n_samples))
}

// --- Parquet + Arrow IPC/Feather readers (opt-in via --features parquet) ---
// Both are Arrow-ecosystem formats that decode to the same RecordBatch type,
// so they share one batch-profiling function and differ only in how the
// reader is opened.

#[cfg(feature = "parquet")]
fn arrow_type_label(dt: &arrow::datatypes::DataType) -> String {
    use arrow::datatypes::DataType;
    match dt {
        DataType::Boolean => "bool".to_string(),
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => "i64".to_string(),
        DataType::Float16 | DataType::Float32 | DataType::Float64 => "f64".to_string(),
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => "String".to_string(),
        DataType::Date32 | DataType::Date64 => "Date".to_string(),
        DataType::Timestamp(..) => "Timestamp".to_string(),
        DataType::Decimal128(..) | DataType::Decimal256(..) => "Decimal".to_string(),
        DataType::List(_) | DataType::LargeList(_) | DataType::FixedSizeList(..) => {
            "List".to_string()
        }
        DataType::Struct(_) => "Struct".to_string(),
        DataType::Map(..) => "Map".to_string(),
        // Dictionary encoding is a storage detail (a compact index into a
        // value dictionary, typically for low-cardinality strings) - it
        // hasn't lost or changed the logical type, so report the value
        // type underneath rather than the encoding wrapping it.
        DataType::Dictionary(_, value_type) => arrow_type_label(value_type),
        other => format!("{other:?}"),
    }
}

#[cfg(feature = "parquet")]
fn is_nested_arrow_type(dt: &arrow::datatypes::DataType) -> bool {
    use arrow::datatypes::DataType;
    matches!(
        dt,
        DataType::List(_)
            | DataType::LargeList(_)
            | DataType::FixedSizeList(..)
            | DataType::Struct(_)
            | DataType::Map(..)
    )
}

/// Profiles any stream of Arrow RecordBatches sharing one schema. Scalar
/// columns are stringified directly (fast path); columns whose type is
/// Struct/List get bridged to serde_json::Value via Arrow's own JSON writer
/// and handed to the same recursive flattener used for JSON/Avro files, so
/// nesting gets identical treatment regardless of source format.
#[cfg(feature = "parquet")]
fn profile_arrow_batches(
    path: &Path,
    schema: &arrow::datatypes::Schema,
    reader: impl Iterator<
        Item = std::result::Result<arrow::record_batch::RecordBatch, arrow::error::ArrowError>,
    >,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    use arrow::util::display::array_value_to_string;

    let names: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();
    let nested: Vec<bool> = schema
        .fields()
        .iter()
        .map(|f| is_nested_arrow_type(f.data_type()))
        .collect();
    let type_labels: Vec<String> = schema
        .fields()
        .iter()
        .map(|f| arrow_type_label(f.data_type()))
        .collect();
    let any_nested = nested.iter().any(|&n| n);

    let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); names.len()];
    let mut nested_values: Vec<Vec<JsonValue>> = vec![Vec::new(); names.len()];
    let mut rows_read = 0usize;

    'batches: for batch_result in reader {
        let batch =
            batch_result.with_context(|| format!("failed reading a batch from {path:?}"))?;

        let json_rows: Vec<serde_json::Map<String, JsonValue>> = if any_nested {
            let mut writer = arrow::json::writer::ArrayWriter::new(Vec::new());
            writer
                .write(&batch)
                .with_context(|| format!("failed converting a batch to JSON in {path:?}"))?;
            writer
                .finish()
                .with_context(|| format!("failed finishing JSON conversion in {path:?}"))?;
            let buf = writer.into_inner();
            serde_json::from_slice(&buf)
                .with_context(|| format!("failed parsing converted JSON for a batch in {path:?}"))?
        } else {
            Vec::new()
        };

        for row in 0..batch.num_rows() {
            if nrows.is_some_and(|limit| rows_read >= limit) {
                break 'batches;
            }
            for (col_idx, array) in batch.columns().iter().enumerate() {
                if nested[col_idx] {
                    let found = json_rows
                        .get(row)
                        .and_then(|m| m.get(&names[col_idx]))
                        .filter(|v| !v.is_null());
                    if let Some(v) = found {
                        nested_values[col_idx].push(v.clone());
                    }
                } else {
                    let value = if array.is_null(row) {
                        None
                    } else {
                        Some(array_value_to_string(array, row).unwrap_or_default())
                    };
                    raw[col_idx].push(value);
                }
            }
            rows_read += 1;
        }
    }

    let mut out = Vec::new();
    for (i, name) in names.into_iter().enumerate() {
        if nested[i] {
            let values: Vec<&JsonValue> = nested_values[i].iter().collect();
            out.extend(profile_json_path(name, rows_read, values, n_samples));
        } else {
            let non_null: Vec<String> = raw[i].iter().filter_map(|v| v.clone()).collect();
            let col = ColumnInput {
                name,
                current_type: type_labels[i].clone(),
                raw_values: non_null,
                total: raw[i].len(),
                skip_heuristics: false,
            };
            out.push(profile_column(col, n_samples));
        }
    }
    Ok(out)
}

#[cfg(feature = "parquet")]
fn columns_from_parquet(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("failed to read parquet metadata from {path:?}"))?;
    let schema = builder.schema().clone();
    let reader = builder
        .build()
        .with_context(|| format!("failed to build a reader for {path:?}"))?;
    profile_arrow_batches(path, &schema, reader, nrows, n_samples)
}

#[cfg(not(feature = "parquet"))]
fn columns_from_parquet(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    bail!(
        "Parquet support isn't compiled in - rebuild with `cargo build --release --features parquet` (or --features full)"
    )
}

#[cfg(feature = "parquet")]
fn columns_from_arrow_ipc(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    use arrow::ipc::reader::FileReader;
    use std::fs::File;

    let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let reader = FileReader::try_new(file, None)
        .with_context(|| format!("failed to read Arrow IPC/Feather file {path:?}"))?;
    let schema = reader.schema();
    profile_arrow_batches(path, schema.as_ref(), reader, nrows, n_samples)
}

#[cfg(not(feature = "parquet"))]
fn columns_from_arrow_ipc(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    bail!(
        "Arrow IPC/Feather support isn't compiled in - rebuild with `cargo build --release --features parquet` (or --features full)"
    )
}

// --- Avro reader (opt-in via --features avro) ---
// Decodes each record to serde_json::Value and reuses the exact same
// column-extraction/flattening path as JSON files.

#[cfg(feature = "avro")]
fn avro_value_to_json(v: &apache_avro::types::Value) -> JsonValue {
    use apache_avro::types::Value as AvroValue;
    match v {
        AvroValue::Null => JsonValue::Null,
        AvroValue::Boolean(b) => JsonValue::Bool(*b),
        AvroValue::Int(i) => JsonValue::Number((*i).into()),
        AvroValue::Long(i) | AvroValue::TimestampMillis(i) | AvroValue::TimestampMicros(i) => {
            JsonValue::Number((*i).into())
        }
        AvroValue::Float(f) => {
            serde_json::Number::from_f64(f64::from(*f)).map_or(JsonValue::Null, JsonValue::Number)
        }
        AvroValue::Double(f) => {
            serde_json::Number::from_f64(*f).map_or(JsonValue::Null, JsonValue::Number)
        }
        AvroValue::String(s) | AvroValue::Enum(_, s) => JsonValue::String(s.clone()),
        AvroValue::Bytes(b) | AvroValue::Fixed(_, b) => {
            JsonValue::String(b.iter().map(|byte| format!("{byte:02x}")).collect())
        }
        AvroValue::Union(_, inner) => avro_value_to_json(inner),
        AvroValue::Array(items) => JsonValue::Array(items.iter().map(avro_value_to_json).collect()),
        AvroValue::Map(m) => JsonValue::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), avro_value_to_json(v)))
                .collect(),
        ),
        AvroValue::Record(fields) => JsonValue::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), avro_value_to_json(v)))
                .collect(),
        ),
        AvroValue::Date(days) => chrono::DateTime::UNIX_EPOCH
            .checked_add_signed(chrono::Duration::days(i64::from(*days)))
            .map_or(JsonValue::Null, |d| {
                JsonValue::String(d.format("%Y-%m-%d").to_string())
            }),
        AvroValue::Uuid(u) => JsonValue::String(u.to_string()),
        other => JsonValue::String(format!("{other:?}")), // best-effort for Decimal/Duration/local timestamps etc.
    }
}

#[cfg(feature = "avro")]
fn columns_from_avro(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    use apache_avro::Reader as AvroReader;
    use std::fs::File;

    let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let reader =
        AvroReader::new(file).with_context(|| format!("failed to read Avro file {path:?}"))?;

    let mut records: Vec<serde_json::Map<String, JsonValue>> = Vec::new();
    for (i, value_result) in reader.enumerate() {
        if nrows.is_some_and(|limit| i >= limit) {
            break;
        }
        let value =
            value_result.with_context(|| format!("failed decoding a record from {path:?}"))?;
        match avro_value_to_json(&value) {
            JsonValue::Object(m) => records.push(m),
            _ => bail!("expected each Avro record to decode to an object in {path:?}"),
        }
    }
    Ok(profile_json_records(&records, n_samples))
}

#[cfg(not(feature = "avro"))]
fn columns_from_avro(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    bail!(
        "Avro support isn't compiled in - rebuild with `cargo build --release --features avro` (or --features full)"
    )
}

// --- MessagePack reader (opt-in via --features msgpack) ---
// Decodes each top-level value to serde_json::Value and reuses the exact
// same column-extraction/flattening path as JSON/Avro files.

#[cfg(feature = "msgpack")]
fn msgpack_key_to_string(k: &rmpv::Value) -> String {
    if let rmpv::Value::String(s) = k
        && let Some(s) = s.as_str()
    {
        return s.to_string();
    }
    msgpack_value_to_json(k).to_string()
}

#[cfg(feature = "msgpack")]
fn msgpack_value_to_json(v: &rmpv::Value) -> JsonValue {
    use rmpv::Value as MpValue;
    match v {
        MpValue::Nil => JsonValue::Null,
        MpValue::Boolean(b) => JsonValue::Bool(*b),
        MpValue::Integer(i) => i
            .as_i64()
            .map(JsonValue::from)
            .or_else(|| i.as_u64().map(JsonValue::from))
            .unwrap_or(JsonValue::Null),
        MpValue::F32(f) => {
            serde_json::Number::from_f64(f64::from(*f)).map_or(JsonValue::Null, JsonValue::Number)
        }
        MpValue::F64(f) => {
            serde_json::Number::from_f64(*f).map_or(JsonValue::Null, JsonValue::Number)
        }
        MpValue::String(s) => JsonValue::String(match s.as_str() {
            Some(s) => s.to_string(),
            None => s.as_bytes().iter().map(|b| format!("{b:02x}")).collect(),
        }),
        MpValue::Binary(b) => {
            JsonValue::String(b.iter().map(|byte| format!("{byte:02x}")).collect())
        }
        MpValue::Array(items) => {
            JsonValue::Array(items.iter().map(msgpack_value_to_json).collect())
        }
        MpValue::Map(pairs) => JsonValue::Object(
            pairs
                .iter()
                .map(|(k, v)| (msgpack_key_to_string(k), msgpack_value_to_json(v)))
                .collect(),
        ),
        MpValue::Ext(kind, data) => JsonValue::String(format!("ext({kind}, {} bytes)", data.len())),
    }
}

/// Reads a stream of top-level MessagePack values (each value is
/// self-delimiting, so records can just be concatenated back-to-back in the
/// file - the common convention for a MessagePack *data* file, as opposed to
/// a single MessagePack-encoded document). If the file holds exactly one
/// top-level value and it's an array, that array's elements are treated as
/// the records instead, mirroring how the JSON reader treats a single
/// top-level `[...]` array.
#[cfg(feature = "msgpack")]
fn columns_from_msgpack(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    use std::fs::File;
    use std::io::BufRead;
    use std::io::BufReader;

    let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let mut reader = BufReader::new(file);

    let mut top_values = Vec::new();
    while !reader
        .fill_buf()
        .with_context(|| format!("failed reading {path:?}"))?
        .is_empty()
    {
        let v = rmpv::decode::read_value(&mut reader)
            .with_context(|| format!("failed decoding a MessagePack value from {path:?}"))?;
        top_values.push(v);
    }

    let values: Vec<rmpv::Value> = if top_values.len() == 1 {
        match top_values.into_iter().next().unwrap() {
            rmpv::Value::Array(items) => items,
            other => vec![other],
        }
    } else {
        top_values
    };

    let mut records: Vec<serde_json::Map<String, JsonValue>> = Vec::new();
    for v in values {
        match msgpack_value_to_json(&v) {
            JsonValue::Object(m) => records.push(m),
            _ => bail!("expected each MessagePack record to decode to a map in {path:?}"),
        }
    }
    if let Some(n) = nrows {
        records.truncate(n);
    }
    Ok(profile_json_records(&records, n_samples))
}

#[cfg(not(feature = "msgpack"))]
fn columns_from_msgpack(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    bail!(
        "MessagePack support isn't compiled in - rebuild with `cargo build --release --features msgpack` (or --features full)"
    )
}

// --- TOML reader (opt-in via --features toml) ---
// A TOML file is a single document, not inherently a table of many rows -
// unlike every other reader in this file, there's no natural "row" to
// repeat. Rather than invent a fake row count, the whole document is
// profiled as one record via profile_json_records (total = 1), so an
// array-of-tables section (`[[servers]]`) becomes a `Vec<object>` column
// that flattens exactly like any other JSON array of objects would.

#[cfg(feature = "toml")]
fn toml_value_to_json(v: &toml::Value) -> JsonValue {
    match v {
        toml::Value::String(s) => JsonValue::String(s.clone()),
        toml::Value::Integer(i) => JsonValue::from(*i),
        toml::Value::Float(f) => {
            serde_json::Number::from_f64(*f).map_or(JsonValue::Null, JsonValue::Number)
        }
        toml::Value::Boolean(b) => JsonValue::Bool(*b),
        toml::Value::Datetime(dt) => JsonValue::String(dt.to_string()),
        toml::Value::Array(items) => {
            JsonValue::Array(items.iter().map(toml_value_to_json).collect())
        }
        toml::Value::Table(t) => JsonValue::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_value_to_json(v)))
                .collect(),
        ),
    }
}

#[cfg(feature = "toml")]
fn columns_from_toml(path: &Path, n_samples: usize) -> Result<Vec<ColumnProfile>> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    let value: toml::Value =
        toml::from_str(&content).with_context(|| format!("failed to parse {path:?} as TOML"))?;
    let record = match toml_value_to_json(&value) {
        JsonValue::Object(m) => m,
        _ => bail!("expected a TOML document with top-level key-value pairs in {path:?}"),
    };
    Ok(profile_json_records(&[record], n_samples))
}

#[cfg(not(feature = "toml"))]
fn columns_from_toml(_path: &Path, _n_samples: usize) -> Result<Vec<ColumnProfile>> {
    bail!(
        "TOML support isn't compiled in - rebuild with `cargo build --release --features toml` (or --features full)"
    )
}

// --- YAML reader (opt-in via --features yaml, via the serde_norway crate -
// a maintained fork of the archived serde_yaml, same API shape) ---
// YAML has three shapes a data file commonly takes, so the record list is
// built differently depending on what's actually in the file rather than
// assuming one: a single top-level sequence is an array of records (like
// JSON's `[...]` mode); a single top-level mapping is one record (the
// whole document is the row - the same choice TOML makes for its own
// single-document format); a `---`-separated multi-document stream is one
// record per document (YAML's own equivalent of JSON Lines).

#[cfg(feature = "yaml")]
fn yaml_key_to_string(k: &serde_norway::Value) -> String {
    match k {
        serde_norway::Value::String(s) => s.clone(),
        other => match yaml_value_to_json(other) {
            JsonValue::String(s) => s,
            other => other.to_string(),
        },
    }
}

#[cfg(feature = "yaml")]
fn yaml_value_to_json(v: &serde_norway::Value) -> JsonValue {
    use serde_norway::Value as YamlValue;
    match v {
        YamlValue::Null => JsonValue::Null,
        YamlValue::Bool(b) => JsonValue::Bool(*b),
        YamlValue::Number(n) => n
            .as_i64()
            .map(JsonValue::from)
            .or_else(|| n.as_u64().map(JsonValue::from))
            .or_else(|| {
                n.as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map(JsonValue::Number)
            })
            .unwrap_or(JsonValue::Null),
        YamlValue::String(s) => JsonValue::String(s.clone()),
        YamlValue::Sequence(items) => {
            JsonValue::Array(items.iter().map(yaml_value_to_json).collect())
        }
        YamlValue::Mapping(m) => JsonValue::Object(
            m.iter()
                .map(|(k, v)| (yaml_key_to_string(k), yaml_value_to_json(v)))
                .collect(),
        ),
        // A tagged scalar/sequence/mapping (YAML's `!Tag value` syntax) -
        // best-effort: keep the tag visible rather than silently dropping it.
        YamlValue::Tagged(t) => {
            let mut obj = serde_json::Map::new();
            obj.insert(t.tag.to_string(), yaml_value_to_json(&t.value));
            JsonValue::Object(obj)
        }
    }
}

#[cfg(feature = "yaml")]
fn yaml_document_to_record(
    v: serde_norway::Value,
    path: &Path,
) -> Result<serde_json::Map<String, JsonValue>> {
    match yaml_value_to_json(&v) {
        JsonValue::Object(m) => Ok(m),
        _ => bail!("expected each YAML document/record to be a mapping in {path:?}"),
    }
}

#[cfg(feature = "yaml")]
fn columns_from_yaml(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    use serde::Deserialize;

    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;

    let mut documents: Vec<serde_norway::Value> = Vec::new();
    for doc in serde_norway::Deserializer::from_str(&content) {
        let value = serde_norway::Value::deserialize(doc)
            .with_context(|| format!("failed to parse a YAML document in {path:?}"))?;
        if !value.is_null() {
            documents.push(value);
        }
    }

    let mut records = Vec::new();
    match documents.len() {
        1 => match documents.into_iter().next().unwrap() {
            serde_norway::Value::Sequence(items) => {
                for item in items {
                    records.push(yaml_document_to_record(item, path)?);
                }
            }
            other => records.push(yaml_document_to_record(other, path)?),
        },
        _ => {
            for doc in documents {
                records.push(yaml_document_to_record(doc, path)?);
            }
        }
    }

    if let Some(n) = nrows {
        records.truncate(n);
    }
    Ok(profile_json_records(&records, n_samples))
}

#[cfg(not(feature = "yaml"))]
fn columns_from_yaml(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    bail!(
        "YAML support isn't compiled in - rebuild with `cargo build --release --features yaml` (or --features full)"
    )
}

// --- CBOR reader (opt-in via --features cbor) ---
// Same shape as the MessagePack reader above: CBOR values are
// self-delimiting, so a data file is read as a stream of concatenated
// top-level values (or, if there's exactly one top-level value and it's an
// array, that array's elements instead - mirroring the JSON reader's
// `[...]` mode).

#[cfg(feature = "cbor")]
fn cbor_key_to_string(k: &ciborium::Value) -> String {
    if let ciborium::Value::Text(s) = k {
        return s.clone();
    }
    match cbor_value_to_json(k) {
        JsonValue::String(s) => s,
        other => other.to_string(),
    }
}

#[cfg(feature = "cbor")]
fn cbor_value_to_json(v: &ciborium::Value) -> JsonValue {
    use ciborium::Value as CborValue;
    match v {
        CborValue::Null => JsonValue::Null,
        CborValue::Bool(b) => JsonValue::Bool(*b),
        CborValue::Integer(i) => i64::try_from(*i)
            .map(JsonValue::from)
            .unwrap_or_else(|_| JsonValue::String(i128::from(*i).to_string())),
        CborValue::Float(f) => {
            serde_json::Number::from_f64(*f).map_or(JsonValue::Null, JsonValue::Number)
        }
        CborValue::Text(s) => JsonValue::String(s.clone()),
        CborValue::Bytes(b) => {
            JsonValue::String(b.iter().map(|byte| format!("{byte:02x}")).collect())
        }
        CborValue::Array(items) => JsonValue::Array(items.iter().map(cbor_value_to_json).collect()),
        CborValue::Map(pairs) => JsonValue::Object(
            pairs
                .iter()
                .map(|(k, v)| (cbor_key_to_string(k), cbor_value_to_json(v)))
                .collect(),
        ),
        // A tagged value (CBOR's major type 6, e.g. a date-time or bignum
        // hint) - best-effort: keep the tag number visible rather than
        // silently dropping it, same choice as YAML's `!Tag` handling.
        CborValue::Tag(tag, inner) => {
            let mut obj = serde_json::Map::new();
            obj.insert(format!("tag({tag})"), cbor_value_to_json(inner));
            JsonValue::Object(obj)
        }
        _ => JsonValue::Null, // ciborium::Value is #[non_exhaustive]
    }
}

#[cfg(feature = "cbor")]
fn columns_from_cbor(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    use std::fs::File;
    use std::io::BufRead;
    use std::io::BufReader;

    let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let mut reader = BufReader::new(file);

    let mut top_values: Vec<ciborium::Value> = Vec::new();
    while !reader
        .fill_buf()
        .with_context(|| format!("failed reading {path:?}"))?
        .is_empty()
    {
        let v: ciborium::Value = ciborium::from_reader(&mut reader)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("failed decoding a CBOR value from {path:?}"))?;
        top_values.push(v);
    }

    let values: Vec<ciborium::Value> = if top_values.len() == 1 {
        match top_values.into_iter().next().unwrap() {
            ciborium::Value::Array(items) => items,
            other => vec![other],
        }
    } else {
        top_values
    };

    let mut records: Vec<serde_json::Map<String, JsonValue>> = Vec::new();
    for v in values {
        match cbor_value_to_json(&v) {
            JsonValue::Object(m) => records.push(m),
            _ => bail!("expected each CBOR record to decode to a map in {path:?}"),
        }
    }
    if let Some(n) = nrows {
        records.truncate(n);
    }
    Ok(profile_json_records(&records, n_samples))
}

#[cfg(not(feature = "cbor"))]
fn columns_from_cbor(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    bail!(
        "CBOR support isn't compiled in - rebuild with `cargo build --release --features cbor` (or --features full)"
    )
}

// --- INI reader (opt-in via --features ini) ---
// An INI file's sections are already "multiple named groups of key=value
// pairs", so - like SQLite's tables and Excel's sheets - this returns one
// profile list per section rather than assuming a single implicit table.
// Within a section there's no repeating "row" concept (it's a flat set of
// keys), so each section is profiled as a single record, the same choice
// TOML/YAML make for their own single-document shapes. A key repeated
// within one section (INI permits this) pools into one array value rather
// than the second occurrence silently overwriting the first.

#[cfg(feature = "ini")]
fn columns_from_ini(path: &Path, n_samples: usize) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
    let conf = ini::Ini::load_from_file(path)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("failed to parse {path:?} as INI"))?;

    let mut out = Vec::new();
    for (section_name, props) in conf.iter() {
        if props.is_empty() {
            continue; // e.g. no general section before the first [header]
        }
        let mut record = serde_json::Map::new();
        for (k, v) in props.iter() {
            match record.get_mut(k) {
                Some(JsonValue::Array(values)) => values.push(JsonValue::String(v.to_string())),
                Some(existing) => {
                    let first = existing.clone();
                    *existing = JsonValue::Array(vec![first, JsonValue::String(v.to_string())]);
                }
                None => {
                    record.insert(k.to_string(), JsonValue::String(v.to_string()));
                }
            }
        }
        let name = section_name.unwrap_or("(default)").to_string();
        out.push((name, profile_json_records(&[record], n_samples)));
    }

    if out.is_empty() {
        bail!("no sections found in {path:?}");
    }
    Ok(out)
}

#[cfg(not(feature = "ini"))]
fn columns_from_ini(_path: &Path, _n_samples: usize) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
    bail!(
        "INI support isn't compiled in - rebuild with `cargo build --release --features ini` (or --features full)"
    )
}

// --- Excel reader (opt-in via --features xlsx; also covers .xls/.xlsb/.ods) ---
// A workbook can hold multiple sheets, so - like SQLite - this returns one
// profile list per sheet rather than assuming a single implicit table.
// Empty sheets are skipped rather than erroring, the same way SQLite skips
// its own internal `sqlite_%` tables.

#[cfg(feature = "xlsx")]
fn columns_from_xlsx(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
    use calamine::{DataType as _, Reader, open_workbook_auto};

    let mut workbook =
        open_workbook_auto(path).with_context(|| format!("failed to open {path:?}"))?;
    let sheet_names = workbook.sheet_names().to_vec();
    if sheet_names.is_empty() {
        bail!("no sheets found in {path:?}");
    }

    let mut out = Vec::new();
    for sheet_name in sheet_names {
        let range = workbook
            .worksheet_range(&sheet_name)
            .with_context(|| format!("failed to read sheet '{sheet_name}' in {path:?}"))?;

        let mut rows = range.rows();
        let Some(header_row) = rows.next() else {
            continue; // empty sheet - no header row, contributes no table
        };
        let headers: Vec<String> = header_row.iter().map(|c| c.to_string()).collect();

        let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); headers.len()];
        for (i, row) in rows.enumerate() {
            if nrows.is_some_and(|limit| i >= limit) {
                break;
            }
            for (col_idx, col) in raw.iter_mut().enumerate() {
                let value = match row.get(col_idx) {
                    Some(cell) if !cell.is_empty() => Some(cell.to_string()),
                    _ => None,
                };
                col.push(value);
            }
        }

        let mut profiles = Vec::new();
        for (i, name) in headers.into_iter().enumerate() {
            let total = raw[i].len();
            let non_null: Vec<String> = raw[i].iter().filter_map(|v| v.clone()).collect();
            let current_type = if non_null.is_empty() {
                "String".to_string()
            } else {
                let refs: Vec<&str> = non_null.iter().map(|s| s.as_str()).collect();
                naive_current_type(&refs).to_string()
            };
            let col = ColumnInput {
                name,
                current_type,
                raw_values: non_null,
                total,
                skip_heuristics: false,
            };
            profiles.push(profile_column(col, n_samples));
        }
        out.push((sheet_name, profiles));
    }

    if out.is_empty() {
        bail!("no non-empty sheets found in {path:?}");
    }
    Ok(out)
}

#[cfg(not(feature = "xlsx"))]
fn columns_from_xlsx(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
    bail!(
        "Excel support isn't compiled in - rebuild with `cargo build --release --features xlsx` (or --features full)"
    )
}

// --- SQLite reader (opt-in via --features sqlite) ---
// A single file can hold multiple tables, so this returns one profile list
// per table rather than a flat column list. SQLite's dynamic typing means a
// column declared INTEGER can still hold TEXT rows (a real, well-known
// SQLite quirk) - Current Type is built from what's actually stored per
// value, not the declared column type, so that quirk surfaces as a "mixed"
// type exactly like an inconsistent JSON field would.

#[cfg(feature = "sqlite")]
fn describe_sql_kinds(counts: &HashMap<&'static str, usize>) -> String {
    if counts.len() == 1 {
        return (*counts.keys().next().unwrap()).to_string();
    }
    let mut parts: Vec<(&str, usize)> = counts.iter().map(|(k, c)| (*k, *c)).collect();
    parts.sort_by(|a, b| a.0.cmp(b.0));
    let inner = parts
        .iter()
        .map(|(label, count)| format!("{label}: {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("mixed({inner})")
}

#[cfg(feature = "sqlite")]
fn columns_from_sqlite(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
    use rusqlite::Connection;
    use rusqlite::types::ValueRef;

    let conn = Connection::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let mut table_stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .with_context(|| format!("failed to list tables in {path:?}"))?;
    let table_names: Vec<String> = table_stmt
        .query_map([], |row| row.get::<_, String>(0))
        .and_then(Iterator::collect)
        .with_context(|| format!("failed to list tables in {path:?}"))?;
    drop(table_stmt);

    if table_names.is_empty() {
        bail!("no user tables found in {path:?}");
    }

    let mut out = Vec::new();
    for table in table_names {
        let query = match nrows {
            Some(n) => format!("SELECT * FROM \"{table}\" LIMIT {n}"),
            None => format!("SELECT * FROM \"{table}\""),
        };
        let mut stmt = conn
            .prepare(&query)
            .with_context(|| format!("failed to query table '{table}' in {path:?}"))?;
        let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let n_cols = col_names.len();

        let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); n_cols];
        let mut kind_counts: Vec<HashMap<&'static str, usize>> = vec![HashMap::new(); n_cols];

        let mut rows = stmt
            .query([])
            .with_context(|| format!("failed to query table '{table}' in {path:?}"))?;
        while let Some(row) = rows
            .next()
            .with_context(|| format!("failed reading a row from '{table}' in {path:?}"))?
        {
            for i in 0..n_cols {
                let value_ref = row.get_ref(i).with_context(|| {
                    format!("failed reading a value from '{table}' in {path:?}")
                })?;
                let value = match value_ref {
                    ValueRef::Null => None,
                    ValueRef::Integer(v) => {
                        *kind_counts[i].entry("i64").or_insert(0) += 1;
                        Some(v.to_string())
                    }
                    ValueRef::Real(v) => {
                        *kind_counts[i].entry("f64").or_insert(0) += 1;
                        Some(v.to_string())
                    }
                    ValueRef::Text(t) => {
                        *kind_counts[i].entry("String").or_insert(0) += 1;
                        Some(String::from_utf8_lossy(t).into_owned())
                    }
                    ValueRef::Blob(b) => {
                        *kind_counts[i].entry("Blob").or_insert(0) += 1;
                        Some(format!("<blob: {} bytes>", b.len()))
                    }
                };
                raw[i].push(value);
            }
        }

        let mut profiles = Vec::new();
        for (i, name) in col_names.into_iter().enumerate() {
            let non_null: Vec<String> = raw[i].iter().filter_map(|v| v.clone()).collect();
            let current_type = if kind_counts[i].is_empty() {
                "null".to_string()
            } else {
                describe_sql_kinds(&kind_counts[i])
            };
            let col = ColumnInput {
                name,
                current_type,
                raw_values: non_null,
                total: raw[i].len(),
                skip_heuristics: false,
            };
            profiles.push(profile_column(col, n_samples));
        }
        out.push((table, profiles));
    }
    Ok(out)
}

#[cfg(not(feature = "sqlite"))]
fn columns_from_sqlite(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
    bail!(
        "SQLite support isn't compiled in - rebuild with `cargo build --release --features sqlite` (or --features full)"
    )
}

// --- Format detection ---

enum InputFormat {
    Csv,
    Tsv,
    Json,
    Parquet,
    ArrowIpc,
    Avro,
    Xlsx,
    Sqlite,
    MsgPack,
    Toml,
    Yaml,
    Cbor,
    Ini,
}

impl InputFormat {
    fn as_str(&self) -> &'static str {
        match self {
            InputFormat::Csv => "csv",
            InputFormat::Tsv => "tsv",
            InputFormat::Json => "json",
            InputFormat::Parquet => "parquet",
            InputFormat::ArrowIpc => "arrow_ipc",
            InputFormat::Avro => "avro",
            InputFormat::Xlsx => "xlsx",
            InputFormat::Sqlite => "sqlite",
            InputFormat::MsgPack => "msgpack",
            InputFormat::Toml => "toml",
            InputFormat::Yaml => "yaml",
            InputFormat::Cbor => "cbor",
            InputFormat::Ini => "ini",
        }
    }
}

fn detect_format(path: &Path, override_fmt: &Option<String>) -> Result<InputFormat> {
    if let Some(f) = override_fmt {
        return match f.to_lowercase().as_str() {
            "csv" => Ok(InputFormat::Csv),
            "tsv" => Ok(InputFormat::Tsv),
            "json" | "jsonl" | "ndjson" => Ok(InputFormat::Json),
            "parquet" => Ok(InputFormat::Parquet),
            "arrow" | "feather" | "ipc" => Ok(InputFormat::ArrowIpc),
            "avro" => Ok(InputFormat::Avro),
            "xlsx" | "xls" | "xlsb" | "ods" => Ok(InputFormat::Xlsx),
            "sqlite" | "db" => Ok(InputFormat::Sqlite),
            "msgpack" | "mp" => Ok(InputFormat::MsgPack),
            "toml" => Ok(InputFormat::Toml),
            "yaml" | "yml" => Ok(InputFormat::Yaml),
            "cbor" => Ok(InputFormat::Cbor),
            "ini" => Ok(InputFormat::Ini),
            other => {
                bail!(
                    "unrecognized --format '{other}' (expected csv, tsv, json, parquet, arrow, avro, xlsx, sqlite, msgpack, toml, yaml, cbor, or ini)"
                )
            }
        };
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "csv" => Ok(InputFormat::Csv),
        "tsv" => Ok(InputFormat::Tsv),
        "json" | "jsonl" | "ndjson" => Ok(InputFormat::Json),
        "parquet" | "pqt" => Ok(InputFormat::Parquet),
        "arrow" | "feather" => Ok(InputFormat::ArrowIpc),
        "avro" => Ok(InputFormat::Avro),
        "xlsx" | "xls" | "xlsb" | "ods" => Ok(InputFormat::Xlsx),
        "db" | "sqlite" | "sqlite3" => Ok(InputFormat::Sqlite),
        "msgpack" | "mp" => Ok(InputFormat::MsgPack),
        "toml" => Ok(InputFormat::Toml),
        "yaml" | "yml" => Ok(InputFormat::Yaml),
        "cbor" => Ok(InputFormat::Cbor),
        "ini" => Ok(InputFormat::Ini),
        other => bail!(
            "can't infer format from extension '.{other}' - pass --format csv|tsv|json|parquet|arrow|avro|xlsx|sqlite|msgpack|toml|yaml|cbor|ini explicitly"
        ),
    }
}

// --- Shared profiling step (format-agnostic) ---

fn profile_column(col: ColumnInput, n_samples: usize) -> ColumnProfile {
    let non_null = &col.raw_values;
    let missing = col.total.saturating_sub(non_null.len());
    let missing_pct = round1(if col.total > 0 {
        missing as f64 / col.total as f64 * 100.0
    } else {
        0.0
    });

    let (ideal_type, mut notes) = if non_null.is_empty() {
        ("String".to_string(), "column is empty/all null".to_string())
    } else if col.skip_heuristics {
        (
            "String".to_string(),
            "nested value (array/object) - consider flattening before typing".to_string(),
        )
    } else {
        let refs: Vec<&str> = non_null.iter().map(|s| s.as_str()).collect();
        suggest_ideal_type(&refs, &col.current_type)
    };

    if missing_pct > 0.0 {
        let extra = "has missing values -> wrap in Option<T> / handle nulls";
        notes = if notes.is_empty() {
            extra.to_string()
        } else {
            format!("{notes}; {extra}")
        };
    }

    let mut seen = HashSet::new();
    let mut samples = Vec::new();
    for v in non_null {
        if seen.insert(v.as_str()) {
            samples.push(v.clone());
            if samples.len() >= n_samples {
                break;
            }
        }
    }

    ColumnProfile {
        name: col.name,
        current_type: col.current_type,
        ideal_type,
        description: String::new(),
        missing_pct,
        sample_values: samples,
        notes,
    }
}

fn render_markdown(file_name: &str, tables: &BTreeMap<String, Vec<ColumnProfile>>) -> String {
    let mut md = format!("# Data Dictionary: {file_name}\n\n");
    let show_headers = tables.len() > 1; // only multi-table sources (SQLite) get ## sections
    for (table_name, profiles) in tables {
        if show_headers {
            md.push_str(&format!("## {}\n\n", escape_md(table_name)));
        }
        md.push_str("| Column | Current Type | Ideal Type | Description | Missing % | Sample Values | Notes |\n");
        md.push_str("|---|---|---|---|---|---|---|\n");
        for p in profiles {
            md.push_str(&format!(
                "| {} | {} | {} | {} | {:.1}% | {} | {} |\n",
                escape_md(&p.name),
                escape_md(&p.current_type),
                escape_md(&p.ideal_type),
                escape_md(&p.description),
                p.missing_pct,
                escape_md(&p.sample_values.join(", ")),
                escape_md(&p.notes),
            ));
        }
        md.push('\n');
    }
    md.truncate(md.trim_end_matches('\n').len());
    md.push('\n');
    md
}

fn render_json(
    file_name: &str,
    format: &InputFormat,
    tables: &BTreeMap<String, Vec<ColumnProfile>>,
) -> Result<String> {
    #[derive(serde::Serialize)]
    struct DataDictionary<'a> {
        file: &'a str,
        format: &'a str,
        tables: &'a BTreeMap<String, Vec<ColumnProfile>>,
    }
    let doc = DataDictionary {
        file: file_name,
        format: format.as_str(),
        tables,
    };
    serde_json::to_string_pretty(&doc).context("failed to serialize JSON output")
}

// --- JSON-Schema-standard output (--output-format json-schema) ---
// A third, more interoperable JSON shape alongside this tool's own rich one
// above: json-schema.org's {"type": ..., "properties": {...}} vocabulary,
// built from each column's ideal_type. Deliberately lossy wherever
// ideal_type itself is lossy or ambiguous (mixed(...) types, flattened
// structs, "enum / category" without the full value list - only samples are
// kept, not the full domain) - those fall back to an unconstrained `{}`
// schema (valid JSON Schema for "anything goes") rather than guessing.

/// Maps an `ideal_type` label to a (JSON Schema type keyword, optional
/// "format" keyword) pair, for the scalar types that map cleanly onto one.
/// `None` covers everything else (mixed/struct/empty/unrecognized).
fn json_schema_scalar_type(ideal_type: &str) -> Option<(&'static str, Option<&'static str>)> {
    match ideal_type {
        "String" | "enum / category" => Some(("string", None)),
        "i64" => Some(("integer", None)),
        "f64" => Some(("number", None)),
        "bool" => Some(("boolean", None)),
        "NaiveDate / DateTime" => Some(("string", Some("date-time"))),
        _ => None,
    }
}

/// Nullable columns (missing_pct > 0) get a ["type", "null"] union instead of
/// a bare type string - the same "missing values never fake a type change"
/// principle the rest of this tool applies, expressed in schema form.
fn json_schema_type_value(base: &str, nullable: bool) -> JsonValue {
    if nullable {
        json!([base, "null"])
    } else {
        json!(base)
    }
}

fn json_schema_property(p: &ColumnProfile) -> JsonValue {
    let nullable = p.missing_pct > 0.0;

    if let Some(inner) = p
        .ideal_type
        .strip_prefix("Vec<")
        .and_then(|s| s.strip_suffix('>'))
    {
        let items = match json_schema_scalar_type(inner) {
            Some((t, Some(fmt))) => json!({"type": t, "format": fmt}),
            Some((t, None)) => json!({"type": t}),
            None => json!({}),
        };
        return json!({"type": json_schema_type_value("array", nullable), "items": items});
    }

    match json_schema_scalar_type(&p.ideal_type) {
        Some((t, Some(fmt))) => {
            json!({"type": json_schema_type_value(t, nullable), "format": fmt})
        }
        Some((t, None)) => json!({"type": json_schema_type_value(t, nullable)}),
        None => json!({}),
    }
}

fn render_json_schema(
    file_name: &str,
    tables: &BTreeMap<String, Vec<ColumnProfile>>,
) -> Result<String> {
    let mut table_schemas = serde_json::Map::new();
    for (table_name, profiles) in tables {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for p in profiles {
            properties.insert(p.name.clone(), json_schema_property(p));
            if p.missing_pct == 0.0 {
                required.push(JsonValue::String(p.name.clone()));
            }
        }
        let mut schema = serde_json::Map::new();
        schema.insert("type".to_string(), json!("object"));
        schema.insert("properties".to_string(), JsonValue::Object(properties));
        if !required.is_empty() {
            schema.insert("required".to_string(), JsonValue::Array(required));
        }
        table_schemas.insert(table_name.clone(), JsonValue::Object(schema));
    }

    let doc = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "file": file_name,
        "tables": table_schemas,
    });
    serde_json::to_string_pretty(&doc).context("failed to serialize JSON Schema output")
}

enum OutputFormat {
    Markdown,
    Json,
    JsonSchema,
}

impl OutputFormat {
    fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "md" | "markdown" => Ok(OutputFormat::Markdown),
            "json" => Ok(OutputFormat::Json),
            "json-schema" | "jsonschema" => Ok(OutputFormat::JsonSchema),
            other => {
                bail!("unrecognized --output-format '{other}' (expected md, json, or json-schema)")
            }
        }
    }
}

pub fn run() -> Result<()> {
    let args = Args::parse();

    let output_format = OutputFormat::parse(&args.output_format)?;

    let format = detect_format(&args.input_path, &args.format)?;
    let file_name = args
        .input_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let file_stem = args
        .input_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    // Every format ends up as the same shape - a table name mapped to its column
    // profiles - so JSON/Markdown rendering never needs to special-case SQLite's
    // (and Excel's, and INI's) multiple tables vs. everything else's single
    // implicit one.
    let tables: BTreeMap<String, Vec<ColumnProfile>> = if matches!(
        format,
        InputFormat::Sqlite | InputFormat::Xlsx | InputFormat::Ini
    ) {
        match format {
            InputFormat::Sqlite => columns_from_sqlite(&args.input_path, args.nrows, args.samples)?,
            InputFormat::Xlsx => columns_from_xlsx(&args.input_path, args.nrows, args.samples)?,
            InputFormat::Ini => columns_from_ini(&args.input_path, args.samples)?,
            _ => unreachable!("handled by the outer matches! guard"),
        }
        .into_iter()
        .collect()
    } else {
        let profiles: Vec<ColumnProfile> = match format {
            InputFormat::Csv => columns_from_csv(
                &args.input_path,
                args.nrows,
                args.delimiter.unwrap_or(',') as u8,
            )?
            .into_iter()
            .map(|c| profile_column(c, args.samples))
            .collect(),
            InputFormat::Tsv => columns_from_csv(
                &args.input_path,
                args.nrows,
                args.delimiter.unwrap_or('\t') as u8,
            )?
            .into_iter()
            .map(|c| profile_column(c, args.samples))
            .collect(),
            InputFormat::Json => columns_from_json(&args.input_path, args.nrows, args.samples)?,
            InputFormat::Parquet => {
                columns_from_parquet(&args.input_path, args.nrows, args.samples)?
            }
            InputFormat::ArrowIpc => {
                columns_from_arrow_ipc(&args.input_path, args.nrows, args.samples)?
            }
            InputFormat::Avro => columns_from_avro(&args.input_path, args.nrows, args.samples)?,
            InputFormat::MsgPack => {
                columns_from_msgpack(&args.input_path, args.nrows, args.samples)?
            }
            InputFormat::Toml => columns_from_toml(&args.input_path, args.samples)?,
            InputFormat::Yaml => columns_from_yaml(&args.input_path, args.nrows, args.samples)?,
            InputFormat::Cbor => columns_from_cbor(&args.input_path, args.nrows, args.samples)?,
            InputFormat::Sqlite | InputFormat::Xlsx | InputFormat::Ini => {
                unreachable!("handled above")
            }
        };
        std::iter::once((file_stem, profiles)).collect()
    };

    let rendered = match output_format {
        OutputFormat::Markdown => render_markdown(&file_name, &tables),
        OutputFormat::Json => render_json(&file_name, &format, &tables)?,
        OutputFormat::JsonSchema => render_json_schema(&file_name, &tables)?,
    };

    let table_count = tables.len();
    let col_count: usize = tables.values().map(Vec::len).sum();

    // '-' means "write to stdout" - and when it does, the status line goes to
    // stderr so stdout stays pure output a script or agent can pipe directly
    // (e.g. `sniff-rs data.csv - --output-format json | jq .`).
    if args.output_path.as_deref() == Some(Path::new("-")) {
        print!("{rendered}");
        eprintln!("{table_count} tables, {col_count} columns -> (stdout)");
    } else {
        let default_ext = match output_format {
            OutputFormat::Markdown => "dictionary.md",
            OutputFormat::Json => "dictionary.json",
            OutputFormat::JsonSchema => "dictionary.schema.json",
        };
        let output_path = args
            .output_path
            .clone()
            .unwrap_or_else(|| args.input_path.with_extension(default_ext));
        fs::write(&output_path, &rendered)
            .with_context(|| format!("failed to write {output_path:?}"))?;
        eprintln!(
            "{table_count} tables, {col_count} columns -> {}",
            output_path.display()
        );
    }

    Ok(())
}

// --- Unit tests for the heuristic engine ---
// The functions below are the part of this tool most likely to grow subtle
// bugs under a small direct test (see CLAUDE.md's Testing section) - unlike
// the readers, which are covered end-to-end by tests/integration.rs.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_zero_requires_a_second_digit() {
        assert!(has_leading_zero("02134"));
        assert!(has_leading_zero("00501"));
        assert!(!has_leading_zero("123"));
        assert!(!has_leading_zero("0")); // single char, nothing after the zero
        assert!(!has_leading_zero("0x")); // second byte isn't a digit
    }

    #[test]
    fn date_format_must_match_every_value() {
        assert_eq!(
            matching_date_format(&["2024-01-15", "2024-12-31"]),
            Some("%Y-%m-%d")
        );
        // one value doesn't match any candidate format -> no format wins
        assert_eq!(matching_date_format(&["2024-01-15", "not-a-date"]), None);
    }

    #[test]
    fn describe_kinds_reports_a_single_kind_plainly() {
        let mut counts = HashMap::new();
        counts.insert(JsonKind::Str, 3);
        assert_eq!(describe_kinds(&counts), "String");
    }

    #[test]
    fn describe_kinds_reports_mixed_kinds_with_sorted_counts() {
        let mut counts = HashMap::new();
        counts.insert(JsonKind::Str, 2);
        counts.insert(JsonKind::Bool, 1);
        assert_eq!(describe_kinds(&counts), "mixed(String: 2, bool: 1)");
    }

    #[test]
    fn suggest_ideal_type_flags_leading_zeros_already_lost_by_numeric_parse() {
        let (ideal, note) = suggest_ideal_type(&["02134", "90210"], "i64");
        assert_eq!(ideal, "String");
        assert!(note.contains("already lost"));
    }

    #[test]
    fn suggest_ideal_type_leaves_a_note_when_leading_zeros_are_not_yet_lost() {
        let (ideal, note) = suggest_ideal_type(&["02134", "90210"], "String");
        assert_eq!(ideal, "String");
        assert!(!note.contains("already lost"));
    }

    #[test]
    fn suggest_ideal_type_recognizes_bool_words() {
        let (ideal, _) = suggest_ideal_type(&["yes", "no", "Y"], "String");
        assert_eq!(ideal, "bool");
    }

    #[test]
    fn suggest_ideal_type_recognizes_currency_formatted_numbers() {
        let (ideal, note) = suggest_ideal_type(&["$1,250.50", "$340.00"], "String");
        assert_eq!(ideal, "f64");
        assert_eq!(note, "numeric strings");
    }

    #[test]
    fn suggest_ideal_type_detects_low_cardinality_as_a_category() {
        // Needs enough rows for the uniqueness ratio to clear the <5% bar -
        // 3 unique values repeated across 201 rows is ~1.5%, not the 3-in-6
        // majority a small example would give.
        let tiers = ["gold", "silver", "bronze"];
        let values: Vec<&str> = (0..201).map(|i| tiers[i % 3]).collect();
        let (ideal, note) = suggest_ideal_type(&values, "String");
        assert_eq!(ideal, "enum / category");
        assert!(note.contains("3 unique values"));
    }

    #[test]
    fn suggest_ideal_type_falls_back_to_string_for_high_cardinality_text() {
        let values = ["alpha", "bravo", "charlie", "delta", "echo"];
        let (ideal, note) = suggest_ideal_type(&values, "String");
        assert_eq!(ideal, "String");
        assert!(note.is_empty());
    }
}
