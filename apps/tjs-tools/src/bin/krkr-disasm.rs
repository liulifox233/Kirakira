//! Standalone TJS2 bytecode disassembler.
//!
//! Usage:
//!   krkr-disasm <path> [member] [options]
//!
//! Inputs:
//!   <file>                loose file: `TJS2100\0` bytecode is dumped directly,
//!                         anything else is compiled as TJS source first and
//!                         the compiler output is dumped instead
//!   <archive.xp3>         list entries (name, size, kind)
//!   <archive.xp3> <name>  dump one archive member
//!   <game_dir> <name>     resolve a member like the engine: loose file first,
//!                         then sys/*.xp3 and *.xp3 (later archives win)
//!
//! Options:
//!   --filter <substr>     only dump code objects whose name contains substr
//!   --object <n>          only dump code object n
//!   --no-data             skip the data pool section
//!   --all                 archive mode: dump every bytecode member
//!   --source              force treating the input as TJS source
//!   --bytecode            force treating the input as TJS2 bytecode

use std::{
    io::Read as _,
    path::{Path, PathBuf},
    process::ExitCode,
};

use krkr_tjs2::{
    bytecode::{BYTECODE_SIGNATURE, BytecodeFile, DisasmOptions},
    compile_source_to_bytecode,
};
use krkr_xp3::Xp3Archive;

#[derive(Default)]
struct Config {
    path: Option<PathBuf>,
    member: Option<String>,
    filter: Option<String>,
    object: Option<usize>,
    no_data: bool,
    all: bool,
    force_source: bool,
    force_bytecode: bool,
}

fn parse_args() -> Result<Config, String> {
    let mut config = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match arg.as_str() {
            "--filter" => config.filter = Some(value("--filter")?),
            "--object" => {
                config.object = Some(
                    value("--object")?
                        .parse()
                        .map_err(|_| "--object must be an object index".to_string())?,
                );
            }
            "--no-data" => config.no_data = true,
            "--all" => config.all = true,
            "--source" => config.force_source = true,
            "--bytecode" => config.force_bytecode = true,
            "-h" | "--help" => return Err(String::new()),
            _ if arg.starts_with('-') => return Err(format!("unknown argument: {arg}")),
            _ if config.path.is_none() => config.path = Some(PathBuf::from(arg)),
            _ if config.member.is_none() => config.member = Some(arg),
            _ => return Err(format!("unexpected argument: {arg}")),
        }
    }
    if config.force_source && config.force_bytecode {
        return Err("--source and --bytecode are mutually exclusive".to_string());
    }
    Ok(config)
}

fn main() -> ExitCode {
    let config = match parse_args() {
        Ok(config) => config,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("error: {message}");
            }
            eprintln!("usage: krkr-disasm <path> [member] [--filter s] [--object n] [--no-data]");
            eprintln!("                   [--all] [--source|--bytecode]");
            return ExitCode::FAILURE;
        }
    };
    match run(&config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(config: &Config) -> Result<(), String> {
    let path = config
        .path
        .as_deref()
        .ok_or_else(|| "missing input path".to_string())?;
    if path.is_dir() {
        return dump_game_dir(config, path);
    }
    if !path.is_file() {
        return Err(format!("{} does not exist", path.display()));
    }
    if is_xp3(path) {
        return dump_archive(config, path);
    }
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    dump_bytes(config, &bytes, &path.display().to_string())
}

fn is_xp3(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("xp3"))
}

/// Dumps a member resolved against a game directory with the same priority
/// as the engine: loose files first, then `sys/*.xp3` followed by `*.xp3`
/// (both sorted), with later archives winning.
fn dump_game_dir(config: &Config, dir: &Path) -> Result<(), String> {
    let member = config
        .member
        .as_deref()
        .ok_or_else(|| "a member name is required for a game directory".to_string())?;
    let loose = dir.join(member);
    if loose.is_file() {
        let bytes = std::fs::read(&loose)
            .map_err(|error| format!("cannot read {}: {error}", loose.display()))?;
        return dump_bytes(config, &bytes, &format!("loose {}", loose.display()));
    }
    let mut archives = xp3_files(&dir.join("sys"));
    archives.extend(xp3_files(dir));
    for archive_path in archives.iter().rev() {
        let archive = Xp3Archive::open_file(archive_path)
            .map_err(|error| format!("cannot open {}: {error}", archive_path.display()))?;
        if let Some(bytes) = read_member(&archive, member)? {
            let archive_name = archive_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| archive_path.display().to_string());
            return dump_bytes(config, &bytes, &format!("{archive_name}>{member}"));
        }
    }
    Err(format!("{member} not found in {}", dir.display()))
}

