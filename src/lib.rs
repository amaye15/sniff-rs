use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use clap::Parser;
use serde_json::Value as JsonValue;
use serde_json::json;

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
/// through the same path. Format is inferred from the file extension
/// unless --format is given. A .gz or .zst extension is transparently decompressed
/// first, so e.g. data.csv.gz reads exactly like data.csv (gzip always
/// available; zstd needs --features zstd). Every optional format needs its
/// own --features flag (see the Supported formats table in CLAUDE.md), or
/// use --features full for everything.
#[derive(Parser)]
#[command(name = "sniff-rs")]
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
    #[arg(long, default_value_t = 3)]
    samples: usize,
    /// Only read the first N rows/records (for large files)
    #[arg(long)]
    nrows: Option<usize>,
    /// Override format detection: csv, tsv, json (covers json/jsonl/ndjson), parquet, arrow, avro, xlsx, sqlite, msgpack, toml, yaml, cbor, ini, xml, fixed-width, npy, npz, common-log, combined-log, syslog, syslog5424, dbase, stata, or sas7bdat
    #[arg(long)]
    format: Option<String>,
    /// Override the field delimiter for csv/tsv (single character)
    #[arg(long)]
    delimiter: Option<char>,
    /// Column widths for --format fixed-width, as comma-separated character
    /// counts (e.g. --widths 10,5,20) - there's no delimiter to split on, so
    /// this format only runs when widths are given explicitly
    #[arg(long, value_delimiter = ',')]
    widths: Option<Vec<usize>>,
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
];

/// Candidate time-of-day formats, tried the same way DATE_FORMATS is: first
/// one every value matches wins. %.f tolerates a value with no fractional
/// seconds (verified the same way as the DATE_FORMATS entries above).
const TIME_FORMATS: &[&str] = &["%H:%M:%S%.f", "%H:%M", "%I:%M:%S %p", "%I:%M %p"];

fn matching_time_format(values: &[&str]) -> Option<&'static str> {
    TIME_FORMATS.iter().copied().find(|fmt| {
        values
            .iter()
            .all(|v| NaiveTime::parse_from_str(v, fmt).is_ok())
    })
}

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

/// Common placeholder tokens a human/tool writes in place of a real value in
/// a format with no native null (CSV/TSV/fixed-width all encode every field
/// as plain text - the "missing" case can only be an actual empty field, or
/// one of these well-established conventions, the same ones pandas'
/// `read_csv` treats as missing by default). Matched case-insensitively
/// against the trimmed field, so " NA " and "na" both count.
const MISSING_SENTINELS: &[&str] = &[
    "na", "n/a", "null", "none", "nan", "nil", "-", "--", "?", "unknown", "missing", "#n/a",
];

