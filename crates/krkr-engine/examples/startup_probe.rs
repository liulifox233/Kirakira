use std::{env, path::PathBuf};

use krkr_engine::storage::ProjectStorage;
use krkr_tjs2::{
    bytecode::{BYTECODE_SIGNATURE, BytecodeFile},
    compile_source_to_bytecode,
};

fn main() {
    let root = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let name = env::args()
        .nth(2)
        .unwrap_or_else(|| "startup.tjs".to_string());
    let dump_all = env::args().nth(3).as_deref() == Some("all");

    let storage = ProjectStorage::for_root(&root).expect("project storage");
    println!("root={}", root.display());
    println!("storage={name}");
    println!("exists={}", storage.storage_exists(&name));
    println!(
        "placed_path={}",
        storage
            .placed_path(&name)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<xp3>".to_string())
    );

    let bytes = storage.read_binary_vec(&name).expect("read bytes");
    let nul_count = bytes.iter().filter(|&&byte| byte == 0).count();
    println!("byte_len={}", bytes.len());
    println!("nul_count={nul_count}");
    println!("head_hex={}", hex_dump(&bytes[..bytes.len().min(32)]));

    match storage.read_text_storage(&name, "UTF-8") {
        Ok(text) => {
            let preview = if dump_all {
                preview_text(&text, usize::MAX)
            } else {
                preview_text(&text, 5)
            };
            let text_nul_count = text.chars().filter(|&ch| ch == '\0').count();
            println!("decoded_chars={}", text.chars().count());
            println!("decoded_nul_count={text_nul_count}");
            println!("preview:\n{preview}");
        }
        Err(error) => {
            println!("decode_error={error}");
        }
    }

    if bytes.starts_with(&BYTECODE_SIGNATURE) {
        println!("bytecode_disasm:");
        dump_bytecode(&bytes, dump_all);
    } else if let Ok(text) = storage.read_text_storage(&name, "UTF-8") {
        println!("compiled_disasm:");
        dump_compiled_source(&name, &text);
    }
}

fn hex_dump(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn preview_text(text: &str, max_lines: usize) -> String {
    text.lines()
        .take(max_lines)
        .enumerate()
        .map(|(index, line)| format!("{:>2}: {}", index + 1, visualize_nul(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn visualize_nul(line: &str) -> String {
    line.chars()
        .map(|ch| if ch == '\0' { '·' } else { ch })
        .collect()
}

fn dump_bytecode(bytes: &[u8], dump_all: bool) {
    let file = match BytecodeFile::parse(bytes) {
        Ok(file) => file,
        Err(error) => {
            println!("bytecode_verify_error={error}");
            BytecodeFile::parse_unverified(bytes).expect("parse unverified bytecode")
        }
    };
    for (object_index, object) in file.objects.iter().enumerate() {
        let object_name = object.name(&file).unwrap_or("<unnamed>");
        println!(
            "object[{object_index}] {object_name:?} {:?} parent={:?} max_frame={} max_var={} reserve={} args={}",
            object.context_type,
            object.parent,
            object.max_frame_count,
            object.max_variable_count,
            object.variable_reserve_count,
            object.func_decl_arg_count
        );
        let lines = file.disassemble_object(object_index).expect("disassemble");
        let limit = if dump_all { usize::MAX } else { 120 };
        for (index, line) in lines.into_iter().enumerate().take(limit) {
            println!("{index:>3}: {line}");
        }
    }
}

fn dump_compiled_source(name: &str, source: &str) {
    let file = compile_source_to_bytecode(name, source).expect("compile source");
    for (object_index, object) in file.objects.iter().enumerate() {
        let object_name = object.name(&file).unwrap_or("<unnamed>");
        println!(
            "object[{object_index}] {object_name:?} {:?} parent={:?} max_frame={} max_var={} reserve={} args={}",
            object.context_type,
            object.parent,
            object.max_frame_count,
            object.max_variable_count,
            object.variable_reserve_count,
            object.func_decl_arg_count
        );
        for (index, line) in file
            .disassemble_object(object_index)
            .expect("disassemble")
            .into_iter()
            .enumerate()
            .take(160)
        {
            println!("{index:>3}: {line}");
        }
    }
}
