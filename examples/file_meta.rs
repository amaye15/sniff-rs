//! Prints, as JSON lines, the facts the knowledge graph reads from each file's
//! own properties (author, camera, artist, album, ...), for tools/check_audio.py.
//!
//!     cargo run --release --example file_meta -- FILE...
//!
//! One line per file: {"file": "...", "facts": [["artist", "..."], ...]}.

fn main() {
    for path in std::env::args().skip(1) {
        let facts = sniff_rs::graph_file_metadata(std::path::Path::new(&path));
        let list: Vec<String> = facts
            .iter()
            .map(|(k, v)| format!("[{}, {}]", json_string(k), json_string(v)))
            .collect();
        println!(
            "{{\"file\": {}, \"facts\": [{}]}}",
            json_string(&path),
            list.join(", ")
        );
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