fn is_missing_sentinel(s: &str) -> bool {
    MISSING_SENTINELS.contains(&s.trim().to_ascii_lowercase().as_str())
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

fn suggest_ideal_type(values: &[&str], current: &str) -> (String, String) {
    // Precise, unambiguous grammars are checked first - each one fully
    // explains the whole string, so there's no risk of a cruder check
    // (leading-zero, in particular) firing on a substring pattern instead.
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
    // ISBN/EAN/UPC are checked ahead of the broader-range credit card check
    // below: they only match an exact 10, 12, or 13-digit length, so the
    // more narrowly-scoped match should win a tie (a 13-digit number can in
    // principle satisfy both a card issuer's Luhn check and EAN-13's own
    // mod-10 check by coincidence - genuinely undecidable from the digits
    // alone without domain context, the same kind of irreducible ambiguity
    // as a dotted-quad value being valid as both IPv4 and a version string).
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
    if values.iter().all(|v| is_credit_card_number(v)) {
        return (
            "Credit Card Number".to_string(),
            "matches card number format (Luhn checksum valid)".to_string(),
        );
    }
    if values.iter().all(|v| is_uuid(v)) {
        return ("UUID".to_string(), "matches UUID format".to_string());
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

    if values.iter().all(|v| is_embedded_json(v)) {
        return (
            "String".to_string(),
            "cell holds embedded JSON (object/array) - consider parsing it separately".to_string(),
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
            "values are yes/no/true/false".to_string(),
        );
    }

    let normalized: Vec<(String, bool)> = values.iter().map(|v| normalize_numeric_str(v)).collect();
    let any_percent = normalized.iter().any(|(_, pct)| *pct);
    let cleaned_refs: Vec<&str> = normalized.iter().map(|(s, _)| s.as_str()).collect();

    if cleaned_refs.iter().all(|v| v.parse::<i64>().is_ok()) {
        return ("i64".to_string(), numeric_note(current, "i64", any_percent));
    }
    if cleaned_refs.iter().all(|v| v.parse::<f64>().is_ok()) {
        return ("f64".to_string(), numeric_note(current, "f64", any_percent));
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
            let trimmed = field.trim();
            let value = if trimmed.is_empty() || is_missing_sentinel(trimmed) {
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
    let header_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("{path:?} is empty"))?;
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
fn weblog_regex(combined: bool) -> regex::Regex {
    let pattern = if combined {
        r#"^(\S+) (\S+) (\S+) \[([^\]]+)\] "([^"]*)" (\d{3}|-) (\d+|-) "([^"]*)" "([^"]*)"$"#
    } else {
        r#"^(\S+) (\S+) (\S+) \[([^\]]+)\] "([^"]*)" (\d{3}|-) (\d+|-)$"#
    };
    regex::Regex::new(pattern).expect("hardcoded weblog regex is always valid")
}

#[cfg(feature = "weblog")]
fn weblog_dash_to_none(s: &str) -> Option<String> {
    if s == "-" { None } else { Some(s.to_string()) }
}

#[cfg(feature = "weblog")]
fn columns_from_weblog(
    path: &Path,
    nrows: Option<usize>,
    combined: bool,
) -> Result<Vec<ColumnInput>> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    let re = weblog_regex(combined);
    let request_re = regex::Regex::new(r"^(\S+) (\S+) (\S+)$").expect("hardcoded regex is valid");

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
        let caps = re.captures(line).ok_or_else(|| {
            anyhow::anyhow!(
                "line {} doesn't match {format_name} Format: {line:?}",
                line_no + 1
            )
        })?;

        let (method, req_path, protocol) = match request_re.captures(&caps[5]) {
            Some(c) => (
                Some(c[1].to_string()),
                Some(c[2].to_string()),
                Some(c[3].to_string()),
            ),
            None => (None, None, None),
        };
        let mut values = vec![
            weblog_dash_to_none(&caps[1]),
            weblog_dash_to_none(&caps[2]),
            weblog_dash_to_none(&caps[3]),
            Some(caps[4].to_string()),
            method,
            req_path,
            protocol,
            weblog_dash_to_none(&caps[6]),
            weblog_dash_to_none(&caps[7]),
        ];
        if combined {
            values.push(weblog_dash_to_none(&caps[8]));
            values.push(weblog_dash_to_none(&caps[9]));
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
fn syslog_regex(rfc5424: bool) -> regex::Regex {
    let pattern = if rfc5424 {
        r#"^<(\d{1,3})>(\d+) (\S+) (\S+) (\S+) (\S+) (\S+) (-|\[[^\]]*\]) ?(.*)$"#
    } else {
        r"^<(\d{1,3})>([A-Za-z]{3}\s+\d{1,2}\s\d{2}:\d{2}:\d{2}) (\S+) ([^:\[\s]+)(?:\[(\d+)\])?: ?(.*)$"
    };
    regex::Regex::new(pattern).expect("hardcoded syslog regex is always valid")
}

#[cfg(feature = "syslog")]
fn syslog_dash_to_none(s: &str) -> Option<String> {
    if s == "-" { None } else { Some(s.to_string()) }
}

#[cfg(feature = "syslog")]
fn columns_from_syslog(
    path: &Path,
    nrows: Option<usize>,
    rfc5424: bool,
) -> Result<Vec<ColumnInput>> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    let re = syslog_regex(rfc5424);

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
        let caps = re.captures(line).ok_or_else(|| {
            anyhow::anyhow!(
                "line {} doesn't match syslog {format_name}: {line:?}",
                line_no + 1
            )
        })?;
        let pri: u32 = caps[1]
            .parse()
            .with_context(|| format!("line {}: PRI '{}' isn't a number", line_no + 1, &caps[1]))?;

        let values: Vec<Option<String>> = if rfc5424 {
            vec![
                Some(syslog_facility_name(pri)),
                Some(syslog_severity_name(pri)),
                Some(caps[2].to_string()),
                Some(caps[3].to_string()),
                syslog_dash_to_none(&caps[4]),
                syslog_dash_to_none(&caps[5]),
                syslog_dash_to_none(&caps[6]),
                syslog_dash_to_none(&caps[7]),
                syslog_dash_to_none(&caps[8]),
                Some(caps[9].to_string()),
            ]
        } else {
            vec![
                Some(syslog_facility_name(pri)),
                Some(syslog_severity_name(pri)),
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
// by the crate itself before this code ever sees it - the same convention
// dBase and every tool built on it already treats as "logically absent",
// not something this tool is choosing to hide. Column order comes from
// Reader::fields() (the file's own field table) rather than from Record's
// internal HashMap, whose iteration order isn't guaranteed to be stable.

#[cfg(feature = "dbase")]
fn dbase_field_type_label(t: dbase::FieldType) -> &'static str {
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

#[cfg(feature = "dbase")]
fn dbase_value_to_string(v: &dbase::FieldValue) -> Option<String> {
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

#[cfg(feature = "dbase")]
fn columns_from_dbase(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    let mut reader =
        dbase::Reader::from_path(path).with_context(|| format!("failed to open {path:?}"))?;
    let fields: Vec<(String, &'static str)> = reader
        .fields()
        .iter()
        .map(|f| (f.name().to_string(), dbase_field_type_label(f.field_type())))
        .collect();

    let mut records = reader
        .read()
        .with_context(|| format!("failed reading records from {path:?}"))?;
    if let Some(n) = nrows {
        records.truncate(n);
    }
    let total = records.len();

    let mut columns = Vec::new();
    for (name, current_type) in fields {
        let raw_values: Vec<String> = records
            .iter()
            .filter_map(|r| r.get(&name).and_then(dbase_value_to_string))
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
// Known limitations.

#[cfg(feature = "stata")]
fn stata_value_to_string(v: &dta::stata::dta::value::Value) -> Option<String> {
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

#[cfg(feature = "stata")]
fn stata_type_label(t: dta::stata::dta::variable_type::VariableType) -> &'static str {
    use dta::stata::dta::variable_type::VariableType;
    match t {
        VariableType::Byte | VariableType::Int | VariableType::Long => "i64",
        VariableType::Float | VariableType::Double => "f64",
        VariableType::FixedString(_) | VariableType::LongString => "String",
    }
}

#[cfg(feature = "stata")]
fn columns_from_stata(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    use dta::stata::dta::dta_reader::DtaReader;

    let mut characteristic_reader = DtaReader::new()
        .from_path(path)
        .with_context(|| format!("failed to open {path:?}"))?
        .read_header()
        .with_context(|| format!("failed to read the header of {path:?}"))?
        .read_schema()
        .with_context(|| format!("failed to read the schema of {path:?}"))?;
    characteristic_reader
        .skip_to_end()
        .with_context(|| format!("failed to skip characteristics in {path:?}"))?;

    let mut record_reader = characteristic_reader
        .into_record_reader()
        .with_context(|| format!("failed to start reading records from {path:?}"))?;
    let variables: Vec<(String, &'static str)> = record_reader
        .schema()
        .variables()
        .iter()
        .map(|v| (v.name().to_string(), stata_type_label(v.variable_type())))
        .collect();

    let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); variables.len()];
    let mut total = 0usize;
    while let Some(record) = record_reader
        .read_record()
        .with_context(|| format!("failed reading a record from {path:?}"))?
    {
        if nrows.is_some_and(|limit| total >= limit) {
            break;
        }
        for (col_idx, value) in record.values().iter().enumerate() {
            raw[col_idx].push(stata_value_to_string(value));
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
        CellValue::Date(d) => chrono::DateTime::UNIX_EPOCH
            .checked_add_signed(chrono::Duration::days(i64::from(d.unix_days())))
            .map(|dt| dt.format("%Y-%m-%d").to_string()),
        CellValue::DateTime(dt) => chrono::DateTime::UNIX_EPOCH
            .checked_add_signed(chrono::Duration::seconds(dt.unix_seconds()))
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
        CellValue::Time(t) => chrono::NaiveTime::from_num_seconds_from_midnight_opt(
            u32::try_from(t.seconds_since_midnight).unwrap_or(0),
            0,
        )
        .map(|nt| nt.format("%H:%M:%S").to_string()),
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
        .map_err(|e| anyhow::anyhow!("{e}"))
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
        .map_err(|e| anyhow::anyhow!("{e}"))
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

// --- XML reader (opt-in via --features xml) ---
// XML's mixed content model (an element can carry attributes, text, and
// child elements all at once) doesn't map onto a single generic Value enum
// the way TOML/YAML/CBOR/MessagePack do, so this bridges by hand instead of
// via a ready-made dynamic type: attributes become `@name` keys, text
// content becomes a `#text` key (or, for a leaf element with only text and
// no attributes, the bare string - so `<name>Alice</name>` becomes "Alice"
// rather than {"#text": "Alice"}), and repeated same-name child elements
// pool into an array, same convention as everywhere else in this file.

#[cfg(feature = "xml")]
fn xml_element_to_json(el: &xmltree::Element) -> JsonValue {
    use xmltree::XMLNode;

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
                    .push(xml_element_to_json(child));
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

/// If the root element's children all share one tag name (the common
/// `<root><item>...</item><item>...</item></root>` shape), each becomes a
/// record - mirroring the JSON reader's `[...]` array-of-objects mode.
/// Otherwise the whole document is one record, the same choice TOML and an
/// INI section make for their own single-document shapes.
#[cfg(feature = "xml")]
fn columns_from_xml(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    use std::fs::File;
    use std::io::BufReader;
    use xmltree::XMLNode;

    let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let root = xmltree::Element::parse(BufReader::new(file))
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("failed to parse {path:?} as XML"))?;

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
// byte per npyz's own DType/TypeStr description, decoded per TypeChar
// (int/uint/float by width and endianness, fixed-width byte/unicode
// strings trimmed of their right-zero-padding), with anything not
// representable as a simple value (a fixed-size sub-array field, `f16`,
// pickled `object` dtype) falling back to a hex dump rather than fabricating
// a value or failing the whole file. A plain (non-structured) array has no
// field names at all - numpy doesn't carry them - so it's treated like a
// headerless CSV: a 1D array is one column, a 2D array gets positional
// `col_0..col_N` columns; anything higher-dimensional doesn't have an
// honest 2D tabular reading, so it's a clear error rather than a guess.
// .npz is just a zip of named .npy arrays, so - like SQLite's tables and
// Excel's sheets - each array becomes its own table.

#[cfg(feature = "npy")]
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

#[cfg(feature = "npy")]
fn npy_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(feature = "npy")]
fn npy_scalar_to_string(ty: &npyz::TypeStr, bytes: &[u8]) -> String {
    use npyz::{Endianness, TypeChar};

    let big_endian = ty.endianness() == Endianness::Big;
    match ty.type_char() {
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
        // RawData, and any future TypeChar variant: hex is always safe.
        _ => npy_hex(bytes),
    }
}

#[cfg(feature = "npy")]
fn npy_value_to_string(dtype: &npyz::DType, bytes: &[u8]) -> String {
    match dtype {
        npyz::DType::Plain(ty) => npy_scalar_to_string(ty, bytes),
        npyz::DType::Array(n, inner) => {
            let Some(elem_size) = inner.num_bytes() else {
                return npy_hex(bytes);
            };
            (0..*n as usize)
                .filter_map(|i| bytes.get(i * elem_size..(i + 1) * elem_size))
                .map(|chunk| npy_value_to_string(inner, chunk))
                .collect::<Vec<_>>()
                .join(";")
        }
        npyz::DType::Record(_) => npy_hex(bytes), // a field nested inside a field - rare
    }
}

/// The declared numpy dtype, mapped to this tool's type labels - this is
/// `current_type`, i.e. what the format *says* it is (mirrors
/// `arrow_type_label` for Parquet/Arrow IPC). `profile_column` still
/// independently re-derives `ideal_type` from the stringified values
/// regardless of this label, same as every other format.
#[cfg(feature = "npy")]
fn npy_type_label(dtype: &npyz::DType) -> String {
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
        npyz::DType::Array(_, inner) => format!("Vec<{}>", npy_type_label(inner)),
        npyz::DType::Record(_) => "Struct".to_string(),
    }
}

/// Reads one already-opened `.npy` stream (a standalone file, or one array
/// inside a `.npz` archive - the two share this same core). A structured
/// dtype gives one column per field; a plain dtype gets positional
/// `col_0..col_N` columns for a 2D array, or a single `value` column for 1D.
#[cfg(feature = "npy")]
fn columns_from_npy_reader<R: std::io::Read>(
    npy: npyz::NpyFile<R>,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    let header = npy.header().clone();
    let dtype = header.dtype();
    if dtype.uses_pickled_array() {
        bail!(
            "this array uses numpy's pickled 'object' dtype, which isn't a fixed byte layout \
             this tool can read - re-save it with a concrete dtype"
        );
    }
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
        reader
            .read_exact(&mut buf)
            .with_context(|| format!("failed reading the array body ({total_elems} elements)"))?;
        for row in 0..rows_to_read {
            for (col_idx, column) in columns.iter_mut().enumerate() {
                let flat_index = match order {
                    npyz::Order::C => row * n_cols + col_idx,
                    npyz::Order::Fortran => col_idx * n_rows + row,
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

#[cfg(feature = "npy")]
fn columns_from_npy(
    path: &Path,
    nrows: Option<usize>,
    n_samples: usize,
) -> Result<Vec<ColumnProfile>> {
    use std::fs::File;
    use std::io::BufReader;

    let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let npy = npyz::NpyFile::new(BufReader::new(file))
        .with_context(|| format!("failed to parse {path:?} as a .npy file"))?;
    columns_from_npy_reader(npy, nrows, n_samples)
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
    let mut archive = npyz::npz::NpzArchive::open(path)
        .with_context(|| format!("failed to open {path:?} as a .npz archive"))?;
    let names: Vec<String> = archive.array_names().map(str::to_string).collect();
    if names.is_empty() {
        bail!("no arrays found in {path:?}");
    }

    let mut out = Vec::new();
    for name in names {
        let npy = archive
            .by_name(&name)
            .with_context(|| format!("failed reading array '{name}' from {path:?}"))?
            .ok_or_else(|| anyhow::anyhow!("array '{name}' disappeared while reading {path:?}"))?;
        let profiles = columns_from_npy_reader(npy, nrows, n_samples)
            .with_context(|| format!("failed reading array '{name}' from {path:?}"))?;
        out.push((name, profiles));
    }
    Ok(out)
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
        "xml" => Ok(InputFormat::Xml),
        "npy" => Ok(InputFormat::Npy),
        "npz" => Ok(InputFormat::Npz),
        "dbf" => Ok(InputFormat::Dbase),
        "dta" => Ok(InputFormat::Stata),
        "sas7bdat" => Ok(InputFormat::Sas7bdat),
        // No extension convention reliably means fixed-width or a log
        // format (.txt/.log/no extension are all ambiguous), so all of
        // these are --format-only, never inferred.
        other => bail!(
            "can't infer format from extension '.{other}' - pass --format csv|tsv|json|parquet|arrow|avro|xlsx|sqlite|msgpack|toml|yaml|cbor|ini|xml|fixed-width|npy|npz|common-log|combined-log|syslog|syslog5424|dbase|stata|sas7bdat explicitly"
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
        "ISBN-10" | "ISBN-13" | "EAN-13 / UPC-A" | "SemVer" => Some(("string", None)),
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

// --- Transparent gzip/zstd decompression ---
// Not a format of its own - a preprocessing step in front of every reader
// above. Every reader just opens a plain file path, so materializing
// compressed input to a real temporary file (rather than trying to hand
// each reader a generic Read stream) means compressed input needs zero
// per-format changes, including formats that need actual random file
// access rather than a stream (Parquet, SQLite, Excel). gzip (via flate2,
// pure Rust, no C toolchain) is always available; zstd needs
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
fn decompress_zstd(reader: std::fs::File, out: &mut std::fs::File, path: &Path) -> Result<()> {
    let mut decoder = zstd::stream::read::Decoder::new(reader)
        .with_context(|| format!("failed to open {path:?} as zstd"))?;
    std::io::copy(&mut decoder, out).with_context(|| format!("failed to decompress {path:?}"))?;
    Ok(())
}

#[cfg(not(feature = "zstd"))]
fn decompress_zstd(_reader: std::fs::File, _out: &mut std::fs::File, path: &Path) -> Result<()> {
    bail!(
        "zstd support isn't compiled in - rebuild with `cargo build --release --features zstd` (or --features full) to read {path:?}"
    )
}

/// If `path` ends in `.gz`/`.gzip` or `.zst`/`.zstd`, decompresses it into a
/// real temporary file and returns (the path to actually read bytes from,
/// the compression-stripped logical path used for format detection and
/// default output naming, a guard that deletes the temp file on drop).
/// Non-compressed input passes through unchanged with no guard.
fn decompress_if_needed(
    path: &Path,
) -> Result<(PathBuf, PathBuf, Option<tempfile::NamedTempFile>)> {
    use std::fs::File;

    let Some(compression) = compression_from_extension(path) else {
        return Ok((path.to_path_buf(), path.to_path_buf(), None));
    };

    let input = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let mut tmp = tempfile::NamedTempFile::new()
        .context("failed to create a temporary file for decompression")?;
    match compression {
        Compression::Gzip => {
            let mut decoder = flate2::read::GzDecoder::new(input);
            std::io::copy(&mut decoder, tmp.as_file_mut())
                .with_context(|| format!("failed to decompress {path:?}"))?;
        }
        Compression::Zstd => decompress_zstd(input, tmp.as_file_mut(), path)?,
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
    let args = Args::parse();

    let output_format = OutputFormat::parse(&args.output_format)?;

    // data.csv.gz reads exactly like data.csv from here on: read_path points
    // at the real (decompressed) bytes every reader below opens, logical_path
    // is the compression-stripped name used for format detection and default
    // output naming, and _decompressed_tmp just needs to outlive the reads.
    let (read_path, logical_path, _decompressed_tmp) = decompress_if_needed(&args.input_path)?;

    let format = detect_format(&logical_path, &args.format)?;
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
                columns_from_csv(&read_path, args.nrows, args.delimiter.unwrap_or(',') as u8)?
                    .into_iter()
                    .map(|c| profile_column(c, args.samples))
                    .collect()
            }
            InputFormat::Tsv => {
                columns_from_csv(&read_path, args.nrows, args.delimiter.unwrap_or('\t') as u8)?
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
                    anyhow::anyhow!(
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
}
