//! Prints the text the knowledge graph reads from each document named on the
//! command line (one paragraph per line), for tools/check_doc_text.py.
//!
//!     cargo run --release --example doc_text -- FILE...
//!
//! Each file's text is followed by a line holding only `\u{1}END`.

fn main() {
    for path in std::env::args().skip(1) {
        match sniff_rs::graph_document_text(std::path::Path::new(&path)) {
            Some(lines) => {
                for line in lines {
                    println!("{line}");
                }
            }
            None => println!("\u{1}UNREADABLE"),
        }
        println!("\u{1}END");
    }
}
