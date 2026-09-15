//! kagexopt.dll compatibility registry (`docs/plugins/kagexopt.md`).
//!
//! The DLL is a pure resource plugin: one export (`GetOptionDesc`), no RTTI,
//! `KERNEL32.dll` as its only import and no TJS object — its payload is the
//! option-descriptor JSON resource `IDR_OPTION_DESC_JSON` (type `TEXT`). The
//! reference engine reads that resource to build its option list
//! (`TVPGetPluginCommandDesc` -> `OptionDescReader` -> `tTVPCommandOptionList`,
//! `krkrz/src/core/msg/win32/ReadOptionDesc.cpp`), presents it in the
//! option/config dialog, and hands each chosen value to the game as a
//! `-<name>=<value>` command-line option. The plugin has no runtime behaviour,
//! so registering it installs no TJS surface: the compatible surface is the
//! recovered descriptor table itself, [`KagexOptPlugin::categories`].
//!
//! All sixteen options are `select`; a value the JSON gives as `null`/`""` is
//! the "unset" choice, which leaves the current setting unchanged (the DLL's
//! descriptions say 「未設定」は設定を変更しません), so
//! [`OptionDesc::command_line_argument`] renders no argument for it — and none
//! for a value the option does not list. Consuming that argument is the
//! engine's side, where any `-<name>=<value>` is already accepted and read
//! back with `System.getArgument("-<name>")`.
//!
//! # What the reference does with a descriptor (and what we do instead)
//!
//! The engine side of `docs/plugins/kagexopt.md` has three links, and the
//! reference keeps all three outside the game scripts it ships:
//!
//! 1. **The option list.** `ConfigFormUnit::LoadOptionTree` merges the engine's
//!    own descriptors with every plugin's (`TVPGetPluginCommandDesc` +
//!    `TVPMargeCommandDesc`, `krkrz/environ/win32/ConfigFormUnit.cpp:127-145`,
//!    `:305-399`) and renders the `user:true` ones; the selected index reads
//!    the *current* value back from the argument stock
//!    (`TVPGetCommandLine(L"-" + option.Name, …)`, `:358-363`).
//! 2. **The chosen values become arguments.** `SaveSetting` writes every
//!    option whose selection differs from its default into
//!    `<datapath>/<exe>.cfu` (`:204-254`), and at startup the engine pushes
//!    the command line, that file and the exe's own `.cf` into the argument
//!    stock (`PushConfigFileOptions`, `krkrz/base/win32/SysInitImpl.cpp:1625-1634`)
//!    before any script runs, so `System.getArgument("-<name>")` answers
//!    (`SystemImpl.cpp:848-861`).
//! 3. **The window group is applied by the game's scripts, not the engine.**
//!    Neither the krkrz nor the krkr2 engine reads `fullscreenmode`,
//!    `maximizemode`, `maximizezoom`, `mzpercent`, `restoremaximizebyf2w` or
//!    `restorewindowpos` — the only consumers are the KAG3EX / KAGEX window
//!    scripts, which map the argument onto the window: `fullScreenMode`
//!    (`kag3ex3/template/system/MainWindow.tjs:1532-1541`), `maximizeMode`
//!    (`:2006-2010`), the `-maximizezoom`/`-mzpercent` zoom choice (`:2046-2058`),
//!    `-restoremaximizebyf2w` (`:1591`), `-restorewindowpos` (`:1053`) and
//!    `-bootfullscreen`. A game that ships those scripts (PARQUET, GINKA and
//!    少女世界的生存之道 all do) therefore changes its own window once the
//!    argument is readable; the engine's part is exactly link 2.
//!
//! Our shell mirrors that split: `apps/desktop` renders the linked plugins'
//! categories and persists a selection into the project's per-user
//! `<exe name>.cfu` (`--options`, `--option`), and installs the file's lines
//! into the engine's argument stock before `start_project()` — see
//! `apps/desktop/src/options.rs`. That file is the games' own: KAGEX's
//! `changeUserConf` writes `System.dataPath + <exe name> + ".cfu"` with
//! `name="\xNN"` lines, so the `dbstyle`/`contfreq`-style choices a player
//! makes in the game's menu are part of what the engine reads back.
//! Nothing is mapped onto winit directly, because that is the game's job in
//! the reference and doing both would apply the option twice.
//!
//! Not in this module: the movie machinery's `vomstyle` switch — movies are
//! always drawn into layers here, which is why
//! [`KagexOptPlugin::movie_display_method`] degrades every listed method to
//! `layer`. `yuzuex.dll` embeds the same three categories; the descriptors
//! live here once, beside the DLL that exports `GetOptionDesc`, so the
//! reference engine's merge by category name (`TVPMargeCommandDesc`) has
//! nothing to dedupe in this crate.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{Result, runtime::Runtime};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "GetOptionDesc option descriptors (ゲーム全般 / ムービー / 拡張ウィンドウ制御)",
    notes: "The DLL's whole payload is the recovered descriptor registry: 16 select options in 3 categories with their values and defaults (KagexOptPlugin::categories). Registration installs no TJS surface, because the reference has none. The registry now has the reference's consumers: linked_option_categories() feeds apps/desktop's option surface (--options / --option <name>=<value>), which stores a selection in the project's per-user <exe>.cfu — the file the games' own changeUserConf writes — and installs it into the engine's argument stock before start_project(), so a chosen -<name>=<value> answers System.getArgument. The 拡張ウィンドウ制御 values are then applied by the games' own KAG window scripts, not by this shell. Movies are always drawn into layers, so every recovered vomstyle value degrades to layer. yuzuex.dll embeds the same categories; the table lives here once.",
    install: |engine| engine.register_plugin(KagexOptPlugin),
};

