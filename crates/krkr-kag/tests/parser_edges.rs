use std::collections::BTreeMap;

use krkr_kag::{
    KagAction, KagError, KagEvent, KagRunner, KagTag, KagText, ScenarioEncoding, decode_scenario,
    parse_scenario,
};

#[test]
fn parser_skips_line_and_block_comments() {
    let scenario = parse_scenario(
        r#"
        ; line comment
        /*
        [wait time=999]
        */
        [cm]
        text
        "#,
    )
    .expect("parse comments");

    assert_eq!(scenario.events().len(), 2);
    assert!(matches!(
        scenario.events()[0],
        KagEvent::Tag(KagTag { ref name, .. }) if name == "cm"
    ));
    assert_eq!(
        scenario.events()[1],
        KagEvent::Text(KagText {
            line: 6,
            value: "text".to_owned(),
        })
    );
}

#[test]
fn parser_reads_command_line_tag_params_with_quotes_equals_and_bare_flags() {
    let scenario =
        parse_scenario(r#"@position layer="message0" path="a=b c" visible=true enabled"#)
            .expect("parse command tag");

    assert_eq!(
        scenario.events()[0],
        KagEvent::Tag(KagTag {
            line: 0,
            name: "position".to_owned(),
            params: BTreeMap::from([
                ("enabled".to_owned(), String::new()),
                ("layer".to_owned(), "message0".to_owned()),
                ("path".to_owned(), "a=b c".to_owned()),
                ("visible".to_owned(), "true".to_owned()),
            ]),
        })
    );
}

#[test]
fn parser_reads_character_lines_with_optional_face() {
    let scenario = parse_scenario("#akane:happy\n#narrator").expect("parse character lines");

    assert!(matches!(
        &scenario.events()[0],
        KagEvent::Character(character)
            if character.name == "akane" && character.face == "happy"
    ));
    assert!(matches!(
        &scenario.events()[1],
        KagEvent::Character(character)
            if character.name == "narrator" && character.face.is_empty()
    ));
}

#[test]
fn parser_handles_inline_tag_closing_bracket_inside_quoted_param() {
    let scenario =
        parse_scenario(r#"before[button text="a]b" target=*next]after"#).expect("parse inline tag");

    assert_eq!(
        scenario.events(),
        [
            KagEvent::Text(KagText {
                line: 0,
                value: "before".to_owned(),
            }),
            KagEvent::Tag(KagTag {
                line: 0,
                name: "button".to_owned(),
                params: BTreeMap::from([
                    ("target".to_owned(), "*next".to_owned()),
                    ("text".to_owned(), "a]b".to_owned()),
                ]),
            }),
            KagEvent::Text(KagText {
                line: 0,
                value: "after".to_owned(),
            }),
        ]
    );
}

#[test]
fn parser_leading_underscore_escapes_command_like_text() {
    let scenario = parse_scenario("_[notatag] literal").expect("parse escaped text line");

    assert_eq!(
        scenario.events(),
        [
            KagEvent::Tag(KagTag {
                line: 0,
                name: "notatag".to_owned(),
                params: BTreeMap::new(),
            }),
            KagEvent::Text(KagText {
                line: 0,
                value: " literal".to_owned(),
            }),
        ]
    );
}

#[test]
fn parser_unterminated_inline_tag_reports_line() {
    let error = parse_scenario("text [wait time=10").expect_err("unterminated tag should fail");

    assert_eq!(error, KagError::UnterminatedInlineTag { line: 0 });
}

#[test]
fn runner_passes_unknown_tags_and_character_actions_through() {
    let scenario = parse_scenario("#akane:happy\n[chara_show name=akane]\nhello")
        .expect("parse runner fixture");

    let actions = KagRunner::new(&scenario).collect::<Vec<_>>();

    assert_eq!(
        actions,
        [
            KagAction::Character {
                line: 0,
                name: "akane".to_owned(),
                face: "happy".to_owned(),
            },
            KagAction::Tag(KagTag {
                line: 1,
                name: "chara_show".to_owned(),
                params: BTreeMap::from([("name".to_owned(), "akane".to_owned())]),
            }),
            KagAction::Text {
                line: 2,
                value: "hello".to_owned(),
            },
        ]
    );
}

#[test]
fn decoder_rejects_invalid_utf8() {
    let error = decode_scenario(&[0xff, 0xff], ScenarioEncoding::Utf8)
        .expect_err("invalid UTF-8 should fail");

    assert_eq!(
        error,
        KagError::Decode {
            encoding: ScenarioEncoding::Utf8,
        }
    );
}
