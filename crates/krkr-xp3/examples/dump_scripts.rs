use std::io::Read as _;
use std::path::Path;

fn main() {
    let game_dir = std::env::args().nth(1).expect("usage: dump_scripts <game_dir> <out_dir>");
    let out_dir = std::env::args().nth(2).expect("usage: dump_scripts <game_dir> <out_dir>");
    let out_root = Path::new(&out_dir);
    std::fs::create_dir_all(out_root).unwrap();

    let mut names: Vec<_> = std::fs::read_dir(&game_dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            let n = p.file_name()?.to_string_lossy().to_string();
            if n.to_ascii_lowercase().ends_with(".xp3") { Some(n) } else { None }
        })
        .collect();
    names.sort();

    let mut count = 0usize;
    for xp3 in &names {
        let path = Path::new(&game_dir).join(xp3);
        let archive = match krkr_xp3::Xp3Archive::open_file(&path) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("skip {xp3}: {e}");
                continue;
            }
        };
        for entry in archive.entries() {
            let name = &entry.name;
            let lower = name.to_ascii_lowercase();
            if !(lower.ends_with(".tjs") || lower.ends_with(".ks") || lower.ends_with(".asd")) {
                continue;
            }
            let Ok(Some(mut stream)) = archive.open_by_name(name) else { continue };
            let mut buf = Vec::new();
            if stream.read_to_end(&mut buf).is_err() {
                continue;
            }
            let out = out_root.join(xp3).join(name.replace('/', "__"));
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&out, &buf).unwrap();
            count += 1;
        }
    }
    println!("extracted {count} script files");
}
