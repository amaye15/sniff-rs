//! Dumps the WHATWG East Asian decoding tables by decoding every legal
//! two-byte (and, for GB18030, four-byte) sequence with `encoding_rs`.
//!
//! Table file layout: `rows * cols` little-endian u16 entries (0 = no
//! character, 0xFFFF = see the extras), then zero or more 12-byte extras
//! `(entry index u32, first code point u32, second code point u32 or 0)`
//! sorted by index. The geometry lives in `src/lib.rs`'s `cjk_support`.

use encoding_rs::{BIG5, EUC_JP, EUC_KR, GB18030, SHIFT_JIS};
use std::fs;
use std::path::PathBuf;

/// What one byte sequence decodes to: one or two characters, or nothing.
fn decode(enc: &'static encoding_rs::Encoding, bytes: &[u8]) -> Option<Vec<u32>> {
    let (text, errors) = enc.decode_without_bom_handling(bytes);
    if errors {
        return None;
    }
    let cps: Vec<u32> = text.chars().map(|c| c as u32).collect();
    (!cps.is_empty() && cps.len() <= 2).then_some(cps)
}

struct Table {
    entries: Vec<u16>,
    extras: Vec<(u32, u32, u32)>,
}

impl Table {
    fn new() -> Self {
        Table {
            entries: Vec::new(),
            extras: Vec::new(),
        }
    }
    fn push(&mut self, cps: Option<Vec<u32>>) {
        let idx = self.entries.len() as u32;
        self.entries.push(match cps.as_deref() {
            None => 0,
            Some([cp]) if *cp < 0xFFFF => *cp as u16,
            Some([a]) => {
                self.extras.push((idx, *a, 0));
                0xFFFF
            }
            Some([a, b]) => {
                self.extras.push((idx, *a, *b));
                0xFFFF
            }
            Some(_) => unreachable!(),
        });
    }
    fn write(&self, dir: &PathBuf, name: &str) {
        let mut out = Vec::new();
        for e in &self.entries {
            out.extend_from_slice(&e.to_le_bytes());
        }
        for (i, a, b) in &self.extras {
            out.extend_from_slice(&i.to_le_bytes());
            out.extend_from_slice(&a.to_le_bytes());
            out.extend_from_slice(&b.to_le_bytes());
        }
        fs::write(dir.join(name), &out).unwrap();
        println!(
            "{name}: {} entries, {} extras, {} bytes",
            self.entries.len(),
            self.extras.len(),
            out.len()
        );
    }
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: gen_cjk_tables <out dir>"));
    fs::create_dir_all(&dir).unwrap();

    // Shift_JIS: lead 0x81..=0x9F then 0xE0..=0xFC (60 rows), trail 0x40..=0xFC.
    let mut t = Table::new();
    for lead in (0x81u8..=0x9F).chain(0xE0..=0xFC) {
        for trail in 0x40u8..=0xFC {
            t.push(decode(SHIFT_JIS, &[lead, trail]));
        }
    }
    t.write(&dir, "sjis.bin");

    // EUC-JP: JIS X 0208 (two bytes) and JIS X 0212 (0x8F + two bytes).
    let mut t = Table::new();
    for lead in 0xA1u8..=0xFE {
        for trail in 0xA1u8..=0xFE {
            t.push(decode(EUC_JP, &[lead, trail]));
        }
    }
    t.write(&dir, "eucjp0208.bin");
    let mut t = Table::new();
    for lead in 0xA1u8..=0xFE {
        for trail in 0xA1u8..=0xFE {
            t.push(decode(EUC_JP, &[0x8F, lead, trail]));
        }
    }
    t.write(&dir, "eucjp0212.bin");

    // EUC-KR: lead 0x81..=0xFE, trail 0x41..=0xFE.
    let mut t = Table::new();
    for lead in 0x81u8..=0xFE {
        for trail in 0x41u8..=0xFE {
            t.push(decode(EUC_KR, &[lead, trail]));
        }
    }
    t.write(&dir, "euckr.bin");

    // GBK / GB18030 two-byte: lead 0x81..=0xFE, trail 0x40..=0xFE.
    let mut t = Table::new();
    for lead in 0x81u8..=0xFE {
        for trail in 0x40u8..=0xFE {
            t.push(decode(GB18030, &[lead, trail]));
        }
    }
    t.write(&dir, "gbk.bin");

    // Big5: lead 0x81..=0xFE, trail 0x40..=0xFE.
    let mut t = Table::new();
    for lead in 0x81u8..=0xFE {
        for trail in 0x40u8..=0xFE {
            t.push(decode(BIG5, &[lead, trail]));
        }
    }
    t.write(&dir, "big5.bin");

    // GB18030 four-byte: runs of consecutive pointers with consecutive code
    // points, as (pointer start u32, first code point u32, length u32).
    let mut runs: Vec<(u32, u32, u32)> = Vec::new();
    for p in 0u32..126 * 10 * 126 * 10 {
        let b4 = (p % 10) as u8 + 0x30;
        let b3 = ((p / 10) % 126) as u8 + 0x81;
        let b2 = ((p / 1260) % 10) as u8 + 0x30;
        let b1 = (p / 12600) as u8 + 0x81;
        let Some(cps) = decode(GB18030, &[b1, b2, b3, b4]) else {
            continue;
        };
        assert_eq!(cps.len(), 1, "four-byte sequence decoded to {cps:?}");
        let cp = cps[0];
        match runs.last_mut() {
            Some((start, first, len)) if *start + *len == p && *first + *len == cp => *len += 1,
            _ => runs.push((p, cp, 1)),
        }
    }
    let mut out = Vec::new();
    for (p, cp, len) in &runs {
        out.extend_from_slice(&p.to_le_bytes());
        out.extend_from_slice(&cp.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
    }
    fs::write(dir.join("gb18030_runs.bin"), &out).unwrap();
    println!("gb18030_runs.bin: {} runs, {} bytes", runs.len(), out.len());
}
