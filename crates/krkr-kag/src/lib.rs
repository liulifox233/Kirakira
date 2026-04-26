use std::{borrow::Cow, collections::BTreeMap, error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenarioEncoding {
    Utf8,
    ShiftJis,
}

impl ScenarioEncoding {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::ShiftJis => "shift_jis",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KagError {
    Decode { encoding: ScenarioEncoding },
    EmptyTag { line: usize },
    UnterminatedInlineTag { line: usize },
    DuplicateLabel { line: usize, name: String },
}

impl fmt::Display for KagError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode { encoding } => write!(formatter, "failed to decode {}", encoding.name()),
            Self::EmptyTag { line } => write!(formatter, "empty KAG tag at line {line}"),
            Self::UnterminatedInlineTag { line } => {
                write!(formatter, "unterminated inline KAG tag at line {line}")
            }
            Self::DuplicateLabel { line, name } => {
                write!(formatter, "duplicate KAG label '{name}' at line {line}")
            }
        }
    }
}

impl Error for KagError {}

pub type KagResult<T> = Result<T, KagError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KagScenario {
    events: Vec<KagEvent>,
    labels: BTreeMap<String, KagLabel>,
}

impl KagScenario {
    pub fn events(&self) -> &[KagEvent] {
        &self.events
    }