fn xp3_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut archives = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_file() && is_xp3(path))
        .collect::<Vec<_>>();
    archives.sort();
    archives
}

fn dump_archive(config: &Config, path: &Path) -> Result<(), String> {
    let archive = Xp3Archive::open_file(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let archive_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    if let Some(member) = &config.member {
        let bytes = read_member(&archive, member)?
            .ok_or_else(|| format!("{member} not found in {}", path.display()))?;
        return dump_bytes(config, &bytes, &format!("{archive_name}>{member}"));
    }
    if config.all {
        let mut dumped = 0;
        for index in 0..archive.entries().len() {
            if !entry_is_bytecode(&archive, index) {
                continue;
            }
            let bytes = read_entry(&archive, index)?;
            let name = &archive.entries()[index].name;
            dump_bytes(config, &bytes, &format!("{archive_name}>{name}"))?;
            dumped += 1;
        }
        println!("; dumped {dumped} bytecode members");
        return Ok(());
    }
    println!("name\tsize\tkind");
    for index in 0..archive.entries().len() {
        let entry = &archive.entries()[index];
        let kind = if entry_is_bytecode(&archive, index) {
            "bytecode"
        } else {
            "data"
        };
        println!("{}\t{}\t{kind}", entry.name, entry.original_size);
    }
    Ok(())
}

/// Peeks at the first bytes of an entry to check the bytecode signature
/// without decompressing the whole entry.
fn entry_is_bytecode(archive: &Xp3Archive<std::fs::File>, index: usize) -> bool {
    let Ok(mut stream) = archive.open_by_index(index) else {
        return false;
    };
    let mut signature = [0u8; 8];
    stream.read_exact(&mut signature).is_ok() && signature == BYTECODE_SIGNATURE
}

fn read_member(archive: &Xp3Archive<std::fs::File>, name: &str) -> Result<Option<Vec<u8>>, String> {
    if archive.get_entry(name).is_none()
        && let Some(entry) = archive.get_entry_ascii_case_insensitive(name)
    {
        let name = entry.name.clone();
        return read_entry_by_name(archive, &name);
    }
    read_entry_by_name(archive, name)
}

fn read_entry_by_name(
    archive: &Xp3Archive<std::fs::File>,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    let Some(mut stream) = archive
        .open_by_name(name)
        .map_err(|error| format!("cannot open {name}: {error}"))?
    else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    stream
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {name}: {error}"))?;
    Ok(Some(bytes))
}

fn read_entry(archive: &Xp3Archive<std::fs::File>, index: usize) -> Result<Vec<u8>, String> {
    let name = archive
        .entries()
        .get(index)
        .map(|entry| entry.name.clone())
        .unwrap_or_else(|| index.to_string());
    let mut stream = archive
        .open_by_index(index)
        .map_err(|error| format!("cannot open {name}: {error}"))?;
    let mut bytes = Vec::new();
    stream
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {name}: {error}"))?;
    Ok(bytes)
}

fn dump_bytes(config: &Config, bytes: &[u8], origin: &str) -> Result<(), String> {
    let is_bytecode = if config.force_source {
        false
    } else {
        config.force_bytecode || bytes.starts_with(&BYTECODE_SIGNATURE)
    };
    let file = if is_bytecode {
        BytecodeFile::parse_unverified(bytes).map_err(|error| format!("{origin}: {error}"))?
    } else {
        let source = decode_source(bytes).map_err(|error| format!("{origin}: {error}"))?;
        compile_source_to_bytecode(origin, &source).map_err(|error| format!("{origin}: {error}"))?
    };
    let options = DisasmOptions {
        include_data_pool: !config.no_data,
        object_name_filter: config.filter.clone(),
        object_index: config.object,
    };
    let dump = file
        .disassemble(&options)
        .map_err(|error| format!("{origin}: {error}"))?;
    println!("; origin: {origin}");
    print!("{dump}");
    Ok(())
}

/// Decodes a TJS source file: UTF-16LE with BOM, UTF-8, then Shift-JIS.
fn decode_source(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0xff, 0xfe]) {
        let units = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16(&units)
            .map_err(|error| format!("invalid UTF-16 source: {error}"));
    }
    if let Ok(text) = String::from_utf8(bytes.to_vec()) {
        return Ok(text);
    }
    let (text, _, _) = encoding_rs::SHIFT_JIS.decode(bytes);
    Ok(text.into_owned())
}
