//! KAGEX option descriptors in the windowed shell — the consumer side of
//! `crates/krkr-plugins/src/kagexopt.rs` (`docs/plugins/kagexopt.md`).
//!
//! The reference engine keeps three things apart, and this module is the
//! first two:
//!
//! 1. **The option list.** The config dialog merges the engine's own
//!    descriptors with every loaded plugin's (`TVPGetPluginCommandDesc` +
//!    `TVPMargeCommandDesc`, `krkrz/environ/win32/ConfigFormUnit.cpp:127-145`,
//!    `:305-399`), renders the `user:true` ones, and reads each option's
//!    current value back out of the command line
//!    (`TVPGetCommandLine(L"-" + option.Name, …)`, `:358-363`).
//! 2. **The chosen values become arguments.** `SaveSetting` writes every
//!    option whose selection differs from its default into
//!    `<datapath>/<exe>.cfu` (`:204-254`); at startup the engine pushes the
//!    command line, that file and the exe's own `.cf` into the argument stock
//!    (`PushConfigFileOptions`, `krkrz/base/win32/SysInitImpl.cpp:1651-1703`),
//!    so `System.getArgument("-<name>")` answers before any script runs.
//! 3. **The window group is applied by the game's own scripts**, not by the
//!    engine: the only readers of `fullscreenmode`/`maximizemode`/
//!    `maximizezoom`/`mzpercent`/`restoremaximizebyf2w`/`restorewindowpos` in
//!    either reference tree are the KAG3EX/KAGEX window scripts
//!    (`kag3ex3/template/system/MainWindow.tjs:1532`, `:2006`, `:2046-2058`,
//!    `:1591`, `:1053`), which map the argument onto the `Window` object the
//!    shell mirrors onto the host window. So nothing here maps an option onto
//!    winit directly — doing that as well would apply the option twice.
//!
//! What the shell does, then: `--options` renders the descriptors the linked
//! plugins declare (the dialog's list), `--option <name>=<value>` stores a
//! selection in the project's per-user config file (the dialog's save), and
//! every launch installs that file into the engine's argument stock before
//! `start_project()` runs a single script. A plugin the profile did not link
//! contributes no descriptors here either (`linked_option_categories`), so a
//! game whose install ships no `kagexopt.dll` gets neither the list nor the
//! arguments.
//!
//! The file is the games' own, not one only this shell knows about: KAGEX's
//! `changeUserConf` (`sysscn/Override.tjs`) writes `System.dataPath` +
//! `Storages.chopStorageExt(Storages.extractStorageName(System.exeName))` +
//! `".cfu"` with `name="\xNN"` lines, i.e. exactly the reference's
//! `ApplicationSpecialPath::GetUserConfigFileName`
//! (`krkrz/environ/win32/ApplicationSpecialPath.h:79-82`) and
//! `ConfigFormUnit::EncodeString` (`:287-300`) shape. Running the game in this
//! shell writes `savedata/krkr-desktop.cfu`, so that — the running
//! executable's name, not the project directory's — is the file this module
//! reads and writes. Its choices (`dbstyle`, `contfreq`, …) were silently
//! dropped by the engine before this consumer existed.
//!
//! The reference's `.cf` layer (a config file *next to the executable*) has no
//! analogue here: our executable lives in a build directory, not in the game's
//! data directory, and the games ship no such file.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use krkr_engine::KrkrEngine;
use krkr_plugins::{OptionCategory, OptionDesc, linked_option_categories};

/// `--option <name>=<value>`: one selection from the shell's own command line,
/// the way the reference's dialog hands one to `ConfigFormUnit::SaveSetting`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OptionSelection {
    pub(crate) name: String,
    pub(crate) raw: String,
}