    pub fn labels(&self) -> &BTreeMap<String, KagLabel> {
        &self.labels
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KagEvent {
    Text(KagText),
    Tag(KagTag),
    Label(KagLabel),
    Character(KagCharacter),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KagText {
    pub line: usize,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KagTag {
    pub line: usize,
    pub name: String,
    pub params: BTreeMap<String, String>,
}

impl KagTag {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.get(name).map(String::as_str)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KagLabel {
    pub line: usize,
    pub event_index: usize,
    pub name: String,
    pub caption: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KagCharacter {
    pub line: usize,
    pub name: String,
    pub face: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KagAction {
    Text {
        line: usize,
        value: String,
    },
    Character {
        line: usize,
        name: String,
        face: String,
    },
    Wait {
        line: usize,
        time_ms: Option<u32>,
    },
    ClearMessage {
        line: usize,
    },
    Tag(KagTag),
}

#[derive(Clone, Debug)]
pub struct KagRunner<'a> {
    scenario: &'a KagScenario,
    program_counter: usize,
}

impl<'a> KagRunner<'a> {
    pub fn new(scenario: &'a KagScenario) -> Self {
        Self {
            scenario,
            program_counter: 0,
        }
    }
}

impl Iterator for KagRunner<'_> {
    type Item = KagAction;

    fn next(&mut self) -> Option<Self::Item> {
        while self.program_counter < self.scenario.events.len() {
            let event = &self.scenario.events[self.program_counter];
            self.program_counter += 1;

            match event {
                KagEvent::Label(_) => {}
                KagEvent::Text(text) => {
                    return Some(KagAction::Text {
                        line: text.line,
                        value: text.value.clone(),
                    });
                }
                KagEvent::Character(character) => {
                    return Some(KagAction::Character {
                        line: character.line,
                        name: character.name.clone(),
                        face: character.face.clone(),
                    });
                }
                KagEvent::Tag(tag) if tag.name == "wait" => {
                    return Some(KagAction::Wait {
                        line: tag.line,
                        time_ms: tag.param("time").and_then(|value| value.parse().ok()),
                    });
                }
                KagEvent::Tag(tag) if tag.name == "cm" => {
                    return Some(KagAction::ClearMessage { line: tag.line });
                }
                KagEvent::Tag(tag) => return Some(KagAction::Tag(tag.clone())),
            }
        }

        None
    }
}

pub fn decode_scenario(bytes: &[u8], encoding: ScenarioEncoding) -> KagResult<String> {
    match encoding {
        ScenarioEncoding::Utf8 => std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| KagError::Decode { encoding }),
        ScenarioEncoding::ShiftJis => {
            let (decoded, _encoding_used, had_errors) = encoding_rs::SHIFT_JIS.decode(bytes);
            if had_errors {
                Err(KagError::Decode { encoding })
            } else {
                Ok(match decoded {
                    Cow::Borrowed(value) => value.to_owned(),
                    Cow::Owned(value) => value,
                })
            }
        }
    }
}

pub fn parse_scenario(source: &str) -> KagResult<KagScenario> {
    let mut events = Vec::new();
    let mut labels = BTreeMap::new();
    let mut is_in_block_comment = false;

    for (line, raw_line) in source.lines().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if is_in_block_comment {
            if trimmed == "*/" {
                is_in_block_comment = false;
            }
            continue;
        }

        if trimmed == "/*" {
            is_in_block_comment = true;
            continue;
        }

        if trimmed.starts_with(';') {
            continue;
        }

        let Some(first_char) = trimmed.chars().next() else {
            continue;
        };

        match first_char {
            '*' => push_label(trimmed, line, &mut events, &mut labels)?,
            '@' => events.push(KagEvent::Tag(parse_tag(&trimmed[1..], line)?)),
            '#' => events.push(KagEvent::Character(parse_character(trimmed, line))),
            _ => push_text_and_inline_tags(trimmed, line, &mut events)?,
        }
    }

    Ok(KagScenario { events, labels })
}

fn push_label(
    line_text: &str,
    line: usize,
    events: &mut Vec<KagEvent>,
    labels: &mut BTreeMap<String, KagLabel>,
) -> KagResult<()> {
    let label_text = line_text.trim_start_matches('*');
    let (name, caption) = label_text
        .split_once('|')
        .map_or((label_text, ""), |(name, caption)| (name, caption));
    let label = KagLabel {
        line,
        event_index: events.len(),
        name: name.trim().to_owned(),
        caption: caption.trim().to_owned(),
    };

    if labels.contains_key(&label.name) {
        return Err(KagError::DuplicateLabel {
            line,
            name: label.name,
        });
    }

    labels.insert(label.name.clone(), label.clone());
    events.push(KagEvent::Label(label));
    Ok(())
}

fn parse_character(line_text: &str, line: usize) -> KagCharacter {
    let character_text = line_text.trim_start_matches('#').trim();
    let (name, face) = character_text
        .split_once(':')
        .map_or((character_text, ""), |(name, face)| (name, face));

    KagCharacter {
        line,
        name: name.trim().to_owned(),
        face: face.trim().to_owned(),
    }
}

fn push_text_and_inline_tags(
    line_text: &str,
    line: usize,
    events: &mut Vec<KagEvent>,
) -> KagResult<()> {
    let line_text = line_text.strip_prefix('_').unwrap_or(line_text);
    let mut cursor = 0;

    while let Some(open_offset) = line_text[cursor..].find('[') {
        let open = cursor + open_offset;
        if open > cursor {
            events.push(KagEvent::Text(KagText {
                line,
                value: line_text[cursor..open].to_owned(),
            }));
        }

        let tag_body_start = open + 1;
        let Some(close_offset) = find_inline_tag_close(&line_text[tag_body_start..]) else {
            return Err(KagError::UnterminatedInlineTag { line });
        };

        let close = tag_body_start + close_offset;
        events.push(KagEvent::Tag(parse_tag(
            &line_text[tag_body_start..close],
            line,
        )?));
        cursor = close + 1;
    }

    if cursor < line_text.len() {
        events.push(KagEvent::Text(KagText {
            line,
            value: line_text[cursor..].to_owned(),
        }));
    }

    Ok(())
}

fn find_inline_tag_close(tag_source: &str) -> Option<usize> {
    let mut quote = None;

    for (index, character) in tag_source.char_indices() {
        match quote {
            Some(quote_char) if character == quote_char => quote = None,
            Some(_) => {}
            None if character == '"' || character == '\'' => quote = Some(character),
            None if character == ']' => return Some(index),
            None => {}
        }
    }

    None
}

fn parse_tag(tag_source: &str, line: usize) -> KagResult<KagTag> {
    let tokens = split_tag_tokens(tag_source);
    let Some((name, params)) = tokens.split_first() else {
        return Err(KagError::EmptyTag { line });
    };

    let params = params
        .iter()
        .map(|token| {
            token.split_once('=').map_or_else(
                || (token.to_owned(), String::new()),
                |(key, value)| (key.to_owned(), value.to_owned()),
            )
        })
        .collect();

    Ok(KagTag {
        line,
        name: name.to_owned(),
        params,
    })
}

fn split_tag_tokens(tag_source: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;

    for character in tag_source.trim().chars() {
        match quote {
            Some(quote_char) if character == quote_char => quote = None,
            Some(_) => token.push(character),
            None if character == '"' || character == '\'' => quote = Some(character),
            None if character.is_whitespace() => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            None => token.push(character),
        }
    }

    if !token.is_empty() {
        tokens.push(token);
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_inline_tags_preserves_surrounding_text() {
        let scenario = parse_scenario(r#"[font color="red"]HTML5[resetfont] text"#)
            .expect("parse inline tags");

        assert_eq!(scenario.events().len(), 4);
        assert_eq!(
            scenario.events()[0],
            KagEvent::Tag(KagTag {
                line: 0,
                name: "font".to_owned(),
                params: BTreeMap::from([("color".to_owned(), "red".to_owned())]),
            })
        );
        assert_eq!(
            scenario.events()[1],
            KagEvent::Text(KagText {
                line: 0,
                value: "HTML5".to_owned(),
            })
        );
    }

    #[test]
    fn parse_duplicate_labels_as_error() {
        let error = parse_scenario("*start\n*start").expect_err("duplicate label should fail");

        assert_eq!(
            error,
            KagError::DuplicateLabel {
                line: 1,
                name: "start".to_owned(),
            }
        );
    }
}
