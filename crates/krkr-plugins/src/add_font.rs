use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "System.addFont",
    notes: "Registers fonts from game storage through the engine font system.",
    install: |engine| engine.register_plugin(AddFontPlugin),
};

pub struct AddFontPlugin;

impl KrkrPlugin for AddFontPlugin {
    fn name(&self) -> &str {
        "addFont.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let Variant::Object(system) = runtime.global_member("System") else {
            return Err(TjsError::runtime(
                "addFont.dll requires the native System object to be installed first",
            ));
        };
        runtime.register_object_native(system, "addFont", system_add_font);
        register_game_font_tables(runtime);
        Ok(())
    }
}

/// The games ship their own font tables: `embfontlist.tjs` names the file and
/// face of every embedded font, and `deffontmap.tjs` maps legacy names onto
/// those. Handing them to the font system lets a request like
/// `源ノ角ゴシックB` resolve against the files `System.addFont` actually
/// loaded instead of falling back to an arbitrary installed face.
fn register_game_font_tables(runtime: &mut Runtime<KrkrHost>) {
    let mut reads = 0;
    for storage in ["embfontlist.tjs", "font/embfontlist.tjs"] {
        let Ok(bytes) = runtime.host().read_binary_storage(storage) else {
            continue;
        };
        let text = decode_font_table_text(&bytes);
        runtime
            .host_mut()
            .font_system_mut()
            .register_embedded_font_list_text(&text);
        reads += 1;
    }
    for storage in ["deffontmap.tjs", "font/deffontmap.tjs"] {
        let Ok(bytes) = runtime.host().read_binary_storage(storage) else {
            continue;
        };
        let text = decode_font_table_text(&bytes);
        runtime
            .host_mut()
            .font_system_mut()
            .register_font_alias_text(&text);
        reads += 1;
    }
    if reads > 0 {
        runtime.host_mut().log(&format!(
            "addFont.dll registered the game's font tables ({reads} storages)"
        ));
    }
}

/// KAG ships the tables as UTF-16LE source, usually behind a byte-order mark;
/// a project may also keep them as UTF-8.
fn decode_font_table_text(bytes: &[u8]) -> String {
    let decode_utf16_le = |data: &[u8]| {
        let units = data
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&units)
    };
    if let Some(data) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16_le(data);
    }
    let zeros = bytes
        .chunks_exact(2)
        .take(64)
        .filter(|pair| pair[1] == 0)
        .count();
    if zeros > 32 {
        return decode_utf16_le(bytes);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn system_add_font(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(storage) = args.first() else {
        return Err(TjsError::runtime("System.addFont requires a storage name"));
    };
    let storage = storage.to_tjs_string()?;
    let Ok(bytes) = runtime.host().read_binary_storage(&storage) else {
        return Ok(Variant::Void);
    };
    let loaded = runtime
        .host_mut()
        .font_system_mut()
        .load_font_data(storage.clone(), bytes)
        .is_ok();
    if loaded {
        runtime.host_mut().log(&format!(
            "System.addFont loaded through addFont.dll: {storage}"
        ));
    }
    Ok(Variant::Integer(i64::from(loaded)))
}
