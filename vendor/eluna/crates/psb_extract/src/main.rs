use eluna::{
    bruteforce_emote_key, collect_resource_refs, collect_schema_paths, load_emote_static_scene,
    PsbBruteforceOptions, PsbDecryptionKey, PsbFile, PsbHeader, PsbNormalizeOptions,
    PsbResourceRange, PsbValue,
};
use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufWriter, Error as IoError, ErrorKind, Write};
use std::path::{Path, PathBuf};

struct Options {
    input: PathBuf,
    output_dir: Option<PathBuf>,
    normalize: PsbNormalizeOptions,
    write_root: bool,
    write_resources: bool,
    write_schema: bool,
    write_normalized: bool,
    write_emote_schema: bool,
    bruteforce_key: bool,
    bruteforce_threads: usize,
    bruteforce_start: u32,
    bruteforce_end: u32,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let options = parse_args()?;
    let input_data = fs::read(&options.input)?;

    if options.bruteforce_key {
        let result = bruteforce_emote_key(
            &input_data,
            PsbBruteforceOptions {
                start_key: options.bruteforce_start,
                end_key: options.bruteforce_end,
                threads: options.bruteforce_threads,
                decode_mdf: options.normalize.decode_mdf,
                decode_lz4: options.normalize.decode_lz4,
            },
        )?;

        match result {
            Some(found) => {
                println!("found key: 0x{:08x}", found.key);
                println!("tested keys: {}", found.tested_keys);
            }
            None => {
                println!("key not found");
            }
        }
        return Ok(());
    }

    let output_dir = options
        .output_dir
        .as_ref()
        .ok_or_else(|| make_io_error(ErrorKind::InvalidInput, "missing --output"))?;

    let (data, psb) = PsbFile::parse_normalized(&input_data, &options.normalize)?;

    fs::create_dir_all(output_dir)?;
    write_manifest(output_dir, &psb, data.len())?;
    write_string_list(&output_dir.join("names.txt"), &psb.names)?;
    write_string_list(&output_dir.join("strings.txt"), &psb.strings)?;

    if options.write_normalized {
        fs::write(output_dir.join("normalized.psb"), &data)?;
    }

    if options.write_root {
        write_root_dump(&output_dir.join("root.txt"), &psb.root)?;
    }

    if options.write_resources {
        extract_resource_group(&data, &psb.resources, output_dir, "resource")?;
        extract_resource_group(&data, &psb.extra_resources, output_dir, "extra_resource")?;
    }

    if options.write_schema {
        write_schema_dump(&output_dir.join("schema_paths.txt"), &psb.root)?;
        write_resource_refs(&output_dir.join("resource_refs.txt"), &psb.root)?;
    }

    if options.write_emote_schema {
        write_emote_schema_dump(output_dir, &psb)?;
    }

    println!(
        "PSB extracted: version={} flags=0x{:04x} names={} strings={} resources={} extra_resources={}",
        psb.version,
        psb.header.flags,
        psb.names.len(),
        psb.strings.len(),
        psb.resources.len(),
        psb.extra_resources.len()
    );

    Ok(())
}