pub struct KagexOptPlugin;

impl KrkrPlugin for KagexOptPlugin {
    fn name(&self) -> &str {
        "kagexopt.dll"
    }

    fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        Ok(())
    }
}

/// The DLL's payload, for the engine side that builds an option list from it:
/// every recovered category, the way `OptionDescReader` reads it out of the
/// embedded JSON.
///
/// The registry is reached through the plugin type because that is the one
/// name `lib.rs` re-exports today; a consumer outside this crate needs the
/// registry types re-exported beside it first.
impl KagexOptPlugin {
    /// The three recovered categories, in the dossier's order.
    pub fn categories() -> &'static [OptionCategory] {
        OPTION_CATEGORIES
    }

    /// The descriptor named `name`. Names are the command-line spellings
    /// (`-<name>`), compared case-sensitively like `TVPGetCommandLine` does.
    pub fn option(name: &str) -> Option<&'static OptionDesc> {
        OPTION_CATEGORIES
            .iter()
            .flat_map(|category| category.options)
            .find(|option| option.name == name)
    }

    /// The movie display method this engine implements for a `vomstyle`
    /// selection. `vomstyle=layer` is the only method that exists here —
    /// movies present as layer quads — so the DirectShow-era `auto`, `overlay`
    /// and `mixer` values degrade to it (the dossier's porting outline), and
    /// only the *unset* choice, which changes nothing, or a value the option
    /// does not list yields `None`.
    pub fn movie_display_method(raw: &str) -> Option<&'static str> {
        let value = Self::option("vomstyle")?.value(raw)?;
        (!value.is_unset()).then_some("layer")
    }
}

/// An option `type` in the descriptor JSON (`OptionDescReader` parses
/// `select` and `string`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptionKind {
    /// A fixed value list (`values[]`). Every option kagexopt ships is a
    /// `select`; the `string` type (with its `length`) does not occur.
    Select,
}

/// One `values[]` entry of a `select` option.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OptionValue {
    /// The value as the reference stores it. `None` is the JSON `null`/`""`
    /// entry — the "unset" choice, which leaves the current setting unchanged.
    pub value: Option<&'static str>,
    /// The entry's description from the DLL's JSON; `None` where the dossier
    /// recovered none (the Japanese strings in the table are verbatim).
    pub desc: Option<&'static str>,
    /// Whether the dossier marks this entry as the option's default (★).
    pub default: bool,
}

impl OptionValue {
    /// True for the `null`/`""` choice: a selection that leaves the setting as
    /// it is, so no `-<name>=<value>` argument is produced at all.
    pub fn is_unset(&self) -> bool {
        self.value.is_none()
    }
}

/// One `options[]` entry of a descriptor JSON category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OptionDesc {
    /// The option name; the engine sees it as the command-line name
    /// `-<name>`, what scripts read with `System.getArgument("-<name>")`.
    pub name: &'static str,
    /// The option's `type`; every recovered option is [`OptionKind::Select`].
    pub kind: OptionKind,
    /// The descriptor's `user` flag: whether the option is offered to the
    /// player. `false` on `fullscreenmode` only.
    pub user: bool,
    /// The `values[]` list, in the recovered order.
    pub values: &'static [OptionValue],
}

