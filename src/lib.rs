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
    /// Skip N leading rows before the header (csv/tsv only) - for a
    /// title/instructions banner row some spreadsheet exports carry above
    /// the real header. If not given, a small run of leading rows is
    /// auto-skipped when it shows a strong structural signal of being a
    /// preamble rather than the header - see detect_preamble_rows.
    #[arg(long)]
    skip_rows: Option<usize>,
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
    // common in older exports and some spreadsheet defaults. chrono's `%y`
    // follows the standard strptime pivot (00-68 -> 2000-2068, 69-99 ->
    // 1969-1999), the same convention every other tool assumes; this is a
    // real, disclosed ambiguity for genuinely 100+-year-old dates, not
    // something this project can resolve any more precisely than the
    // format itself allows.
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

fn columns_from_csv(
    path: &Path,
    nrows: Option<usize>,
    delimiter: u8,
    skip_rows: usize,
) -> Result<Vec<ColumnInput>> {
    // Two passes, because a skipped preamble row often doesn't share the
    // real header's field count (a banner line with no commas at all,
    // above a 9-column header, is a real shape - see
    // detect_preamble_rows), which the strict reader below would otherwise
    // reject as ragged before ever getting to decide it's not a data row.
    // Pass one is flexible specifically to tolerate that while walking
    // past skip_rows rows to the header; pass two seeks a strict
    // (non-flexible) reader to resume exactly where pass one left off, so
    // a genuinely ragged *data* row is still the same hard, actionable
    // csv-crate error it always has been - flexible(true) for the whole
    // read would have silently swallowed that instead.
    let mut header_reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_path(path)
        .with_context(|| format!("failed to open {path:?}"))?;
    let mut rec = csv::StringRecord::new();
    let mut headers: Vec<String> = Vec::new();
    for i in 0..=skip_rows {
        if !header_reader.read_record(&mut rec)? {
            break;
        }
        if i == skip_rows {
            headers = rec.iter().map(|s| s.to_string()).collect();
        }
    }
    let resume_at = header_reader.position().clone();

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_path(path)
        .with_context(|| format!("failed to open {path:?}"))?;
    // seek() internally calls byte_headers(), which - if headers haven't
    // been read yet - reads a record from the *start* of the file (the
    // skipped preamble, not the header) purely to populate its own cache,
    // before the actual seek happens. Pre-seeding a dummy header
    // short-circuits that internal read (byte_headers only reads when
    // `state.headers` is still `None`), so seek() performs a pure position
    // jump with no wasted side-read.
    reader.set_headers(csv::StringRecord::new());
    reader.seek(resume_at)?;

    // Deliberately flexible(true) plus a manual length check here, rather
    // than leaning on the csv crate's own strict-mode ragged-row
    // detection: that detection anchors itself to whichever record it
    // happens to read *first* on a given Reader (tracked internally,
    // starting from None), and after a seek there's no way to seed it
    // with the header's field count directly - confirmed directly against
    // the csv crate's own source (`ReaderState::add_record`), not
    // assumed, after an earlier version of this function silently passed
    // through a real ragged header/data mismatch instead of erroring
    // because of exactly this gap. Checking every record's length against
    // `headers.len()` explicitly sidesteps that internal state machine
    // entirely and states the actual invariant this tool cares about -
    // every row matches the header - rather than "every row matches
    // whatever the first one happened to be."
    let mut raw: Vec<Vec<Option<String>>> = vec![Vec::new(); headers.len()];
    for (i, result) in reader.records().enumerate() {
        if nrows.is_some_and(|limit| i >= limit) {
            break;
        }
        let record = result?;
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

// A leading title/instructions row above the real header is a real,
// observed pattern in human-authored spreadsheets exported to CSV (found
// via real-world testing against the Ask A Manager salary survey and the
// HPI Pollock benchmark's own file_preamble.csv fixture, both independently
// showing the same shape). Detecting it only ever fires on a structural
// fill-pattern, never on cell content or column-name guessing, the same
// bar every other heuristic in this file holds to: a leading row counts as
// "preamble" only if it has at least two fields and at most one of them is
// non-empty (a real header virtually always names every column; a real
// single-column data row is the one thing this rules out on purpose, by
// requiring >= 2 fields), and the run of such rows must be immediately
// followed by a row where *every* field is non-empty (the strongest
// available signal that this is the real header, not just another sparse
// row). Capped at MAX_PREAMBLE_SCAN so a genuinely sparse dataset can never
// have an unbounded chunk silently skipped. Either condition failing to
// hold leaves skip_rows at 0 - the safe, old-behavior direction - rather
// than guessing.
const MAX_PREAMBLE_SCAN: usize = 5;

fn detect_preamble_rows(path: &Path, delimiter: u8) -> usize {
    let Ok(reader) = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_path(path)
    else {
        return 0;
    };
    let records: Vec<csv::StringRecord> = reader
        .into_records()
        .take(MAX_PREAMBLE_SCAN + 1)
        .filter_map(|r| r.ok())
        .collect();

    fn fill(record: &csv::StringRecord) -> (usize, usize) {
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
    if skip == 0 || skip >= records.len() {
        return 0;
    }
    let (non_empty, total) = fill(&records[skip]);
    if total >= 2 && non_empty == total {
        skip
    } else {
        0
    }
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

/// Avro's decimal logical type stores only the unscaled two's-complement
/// integer in the *value* - the scale that says where the decimal point
/// goes lives in the *schema*, not the value, so rendering a decimal
/// column correctly needs both together (see avro_value_to_json's
/// Option<&Schema> parameter). Delegates the actual big-integer decoding
/// to num_bigint (already a transitive dependency of apache-avro itself -
/// see the Cargo.toml comment) rather than hand-rolling two's-complement
/// arithmetic, the same "don't reimplement what a well-tested crate
/// already does for free" call this project makes elsewhere (chrono,
/// serde_json).
#[cfg(feature = "avro")]
fn avro_decimal_to_string(decimal: apache_avro::Decimal, scale: usize) -> String {
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

#[cfg(feature = "avro")]
fn avro_millis_to_string(millis: i64) -> JsonValue {
    chrono::DateTime::from_timestamp_millis(millis).map_or(JsonValue::Null, |dt| {
        JsonValue::String(dt.format("%Y-%m-%dT%H:%M:%S%.3f").to_string())
    })
}

#[cfg(feature = "avro")]
fn avro_micros_to_string(micros: i64) -> JsonValue {
    chrono::DateTime::from_timestamp_micros(micros).map_or(JsonValue::Null, |dt| {
        JsonValue::String(dt.format("%Y-%m-%dT%H:%M:%S%.6f").to_string())
    })
}

#[cfg(feature = "avro")]
fn avro_nanos_to_string(nanos: i64) -> JsonValue {
    let secs = nanos.div_euclid(1_000_000_000);
    let subsec_nanos = nanos.rem_euclid(1_000_000_000) as u32;
    chrono::DateTime::from_timestamp(secs, subsec_nanos).map_or(JsonValue::Null, |dt| {
        JsonValue::String(dt.format("%Y-%m-%dT%H:%M:%S%.9f").to_string())
    })
}

#[cfg(feature = "avro")]
fn avro_time_millis_to_string(millis: i32) -> JsonValue {
    let secs = (millis.div_euclid(1000)).rem_euclid(86_400) as u32;
    let nanos = millis.rem_euclid(1000) as u32 * 1_000_000;
    chrono::NaiveTime::from_num_seconds_from_midnight_opt(secs, nanos)
        .map_or(JsonValue::Null, |t| {
            JsonValue::String(t.format("%H:%M:%S%.3f").to_string())
        })
}

#[cfg(feature = "avro")]
fn avro_time_micros_to_string(micros: i64) -> JsonValue {
    let secs = (micros.div_euclid(1_000_000)).rem_euclid(86_400) as u32;
    let nanos = micros.rem_euclid(1_000_000) as u32 * 1000;
    chrono::NaiveTime::from_num_seconds_from_midnight_opt(secs, nanos)
        .map_or(JsonValue::Null, |t| {
            JsonValue::String(t.format("%H:%M:%S%.6f").to_string())
        })
}

/// `schema` co-recurses alongside `v` so logical types whose meaning isn't
/// recoverable from the value alone - decimal's scale is the one real case
/// here, see avro_decimal_to_string - can still be resolved correctly. A
/// schema/value shape mismatch (which shouldn't happen with a
/// spec-compliant file) degrades gracefully to `None` at that point rather
/// than failing the whole record - every other case here doesn't actually
/// need the schema at all, so recursion continues unaffected.
#[cfg(feature = "avro")]
fn avro_value_to_json(
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
        AvroValue::Union(idx, inner) => {
            let variant_schema = match schema {
                Some(Schema::Union(u)) => u.get_variant(*idx as usize).ok(),
                _ => None,
            };
            avro_value_to_json(inner, variant_schema)
        }
        AvroValue::Array(items) => {
            let item_schema = match schema {
                Some(Schema::Array(a)) => Some(a.items.as_ref()),
                _ => None,
            };
            JsonValue::Array(
                items
                    .iter()
                    .map(|i| avro_value_to_json(i, item_schema))
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
                    .map(|(k, v)| (k.clone(), avro_value_to_json(v, value_schema)))
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
                        (k.clone(), avro_value_to_json(v, field_schema))
                    })
                    .collect(),
            )
        }
        AvroValue::Date(days) => chrono::DateTime::UNIX_EPOCH
            .checked_add_signed(chrono::Duration::days(i64::from(*days)))
            .map_or(JsonValue::Null, |d| {
                JsonValue::String(d.format("%Y-%m-%d").to_string())
            }),
        AvroValue::Uuid(u) => JsonValue::String(u.to_string()),
        AvroValue::TimestampMillis(ms) | AvroValue::LocalTimestampMillis(ms) => {
            avro_millis_to_string(*ms)
        }
        AvroValue::TimestampMicros(us) | AvroValue::LocalTimestampMicros(us) => {
            avro_micros_to_string(*us)
        }
        AvroValue::TimestampNanos(ns) | AvroValue::LocalTimestampNanos(ns) => {
            avro_nanos_to_string(*ns)
        }
        AvroValue::TimeMillis(ms) => avro_time_millis_to_string(*ms),
        AvroValue::TimeMicros(us) => avro_time_micros_to_string(*us),
        AvroValue::Decimal(d) => match schema {
            Some(Schema::Decimal(ds)) => {
                JsonValue::String(avro_decimal_to_string(d.clone(), ds.scale))
            }
            // No schema/scale available - shouldn't happen for a
            // spec-compliant file (the writer schema always carries the
            // scale), so fall back to a visible placeholder rather than
            // guessing a scale and silently showing the wrong number.
            _ => JsonValue::String(format!("{d:?}")),
        },
        AvroValue::BigDecimal(bg) => JsonValue::String(bg.to_string()),
        other => JsonValue::String(format!("{other:?}")), // best-effort for Duration, the one remaining compound logical type
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
    // Cloned before `reader` is consumed by `.enumerate()` below - needed so
    // avro_value_to_json can resolve logical-type metadata (a decimal
    // field's scale, the one case that isn't recoverable from the value
    // alone) that only the schema carries.
    let schema = reader.writer_schema().clone();

    let mut records: Vec<serde_json::Map<String, JsonValue>> = Vec::new();
    for (i, value_result) in reader.enumerate() {
        if nrows.is_some_and(|limit| i >= limit) {
            break;
        }
        let value =
            value_result.with_context(|| format!("failed decoding a record from {path:?}"))?;
        match avro_value_to_json(&value, Some(&schema)) {
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

// Every other nested format here (JSON, TOML, YAML, MessagePack, CBOR) is
// protected against a stack-overflow crash on a deeply-nested adversarial
// document by an explicit recursion/depth limit in its own parsing crate
// (serde_json's built-in limit, toml_edit's `#![recursion_limit = "256"]`,
// serde_norway's, rmpv's, ciborium's - each verified directly, not
// assumed, the same discipline as everywhere else in this file). `xmltree`
// has no such guard at all - `Element::parse`'s own tree-building recurses
// once per nesting level with nothing capping it, so a document nested
// tens of thousands of levels deep reliably aborts the whole process with
// a real stack overflow, not a catchable error (confirmed empirically: a
// 50,000-level `<a><a><a>...` document crashes the compiled binary).
// Since the crash happens *inside* `Element::parse` itself, a depth check
// added after that call would be too late - `xml_nesting_too_deep` scans
// the raw text first and refuses to hand xmltree anything more deeply
// nested than `MAX_XML_DEPTH`, the same "clean, actionable error instead
// of a crash" contract every other format already has.
#[cfg(feature = "xml")]
const MAX_XML_DEPTH: usize = 512;

/// A conservative pre-parse scan for excessive tag-nesting depth - not a
/// full XML tokenizer, just enough state to walk past comments/CDATA/
/// processing instructions/DOCTYPE (whose content must never affect the
/// depth count) and tell an opening tag from a closing or self-closing
/// one. Deliberately errs toward over-counting depth rather than under-
/// counting it: the one known gap is a literal, unescaped '>' inside an
/// attribute value (legal but very rare in practice - virtually every
/// real XML writer escapes it as `&gt;` even though the spec doesn't
/// strictly require it outside the literal sequence "]]>"), which could
/// end a tag scan early. In that specific, narrow case this scan can
/// still miss a genuinely deep document - a false negative that only
/// returns this project to its pre-fix behavior for that one exotic
/// shape, never worse than before.
#[cfg(feature = "xml")]
fn xml_nesting_too_deep(content: &str) -> bool {
    let bytes = content.as_bytes();
    let mut i = 0;
    let mut depth: usize = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &content[i..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(bytes.len(), |p| i + p + 3);
        } else if rest.starts_with("<![CDATA[") {
            i = rest.find("]]>").map_or(bytes.len(), |p| i + p + 3);
        } else if rest.starts_with("<?") {
            i = rest.find("?>").map_or(bytes.len(), |p| i + p + 2);
        } else if rest.starts_with("<!") {
            // DOCTYPE or another markup declaration - skip to its closing '>'.
            i = rest.find('>').map_or(bytes.len(), |p| i + p + 1);
        } else if rest.as_bytes().get(1) == Some(&b'/') {
            depth = depth.saturating_sub(1);
            i = rest.find('>').map_or(bytes.len(), |p| i + p + 1);
        } else {
            let end = rest.find('>').map_or(bytes.len(), |p| i + p + 1);
            let self_closing = end >= 2 && bytes.get(end - 2) == Some(&b'/');
            if !self_closing {
                depth += 1;
                if depth > MAX_XML_DEPTH {
                    return true;
                }
            }
            i = end;
        }
    }
    false
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
    use xmltree::XMLNode;

    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    if xml_nesting_too_deep(&content) {
        bail!(
            "{path:?} has more than {MAX_XML_DEPTH} levels of nested XML elements - refusing to parse it (this would otherwise risk a stack overflow, since xmltree has no recursion limit of its own)"
        );
    }
    let root = xmltree::Element::parse(content.as_bytes())
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

    // xml_nesting_too_deep is the pre-parse guard added after discovering
    // xmltree has no recursion limit of its own (unlike every other nested
    // format's crate here) - a genuinely deep document reliably stack-
    // overflowed the compiled binary before this existed, confirmed
    // empirically at 50,000 levels of nesting, not assumed. These test the
    // scanner directly; deeply_nested_xml_fails_cleanly_instead_of_a_stack_overflow
    // in tests/integration.rs proves the fix holds through the full binary.

    #[cfg(feature = "xml")]
    #[test]
    fn xml_nesting_too_deep_rejects_a_document_past_the_limit() {
        let deep = format!("<root>{}1{}</root>", "<a>".repeat(600), "</a>".repeat(600));
        assert!(xml_nesting_too_deep(&deep));
    }

    #[cfg(feature = "xml")]
    #[test]
    fn xml_nesting_too_deep_accepts_a_document_comfortably_under_the_limit() {
        let shallow = format!("<root>{}1{}</root>", "<a>".repeat(50), "</a>".repeat(50));
        assert!(!xml_nesting_too_deep(&shallow));
    }

    #[cfg(feature = "xml")]
    #[test]
    fn xml_nesting_too_deep_ignores_angle_brackets_inside_comments_and_cdata() {
        let noisy = format!(
            "<root><!-- {} --><item><![CDATA[{}]]></item></root>",
            "<a>".repeat(600),
            "<a>".repeat(600)
        );
        assert!(!xml_nesting_too_deep(&noisy));
    }

    #[cfg(feature = "xml")]
    #[test]
    fn xml_nesting_too_deep_does_not_count_self_closing_tags_as_adding_depth() {
        let wide = format!("<root>{}</root>", "<item/>".repeat(5000));
        assert!(!xml_nesting_too_deep(&wide));
    }

    // avro_decimal_to_string is what stands between Avro's decimal logical
    // type and a Rust Debug dump like "Decimal(Decimal { value: 12345, len:
    // 2 })" - found via exactly this kind of direct testing while checking
    // whether cloud-platform-produced Avro files (Kinesis/Event Hubs/
    // Pub-Sub, which lean on decimal for money/precise numeric fields)
    // actually render correctly. Every case here was hand-verified against
    // the digit-shifting logic before being relied on (see the comment
    // above avro_decimal_to_string's definition for the worked examples).

    #[cfg(feature = "avro")]
    fn avro_decimal_from_i64(unscaled: i64) -> apache_avro::Decimal {
        let big = num_bigint::BigInt::from(unscaled);
        apache_avro::Decimal::from(big.to_signed_bytes_be())
    }

    #[cfg(feature = "avro")]
    #[test]
    fn avro_decimal_to_string_places_the_decimal_point_correctly() {
        assert_eq!(
            avro_decimal_to_string(avro_decimal_from_i64(12345), 2),
            "123.45"
        );
        assert_eq!(
            avro_decimal_to_string(avro_decimal_from_i64(-100), 2),
            "-1.00"
        );
        assert_eq!(avro_decimal_to_string(avro_decimal_from_i64(100), 0), "100");
    }

    #[cfg(feature = "avro")]
    #[test]
    fn avro_decimal_to_string_zero_pads_a_magnitude_smaller_than_the_scale() {
        // unscaled=5, scale=2 must become "0.05", not "0.5" or ".05" - the
        // exact off-by-one a naive "just insert a dot N digits from the
        // right" implementation would get wrong on a short digit string.
        assert_eq!(avro_decimal_to_string(avro_decimal_from_i64(5), 2), "0.05");
        assert_eq!(
            avro_decimal_to_string(avro_decimal_from_i64(-5), 2),
            "-0.05"
        );
        assert_eq!(avro_decimal_to_string(avro_decimal_from_i64(0), 2), "0.00");
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
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
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

    #[test]
    fn detect_preamble_rows_does_not_error_on_an_empty_file() {
        assert_eq!(preamble_rows(""), 0);
    }

    #[test]
    fn columns_from_csv_skip_rows_matches_manual_preamble_removal() {
        let with_preamble = "Banner,,,,\nid,name,age\n1,Alice,30\n2,Bob,40\n";
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp, with_preamble.as_bytes()).unwrap();

        let cols = columns_from_csv(tmp.path(), None, b',', 1).unwrap();
        let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "age"]);
        assert_eq!(cols[0].raw_values, vec!["1", "2"]);
    }

    #[test]
    fn columns_from_csv_skip_rows_past_everything_yields_an_empty_table_not_an_error() {
        let tiny = "id,name\n1,Alice\n";
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
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
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
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
}