fn parse_args() -> Result<Options, Box<dyn Error>> {
    let mut input = None;
    let mut output_dir = None;
    let mut write_root = true;
    let mut write_resources = true;
    let mut write_schema = false;
    let mut write_normalized = false;
    let mut write_emote_schema = false;
    let mut decode_mdf = true;
    let mut decode_lz4 = true;
    let mut key: Option<u32> = None;
    let mut bruteforce_key = false;
    let mut bruteforce_threads = 0usize;
    let mut bruteforce_start = 0u32;
    let mut bruteforce_end = u32::MAX;

    let mut args = env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let arg_text = arg.to_string_lossy();
        match arg_text.as_ref() {
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            "--input" => input = Some(next_path_arg(&mut args, "--input")?),
            "--output" => output_dir = Some(next_path_arg(&mut args, "--output")?),
            "--no-root" => write_root = false,
            "--no-resources" => write_resources = false,
            "--schema" => write_schema = true,
            "--write-normalized" => write_normalized = true,
            "--emote-schema" => write_emote_schema = true,
            "--no-mdf" => decode_mdf = false,
            "--no-lz4" => decode_lz4 = false,
            "--key" => key = Some(next_u32_arg(&mut args, "--key")?),
            "--bruteforce-key" => bruteforce_key = true,
            "--bruteforce-threads" => {
                bruteforce_threads = next_usize_arg(&mut args, "--bruteforce-threads")?
            }
            "--bruteforce-start" => {
                bruteforce_start = next_u32_arg(&mut args, "--bruteforce-start")?
            }
            "--bruteforce-end" => {
                bruteforce_end = next_u32_arg(&mut args, "--bruteforce-end")?
            }
            _ if arg_text.starts_with("--input=") => input = Some(PathBuf::from(&arg_text[8..])),
            _ if arg_text.starts_with("--output=") => output_dir = Some(PathBuf::from(&arg_text[9..])),
            _ if arg_text.starts_with("--key=") => key = Some(parse_u32_value(&arg_text[6..])?),
            _ if arg_text.starts_with("--bruteforce-threads=") => {
                bruteforce_threads = parse_usize_value(&arg_text[21..])?
            }
            _ if arg_text.starts_with("--bruteforce-start=") => {
                bruteforce_start = parse_u32_value(&arg_text[19..])?
            }
            _ if arg_text.starts_with("--bruteforce-end=") => {
                bruteforce_end = parse_u32_value(&arg_text[17..])?
            }
            _ => {
                return Err(make_error(
                    ErrorKind::InvalidInput,
                    "unexpected argument; use --input <path> --output <dir>",
                ));
            }
        }
    }

    let decrypt_key = key.map(PsbDecryptionKey::emote_key);

    if bruteforce_key && key.is_some() {
        return Err(make_error(
            ErrorKind::InvalidInput,
            "--bruteforce-key and --key are mutually exclusive",
        ));
    }

    let input = input.ok_or_else(|| make_io_error(ErrorKind::InvalidInput, "missing --input"))?;
    if !bruteforce_key && output_dir.is_none() {
        return Err(Box::new(make_io_error(ErrorKind::InvalidInput, "missing --output")));
    }

    Ok(Options {
        input,
        output_dir,
        normalize: PsbNormalizeOptions {
            decrypt_key,
            decode_mdf,
            decode_lz4,
        },
        write_root,
        write_resources,
        write_schema,
        write_normalized,
        write_emote_schema,
        bruteforce_key,
        bruteforce_threads,
        bruteforce_start,
        bruteforce_end,
    })
}


fn next_path_arg(args: &mut impl Iterator<Item = OsString>, option: &'static str) -> Result<PathBuf, Box<dyn Error>> {
    let value = args
        .next()
        .ok_or_else(|| make_io_error(ErrorKind::InvalidInput, option))?;
    Ok(PathBuf::from(value))
}

fn next_u32_arg(args: &mut impl Iterator<Item = OsString>, option: &'static str) -> Result<u32, Box<dyn Error>> {
    let value = args
        .next()
        .ok_or_else(|| make_io_error(ErrorKind::InvalidInput, option))?;
    parse_u32_value(&value.to_string_lossy())
}

fn next_usize_arg(args: &mut impl Iterator<Item = OsString>, option: &'static str) -> Result<usize, Box<dyn Error>> {
    let value = args
        .next()
        .ok_or_else(|| make_io_error(ErrorKind::InvalidInput, option))?;
    parse_usize_value(&value.to_string_lossy())
}

fn parse_u32_value(value: &str) -> Result<u32, Box<dyn Error>> {
    let trimmed = value.trim();
    let parsed = if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)?
    } else {
        trimmed.parse::<u32>()?
    };
    Ok(parsed)
}

fn parse_usize_value(value: &str) -> Result<usize, Box<dyn Error>> {
    let trimmed = value.trim();
    let parsed = if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        usize::from_str_radix(hex, 16)?
    } else {
        trimmed.parse::<usize>()?
    };
    Ok(parsed)
}

fn make_io_error(kind: ErrorKind, message: &'static str) -> IoError {
    IoError::new(kind, message)
}

fn make_error(kind: ErrorKind, message: &'static str) -> Box<dyn Error> {
    Box::new(make_io_error(kind, message))
}

