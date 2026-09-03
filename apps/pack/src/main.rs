use std::{env, io, path::PathBuf};

use krkr_assets::pack_web_directory_with_entry;

const USAGE: &str = "usage: krkr-pack <input> <output> [--entry <scenario>]";

fn main() -> io::Result<()> {
    let mut args = env::args_os().skip(1);
    let input = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, USAGE))?;
    let output = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, USAGE))?;
    let mut entry = None;
    while let Some(flag) = args.next() {
        match flag.to_string_lossy().as_ref() {
            "--entry" => {
                let value = args.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--entry requires a scenario path",
                    )
                })?;
                entry = Some(value);
            }
            _ => {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE));
            }
        }
    }
    let entry = entry.as_deref().map(|value| value.to_string_lossy());
    // Static Web deployments cannot seek into native XP3 archives. Always
    // materialize their members as semantic files; the CDN remains
    // responsible for compression and caching.
    let manifest = pack_web_directory_with_entry(input, output, entry.as_deref())?;
    println!(
        "published Web v1 semantic package with {} assets{}",
        manifest.entries.len(),
        manifest
            .entry
            .as_deref()
            .map(|entry| format!(" (entry: {entry})"))
            .unwrap_or_else(
                || " (no entry; configure manifest.entry or the static shell)".to_string()
            )
    );
    Ok(())
}
