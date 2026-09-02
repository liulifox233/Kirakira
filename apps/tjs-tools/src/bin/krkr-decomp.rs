//! Standalone TJS2 bytecode decompiler.
//!
//! Usage:
//!   krkr-decomp <path> [member] [options]
//!
//! Inputs:
//!   <file>                loose file: `TJS2100\0` bytecode is decompiled
//!                         directly, anything else is compiled as TJS source
//!                         first and that compiler output is decompiled
//!   <archive.xp3>         list entries (name, size, kind)
//!   <archive.xp3> <name>  decompile one archive member
//!   <game_dir> <name>     resolve a member like the engine: loose file first,
//!                         then sys/*.xp3 and *.xp3 (later archives win)
//!
//! Options:
//!   --filter <substr>     only decompile code objects whose name contains
//!                         substr (other bodies become unhandled placeholders)
//!   --object <n>          only decompile code object n
//!   --all                 archive mode: decompile every bytecode member
//!   --output <dir>        write .tjs files next to the input names instead of
//!                         printing to stdout
//!   --verify              after decompiling, reparse and recompile the
//!                         output and report the result
//!   --source              force treating the input as TJS source
//!   --bytecode            force treating the input as TJS2 bytecode

use std::{
    io::Read as _,
    path::{Path, PathBuf},
    process::ExitCode,
};

use krkr_tjs2::{
    bytecode::{BYTECODE_SIGNATURE, BytecodeFile},
    compile_source_to_bytecode,
    decompile::{DecompileOptions, decompile},
};
use krkr_xp3::Xp3Archive;

#[derive(Default)]
struct Config {
    path: Option<PathBuf>,
    member: Option<String>,
    filter: Option<String>,
    object: Option<usize>,
    all: bool,
    output: Option<PathBuf>,
    verify: bool,
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
            "--all" => config.all = true,
            "--output" => config.output = Some(PathBuf::from(value("--output")?)),
            "--verify" => config.verify = true,
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
    if config.all && config.output.is_none() {
        return Err("--all requires --output".to_string());
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
            eprintln!("usage: krkr-decomp <path> [member] [--filter s] [--object n] [--all]");
            eprintln!("                    [--output dir] [--verify] [--source|--bytecode]");
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
        return decompile_game_dir(config, path);
    }
    if !path.is_file() {
        return Err(format!("{} does not exist", path.display()));
    }
    if is_xp3(path) {
        return decompile_archive(config, path);
    }
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    decompile_bytes(config, &bytes, &path.display().to_string(), path)
}

fn is_xp3(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("xp3"))
}

fn decompile_game_dir(config: &Config, dir: &Path) -> Result<(), String> {
    let member = config
        .member
        .as_deref()
        .ok_or_else(|| "a member name is required for a game directory".to_string())?;
    let loose = dir.join(member);
    if loose.is_file() {
        let bytes = std::fs::read(&loose)
            .map_err(|error| format!("cannot read {}: {error}", loose.display()))?;
        return decompile_bytes(
            config,
            &bytes,
            &format!("loose {}", loose.display()),
            &loose,
        );
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
            return decompile_bytes(
                config,
                &bytes,
                &format!("{archive_name}>{member}"),
                archive_path,
            );
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

fn decompile_archive(config: &Config, path: &Path) -> Result<(), String> {
    let archive = Xp3Archive::open_file(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let archive_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    if let Some(member) = &config.member {
        let bytes = read_member(&archive, member)?
            .ok_or_else(|| format!("{member} not found in {}", path.display()))?;
        return decompile_bytes(config, &bytes, &format!("{archive_name}>{member}"), path);
    }
    if config.all {
        let mut decompiled = 0;
        for index in 0..archive.entries().len() {
            if !entry_is_bytecode(&archive, index) {
                continue;
            }
            let bytes = read_entry(&archive, index)?;
            let name = &archive.entries()[index].name;
            decompile_bytes(config, &bytes, &format!("{archive_name}>{name}"), path)?;
            decompiled += 1;
        }
        eprintln!("; decompiled {decompiled} bytecode members");
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

fn decompile_bytes(
    config: &Config,
    bytes: &[u8],
    origin: &str,
    input_path: &Path,
) -> Result<(), String> {
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
    let options = DecompileOptions {
        object_name_filter: config.filter.clone(),
        object_index: config.object,
    };
    let output = decompile(&file, &options).map_err(|error| format!("{origin}: {error}"))?;
    let stats = output.stats;
    let source = output
        .sources
        .into_iter()
        .next()
        .ok_or_else(|| format!("{origin}: no decompiled source produced"))?;

    if let Some(directory) = &config.output {
        let file_name = origin
            .split(['>', '/', '\\'])
            .next_back()
            .unwrap_or("decompiled")
            .replace(['/', '\\', ':'], "_");
        let out_path = directory.join(format!("{file_name}.tjs"));
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
        std::fs::write(&out_path, &source.text)
            .map_err(|error| format!("cannot write {}: {error}", out_path.display()))?;
        eprintln!("; wrote {}", out_path.display());
    } else {
        println!("; origin: {origin}");
        print!("{}", source.text);
    }

    if config.verify {
        match krkr_tjs2::compiler::parse_source(&source.text) {
            Ok(program) => {
                let reparsed =
                    krkr_tjs2::compiler::compile_source_to_bytecode(&source.name, &source.text);
                eprintln!(
                    "; verify: reparse ok ({} statements), recompile {}",
                    program.statements.len(),
                    match reparsed {
                        Ok(_) => "ok".to_string(),
                        Err(error) => format!("FAILED: {error}"),
                    }
                );
            }
            Err(error) => {
                eprintln!("; verify: reparse FAILED: {error}");
            }
        }
    } else {
        eprintln!(
            "; {} objects, {} unhandled fragments",
            stats.objects, stats.unhandled
        );
    }
    let _ = input_path;
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