fn print_usage() {
    println!("usage: psb_extract --input <input.psb|input.mdf|input.lz4> [--output <output_dir>] [options]");
    println!();
    println!("required:");
    println!("  --input <path>       input PSB/MDF/LZ4 file");
    println!("  --output <dir>       output directory, required unless --bruteforce-key is used");
    println!();
    println!("optional extraction:");
    println!("  --no-root            skip root.txt");
    println!("  --no-resources       skip resource_*.bin extraction");
    println!("  --schema             write schema_paths.txt and resource_refs.txt");
    println!("  --emote-schema       write emote_textures.txt and emote_scene.txt");
    println!("  --write-normalized   write normalized.psb after wrapper decode/decrypt");
    println!();
    println!("optional wrapper/decrypt:");
    println!("  --no-mdf             do not unwrap MDF zlib containers");
    println!("  --no-lz4             do not unwrap LZ4 frame containers");
    println!("  --key <u32>          Emote key DWORD; fixed seeds are 0x075BCD15, 0x159A55E5, 0x1F123BB5, key, 0, 0");
    println!("  --bruteforce-key     brute-force the single Emote key DWORD and exit");
    println!("  --bruteforce-threads <n>  worker threads for --bruteforce-key; 0 means auto");
    println!("  --bruteforce-start <u32>  inclusive start key; default 0");
    println!("  --bruteforce-end <u32>    inclusive end key; default 0xffffffff");
    println!();
    println!("outputs:");
    println!("  manifest.txt");
    println!("  names.txt");
    println!("  strings.txt");
    println!("  root.txt");
    println!("  resource_0000.bin ...");
    println!("  extra_resource_0000.bin ...");
}

fn write_manifest(output_dir: &Path, psb: &PsbFile, normalized_len: usize) -> Result<(), Box<dyn Error>> {
    let path = output_dir.join("manifest.txt");
    let mut out = BufWriter::new(File::create(path)?);

    writeln!(out, "version={}", psb.version)?;
    writeln!(out, "flags=0x{:04x}", psb.header.flags)?;
    writeln!(out, "encrypted={}", psb.encrypted)?;
    writeln!(out, "normalized_size={normalized_len}")?;
    writeln!(out, "header_offset=0x{:x}", psb.header.header_offset)?;
    writeln!(out, "name_offset=0x{:x}", psb.header.name_offset)?;
    writeln!(out, "string_offset=0x{:x}", psb.header.string_offset)?;
    writeln!(out, "string_data_offset=0x{:x}", psb.header.string_data_offset)?;
    writeln!(out, "resource_offset=0x{:x}", psb.header.resource_offset)?;
    writeln!(out, "resource_length_offset=0x{:x}", psb.header.resource_length_offset)?;
    writeln!(out, "resource_data_offset=0x{:x}", psb.header.resource_data_offset)?;
    writeln!(out, "root_offset=0x{:x}", psb.header.root_offset)?;
    match psb.header.extra {
        Some(extra) => writeln!(out, "checksum=0x{extra:08x}")?,
        None => writeln!(out, "checksum=")?,
    }
    match PsbHeader::calculated_checksum_from_header_bytes(&serialize_header_prefix(psb))? {
        Some(calculated) => writeln!(out, "checksum_calculated=0x{calculated:08x}")?,
        None => writeln!(out, "checksum_calculated=")?,
    }
    writeln!(out, "names={}", psb.names.len())?;
    writeln!(out, "strings={}", psb.strings.len())?;
    writeln!(out, "resources={}", psb.resources.len())?;
    for (index, range) in psb.resources.iter().enumerate() {
        writeln_resource_range(&mut out, "resource", index, *range)?;
    }
    writeln!(out, "extra_resources={}", psb.extra_resources.len())?;
    for (index, range) in psb.extra_resources.iter().enumerate() {
        writeln_resource_range(&mut out, "extra_resource", index, *range)?;
    }

    Ok(())
}

