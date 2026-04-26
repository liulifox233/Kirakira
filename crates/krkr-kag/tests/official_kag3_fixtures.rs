use std::collections::BTreeMap;

use krkr_kag::{
    KagAction, KagEvent, KagLabel, KagRunner, KagTag, KagText, ScenarioEncoding, decode_scenario,
    parse_scenario,
};

const FIRST_KS: &str = include_str!("fixtures/third_party/krkrz-kag3/scenario/first.ks");
const STARTUP_TJS: &str = include_str!("fixtures/third_party/krkrz-kag3/startup.tjs");
const INITIALIZE_TJS: &str = include_str!("fixtures/third_party/krkrz-kag3/system/Initialize.tjs");

#[test]
fn official_kag3_first_scenario_parses_expected_events() {
    let scenario = parse_scenario(FIRST_KS).expect("parse official KAG3 first.ks");

    assert_eq!(
        scenario.events(),
        [
            KagEvent::Tag(KagTag {
                line: 0,
                name: "wait".to_owned(),
                params: BTreeMap::from([("time".to_owned(), "200".to_owned())]),
            }),
            KagEvent::Label(KagLabel {
                line: 1,
                event_index: 1,
                name: "start".to_owned(),
                caption: "スタート".to_owned(),
            }),
            KagEvent::Tag(KagTag {
                line: 2,
                name: "cm".to_owned(),
                params: BTreeMap::new(),
            }),
            KagEvent::Text(KagText {
                line: 3,
                value: "こんにちは。".to_owned(),
            }),
        ]
    );

    assert_eq!(scenario.labels()["start"].event_index, 1);
}

#[test]
fn official_kag3_first_scenario_steps_to_runtime_actions() {
    let scenario = parse_scenario(FIRST_KS).expect("parse official KAG3 first.ks");
    let actions = KagRunner::new(&scenario).collect::<Vec<_>>();

    assert_eq!(
        actions,
        [
            KagAction::Wait {
                line: 0,
                time_ms: Some(200),
            },
            KagAction::ClearMessage { line: 2 },
            KagAction::Text {
                line: 3,
                value: "こんにちは。".to_owned(),
            },
        ]
    );
}

#[test]
fn kag3_scenario_decoder_accepts_utf8_and_shift_jis_inputs() {
    let decoded = decode_scenario(FIRST_KS.as_bytes(), ScenarioEncoding::Utf8)
        .expect("decode official UTF-8 fixture");
    assert_eq!(decoded, FIRST_KS);

    let shift_jis = [
        0x5b, 0x63, 0x6d, 0x5d, 0x0a, 0x82, 0xb1, 0x82, 0xf1, 0x82, 0xc9, 0x82, 0xbf, 0x82, 0xcd,
        0x81, 0x42, 0x0a,
    ];
    let decoded =
        decode_scenario(&shift_jis, ScenarioEncoding::ShiftJis).expect("decode Shift_JIS KAG text");

    assert_eq!(decoded, "[cm]\nこんにちは。\n");
    assert!(parse_scenario(&decoded).is_ok());
}

#[test]
fn official_kag3_startup_tjs_fixture_is_tracked_for_boot_tests() {
    assert!(STARTUP_TJS.contains("Scripts.execStorage(\"system/Initialize.tjs\")"));
    assert!(STARTUP_TJS.contains("Plugins.link(\"KAGParser.dll\")"));
    assert!(INITIALIZE_TJS.contains("kag.process(\"first.ks\")"));
    assert!(INITIALIZE_TJS.contains("KAGLoadScript(\"MessageLayer.tjs\")"));
}

#[test]
#[ignore = "requires KAG3 boot orchestration, TJS Scripts.execStorage, and plugin compatibility shims"]
fn conformance_kag3_boot_executes_startup_then_first_scenario() {
    assert!(!STARTUP_TJS.trim().is_empty());
    assert!(!INITIALIZE_TJS.trim().is_empty());
    assert!(!FIRST_KS.trim().is_empty());
    panic!("KAG3 boot/runtime orchestration is not implemented yet");
}
