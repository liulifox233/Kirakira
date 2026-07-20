fn main() {
    let game_dir = std::env::args().nth(1).expect("usage: xp3_list <game_dir> <filter>");
    let filter = std::env::args().nth(2).unwrap_or_default().to_lowercase();
    let mut names: Vec<_> = std::fs::read_dir(&game_dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            let n = p.file_name()?.to_string_lossy().to_string();
            if n.to_ascii_lowercase().ends_with(".xp3") { Some(n) } else { None }
        })
        .collect();
    names.sort();
    for xp3 in &names {
        let path = std::path::Path::new(&game_dir).join(xp3);
        let Ok(archive) = krkr_xp3::Xp3Archive::open_file(&path) else { continue };
        for entry in archive.entries() {
            if entry.name.to_lowercase().contains(&filter) {
                println!("{xp3}: {}", entry.name);
            }
        }
    }
}