fn serialize_header_prefix(psb: &PsbFile) -> Vec<u8> {
    let mut out = Vec::with_capacity(0x2c);
    out.extend_from_slice(&psb.header.signature.to_le_bytes());
    out.extend_from_slice(&psb.header.version.to_le_bytes());
    out.extend_from_slice(&psb.header.flags.to_le_bytes());
    out.extend_from_slice(&psb.header.header_offset.to_le_bytes());
    out.extend_from_slice(&psb.header.name_offset.to_le_bytes());
    out.extend_from_slice(&psb.header.string_offset.to_le_bytes());
    out.extend_from_slice(&psb.header.string_data_offset.to_le_bytes());
    out.extend_from_slice(&psb.header.resource_offset.to_le_bytes());
    out.extend_from_slice(&psb.header.resource_length_offset.to_le_bytes());
    out.extend_from_slice(&psb.header.resource_data_offset.to_le_bytes());
    out.extend_from_slice(&psb.header.root_offset.to_le_bytes());
    if let Some(extra) = psb.header.extra {
        out.extend_from_slice(&extra.to_le_bytes());
    }
    out
}

fn writeln_resource_range(
    out: &mut dyn Write,
    prefix: &str,
    index: usize,
    range: PsbResourceRange,
) -> std::io::Result<()> {
    writeln!(
        out,
        "{prefix}[{index}].offset=0x{:x} {prefix}[{index}].length={}",
        range.offset, range.length
    )
}

fn write_string_list(path: &Path, values: &[String]) -> Result<(), Box<dyn Error>> {
    let mut out = BufWriter::new(File::create(path)?);
    for (index, value) in values.iter().enumerate() {
        writeln!(out, "{index}\t{}", escape_string(value))?;
    }
    Ok(())
}


fn write_emote_schema_dump(output_dir: &Path, psb: &PsbFile) -> Result<(), Box<dyn Error>> {
    let (schema, scene) = load_emote_static_scene(psb)?;

    let mut textures = BufWriter::new(File::create(output_dir.join("emote_textures.txt"))?);
    writeln!(textures, "base_object={}", schema.base_object)?;
    writeln!(textures, "spec={}", schema.spec.as_deref().unwrap_or(""))?;
    for (name, texture) in &schema.textures {
        writeln!(
            textures,
            "texture\t{}\tresource={}\tsize={}x{}\tformat={}\tcompress={}\tbit_count={}",
            name,
            texture.resource_index,
            texture.width,
            texture.height,
            texture.format.as_deref().unwrap_or(""),
            texture.compress.as_deref().unwrap_or(""),
            texture.bit_count.map(|v| v.to_string()).unwrap_or_default()
        )?;
        for (icon_name, icon) in &texture.icons {
            writeln!(
                textures,
                "icon\t{}\t{}\tleft={}\ttop={}\twidth={}\theight={}\torigin=({}, {})\tresolution={}",
                name,
                icon_name,
                icon.left,
                icon.top,
                icon.width,
                icon.height,
                icon.origin_x,
                icon.origin_y,
                icon.resolution
            )?;
        }
    }

    let mut scene_out = BufWriter::new(File::create(output_dir.join("emote_scene.txt"))?);
    writeln!(scene_out, "base_object={}", scene.base_object)?;
    match scene.bounds {
        Some(bounds) => writeln!(
            scene_out,
            "bounds=min({}, {}) max({}, {}) size={}x{}",
            bounds.min_x,
            bounds.min_y,
            bounds.max_x,
            bounds.max_y,
            bounds.width(),
            bounds.height()
        )?,
        None => writeln!(scene_out, "bounds=")?,
    }
    for (index, sprite) in scene.sprites.iter().enumerate() {
        writeln!(
            scene_out,
            concat!(
                "sprite[{}]\tmotion={}\tlabel={}\ttexture={}\tresource={}\ticon={}\t",
                "visible={}\topacity={}\tz={}\tcenter=({}, {})\tsize={}x{}\t",
                "uv=({}, {})-({}, {})"
            ),
            index,
            sprite.motion_name,
            sprite.label.as_deref().unwrap_or(""),
            sprite.texture_name,
            sprite.texture_resource_index,
            sprite.icon_name,
            sprite.visible,
            sprite.opacity,
            sprite.z,
            sprite.center_x,
            sprite.center_y,
            sprite.width,
            sprite.height,
            sprite.uv_left,
            sprite.uv_top,
            sprite.uv_right,
            sprite.uv_bottom
        )?;
    }

    Ok(())
}

