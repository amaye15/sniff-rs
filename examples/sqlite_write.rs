//! Reads a JSON description of tables on stdin and writes a SQLite database to
//! the path given, for tools/check_export_formats.py.
//!
//!     cargo run --release --example sqlite_write -- OUT.sqlite < spec.json

use std::io::Read;

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("usage: sqlite_write OUT.sqlite < spec.json");
    let mut spec = String::new();
    std::io::stdin()
        .read_to_string(&mut spec)
        .expect("read stdin");
    match sniff_rs::graph_write_sqlite(&spec) {
        Ok(bytes) => std::fs::write(&out, bytes).expect("write"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
