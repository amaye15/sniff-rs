//! Prints, as JSON lines, the coarse places and days the knowledge graph reads
//! from each file named on the command line, for tools/check_geotime.py.
//!
//!     cargo run --release --example place_time -- [--cell DEGREES] [--timeline] FILE...
//!
//! One line per file: {"file": "...", "places": ["lat,lon", ...], "days": ["YYYY-MM-DD", ...]}.
//! Places are read only with `--cell`, days only with `--timeline`.

fn main() {
    let mut cell = None;
    let mut timeline = false;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cell" => cell = args.next().and_then(|v| v.parse::<f64>().ok()),
            "--timeline" => timeline = true,
            _ => files.push(a),
        }
    }
    let list = |v: &[String]| {
        v.iter()
            .map(|s| format!("{s:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    for f in files {
        let (places, days) =
            sniff_rs::graph_places_and_days(std::path::Path::new(&f), cell, timeline);
        println!(
            "{{\"file\": {f:?}, \"places\": [{}], \"days\": [{}]}}",
            list(&places),
            list(&days)
        );
    }
}