impl OptionDesc {
    /// The listed choice `raw` names. `""` names the `null`/`""` entry of the
    /// options that have one; `None` means the option lists no such value.
    pub fn value(&self, raw: &str) -> Option<&'static OptionValue> {
        self.values.iter().find(|value| match value.value {
            Some(candidate) => candidate == raw,
            None => raw.is_empty(),
        })
    }

    /// The default choice (★); every recovered option marks exactly one.
    pub fn default_value(&self) -> Option<&'static OptionValue> {
        self.values.iter().find(|value| value.default)
    }

    /// The command-line argument a selected `raw` turns into, in the form the
    /// reference's config dialog emits and this engine reads back:
    /// `-<name>=<value>`. `None` for the unset choice — the setting stays as
    /// it is — and for a `raw` the option does not list.
    pub fn command_line_argument(&self, raw: &str) -> Option<String> {
        let value = self.value(raw)?.value?;
        Some(format!("-{}={}", self.name, value))
    }
}

/// One descriptor category (a `category` block of the JSON, which
/// `TVPMargeCommandDesc` merges by name).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OptionCategory {
    /// Category name as recovered.
    pub name: &'static str,
    /// The category's options, in the recovered order.
    pub options: &'static [OptionDesc],
}

/// `null` / `yes` / `no`, the unset choice being the default: the value list
/// five of the ゲーム全般 options share (no descriptions were recovered for
/// them).
const UNSET_YES_NO: &[OptionValue] = &[
    OptionValue {
        value: None,
        desc: None,
        default: true,
    },
    OptionValue {
        value: Some("yes"),
        desc: None,
        default: false,
    },
    OptionValue {
        value: Some("no"),
        desc: None,
        default: false,
    },
];