/// The value of a `--option` argument: `name=value`, with the leading dash of
/// the argument spelling optional (`--option -unseenskip=yes` reads the same
/// as `--option unseenskip=yes`). An empty value names the descriptor's unset
/// choice, which stores nothing.
pub(crate) fn parse_selection(text: &str) -> Result<OptionSelection, String> {
    let Some((name, raw)) = text.split_once('=') else {
        return Err(format!(
            "`--option` wants <name>=<value>, got `{text}`; run `--options` for the list"
        ));
    };
    let name = name.trim_start_matches('-');
    if name.is_empty() {
        return Err(format!("`--option` has no option name in `{text}`"));
    }
    Ok(OptionSelection {
        name: name.to_string(),
        raw: raw.to_string(),
    })
}

/// The project's per-user configuration file:
/// `<data path>/<executable name>.cfu`. `data path` is the `savedata/` storage
/// the shell hands the engine (`SystemPaths::data_path`), which is the
/// reference's `$(exepath)\savedata` default for `GetDataPathDirectory`
/// (`krkrz/environ/win32/ApplicationSpecialPath.h:53-78`), and the name is the
/// running executable's file stem, which is what `GetUserConfigFileName` uses
/// and what the games themselves ask `System.exeName` for. Two shells running
/// the same project therefore keep separate option files, exactly as two
/// executables would in the reference.
pub(crate) fn user_config_path(root: &Path) -> PathBuf {
    root.join("savedata")
        .join(format!("{}.cfu", executable_stem()))
}

fn executable_stem() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "krkr".to_string())
}

/// Installs the project's per-user config file into the engine's argument
/// stock — [`KrkrHost::install_config_arguments`](krkr_engine::KrkrHost),
/// which is the reference's `PushConfigFileOptions` reader. Call it after the
/// plugins are registered and before `start_project()`, which is when the
/// reference does it (`TVPInitProgramArgumentsAndDataPath`,
/// `krkrz/base/win32/SysInitImpl.cpp:1651-1703`).
///
/// Returns `(path, installed arguments)` when the file was read, so the shell
/// can log what actually took effect; a missing file is not an error (the
/// games create it when a setting changes) and is skipped. An argument already
/// on the command line wins, so the file only fills in what was not given.
pub(crate) fn install_project_options(
    engine: &mut KrkrEngine,
    root: &Path,
) -> Vec<(PathBuf, Vec<String>)> {
    let path = user_config_path(root);
    let Ok(contents) = fs::read(&path) else {
        return Vec::new();
    };
    // A file we cannot decode is a file we leave alone rather than read as
    // mojibake: report it as read-with-nothing-applied.
    let installed = decode_config(&contents)
        .map(|text| engine.host_mut().install_config_arguments(&text))
        .unwrap_or_default();
    vec![(path, installed)]
}

/// Renders `selection` against the linked plugins' descriptors and stores it
/// in the project's per-user config file, the way `ConfigFormUnit::SaveSetting`
/// and the games' own `changeUserConf` store theirs. A selection whose value
/// is the descriptor's unset entry removes the line instead (「未設定」は設定を
/// 変更しません).
///
/// The file's other lines are preserved verbatim and the file keeps the
/// encoding it already has (a new one is created the way the games' writer
/// creates it — UTF-16LE with a BOM, see [`decode_config`]), so a file that
/// also carries the game's own engine options survives the edit and stays
/// readable by the game.
pub(crate) fn persist_selection(
    engine: &KrkrEngine,
    root: &Path,
    selection: &OptionSelection,
) -> Result<PathBuf, String> {
    if selection_argument(engine, &selection.name, &selection.raw)?.is_none() {
        return store_selection(root, &selection.name, None);
    }
    store_selection(root, &selection.name, Some(&selection.raw))
}

