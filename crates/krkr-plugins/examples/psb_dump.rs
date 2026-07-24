//! Dumps the structure of a PSB file (e.g. `.ks.scn` scenario files).
//!
//! Usage: psb_dump <file.psb> [max_depth]

use krkr_plugins::{PsbValue, debug_parse_psb};

fn print_value(value: &PsbValue, depth: usize, max_depth: usize, name: &str) {
    let indent = "  ".repeat(depth);
    match value {
        PsbValue::Null => println!("{indent}{name}: null"),
        PsbValue::Bool(v) => println!("{indent}{name}: {v}"),
        PsbValue::Integer(v) => println!("{indent}{name}: {v}"),
        PsbValue::Real(v) => println!("{indent}{name}: {v}"),
        PsbValue::String(v) => println!("{indent}{name}: {v:?}"),
        PsbValue::Octet(v) => println!("{indent}{name}: <octet {} bytes>", v.len()),
        PsbValue::Array(items) => {
            println!("{indent}{name}: [ {} items ]", items.len());
            if depth < max_depth {
                for (i, item) in items.iter().enumerate() {
                    print_value(item, depth + 1, max_depth, &format!("[{i}]"));
                }
            }
        }
        PsbValue::Object(map) => {
            println!("{indent}{name}: {{ {} keys }}", map.len());
            if depth < max_depth {
                for (key, item) in map {
                    print_value(item, depth + 1, max_depth, key);
                }
            }
        }
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: psb_dump <file> [depth]");
    let max_depth: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let bytes = std::fs::read(&path).expect("read file");
    match debug_parse_psb(&bytes) {
        Ok(value) => print_value(&value, 0, max_depth, "root"),
        Err(err) => {
            eprintln!("parse failed: {err}");
            std::process::exit(1);
        }
    }
}
