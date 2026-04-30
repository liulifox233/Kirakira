use std::path::PathBuf;

use krkr_engine::KrkrEngine;

fn main() {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    match KrkrEngine::for_project(&root).and_then(|mut engine| engine.execute_startup()) {
        Ok(value) => {
            println!("startup completed: {value}");
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