fn store_selection(root: &Path, name: &str, raw: Option<&str>) -> Result<PathBuf, String> {
    let path = user_config_path(root);
    let previous = fs::read(&path).ok();
    let encoding = ConfigEncoding::of(previous.as_deref());
    let contents = match previous.as_deref() {
        // A file this shell cannot decode is a file it must not rewrite: it
        // may be a game's or a player's, in an encoding we do not know, and
        // dropping its lines would lose settings silently.
        Some(bytes) => decode_config(bytes).ok_or_else(|| {
            format!(
                "{} is not UTF-8 or BOM-marked UTF-16; refusing to rewrite it",
                path.display()
            )
        })?,
        None => String::new(),
    };
    let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();
    let position = lines
        .iter()
        .position(|line| option_name_of(line) == Some(name));
    let line = raw.map(|raw| format!("{name}={}", escaped_value(raw)));
    match (position, line) {
        (Some(index), Some(line)) => lines[index] = line,
        (None, Some(line)) => lines.push(line),
        (Some(index), None) => {
            lines.remove(index);
        }
        (None, None) => {}
    }
    if lines.is_empty() {
        // The reference's dialog banner (`ConfigFormUnit::SaveSetting`,
        // `:230-242`); its reader and the games' array reader both skip `;`
        // lines.
        lines.push(
            "; Kirakira option selections: each line is <name>=<value>, read at startup"
                .to_string(),
        );
        lines.push(
            "; as `-<name>=<value>` and seen by scripts as System.getArgument(\"-<name>\")."
                .to_string(),
        );
    }
    let mut body = String::new();
    for line in &lines {
        body.push_str(line);
        body.push('\n');
    }
    write_file(&path, &encode_config(&body, encoding))
        .map_err(|error| format!("failed to store `{name}` in {}: {error}", path.display()))?;
    Ok(path)
}

/// The option name a config line declares, or `None` for a comment or blank
/// line.
fn option_name_of(line: &str) -> Option<&str> {
    let line = line.trim_end_matches('\r');
    if line.trim_start().starts_with(';') || line.trim().is_empty() {
        return None;
    }
    let name = line.split('=').next().unwrap_or_default().trim();
    (!name.is_empty()).then_some(name)
}

/// The value as the reference's dialog and the games' `changeUserConf` both
/// write it: `"\xNN"` per UTF-16 code unit
/// (`ConfigFormUnit::EncodeString`, `krkrz/environ/win32/ConfigFormUnit.cpp:287-300`;
/// `"\\x%X".sprintf(...)`, `sysscn/Override.tjs`).
fn escaped_value(raw: &str) -> String {
    let mut value = String::from("\"");
    for unit in raw.encode_utf16() {
        value.push_str(&format!("\\x{unit:X}"));
    }
    value.push('"');
    value
}

/// The encoding of a per-user config file in the wild.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigEncoding {
    /// What the games' `changeUserConf` writes through `Array.save` — observed
    /// as `savedata/krkr-desktop.cfu` after a live boot, BOM included.
    Utf16Le,
    /// What the reference engine's dialog writes, and what a hand-edited file
    /// is.
    Utf8,
}

impl ConfigEncoding {
    fn of(contents: Option<&[u8]>) -> Self {
        match contents {
            None | Some([0xFF, 0xFE, ..]) => Self::Utf16Le,
            Some(_) => Self::Utf8,
        }
    }
}

