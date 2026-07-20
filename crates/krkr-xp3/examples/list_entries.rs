use std::io::Read as _;

fn main() {
    let game_dir = std::env::args().nth(1).unwrap();
    let pat = std::env::args().nth(2).unwrap_or_default().to_lowercase();
    let out_dir = std::env::args().nth(3);
    let mut names: Vec<_> = std::fs::read_dir(&game_dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            let n = p.file_name()?.to_string_lossy().to_string();
            if n.to_ascii_lowercase().ends_with(".xp3") {
                Some(n)
            } else {
                None
            }
        })
        .collect();
    names.sort();
    for xp3 in &names {
        let archive =
            match krkr_xp3::Xp3Archive::open_file(std::path::Path::new(&game_dir).join(xp3)) {
                Ok(a) => a,
                Err(_) => continue,
            };
        for entry in archive.entries() {
            if !(pat.is_empty() || entry.name.to_lowercase().contains(&pat)) {
                continue;
            }
            println!("{xp3}\t{}", entry.name);
            if let Some(dir) = &out_dir {
                if let Ok(Some(mut stream)) = archive.open_by_name(&entry.name) {
                    let mut buf = Vec::new();
                    if stream.read_to_end(&mut buf).is_ok() {
                        let out = std::path::Path::new(dir).join(entry.name.replace('/', "__"));
                        std::fs::create_dir_all(dir).unwrap();
                        std::fs::write(out, &buf).unwrap();
                    }
                }
            }
        }
    }
}