/// The complete recovered registry — three categories, sixteen options.
pub const OPTION_CATEGORIES: &[OptionCategory] = &[
    OptionCategory {
        name: "ゲーム全般",
        options: &[
            OptionDesc {
                name: "unseenskip",
                kind: OptionKind::Select,
                user: true,
                values: &[
                    OptionValue {
                        value: None,
                        desc: Some("未設定"),
                        default: true,
                    },
                    OptionValue {
                        value: Some("yes"),
                        desc: Some("する"),
                        default: false,
                    },
                    OptionValue {
                        value: Some("no"),
                        desc: Some("しない"),
                        default: false,
                    },
                ],
            },
            OptionDesc {
                name: "seoffinskip",
                kind: OptionKind::Select,
                user: true,
                values: UNSET_YES_NO,
            },
            OptionDesc {
                name: "stopskipbyselect",
                kind: OptionKind::Select,
                user: true,
                values: UNSET_YES_NO,
            },
            OptionDesc {
                name: "stopskipbyclick",
                kind: OptionKind::Select,
                user: true,
                values: UNSET_YES_NO,
            },
            OptionDesc {
                name: "stopautobyselect",
                kind: OptionKind::Select,
                user: true,
                values: UNSET_YES_NO,
            },
            OptionDesc {
                name: "stopautobyclick",
                kind: OptionKind::Select,
                user: true,
                values: UNSET_YES_NO,
            },
            OptionDesc {
                name: "voicestopbyclick",
                kind: OptionKind::Select,
                user: true,
                values: &[
                    OptionValue {
                        value: None,
                        desc: None,
                        default: true,
                    },
                    OptionValue {
                        value: Some("page"),
                        desc: Some("ページ消去時に停止"),
                        default: false,
                    },
                    OptionValue {
                        value: Some("name"),
                        desc: Some("名前表示時に停止"),
                        default: false,
                    },
                    OptionValue {
                        value: Some("no"),
                        desc: Some("停止しない"),
                        default: false,
                    },
                ],
            },
            OptionDesc {
                name: "bgmdownbyvoice",
                kind: OptionKind::Select,
                user: true,
                // Percent of the BGM volume while a voice plays.
                values: &[
                    OptionValue {
                        value: None,
                        desc: None,
                        default: true,
                    },
                    OptionValue {
                        value: Some("0"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("5"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("10"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("15"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("20"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("25"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("30"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("35"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("40"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("45"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("50"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("55"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("60"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("65"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("70"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("75"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("80"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("85"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("90"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("95"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("100"),
                        desc: None,
                        default: false,
                    },
                ],
            },
            OptionDesc {
                name: "voicespeed",
                kind: OptionKind::Select,
                user: true,
                // Percent playback speed.
                values: &[
                    OptionValue {
                        value: None,
                        desc: None,
                        default: true,
                    },
                    OptionValue {
                        value: Some("100"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("150"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("200"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("250"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("300"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("350"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("400"),
                        desc: None,
                        default: false,
                    },
                ],
            },
        ],
    },
    OptionCategory {
        name: "ムービー",
        options: &[OptionDesc {
            name: "vomstyle",
            kind: OptionKind::Select,
            user: true,
            // `mixer` is the DirectX9 VMR path per the dossier.
            values: &[
                OptionValue {
                    value: Some("auto"),
                    desc: Some("自動"),
                    default: true,
                },
                OptionValue {
                    value: Some("overlay"),
                    desc: Some("オーバーレイ"),
                    default: false,
                },
                OptionValue {
                    value: Some("mixer"),
                    desc: Some("ミキサー"),
                    default: false,
                },
                OptionValue {
                    value: Some("layer"),
                    desc: Some("レイヤー"),
                    default: false,
                },
            ],
        }],
    },
    OptionCategory {
        name: "拡張ウィンドウ制御",
        options: &[
            OptionDesc {
                name: "fullscreenmode",
                kind: OptionKind::Select,
                // The only option the descriptor withholds from the player.
                user: false,
                values: &[
                    OptionValue {
                        value: Some("auto"),
                        desc: None,
                        default: true,
                    },
                    OptionValue {
                        value: Some("primaryonly"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("usepseudo"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("pseudoall"),
                        desc: None,
                        default: false,
                    },
                ],
            },
            OptionDesc {
                name: "maximizemode",
                kind: OptionKind::Select,
                user: true,
                values: &[
                    OptionValue {
                        value: Some("auto"),
                        desc: None,
                        default: true,
                    },
                    OptionValue {
                        value: Some("maximize"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("fullscreen"),
                        desc: None,
                        default: false,
                    },
                ],
            },
            OptionDesc {
                name: "maximizezoom",
                kind: OptionKind::Select,
                user: true,
                values: &[
                    OptionValue {
                        value: Some("inner"),
                        desc: None,
                        default: true,
                    },
                    OptionValue {
                        value: Some("outer"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("middle"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("no"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("fszoom"),
                        desc: None,
                        default: false,
                    },
                ],
            },
            OptionDesc {
                name: "mzpercent",
                kind: OptionKind::Select,
                user: true,
                values: &[
                    OptionValue {
                        value: Some("0"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("5"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("10"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("15"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("20"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("25"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("30"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("35"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("40"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("45"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("50"),
                        desc: None,
                        default: true,
                    },
                    OptionValue {
                        value: Some("55"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("60"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("65"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("70"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("75"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("80"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("85"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("90"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("95"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("100"),
                        desc: None,
                        default: false,
                    },
                ],
            },
            OptionDesc {
                name: "restoremaximizebyf2w",
                kind: OptionKind::Select,
                user: true,
                values: &[
                    OptionValue {
                        value: Some("yes"),
                        desc: None,
                        default: false,
                    },
                    OptionValue {
                        value: Some("no"),
                        desc: None,
                        default: true,
                    },
                ],
            },
            OptionDesc {
                name: "restorewindowpos",
                kind: OptionKind::Select,
                user: true,
                values: &[
                    OptionValue {
                        value: Some("yes"),
                        desc: None,
                        default: true,
                    },
                    OptionValue {
                        value: Some("no"),
                        desc: None,
                        default: false,
                    },
                ],
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::{KagexOptPlugin, OptionDesc, OptionKind, OptionValue};

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(KagexOptPlugin).expect("plugin");
        engine
    }

    fn option(name: &str) -> &'static OptionDesc {
        KagexOptPlugin::option(name).unwrap_or_else(|| panic!("{name} is not in the registry"))
    }

    /// The option's value list as the dossier spells it out: the unset choice
    /// as `null`, everything else verbatim, comma-separated.
    fn values_of(option: &OptionDesc) -> String {
        option
            .values
            .iter()
            .map(|value| value.value.unwrap_or("null"))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn global_names(engine: &KrkrEngine) -> Vec<String> {
        let runtime = engine.tjs_runtime();
        runtime
            .object_members(runtime.global_handle())
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    #[test]
    fn the_categories_and_their_options_match_the_dossier() {
        let expected: &[(&str, &[&str])] = &[
            (
                "ゲーム全般",
                &[
                    "unseenskip",
                    "seoffinskip",
                    "stopskipbyselect",
                    "stopskipbyclick",
                    "stopautobyselect",
                    "stopautobyclick",
                    "voicestopbyclick",
                    "bgmdownbyvoice",
                    "voicespeed",
                ],
            ),
            ("ムービー", &["vomstyle"]),
            (
                "拡張ウィンドウ制御",
                &[
                    "fullscreenmode",
                    "maximizemode",
                    "maximizezoom",
                    "mzpercent",
                    "restoremaximizebyf2w",
                    "restorewindowpos",
                ],
            ),
        ];

        let categories = KagexOptPlugin::categories();
        assert_eq!(categories.len(), expected.len(), "category count");
        for (category, (name, options)) in categories.iter().zip(expected) {
            assert_eq!(category.name, *name, "category name");
            let names: Vec<&str> = category.options.iter().map(|option| option.name).collect();
            assert_eq!(names.as_slice(), *options, "options of {name}");
        }

        assert_eq!(
            categories
                .iter()
                .flat_map(|category| category.options)
                .count(),
            16,
            "every recovered option is registered"
        );
        assert!(KagexOptPlugin::option("nosuchoption").is_none());
    }

    #[test]
    fn every_option_is_a_user_select_with_exactly_one_default() {
        for category in KagexOptPlugin::categories() {
            for option in category.options {
                assert_eq!(option.kind, OptionKind::Select, "{}", option.name);
                assert_eq!(
                    option.user,
                    option.name != "fullscreenmode",
                    "{}: user flag",
                    option.name
                );
                assert!(
                    option.values.iter().filter(|value| value.default).count() == 1,
                    "{}: not exactly one default",
                    option.name
                );
            }
        }
    }

    #[test]
    fn the_value_vocabulary_matches_the_dossier() {
        let percents = (0..=100)
            .step_by(5)
            .map(|percent| percent.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let expected: &[(&str, String)] = &[
            ("unseenskip", "null,yes,no".to_string()),
            ("seoffinskip", "null,yes,no".to_string()),
            ("stopskipbyselect", "null,yes,no".to_string()),
            ("stopskipbyclick", "null,yes,no".to_string()),
            ("stopautobyselect", "null,yes,no".to_string()),
            ("stopautobyclick", "null,yes,no".to_string()),
            ("voicestopbyclick", "null,page,name,no".to_string()),
            ("bgmdownbyvoice", format!("null,{percents}")),
            ("voicespeed", "null,100,150,200,250,300,350,400".to_string()),
            ("vomstyle", "auto,overlay,mixer,layer".to_string()),
            (
                "fullscreenmode",
                "auto,primaryonly,usepseudo,pseudoall".to_string(),
            ),
            ("maximizemode", "auto,maximize,fullscreen".to_string()),
            ("maximizezoom", "inner,outer,middle,no,fszoom".to_string()),
            ("mzpercent", percents),
            ("restoremaximizebyf2w", "yes,no".to_string()),
            ("restorewindowpos", "yes,no".to_string()),
        ];

        assert_eq!(expected.len(), 16);
        for (name, values) in expected {
            assert_eq!(&values_of(option(name)), values, "{name} values");
        }
    }

    #[test]
    fn the_defaults_match_the_dossier() {
        let expected: &[(&str, Option<&str>)] = &[
            ("unseenskip", None),
            ("seoffinskip", None),
            ("stopskipbyselect", None),
            ("stopskipbyclick", None),
            ("stopautobyselect", None),
            ("stopautobyclick", None),
            ("voicestopbyclick", None),
            ("bgmdownbyvoice", None),
            ("voicespeed", None),
            ("vomstyle", Some("auto")),
            ("fullscreenmode", Some("auto")),
            ("maximizemode", Some("auto")),
            ("maximizezoom", Some("inner")),
            ("mzpercent", Some("50")),
            ("restoremaximizebyf2w", Some("no")),
            ("restorewindowpos", Some("yes")),
        ];

        for (name, default) in expected {
            let marked = option(name)
                .default_value()
                .unwrap_or_else(|| panic!("{name} marks no default"))
                .value;
            assert_eq!(marked, *default, "{name} default");
        }
    }

    #[test]
    fn only_the_recovered_value_descriptions_are_present() {
        let expected: &[(&str, &str, &str)] = &[
            ("unseenskip", "null", "未設定"),
            ("unseenskip", "yes", "する"),
            ("unseenskip", "no", "しない"),
            ("voicestopbyclick", "page", "ページ消去時に停止"),
            ("voicestopbyclick", "name", "名前表示時に停止"),
            ("voicestopbyclick", "no", "停止しない"),
            ("vomstyle", "auto", "自動"),
            ("vomstyle", "overlay", "オーバーレイ"),
            ("vomstyle", "mixer", "ミキサー"),
            ("vomstyle", "layer", "レイヤー"),
        ];

        let mut recovered = 0;
        for category in KagexOptPlugin::categories() {
            for option in category.options {
                for value in option.values {
                    let Some(desc) = value.desc else { continue };
                    recovered += 1;
                    let raw = value.value.unwrap_or("null");
                    assert!(
                        expected.contains(&(option.name, raw, desc)),
                        "{}.{raw} carries a description the dossier did not recover",
                        option.name
                    );
                }
            }
        }
        assert_eq!(recovered, expected.len(), "recovered descriptions");
    }

    #[test]
    fn an_unset_choice_renders_no_argument_and_listed_values_render_one() {
        let skip = option("unseenskip");
        assert_eq!(
            skip.command_line_argument("yes").as_deref(),
            Some("-unseenskip=yes")
        );
        // 「未設定」は設定を変更しません: no argument, the setting stays.
        assert_eq!(skip.command_line_argument(""), None);
        // A value the option does not list is not a setting either.
        assert_eq!(skip.command_line_argument("maybe"), None);

        // `mzpercent` steps by five, so 51 is not a value it lists.
        let percent = option("mzpercent");
        assert_eq!(
            percent.command_line_argument("50").as_deref(),
            Some("-mzpercent=50")
        );
        assert_eq!(percent.command_line_argument("51"), None);

        let speed = option("voicespeed");
        assert_eq!(
            speed.command_line_argument("150").as_deref(),
            Some("-voicespeed=150")
        );
        assert_eq!(speed.command_line_argument("175"), None);

        let unset: &OptionValue = skip.value("").expect("unset entry");
        assert!(unset.is_unset());
        assert!(!skip.value("yes").expect("yes entry").is_unset());
        assert!(option("vomstyle").value("").is_none());
    }

    #[test]
    fn the_engine_reads_back_the_argument_this_registry_renders() {
        let mut engine = engine();
        for (name, raw) in [
            ("unseenskip", "yes"),
            ("vomstyle", "layer"),
            ("mzpercent", "50"),
        ] {
            let argument = option(name)
                .command_line_argument(raw)
                .unwrap_or_else(|| panic!("{name}={raw} renders an argument"));
            let (argument_name, argument_value) = argument.split_once('=').expect("name=value");
            let value = engine
                .execute_script(
                    "probe.tjs",
                    &format!(
                        "System.setArgument(\"{argument_name}\", \"{argument_value}\");\n\
                         return System.getArgument(\"{argument_name}\");"
                    ),
                )
                .expect("probe argument");
            assert_eq!(value, Variant::String(raw.to_string()), "{argument}");
        }
    }

    #[test]
    fn every_listed_vomstyle_method_degrades_to_layer() {
        for raw in ["auto", "overlay", "mixer", "layer"] {
            assert_eq!(
                KagexOptPlugin::movie_display_method(raw),
                Some("layer"),
                "{raw}"
            );
        }
        // Unset leaves the setting alone; an unlisted value is no setting.
        assert_eq!(KagexOptPlugin::movie_display_method(""), None);
        assert_eq!(KagexOptPlugin::movie_display_method("directshow"), None);
    }

    #[test]
    fn installing_the_plugin_links_the_name_and_installs_no_tjs_surface() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        let globals_before = global_names(&engine);

        engine.register_plugin(KagexOptPlugin).expect("plugin");
        // yuzuex.dll embeds the same categories, so both DLLs may be loaded;
        // loading this one twice must not double anything either.
        engine
            .register_plugin(KagexOptPlugin)
            .expect("plugin again");

        assert!(
            engine
                .host()
                .linked_plugins()
                .any(|name| name == "kagexopt.dll")
        );
        assert!(
            !engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("kagexopt")),
            "the reference DLL has one export and no TJS object, so nothing reports itself"
        );
        assert_eq!(
            global_names(&engine),
            globals_before,
            "registration must install no TJS surface"
        );
    }
}
