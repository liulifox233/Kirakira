fn main() {
    let xp3 = std::env::args().nth(1).unwrap();
    let name = std::env::args().nth(2).unwrap();
    let out = std::env::args().nth(3).unwrap();
    let archive = krkr_xp3::Xp3Archive::open_file(&xp3).expect("open");
    use std::io::Read as _;
    let mut stream = archive.open_by_name(&name).expect("entry").expect("found");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    std::fs::write(&out, &buf).unwrap();
    println!("wrote {} bytes", buf.len());
}
