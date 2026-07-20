use krkr_tjs2::bytecode::BytecodeFile;

fn main() {
    let path = std::env::args().nth(1).expect("usage: disasm_probe <file.tjs> [name-filter]");
    let filter = std::env::args().nth(2);
    let bytes = std::fs::read(&path).expect("read");
    let file = BytecodeFile::parse_unverified(&bytes).expect("parse");
    for (index, object) in file.objects.iter().enumerate() {
        let name = &file.data.strings[object.name];
        if let Some(f) = &filter {
            if !name.contains(f.as_str()) {
                continue;
            }
        }
        println!("=== object {index} name={name} type={:?} parent={:?}", object.context_type, object.parent);
        match file.disassemble_object(index) {
            Ok(lines) => {
                for line in &lines {
                    println!("{line}");
                }
            }
            Err(e) => println!("<disasm error: {e}>"),
        }
    }
}