/// Decodes a per-user config file: UTF-16LE or UTF-16BE when it opens with a
/// BOM (the games' writer), UTF-8 otherwise (the reference's dialog, and any
/// hand-edited file). `None` when the bytes are neither — a file this shell
/// must not rewrite blindly.
fn decode_config(contents: &[u8]) -> Option<String> {
    match contents {
        [0xFF, 0xFE, rest @ ..] => Some(decode_utf16(rest, true)),
        [0xFE, 0xFF, rest @ ..] => Some(decode_utf16(rest, false)),
        _ => std::str::from_utf8(contents).ok().map(str::to_string),
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units = bytes.chunks_exact(2).map(|pair| {
        let pair = [pair[0], pair[1]];
        if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    });
    char::decode_utf16(units)
        .map(|unit| unit.unwrap_or('\u{FFFD}'))
        .collect()
}

fn encode_config(text: &str, encoding: ConfigEncoding) -> Vec<u8> {
    match encoding {
        ConfigEncoding::Utf16Le => {
            let mut bytes = vec![0xFF, 0xFE];
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            bytes
        }
        ConfigEncoding::Utf8 => text.as_bytes().to_vec(),
    }
}

fn write_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        // The reference ensures the data path directory before it reads a
        // config file back (`TVPEnsureDataPathDirectory`,
        // `krkrz/base/win32/SysInitImpl.cpp:1589-1601`).
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

/// The `-<name>=<value>` argument one selection renders, validated against the
/// descriptors the *linked* plugins declare: `Ok(None)` for the descriptor's
/// unset entry (no argument — the setting stays as it is), `Err` for a name no
/// linked plugin declares or a value the descriptor does not list, which is
/// what the reference's dialog can offer too (it renders exactly these values).
fn selection_argument(
    engine: &KrkrEngine,
    name: &str,
    raw: &str,
) -> Result<Option<String>, String> {
    let categories = linked_option_categories(engine);
    let Some(option) = descriptor(&categories, name) else {
        return Err(format!(
            "`{name}` is not an option any linked plugin declares; run `--options` for the list"
        ));
    };
    match option.value(raw) {
        Some(value) if value.is_unset() => Ok(None),
        Some(_) => Ok(option.command_line_argument(raw)),
        None => Err(format!(
            "`{name}` does not list `{raw}`; it offers {}",
            values_of(option)
        )),
    }
}

fn descriptor(categories: &[&'static OptionCategory], name: &str) -> Option<&'static OptionDesc> {
    categories
        .iter()
        .flat_map(|category| category.options)
        .find(|option| option.name == name)
}

fn values_of(option: &OptionDesc) -> String {
    option
        .values
        .iter()
        .map(|value| value.value.unwrap_or("unset"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The option list `--options` prints: every descriptor the linked plugins
/// declare, with the value the engine would answer right now and the choices
/// the reference's dialog offers. `root` is optional because the list is worth
/// printing without a project; with one, its config file is installed first so
/// the printed values are the ones the run would see.
pub(crate) fn option_listing(engine: &KrkrEngine, root: Option<&Path>) -> String {
    let categories = linked_option_categories(engine);
    let mut listing = String::new();
    listing.push_str(
        "option descriptors of the linked plugins (the reference's config dialog list).\n\
         A selection is stored as `-<name>=<value>` and read back by scripts as\n\
         System.getArgument(\"-<name>\"); the extended-window group is applied by the game's\n\
         own KAG window scripts, not by this shell.\n",
    );
    match root {
        Some(root) => listing.push_str(&format!(
            "project file: {}\n",
            user_config_path(root).display()
        )),
        None => listing.push_str("no project given: values come from the command line only\n"),
    }
    if categories.is_empty() {
        listing.push_str(
            "no linked plugin declares option descriptors (a profile without kagexopt.dll has none)\n",
        );
        return listing;
    }
    let options: usize = categories
        .iter()
        .map(|category| category.options.len())
        .sum();
    listing.push_str(&format!(
        "{} categories, {options} options\n",
        categories.len()
    ));
    for category in categories {
        listing.push_str(&format!("\n{}\n", category.name));
        for option in category.options {
            let argument = format!("-{}", option.name);
            let current = engine
                .host()
                .command_argument(&argument)
                .unwrap_or_else(|| "unset".to_string());
            let default = option
                .default_value()
                .map(|value| value.value.unwrap_or("unset"))
                .unwrap_or("none");
            let player = if option.user {
                ""
            } else {
                " [not offered to the player]"
            };
            listing.push_str(&format!(
                "  {argument}: current {current}; values {}; default {default}{player}\n",
                values_of(option)
            ));
        }
    }
    listing
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, path::PathBuf, sync::atomic::AtomicU64};

    use krkr_engine::{EngineConfig as KrkrEngineConfig, KrkrEngine};
    use krkr_plugins::{GameProfile, register_profile_plugins, register_reference_plugins};

    use super::*;

    /// Each test gets its own project directory: two tests writing the same
    /// per-user file in one directory would race.
    fn scratch_project(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "krkr-desktop-options-{name}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("project directory");
        root
    }

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(KrkrEngineConfig::default()).expect("engine");
        register_reference_plugins(&mut engine).expect("plugins");
        engine
    }

    /// A project config file as the games' `changeUserConf` leaves it:
    /// UTF-16LE with a BOM, `name="\xNN"` lines.
    fn write_game_config(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("savedata");
        fs::write(path, encode_config(text, ConfigEncoding::Utf16Le)).expect("config file");
    }

    fn read_text(path: &Path) -> String {
        decode_config(&fs::read(path).expect("config file")).expect("decodable")
    }

    #[test]
    fn the_project_config_file_is_named_after_the_running_executable() {
        let root = PathBuf::from("/games/PARQUET");
        let path = user_config_path(&root);
        assert_eq!(
            path.parent(),
            Some(PathBuf::from("/games/PARQUET/savedata").as_path())
        );
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("cfu"));
        assert_eq!(
            path.file_stem().and_then(|stem| stem.to_str()),
            Some(executable_stem().as_str()),
            "the games ask `System.exeName` for this name"
        );
    }

    #[test]
    fn a_selection_is_validated_against_the_linked_registry() {
        let engine = engine();
        assert_eq!(
            selection_argument(&engine, "unseenskip", "yes").expect("listed value"),
            Some("-unseenskip=yes".to_string())
        );
        assert_eq!(
            selection_argument(&engine, "mzpercent", "50").expect("listed value"),
            Some("-mzpercent=50".to_string())
        );
        // The unset choice stores no argument at all.
        assert_eq!(
            selection_argument(&engine, "unseenskip", "").expect("unset choice"),
            None
        );
        // A value the descriptor does not list, and a name no linked plugin
        // declares, are both refused — the dialog cannot produce either.
        let error = selection_argument(&engine, "mzpercent", "51").expect_err("unlisted value");
        assert!(error.contains("does not list `51`"), "{error}");
        let error = selection_argument(&engine, "nosuchoption", "yes").expect_err("unknown name");
        assert!(error.contains("not an option"), "{error}");
    }

    /// The window half of the reference's chain, at the level this shell can
    /// test without a window manager: the selection stored for the project is
    /// what the game's own KAG window script reads, and `Window.fullScreen` —
    /// which the shell mirrors onto the winit window every frame
    /// (`MainWindow.tjs`'s `fullScreenMode`/`isPseudoMode`, see the module
    /// docs) — follows from it.
    #[test]
    fn a_stored_window_option_reaches_the_script_and_the_window_state() {
        let root = scratch_project("window-option");
        let mut engine = engine();
        // A project file may carry any engine option, such as the KAGEX
        // `-bootfullscreen` the games' own menu persists; the descriptor-backed
        // one goes through the `--option` path, which preserves the rest.
        write_game_config(
            &user_config_path(&root),
            "bootfullscreen=\"\\x79\\x65\\x73\"\n",
        );
        persist_selection(
            &engine,
            &root,
            &parse_selection("fullscreenmode=primaryonly").expect("selection"),
        )
        .expect("stored");

        let installed = install_project_options(&mut engine, &root);
        assert_eq!(installed.len(), 1, "one config file");
        assert_eq!(
            installed[0].1,
            ["-bootfullscreen=yes", "-fullscreenmode=primaryonly"]
        );

        engine
            .execute_script("inline.tjs", "global.kag = new Window();")
            .expect("window");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var boot = System.getArgument("-bootfullscreen");
                var initial = (boot != "" && boot == "yes");
                var raw = System.getArgument("-fullscreenmode");
                var map = %[ auto:-1, primaryonly:0, usepseudo:1, pseudoall:3 ];
                var mode = (raw != "" && typeof map[raw] != "undefined") ? map[raw] : -1;
                var pseudo = (mode == 1 || mode == 3);
                kag.fullScreen = initial && !pseudo;
                return System.getArgument("-fullscreenmode") + ":" + kag.fullScreen;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            krkr_tjs2::runtime::Variant::String("primaryonly:1".into())
        );
        assert!(
            engine.window_fullscreen(),
            "the shell mirrors `Window.fullScreen` onto the winit window"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// The file the games' own settings menu writes is the file this shell
    /// reads: `savedata/krkr-desktop.cfu` after a live boot holds the KAGEX
    /// `SystemArgumentInfo` choices (`dbstyle`, `contfreq`, …) in UTF-16LE with
    /// `\xNN` values, and none of them reached a script before this consumer.
    #[test]
    fn a_game_written_config_file_reaches_the_arguments() {
        let root = scratch_project("game-written");
        let mut engine = engine();
        write_game_config(
            &user_config_path(&root),
            "; ============================================================================\n\
             ; *DO NOT EDIT* this file unless you are understanding what you are doing.\n\
             curmove=\"\\x79\\x65\\x73\"\n\
             contfreq=\"\\x36\\x30\"\n\
             dbstyle=\"\\x64\\x33\\x64\"\n",
        );

        let installed = install_project_options(&mut engine, &root);
        assert_eq!(
            installed[0].1,
            ["-curmove=yes", "-contfreq=60", "-dbstyle=d3d"]
        );
        let value = engine
            .execute_script("inline.tjs", "return System.getArgument(\"-dbstyle\");")
            .expect("script");
        assert_eq!(value, krkr_tjs2::runtime::Variant::String("d3d".into()));

        // The command line outranks the file, and an argument the file already
        // installed is not installed twice.
        engine.host_mut().set_command_argument("-contfreq", "30");
        let installed = install_project_options(&mut engine, &root);
        assert!(installed[0].1.is_empty(), "{:?}", installed[0].1);
        assert_eq!(
            engine.host().command_argument("-contfreq").as_deref(),
            Some("30")
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Storing a selection keeps the rest of the file — the game's own lines
    /// and their encoding — and the unset choice removes only its own line.
    #[test]
    fn storing_and_reading_back_a_selection_keeps_the_rest_of_the_file() {
        let root = scratch_project("round-trip");
        let engine = engine();
        write_game_config(
            &user_config_path(&root),
            "; as the game leaves it\n\
             vomstyle=\"\\x6c\\x61\\x79\\x65\\x72\"\n\
             unseenskip=\"\\x79\\x65\\x73\"\n",
        );

        persist_selection(
            &engine,
            &root,
            &parse_selection("fullscreenmode=usepseudo").expect("selection"),
        )
        .expect("stored");
        persist_selection(
            &engine,
            &root,
            &parse_selection("unseenskip=").expect("selection"),
        )
        .expect("stored");

        let contents = read_text(&user_config_path(&root));
        assert!(
            contents.contains("vomstyle=\"\\x6c\\x61\\x79\\x65\\x72\""),
            "the untouched line survives: {contents}"
        );
        assert!(
            contents.contains("fullscreenmode=\"\\x75\\x73\\x65\\x70\\x73\\x65\\x75\\x64\\x6F\""),
            "the selection is stored in the reference's escaped form: {contents}"
        );
        assert!(!contents.contains("unseenskip"), "{contents}");
        assert_eq!(
            ConfigEncoding::of(Some(&fs::read(user_config_path(&root)).expect("file"))),
            ConfigEncoding::Utf16Le,
            "the encoding the game wrote is preserved"
        );

        let mut engine = engine;
        let installed = install_project_options(&mut engine, &root);
        assert_eq!(
            installed[0].1,
            ["-vomstyle=layer", "-fullscreenmode=usepseudo"]
        );
        assert_eq!(engine.host().command_argument("-unseenskip"), None);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_new_config_file_is_created_the_way_the_games_write_it() {
        let root = scratch_project("fresh-file");
        let engine = engine();
        persist_selection(
            &engine,
            &root,
            &parse_selection("maximizemode=fullscreen").expect("selection"),
        )
        .expect("stored");

        let bytes = fs::read(user_config_path(&root)).expect("file");
        assert_eq!(&bytes[..2], &[0xFF, 0xFE], "UTF-16LE with a BOM");
        let text = decode_config(&bytes).expect("decodable");
        assert!(
            text.contains("maximizemode=\"\\x66\\x75\\x6C\\x6C\\x73\\x63\\x72\\x65\\x65\\x6E\""),
            "{text}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_config_file_this_shell_cannot_decode_is_not_rewritten() {
        let root = scratch_project("undecodable");
        let engine = engine();
        // Shift-JIS text, say: readable for a player, not something this shell
        // decodes — and rewriting it from an empty decode would drop settings.
        let original: &[u8] = b"; \x83\x4c\x83\x8b\x83\x4c\x83\x8b\n";
        fs::create_dir_all(user_config_path(&root).parent().expect("savedata")).expect("savedata");
        fs::write(user_config_path(&root), original).expect("file");

        let error = persist_selection(
            &engine,
            &root,
            &parse_selection("unseenskip=yes").expect("selection"),
        )
        .expect_err("undecodable file");
        assert!(error.contains("refusing to rewrite"), "{error}");
        assert_eq!(
            fs::read(user_config_path(&root)).expect("file"),
            original,
            "the file is left exactly as it was"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_profile_without_kagexopt_offers_and_applies_no_descriptors() {
        let root = scratch_project("unlinked");
        let mut engine = KrkrEngine::new(KrkrEngineConfig::default()).expect("engine");
        register_profile_plugins(&mut engine, &GameProfile::only(["krmovie.dll"]))
            .expect("plugins");

        let listing = option_listing(&engine, Some(&root));
        assert!(!listing.contains("拡張ウィンドウ制御"), "{listing}");
        assert!(listing.contains("movie_reg_rot"), "{listing}");
        let error = persist_selection(
            &engine,
            &root,
            &parse_selection("fullscreenmode=primaryonly").expect("selection"),
        )
        .expect_err("kagexopt.dll is not linked");
        assert!(error.contains("not an option"), "{error}");
        assert!(!user_config_path(&root).exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_listing_renders_the_registry_and_the_current_values() {
        let root = scratch_project("listing");
        let mut engine = engine();
        persist_selection(
            &engine,
            &root,
            &parse_selection("maximizemode=fullscreen").expect("selection"),
        )
        .expect("stored");
        install_project_options(&mut engine, &root);

        let listing = option_listing(&engine, Some(&root));
        for expected in [
            "ゲーム全般",
            "ムービー",
            "拡張ウィンドウ制御",
            "-unseenskip: current unset; values unset, yes, no",
            "-vomstyle: current unset; values auto, overlay, mixer, layer",
            "-fullscreenmode: current unset; values auto, primaryonly, usepseudo, pseudoall; default auto [not offered to the player]",
            "-maximizemode: current fullscreen; values auto, maximize, fullscreen",
            "-mzpercent: current unset",
        ] {
            assert!(
                listing.contains(expected),
                "missing `{expected}`:\n{listing}"
            );
        }
        assert!(listing.contains(&user_config_path(&root).display().to_string()));

        let mut names: BTreeSet<&str> = BTreeSet::new();
        for category in linked_option_categories(&engine) {
            names.extend(category.options.iter().map(|option| option.name));
        }
        assert_eq!(names.len(), 17, "16 kagexopt options plus krmovie's");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_selection_accepts_both_spellings_and_rejects_the_rest() {
        let expected = OptionSelection {
            name: "unseenskip".to_string(),
            raw: "yes".to_string(),
        };
        assert_eq!(parse_selection("unseenskip=yes").expect("plain"), expected);
        assert_eq!(
            parse_selection("-unseenskip=yes").expect("dashed"),
            expected
        );
        assert!(parse_selection("unseenskip").is_err());
        assert!(parse_selection("=yes").is_err());
        assert_eq!(
            parse_selection("unseenskip=").expect("unset spelling").raw,
            ""
        );
    }
}
