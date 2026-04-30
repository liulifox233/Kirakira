use crate::error::{Result, Span, TjsError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DateFields {
    pub year: i32,
    pub month: i32,
    pub month_day: i32,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
    pub timezone_seconds: Option<i32>,
    pub timezone_offset_seconds: Option<i32>,
}

pub fn parse_date_fields(source: &str) -> Result<DateFields> {
    let tokens = DateLexer::new(source).lex_all()?;
    DateParser::new(tokens).parse()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DateToken {
    kind: DateTokenKind,
    span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DateTokenKind {
    Eof,
    Number(i32),
    Month(i32),
    Wday,
    TimeZone(i32),
    Am,
    Pm,
    Char(char),
}

struct DateLexer<'a> {
    source: &'a str,
    chars: std::str::CharIndices<'a>,
    lookahead: Option<(usize, char)>,
}

impl<'a> DateLexer<'a> {
    fn new(source: &'a str) -> Self {
        let mut chars = source.char_indices();
        let lookahead = chars.next();
        Self {
            source,
            chars,
            lookahead,
        }
    }

    fn lex_all(mut self) -> Result<Vec<DateToken>> {
        let mut tokens = Vec::new();
        loop {
            self.skip_ws();
            let start = self.offset();
            let Some((_, ch)) = self.peeked() else {
                tokens.push(DateToken {
                    kind: DateTokenKind::Eof,
                    span: Span::empty(start),
                });
                return Ok(tokens);
            };

            if ch.is_ascii_digit() {
                tokens.push(self.lex_number(start));
                continue;
            }

            if let Some((kind, len)) = self.match_word() {
                for _ in 0..len {
                    self.bump();
                }
                tokens.push(DateToken {
                    kind,
                    span: Span::new(start, self.offset()),
                });
                continue;
            }

            let (_, ch) = self.bump().expect("peeked");
            let ch = ch.to_ascii_lowercase();
            if ch == '(' {
                while let Some((_, inner)) = self.peeked() {
                    if inner == ')' {
                        break;
                    }
                    self.bump();
                }
            }
            tokens.push(DateToken {
                kind: DateTokenKind::Char(ch),
                span: Span::new(start, self.offset()),
            });
        }
    }

    fn lex_number(&mut self, start: usize) -> DateToken {
        let mut value = 0_i32;
        while let Some((_, ch)) = self.peeked() {
            let Some(digit) = ch.to_digit(10) else {
                break;
            };
            value = value.wrapping_mul(10).wrapping_add(digit as i32);
            self.bump();
        }
        DateToken {
            kind: DateTokenKind::Number(value),
            span: Span::new(start, self.offset()),
        }
    }

    fn match_word(&self) -> Option<(DateTokenKind, usize)> {
        WORDS
            .iter()
            .filter(|word| self.starts_with_ignore_ascii_case(word.text))
            .filter(|word| self.has_word_boundary(word.text.len()))
            .max_by_key(|word| word.text.len())
            .map(|word| (word.kind.clone(), word.text.len()))
    }

    fn starts_with_ignore_ascii_case(&self, text: &str) -> bool {
        self.source[self.offset()..]
            .get(..text.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(text))
    }

    fn has_word_boundary(&self, byte_len: usize) -> bool {
        let offset = self.offset() + byte_len;
        self.source[offset..]
            .chars()
            .next()
            .is_none_or(|ch| !is_date_alpha(ch))
    }

    fn skip_ws(&mut self) {
        while self
            .peeked()
            .is_some_and(|(_, ch)| ch.is_ascii_whitespace())
        {
            self.bump();
        }
    }

    fn offset(&self) -> usize {
        self.lookahead
            .map(|(offset, _)| offset)
            .unwrap_or(self.source.len())
    }

    fn peeked(&self) -> Option<(usize, char)> {
        self.lookahead
    }

    fn bump(&mut self) -> Option<(usize, char)> {
        let current = self.lookahead;
        self.lookahead = self.chars.next();
        current
    }
}

#[derive(Clone)]
struct DateWord {
    text: &'static str,
    kind: DateTokenKind,
}

const WORDS: &[DateWord] = &[
    tz("jst", 900),
    tz("ut", 0),
    tz("utc", 0),
    tz("gmt", 0),
    tz("est", -500),
    tz("edt", -400),
    tz("cst", -600),
    tz("cdt", -500),
    tz("mst", -700),
    tz("mdt", -600),
    tz("pst", -800),
    tz("pdt", -700),
    tz("z", 0),
    tz("a", -100),
    tz("m", -1200),
    tz("n", 100),
    tz("y", 1200),
    tz("nzdt", 1300),
    tz("idle", 1200),
    tz("nzst", 1200),
    tz("nzt", 1200),
    tz("aesst", 1100),
    tz("acsst", 1030),
    tz("cadt", 1030),
    tz("sadt", 1030),
    tz("aest", 1000),
    tz("east", 1000),
    tz("gst", 1000),
    tz("ligt", 1000),
    tz("acst", 930),
    tz("sast", 930),
    tz("cast", 930),
    tz("awsst", 900),
    tz("kst", 900),
    tz("wdt", 900),
    tz("mt", 830),
    tz("awst", 800),
    tz("cct", 800),
    tz("wadt", 800),
    tz("wst", 800),
    tz("jt", 730),
    tz("wast", 700),
    tz("it", 330),
    tz("bt", 300),
    tz("eetdst", 300),
    tz("cetdst", 200),
    tz("eet", 200),
    tz("fwt", 200),
    tz("ist", 200),
    tz("mest", 200),
    tz("metdst", 200),
    tz("sst", 200),
    tz("bst", 100),
    tz("cet", 100),
    tz("dnt", 100),
    tz("fst", 100),
    tz("met", 100),
    tz("mewt", 100),
    tz("mez", 100),
    tz("nor", 100),
    tz("set", 100),
    tz("swt", 100),
    tz("wetdst", 100),
    tz("wet", 0),
    tz("wat", -100),
    tz("ndt", -230),
    tz("adt", -300),
    tz("nft", -330),
    tz("nst", -330),
    tz("ast", -400),
    tz("ydt", -800),
    tz("hdt", -900),
    tz("ahst", -1000),
    tz("cat", -1000),
    tz("nt", -1100),
    tz("idlw", -1200),
    wday("sun"),
    wday("sun."),
    wday("sunday"),
    wday("mon"),
    wday("mon."),
    wday("monday"),
    wday("tue"),
    wday("tue."),
    wday("tues"),
    wday("tues."),
    wday("tuesday"),
    wday("wed"),
    wday("wed."),
    wday("wednesday"),
    wday("thu"),
    wday("thu."),
    wday("thurs"),
    wday("thurs."),
    wday("thursday"),
    wday("fri"),
    wday("fri."),
    wday("friday"),
    wday("sat"),
    wday("sat."),
    wday("saturday"),
    month("jan", 0),
    month("jan.", 0),
    month("january", 0),
    month("feb", 1),
    month("feb.", 1),
    month("february", 1),
    month("mar", 2),
    month("mar.", 2),
    month("march", 2),
    month("apr", 3),
    month("apr.", 3),
    month("april", 3),
    month("may", 4),
    month("ju", 5),
    month("ju.", 5),
    month("jun", 5),
    month("jun.", 5),
    month("june", 5),
    month("jul", 6),
    month("jul.", 6),
    month("july", 6),
    month("aug", 7),
    month("aug.", 7),
    month("august", 7),
    month("sep", 8),
    month("sep.", 8),
    month("sept", 8),
    month("sept.", 8),
    month("september", 8),
    month("oct", 9),
    month("oct.", 9),
    month("october", 9),
    month("nov", 10),
    month("nov.", 10),
    month("november", 10),
    month("dec", 11),
    month("dec.", 11),
    month("december", 11),
    DateWord {
        text: "am",
        kind: DateTokenKind::Am,
    },
    DateWord {
        text: "pm",
        kind: DateTokenKind::Pm,
    },
];

const fn tz(text: &'static str, value: i32) -> DateWord {
    DateWord {
        text,
        kind: DateTokenKind::TimeZone(value),
    }
}

const fn wday(text: &'static str) -> DateWord {
    DateWord {
        text,
        kind: DateTokenKind::Wday,
    }
}

const fn month(text: &'static str, value: i32) -> DateWord {
    DateWord {
        text,
        kind: DateTokenKind::Month(value),
    }
}

fn is_date_alpha(ch: char) -> bool {
    ch.is_ascii_alphabetic() || !ch.is_ascii()
}

struct DateParser {
    tokens: Vec<DateToken>,
    pos: usize,
}

impl DateParser {
    fn new(tokens: Vec<DateToken>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn parse(mut self) -> Result<DateFields> {
        let start = self.current().span.start;
        self.consume_wday();
        let after_wday = self.pos;

        for shape in DATE_SHAPES {
            self.pos = after_wday;
            let mut fields = DateFieldsBuilder::default();
            if shape(&mut self, &mut fields).is_ok()
                && self.is(&DateTokenKind::Eof)
                && let Some(fields) = fields.finish()
            {
                return Ok(fields);
            }
        }

        Err(TjsError::parse(
            Span::new(start, self.current().span.end),
            "cannot parse date string",
        ))
    }

    fn parse_day_month_year_time(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        let day = self.expect_number()?;
        fields.month = Some(self.expect_month()?);
        fields.year = Some(adjust_year(self.expect_number()?));
        self.parse_time(fields)?;
        self.parse_timezone(fields)?;
        fields.month_day = Some(day);
        Ok(())
    }

    fn parse_day_dash_month_year_time(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        let day = self.expect_number()?;
        self.expect_char('-')?;
        fields.month = Some(self.expect_month()?);
        fields.year = Some(adjust_year(self.expect_number()?));
        self.parse_time(fields)?;
        self.parse_timezone(fields)?;
        fields.month_day = Some(day);
        Ok(())
    }

    fn parse_day_dash_month_dash_year_time(
        &mut self,
        fields: &mut DateFieldsBuilder,
    ) -> Result<()> {
        let day = self.expect_number()?;
        self.expect_char('-')?;
        fields.month = Some(self.expect_month()?);
        self.expect_char('-')?;
        fields.year = Some(adjust_year(self.expect_number()?));
        self.parse_time(fields)?;
        self.parse_timezone(fields)?;
        fields.month_day = Some(day);
        Ok(())
    }

    fn parse_month_day_year_time(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        fields.month = Some(self.expect_month()?);
        fields.month_day = Some(self.expect_number()?);
        fields.year = Some(adjust_year(self.expect_number()?));
        self.parse_time(fields)?;
        self.parse_timezone(fields)
    }

    fn parse_month_dash_day_year_time(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        fields.month = Some(self.expect_month()?);
        self.expect_char('-')?;
        fields.month_day = Some(self.expect_number()?);
        fields.year = Some(adjust_year(self.expect_number()?));
        self.parse_time(fields)?;
        self.parse_timezone(fields)
    }

    fn parse_month_dash_day_dash_year_time(
        &mut self,
        fields: &mut DateFieldsBuilder,
    ) -> Result<()> {
        fields.month = Some(self.expect_month()?);
        self.expect_char('-')?;
        fields.month_day = Some(self.expect_number()?);
        self.expect_char('-')?;
        fields.year = Some(adjust_year(self.expect_number()?));
        self.parse_time(fields)?;
        self.parse_timezone(fields)
    }

    fn parse_day_month_time_year(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        let day = self.expect_number()?;
        fields.month = Some(self.expect_month()?);
        self.parse_time(fields)?;
        fields.year = Some(adjust_year(self.expect_number()?));
        self.parse_timezone(fields)?;
        fields.month_day = Some(day);
        Ok(())
    }

    fn parse_day_dash_month_time_year(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        let day = self.expect_number()?;
        self.expect_char('-')?;
        fields.month = Some(self.expect_month()?);
        self.parse_time(fields)?;
        fields.year = Some(adjust_year(self.expect_number()?));
        self.parse_timezone(fields)?;
        fields.month_day = Some(day);
        Ok(())
    }

    fn parse_month_day_time_year(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        fields.month = Some(self.expect_month()?);
        fields.month_day = Some(self.expect_number()?);
        self.parse_time(fields)?;
        fields.year = Some(adjust_year(self.expect_number()?));
        self.parse_timezone(fields)
    }

    fn parse_month_dash_day_time_year(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        fields.month = Some(self.expect_month()?);
        self.expect_char('-')?;
        fields.month_day = Some(self.expect_number()?);
        self.parse_time(fields)?;
        fields.year = Some(adjust_year(self.expect_number()?));
        self.parse_timezone(fields)
    }

    fn parse_numeric_year_month_day_time(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        fields.year = Some(adjust_year(self.expect_number()?));
        self.expect_hyphen_or_slash()?;
        fields.month = Some(self.expect_number()? - 1);
        self.expect_hyphen_or_slash()?;
        fields.month_day = Some(self.expect_number()?);
        self.parse_time(fields)?;
        self.parse_timezone(fields)
    }

    fn parse_time(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        let prefix_ampm = self.consume_ampm();
        self.parse_hms(fields)?;
        let suffix_ampm = if prefix_ampm.is_none() {
            self.consume_ampm()
        } else {
            None
        };
        fields.pm = prefix_ampm.or(suffix_ampm);
        if fields.pm == Some(true) {
            fields.hour = fields.hour.map(|hour| hour + 12);
        }
        Ok(())
    }

    fn parse_hms(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        fields.hour = Some(self.expect_number()?);
        self.expect_char(':')?;
        fields.minute = Some(self.expect_number()?);
        fields.second = if self.consume_char(':') {
            let sec = self.expect_number()?;
            if self.consume_char('.') {
                self.expect_number()?;
            }
            Some(sec)
        } else {
            Some(0)
        };
        Ok(())
    }

    fn parse_timezone(&mut self, fields: &mut DateFieldsBuilder) -> Result<()> {
        if let DateTokenKind::TimeZone(value) = self.current().kind {
            fields.timezone_seconds = Some(hhmm_to_seconds(value));
            self.advance();
        }

        if self.consume_char('+') {
            fields.timezone_offset_seconds = Some(hhmm_to_seconds(self.expect_number()?));
        } else if self.consume_char('-') {
            fields.timezone_offset_seconds = Some(hhmm_to_seconds(-self.expect_number()?));
        }

        if self.consume_char('(') {
            self.expect_char(')')?;
        }
        Ok(())
    }

    fn consume_wday(&mut self) {
        if self.is(&DateTokenKind::Wday) {
            self.advance();
            self.consume_char(',');
        }
    }

    fn consume_ampm(&mut self) -> Option<bool> {
        match self.current().kind {
            DateTokenKind::Am => {
                self.advance();
                Some(false)
            }
            DateTokenKind::Pm => {
                self.advance();
                Some(true)
            }
            _ => None,
        }
    }

    fn expect_number(&mut self) -> Result<i32> {
        let token = self.advance().clone();
        match token.kind {
            DateTokenKind::Number(value) => Ok(value),
            _ => Err(TjsError::parse(
                token.span,
                "expected number in date string",
            )),
        }
    }

    fn expect_month(&mut self) -> Result<i32> {
        let token = self.advance().clone();
        match token.kind {
            DateTokenKind::Month(value) => Ok(value),
            _ => Err(TjsError::parse(token.span, "expected month in date string")),
        }
    }

    fn expect_hyphen_or_slash(&mut self) -> Result<()> {
        if self.consume_char('-') || self.consume_char('/') {
            Ok(())
        } else {
            Err(TjsError::parse(
                self.current().span,
                "expected date separator",
            ))
        }
    }

    fn expect_char(&mut self, expected: char) -> Result<()> {
        if self.consume_char(expected) {
            Ok(())
        } else {
            Err(TjsError::parse(
                self.current().span,
                format!("expected {expected:?} in date string"),
            ))
        }
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.current().kind == DateTokenKind::Char(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn is(&self, kind: &DateTokenKind) -> bool {
        self.current().kind == *kind
    }

    fn current(&self) -> &DateToken {
        self.tokens
            .get(self.pos)
            .unwrap_or_else(|| self.tokens.last().expect("lexer emits eof"))
    }

    fn advance(&mut self) -> &DateToken {
        let index = self.pos;
        if !self.is(&DateTokenKind::Eof) {
            self.pos += 1;
        }
        &self.tokens[index]
    }
}

type DateShape = fn(&mut DateParser, &mut DateFieldsBuilder) -> Result<()>;

const DATE_SHAPES: &[DateShape] = &[
    DateParser::parse_day_month_year_time,
    DateParser::parse_day_dash_month_year_time,
    DateParser::parse_day_dash_month_dash_year_time,
    DateParser::parse_month_day_year_time,
    DateParser::parse_month_dash_day_year_time,
    DateParser::parse_month_dash_day_dash_year_time,
    DateParser::parse_day_month_time_year,
    DateParser::parse_day_dash_month_time_year,
    DateParser::parse_month_day_time_year,
    DateParser::parse_month_dash_day_time_year,
    DateParser::parse_numeric_year_month_day_time,
];

#[derive(Default)]
struct DateFieldsBuilder {
    year: Option<i32>,
    month: Option<i32>,
    month_day: Option<i32>,
    hour: Option<i32>,
    minute: Option<i32>,
    second: Option<i32>,
    pm: Option<bool>,
    timezone_seconds: Option<i32>,
    timezone_offset_seconds: Option<i32>,
}

impl DateFieldsBuilder {
    fn finish(self) -> Option<DateFields> {
        Some(DateFields {
            year: self.year?,
            month: self.month?,
            month_day: self.month_day?,
            hour: self.hour?,
            minute: self.minute?,
            second: self.second?,
            timezone_seconds: self.timezone_seconds,
            timezone_offset_seconds: self.timezone_offset_seconds,
        })
    }
}

fn adjust_year(year: i32) -> i32 {
    if year < 100 {
        if year <= 50 { 2000 + year } else { 1900 + year }
    } else {
        year
    }
}

fn hhmm_to_seconds(value: i32) -> i32 {
    let sign = if value < 0 { -1 } else { 1 };
    let value = value.abs();
    sign * ((value / 100) * 60 * 60 + (value % 100) * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(source: &str) -> DateFields {
        parse_date_fields(source).expect("date parse")
    }

    #[test]
    fn parses_krkrz_date_shapes() {
        assert_eq!(
            fields("Sun, 3 May 2004 11:22:33 GMT +900 (JST)"),
            DateFields {
                year: 2004,
                month: 4,
                month_day: 3,
                hour: 11,
                minute: 22,
                second: 33,
                timezone_seconds: Some(0),
                timezone_offset_seconds: Some(9 * 60 * 60),
            }
        );
        assert_eq!(fields("3-May-04 11:22").year, 2004);
        assert_eq!(fields("May-3 11:22:33 99").year, 1999);
        assert_eq!(fields("2004/03/03 pm 11:22").hour, 23);
    }

    #[test]
    fn parses_date_words_with_krkrz_boundaries() {
        let parsed = fields("sept1 2004 11:22");
        assert_eq!(parsed.month, 8);
        assert_eq!(parsed.month_day, 1);
    }

    #[test]
    fn rejects_missing_required_date_fields() {
        assert!(parse_date_fields("May 3 2004").is_err());
        assert!(parse_date_fields("2004-03 11:22").is_err());
        assert!(parse_date_fields("not a date").is_err());
    }
}