fn write_root_dump(path: &Path, root: &PsbValue) -> Result<(), Box<dyn Error>> {
    let mut out = BufWriter::new(File::create(path)?);
    write_value(&mut out, root, 0)?;
    writeln!(out)?;
    Ok(())
}

fn write_schema_dump(path: &Path, root: &PsbValue) -> Result<(), Box<dyn Error>> {
    let mut out = BufWriter::new(File::create(path)?);
    for entry in collect_schema_paths(root) {
        match entry.len {
            Some(len) => writeln!(out, "{}\t{:?}\tlen={}", entry.path, entry.kind, len)?,
            None => writeln!(out, "{}\t{:?}", entry.path, entry.kind)?,
        }
    }
    Ok(())
}

fn write_resource_refs(path: &Path, root: &PsbValue) -> Result<(), Box<dyn Error>> {
    let refs = collect_resource_refs(root);
    let mut out = BufWriter::new(File::create(path)?);
    writeln!(out, "resources={}", refs.resources.len())?;
    for index in refs.resources {
        writeln!(out, "resource({index})")?;
    }
    writeln!(out, "extra_resources={}", refs.extra_resources.len())?;
    for index in refs.extra_resources {
        writeln!(out, "extra_resource({index})")?;
    }
    Ok(())
}

fn extract_resource_group(
    data: &[u8],
    ranges: &[PsbResourceRange],
    output_dir: &Path,
    prefix: &str,
) -> Result<(), Box<dyn Error>> {
    for (index, range) in ranges.iter().enumerate() {
        let bytes = range_bytes(data, *range)?;
        let path = output_dir.join(format!("{prefix}_{index:04}.bin"));
        fs::write(path, bytes)?;
    }
    Ok(())
}

fn range_bytes(data: &[u8], range: PsbResourceRange) -> Result<&[u8], Box<dyn Error>> {
    let start = usize::try_from(range.offset)?;
    let length = usize::try_from(range.length)?;
    let end = start.checked_add(length).ok_or_else(|| make_io_error(ErrorKind::InvalidData, "resource range overflow"))?;
    data.get(start..end).ok_or_else(|| make_error(ErrorKind::InvalidData, "resource range outside source data"))
}

fn write_value(out: &mut dyn Write, value: &PsbValue, indent: usize) -> std::io::Result<()> {
    match value {
        PsbValue::Null => write!(out, "null"),
        PsbValue::Bool(v) => write!(out, "{v}"),
        PsbValue::Int(v) => write!(out, "{v}"),
        PsbValue::Float(v) => write!(out, "{v:?}f"),
        PsbValue::Double(v) => write!(out, "{v:?}"),
        PsbValue::String(v) => write!(out, "\"{}\"", escape_string(v)),
        PsbValue::Resource(index) => write!(out, "resource({index})"),
        PsbValue::ExtraResource(index) => write!(out, "extra_resource({index})"),
        PsbValue::Compiler(tag) => write!(out, "compiler({tag:?})"),
        PsbValue::List(values) => {
            writeln!(out, "[")?;
            for value in values {
                write_indent(out, indent + 2)?;
                write_value(out, value, indent + 2)?;
                writeln!(out, ",")?;
            }
            write_indent(out, indent)?;
            write!(out, "]")
        }
        PsbValue::Object(fields) => {
            writeln!(out, "{{")?;
            for (key, value) in fields {
                write_indent(out, indent + 2)?;
                write!(out, "{}: ", escape_key(key))?;
                write_value(out, value, indent + 2)?;
                writeln!(out, ",")?;
            }
            write_indent(out, indent)?;
            write!(out, "}}")
        }
    }
}

fn write_indent(out: &mut dyn Write, indent: usize) -> std::io::Result<()> {
    for _ in 0..indent {
        write!(out, " ")?;
    }
    Ok(())
}

fn escape_key(value: &str) -> String {
    if value.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        value.to_owned()
    } else {
        format!("\"{}\"", escape_string(value))
    }
}

fn escape_string(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}
