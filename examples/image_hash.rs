//! Prints, as JSON lines, the perceptual hash the knowledge graph gives each
//! PNG or JPEG named on the command line, for tools/check_images.py.
//!
//!     cargo run --release --example image_hash -- [--plane] FILE...
//!
//! One line per file: {"file": "...", "hash": "<16 hex digits>" or null}. With
//! `--plane` also "w", "h" and "luma" (the brightness plane the hash is made
//! from: pixels for a PNG, one value per 8x8 block for a JPEG).

fn main() {
    let mut plane = false;
    for arg in std::env::args().skip(1) {
        if arg == "--plane" {
            plane = true;
            continue;
        }
        let (hash, p) = sniff_rs::graph_image_hash(std::path::Path::new(&arg), plane);
        let hash = hash.map_or("null".to_string(), |h| format!("\"{h:016x}\""));
        let mut line = format!("{{\"file\": {:?}, \"hash\": {hash}", arg);
        match p {
            Some((w, h, data)) => {
                let list: Vec<String> = data.iter().map(u8::to_string).collect();
                line.push_str(&format!(
                    ", \"w\": {w}, \"h\": {h}, \"luma\": [{}]",
                    list.join(",")
                ));
            }
            None if plane => line.push_str(", \"w\": null"),
            None => {}
        }
        line.push('}');
        println!("{line}");
    }
}
