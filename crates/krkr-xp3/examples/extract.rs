//! One-off helper: extract a member from an XP3 archive to a file.
//! Usage: extract <archive.xp3> <member> <out-path>
//!        extract --list <archive.xp3> [substring-filter]
//!        extract --grep <archive.xp3> <needle>   (matches UTF-8 and UTF-16LE)
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str);
    if mode == Some("--list") {
        let archive = krkr_xp3::Xp3Archive::open_file(&args[1]).expect("open archive");
        let filter = args.get(2).map(String::as_str).unwrap_or("");
        for entry in archive.entries() {
            if entry.name.contains(filter) {
                println!("{}", entry.name);
            }
        }
        return;
    }
    if mode == Some("--grep") {
        let archive = krkr_xp3::Xp3Archive::open_file(&args[1]).expect("open archive");
        let needle = args[2].as_str();
        let utf8 = needle.as_bytes().to_vec();
        let utf16: Vec<u8> = needle.encode_utf16().flat_map(u16::to_le_bytes).collect();
        for index in 0..archive.entries().len() {
            let name = archive.entries()[index].name.clone();
            let mut stream = match archive.open_by_index(index) {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let mut data = Vec::new();
            if std::io::Read::read_to_end(&mut stream, &mut data).is_err() {
                continue;
            }
            let hit =
                |pat: &[u8]| !pat.is_empty() && data.windows(pat.len()).any(|window| window == pat);
            if hit(&utf8) || hit(&utf16) {
                println!("{name}");
            }
        }
        return;
    }
    let (archive, member, out) = match (args.first(), args.get(1), args.get(2)) {
        (Some(a), Some(m), Some(o)) => (a, m, o),
        _ => {
            eprintln!("usage: extract <archive.xp3> <member> <out-path>");
            std::process::exit(2);
        }
    };
    let archive = krkr_xp3::Xp3Archive::open_file(archive).expect("open archive");
    let mut stream = archive
        .open_by_name(member)
        .expect("open member")
        .unwrap_or_else(|| panic!("member {member} not found"));
    let mut data = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut data).expect("read member");
    std::fs::write(out, &data).expect("write out");
    println!("wrote {} bytes to {out}", data.len());
}
