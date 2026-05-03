use std::{collections::BTreeMap, sync::Arc};

use crate::{
    error::{KagError, Result},
    source::{SourceLocation, SourceSpan},
    tag::{Attribute, AttributeValue, Tag, TagOrigin},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DebugLevel {
    #[default]
    None,
    Simple,
    Verbose,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParserOptions {
    pub ignore_cr: bool,
    pub process_special_tags: bool,
    pub process_cond: bool,
    pub resolve_entities: bool,
    pub max_macro_depth: usize,
}

impl Default for ParserOptions {
    fn default() -> Self {
        Self {
            ignore_cr: false,
            process_special_tags: true,
            process_cond: true,
            resolve_entities: true,
            max_macro_depth: 64,
        }
    }
}

pub trait KagHost {
    fn load_scenario(&mut self, storage: &str) -> Result<String> {
        Err(KagError::ScenarioLoadUnsupported {
            storage: storage.to_owned(),
        })
    }

    fn on_scenario_load(&mut self, _event: ScenarioLoadEvent<'_>) -> Result<Option<String>> {
        Ok(None)
    }

    fn on_scenario_loaded(&mut self, _event: ScenarioLoadEvent<'_>) -> Result<()> {
        Ok(())
    }

    fn eval_bool(&mut self, expression: &str) -> Result<bool> {
        Err(KagError::EvalUnsupported {
            expression: expression.to_owned(),
        })
    }

    fn eval_string(&mut self, expression: &str) -> Result<String> {
        Err(KagError::EvalUnsupported {
            expression: expression.to_owned(),
        })
    }

    fn on_label(&mut self, _event: LabelEvent<'_>) -> Result<()> {
        Ok(())
    }

    fn on_script(&mut self, _event: ScriptEvent<'_>) -> Result<()> {
        Ok(())
    }

    fn on_jump(
        &mut self,
        _tag: &Tag,
        _storage: Option<&str>,
        _target: Option<&str>,
    ) -> Result<bool> {
        Ok(true)
    }

    fn on_call(
        &mut self,
        _tag: &Tag,
        _storage: Option<&str>,
        _target: Option<&str>,
    ) -> Result<bool> {
        Ok(true)
    }

    fn on_return(
        &mut self,
        _tag: &Tag,
        _storage: Option<&str>,
        _target: Option<&str>,
    ) -> Result<bool> {
        Ok(true)
    }

    fn on_call_stack_depth(&mut self, _depth: usize) -> Result<()> {
        Ok(())
    }

    fn on_after_return(&mut self, _frame: &CallFrame) -> Result<()> {
        Ok(())
    }
}

impl KagHost for () {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScenarioLoadEvent<'a> {
    pub storage: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LabelEvent<'a> {
    pub storage: &'a str,
    pub label: &'a Label,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScriptEvent<'a> {
    pub storage: &'a str,
    pub script: &'a str,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Label {
    pub name: String,
    pub page_name: Option<String>,
    pub span: SourceSpan,
    pub location: SourceLocation,
    cursor_offset: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallFrame {
    storage: String,
    offset: usize,
    line_text: String,
    current_label: Option<String>,
    resume: ResumeState,
}

impl CallFrame {
    pub fn storage(&self) -> &str {
        &self.storage
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn current_label(&self) -> Option<&str> {
        self.current_label.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParserSnapshot {
    current_storage: Option<String>,
    cursor: Cursor,
    current_label: Option<String>,
    call_stack: Vec<CallFrame>,
    macros: BTreeMap<String, MacroDefinition>,
    macro_stack: Vec<ExpansionFrame>,
    macro_params: Vec<Attribute>,
    condition_stack: Vec<ConditionFrame>,
    interrupted: bool,
}

#[derive(Clone, Debug)]
pub struct KagParser {
    options: ParserOptions,
    debug_level: DebugLevel,
    scenarios: Arc<BTreeMap<String, Scenario>>,
    current_storage: Option<String>,
    cursor: Cursor,
    current_label: Option<String>,
    call_stack: Vec<CallFrame>,
    macros: BTreeMap<String, MacroDefinition>,
    macro_stack: Vec<ExpansionFrame>,
    macro_params: Vec<Attribute>,
    condition_stack: Vec<ConditionFrame>,
    interrupted: bool,
}

impl Default for KagParser {
    fn default() -> Self {
        Self::new()
    }
}

impl KagParser {
    pub fn new() -> Self {
        Self::with_options(ParserOptions::default())
    }

    pub fn with_options(options: ParserOptions) -> Self {
        Self {
            options,
            debug_level: DebugLevel::Simple,
            scenarios: Arc::new(BTreeMap::new()),
            current_storage: None,
            cursor: Cursor::default(),
            current_label: None,
            call_stack: Vec::new(),
            macros: BTreeMap::new(),
            macro_stack: Vec::new(),
            macro_params: Vec::new(),
            condition_stack: Vec::new(),
            interrupted: false,
        }
    }

    pub fn options(&self) -> &ParserOptions {
        &self.options
    }

    pub fn options_mut(&mut self) -> &mut ParserOptions {
        &mut self.options
    }

    pub fn debug_level(&self) -> DebugLevel {
        self.debug_level
    }

    pub fn set_debug_level(&mut self, debug_level: DebugLevel) {
        self.debug_level = debug_level;
    }

    pub fn ignore_cr(&self) -> bool {
        self.options.ignore_cr
    }

    pub fn set_ignore_cr(&mut self, ignore_cr: bool) {
        self.options.ignore_cr = ignore_cr;
    }

    pub fn process_special_tags(&self) -> bool {
        self.options.process_special_tags
    }

    pub fn set_process_special_tags(&mut self, process_special_tags: bool) {
        self.options.process_special_tags = process_special_tags;
    }

    pub fn clear(&mut self) {
        self.scenarios = Arc::new(BTreeMap::new());
        self.current_storage = None;
        self.cursor = Cursor::default();
        self.current_label = None;
        self.call_stack.clear();
        self.macro_stack.clear();
        self.macro_params.clear();
        self.condition_stack.clear();
        self.interrupted = false;
    }

    pub fn assign(&mut self, source: &Self) {
        self.options = source.options.clone();
        self.debug_level = source.debug_level;
        self.scenarios = source.scenarios.clone();
        self.current_storage = source.current_storage.clone();
        self.cursor = source.cursor;
        self.current_label = source.current_label.clone();
        self.call_stack = source.call_stack.clone();
        self.macros = source.macros.clone();
        self.macro_stack = source.macro_stack.clone();
        self.macro_params = source.macro_params.clone();
        self.condition_stack = source.condition_stack.clone();
        self.interrupted = source.interrupted;
    }

    pub fn load_scenario_text(
        &mut self,
        storage: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<()> {
        let storage = storage.into();
        self.install_scenario(storage, source.into())
    }

    pub fn load_scenario_with<H>(&mut self, storage: impl Into<String>, host: &mut H) -> Result<()>
    where
        H: KagHost,
    {
        let storage = storage.into();
        self.load_scenario_from_host(&storage, host)
    }

    pub fn cur_storage(&self) -> Option<&str> {
        self.current_storage.as_deref()
    }

    pub fn set_cur_storage(&mut self, storage: impl Into<String>) -> Result<()> {
        let storage = storage.into();
        if !self.scenarios.contains_key(&storage) {
            return Err(KagError::ScenarioNotLoaded { storage });
        }
        self.current_storage = Some(storage);
        self.cursor = Cursor::default();
        self.current_label = None;
        self.condition_stack.clear();
        self.macro_stack.clear();
        self.macro_params.clear();
        Ok(())
    }

    pub fn cur_label(&self) -> Option<&str> {
        self.current_label.as_deref()
    }

    pub fn cur_location(&self) -> Option<SourceLocation> {
        let (source, line_starts) = self.active_source_parts().ok()?;
        Some(location_in(source, line_starts, self.active_offset()))
    }

    pub fn cur_line(&self) -> Option<usize> {
        self.cur_location().map(|location| location.line)
    }

    pub fn cur_pos(&self) -> Option<usize> {
        let (source, line_starts) = self.active_source_parts().ok()?;
        Some(column_index_in(source, line_starts, self.active_offset()))
    }

    pub fn cur_line_str(&self) -> Option<&str> {
        let (source, line_starts) = self.active_source_parts().ok()?;
        Some(line_text_at_in(source, line_starts, self.active_offset()))
    }

    pub fn call_stack_depth(&self) -> usize {
        self.call_stack.len()
    }

    pub fn call_stack(&self) -> &[CallFrame] {
        &self.call_stack
    }

    pub fn macro_definition(&self, name: &str) -> Option<&str> {
        self.macros
            .get(name)
            .map(|definition| definition.source.as_str())
    }

    pub fn macro_definitions(&self) -> impl Iterator<Item = (&str, &str)> {
        self.macros
            .iter()
            .map(|(name, definition)| (name.as_str(), definition.source.as_str()))
    }

    pub fn set_macro_definitions<I, N, S>(&mut self, definitions: I)
    where
        I: IntoIterator<Item = (N, S)>,
        N: Into<String>,
        S: Into<String>,
    {
        self.macros = definitions
            .into_iter()
            .map(|(name, source)| {
                (
                    name.into(),
                    MacroDefinition {
                        source: source.into(),
                    },
                )
            })
            .collect();
    }

    pub fn macro_names(&self) -> impl Iterator<Item = &str> {
        self.macros.keys().map(String::as_str)
    }

    pub fn macro_params(&self) -> &[Attribute] {
        &self.macro_params
    }

    pub fn clear_call_stack(&mut self) {
        self.call_stack.clear();
        self.macro_params.clear();
    }

    pub fn pop_macro_args(&mut self) -> Result<()> {
        self.pop_macro_arguments(SourceSpan::empty(self.active_offset()))
    }

    pub fn store(&self) -> ParserSnapshot {
        ParserSnapshot {
            current_storage: self.current_storage.clone(),
            cursor: self.cursor,
            current_label: self.current_label.clone(),
            call_stack: self.call_stack.clone(),
            macros: self.macros.clone(),
            macro_stack: self.macro_stack.clone(),
            macro_params: self.macro_params.clone(),
            condition_stack: self.condition_stack.clone(),
            interrupted: self.interrupted,
        }
    }

    pub fn restore(&mut self, snapshot: ParserSnapshot) -> Result<()> {
        if let Some(storage) = &snapshot.current_storage
            && !self.scenarios.contains_key(storage)
        {
            return Err(KagError::ScenarioNotLoaded {
                storage: storage.clone(),
            });
        }

        self.current_storage = snapshot.current_storage;
        self.cursor = snapshot.cursor;
        self.current_label = snapshot.current_label;
        self.call_stack = snapshot.call_stack;
        self.macros = snapshot.macros;
        self.macro_stack = snapshot.macro_stack;
        self.macro_params = snapshot.macro_params;
        self.condition_stack = snapshot.condition_stack;
        self.interrupted = snapshot.interrupted;
        Ok(())
    }

    pub fn interrupt(&mut self) {
        self.interrupted = true;
    }

    pub fn reset_interrupt(&mut self) {
        self.interrupted = false;
    }

    pub fn go_to_label(&mut self, label: &str) -> Result<()> {
        if label.trim().is_empty() {
            return Ok(());
        }
        let storage = self.current_storage_name()?.to_owned();
        self.move_to(Some(&storage), Some(label), None::<&mut ()>)
    }

    pub fn go_to_with<H>(
        &mut self,
        storage: Option<&str>,
        target: Option<&str>,
        host: &mut H,
    ) -> Result<()>
    where
        H: KagHost,
    {
        self.move_to(storage, target, Some(host))
    }

    pub fn call_label(&mut self, label: &str) -> Result<()> {
        if label.trim().is_empty() {
            self.push_call_frame()?;
            return Ok(());
        }
        let storage = self.current_storage_name()?.to_owned();
        let preserved_params = self.macro_params.clone();
        self.push_call_frame()?;
        self.move_to(Some(&storage), Some(label), None::<&mut ()>)?;
        self.macro_params = preserved_params;
        Ok(())
    }

    pub fn call_with<H>(
        &mut self,
        storage: Option<&str>,
        target: Option<&str>,
        host: &mut H,
    ) -> Result<()>
    where
        H: KagHost,
    {
        let preserved_params = self.macro_params.clone();
        self.push_call_frame()?;
        self.move_to(storage, target, Some(host))?;
        self.macro_params = preserved_params;
        Ok(())
    }

    pub fn next_tag(&mut self) -> Result<Option<Tag>> {
        let mut host = ();
        self.next_tag_with(&mut host)
    }

    pub fn next_tag_with<H>(&mut self, host: &mut H) -> Result<Option<Tag>>
    where
        H: KagHost,
    {
        loop {
            if self.interrupted {
                self.interrupted = false;
                return Ok(Some(Tag::interrupt()));
            }

            let item = self.next_raw_item()?;

            let mut tag = match item {
                RawItem::Eof => return Ok(None),
                RawItem::ScriptStart { script_start, span } => {
                    let storage = self.current_storage_name()?.to_owned();
                    let (script, span) = self.collect_script_block_from(script_start, span)?;
                    if self.is_executing() {
                        host.on_script(ScriptEvent {
                            storage: &storage,
                            script: &script,
                            span,
                        })?;
                    }
                    continue;
                }
                RawItem::Label(label) => {
                    let storage = self.current_storage_name()?.to_owned();
                    self.current_label = Some(label.name.clone());
                    host.on_label(LabelEvent {
                        storage: &storage,
                        label: &label,
                    })?;
                    continue;
                }
                RawItem::Tag(tag) => tag,
            };

            if self.options.process_special_tags && self.handle_condition_tag(&tag, host)? {
                continue;
            }

            if !self.is_executing() {
                continue;
            }

            self.apply_macro_arguments(&mut tag);

            if self.options.process_cond
                && tag_supports_cond(&tag.tagname, self.options.process_special_tags)
                && !self.process_cond_attr(&mut tag, host)?
            {
                continue;
            }

            if self.options.resolve_entities {
                self.resolve_entities(&mut tag, host)?;
            }

            if !self.options.process_special_tags && self.macros.contains_key(&tag.tagname) {
                self.expand_macro(tag)?;
                continue;
            }

            if self.options.process_special_tags && self.handle_special_tag(tag.clone(), host)? {
                continue;
            }

            return Ok(Some(tag));
        }
    }

    fn install_scenario(&mut self, storage: String, source: String) -> Result<()> {
        let scenario = Scenario::new(storage.clone(), source)?;
        Arc::make_mut(&mut self.scenarios).insert(storage.clone(), scenario);
        self.current_storage = Some(storage);
        self.cursor = Cursor::default();
        self.current_label = None;
        self.condition_stack.clear();
        self.macro_stack.clear();
        self.macro_params.clear();
        Ok(())
    }

    fn load_scenario_from_host<H>(&mut self, storage: &str, host: &mut H) -> Result<()>
    where
        H: KagHost,
    {
        let event = ScenarioLoadEvent { storage };
        let source = match host.on_scenario_load(event)? {
            Some(source) => source,
            None => host.load_scenario(storage)?,
        };
        self.install_scenario(storage.to_owned(), source)?;
        host.on_scenario_loaded(ScenarioLoadEvent { storage })?;
        Ok(())
    }

    fn current_storage_name(&self) -> Result<&str> {
        self.current_storage.as_deref().ok_or(KagError::NoScenario)
    }

    fn current_scenario(&self) -> Result<&Scenario> {
        let storage = self.current_storage_name()?;
        self.scenarios
            .get(storage)
            .ok_or_else(|| KagError::ScenarioNotLoaded {
                storage: storage.to_owned(),
            })
    }

    fn parse_error(&self, span: SourceSpan, message: impl Into<String>) -> KagError {
        let storage = self.current_storage.clone();
        KagError::Parse {
            storage,
            span: Some(span),
            message: message.into(),
        }
    }

    fn active_is_scenario(&self) -> bool {
        self.macro_stack.is_empty()
    }

    fn active_offset(&self) -> usize {
        self.macro_stack
            .last()
            .map(|frame| frame.offset)
            .unwrap_or(self.cursor.offset)
    }

    fn set_active_offset(&mut self, offset: usize) {
        if let Some(frame) = self.macro_stack.last_mut() {
            frame.offset = offset;
        } else {
            self.cursor.offset = offset;
        }
    }

    fn active_source_parts(&self) -> Result<(&str, &[usize])> {
        if let Some(frame) = self.macro_stack.last() {
            Ok((&frame.source, &frame.line_starts))
        } else {
            let scenario = self.current_scenario()?;
            Ok((&scenario.source, &scenario.line_starts))
        }
    }

    fn active_origin(&self, scenario_origin: TagOrigin) -> TagOrigin {
        self.macro_stack
            .last()
            .map(|frame| frame.origin.clone())
            .unwrap_or(scenario_origin)
    }

    fn pop_finished_expansion(&mut self) -> bool {
        let Some(frame) = self.macro_stack.last() else {
            return false;
        };
        if frame.offset < frame.source.len() {
            return false;
        }

        let frame = self
            .macro_stack
            .pop()
            .expect("last checked that an expansion frame exists");
        drop(frame);
        true
    }

    fn skip_line_start_tabs(&mut self) -> Result<()> {
        loop {
            let should_skip = {
                let (source, line_starts) = self.active_source_parts()?;
                let offset = self.active_offset();
                only_tabs_since_line_start_in(source, line_starts, offset)
                    && char_at_in(source, offset) == Some('\t')
            };

            if !should_skip {
                return Ok(());
            }
            self.set_active_offset(self.active_offset() + 1);
        }
    }

    fn next_raw_item(&mut self) -> Result<RawItem> {
        loop {
            while self.pop_finished_expansion() {}
            self.skip_line_start_tabs()?;

            let offset = self.active_offset();
            let Some(ch) = ({
                let (source, _) = self.active_source_parts()?;
                char_at_in(source, offset)
            }) else {
                return Ok(RawItem::Eof);
            };

            if !self.options.ignore_cr && {
                let (source, _) = self.active_source_parts()?;
                is_line_continuation_in(source, offset)
            } {
                let next = {
                    let (source, _) = self.active_source_parts()?;
                    line_end_with_newline_in(source, offset)
                };
                self.set_active_offset(next);
                continue;
            }

            let at_line_head = {
                let (source, line_starts) = self.active_source_parts()?;
                only_tabs_since_line_start_in(source, line_starts, offset)
            };
            if self.active_is_scenario() && at_line_head {
                match ch {
                    ';' => {
                        let next = {
                            let (source, _) = self.active_source_parts()?;
                            line_end_with_newline_in(source, offset)
                        };
                        self.set_active_offset(next);
                        continue;
                    }
                    '*' => {
                        let label = self.current_scenario()?.parse_label_at(offset)?;
                        let next = self.current_scenario()?.line_end_with_newline(offset);
                        self.set_active_offset(next);
                        return Ok(RawItem::Label(label));
                    }
                    '[' | '@' => {
                        if let Some((script_start, span)) =
                            self.current_scenario()?.script_start_at(offset)
                        {
                            self.set_active_offset(script_start);
                            return Ok(RawItem::ScriptStart { script_start, span });
                        }
                        if ch == '@' {
                            return self.read_command_line();
                        }
                    }
                    _ => {}
                }
            }

            if ch == '\r' || ch == '\n' {
                if !self.active_is_scenario() {
                    let (span, location) = {
                        let (source, line_starts) = self.active_source_parts()?;
                        (
                            SourceSpan::new(offset, newline_end_in(source, offset)),
                            location_in(source, line_starts, offset),
                        )
                    };
                    self.set_active_offset(span.end);
                    return Ok(RawItem::Tag(Tag::new(
                        "r",
                        Vec::new(),
                        self.active_origin(TagOrigin::Newline),
                        span,
                        location,
                    )));
                }

                let (span, location, suppress_newline) = {
                    let (source, line_starts) = self.active_source_parts()?;
                    (
                        SourceSpan::new(offset, newline_end_in(source, offset)),
                        location_in(source, line_starts, offset),
                        self.options.ignore_cr
                            || line_ends_with_page_break_in(source, line_starts, offset),
                    )
                };
                self.set_active_offset(span.end);
                if suppress_newline {
                    continue;
                }
                return Ok(RawItem::Tag(Tag::newline(span, location)));
            }

            if ch == '\t' {
                self.set_active_offset(offset + ch.len_utf8());
                continue;
            }

            if ch == '[' {
                let is_escaped_bracket = {
                    let (source, _) = self.active_source_parts()?;
                    source[offset..].starts_with("[[")
                };
                if is_escaped_bracket {
                    let location = {
                        let (source, line_starts) = self.active_source_parts()?;
                        location_in(source, line_starts, offset)
                    };
                    let span = SourceSpan::new(offset, offset + 2);
                    self.set_active_offset(offset + 2);
                    return Ok(RawItem::Tag(Tag::new(
                        "ch",
                        vec![Attribute::named(
                            "text",
                            AttributeValue::Literal("[".into()),
                        )],
                        self.active_origin(TagOrigin::Character),
                        span,
                        location,
                    )));
                }
                return self.read_bracket_tag();
            }

            return self.read_character();
        }
    }

    fn read_character(&mut self) -> Result<RawItem> {
        let offset = self.active_offset();
        let (ch, end, location) = {
            let (source, line_starts) = self.active_source_parts()?;
            let ch = char_at_in(source, offset).expect("read_character requires a valid character");
            (
                ch,
                offset + ch.len_utf8(),
                location_in(source, line_starts, offset),
            )
        };
        let tag = Tag::new(
            "ch",
            vec![Attribute::named(
                "text",
                AttributeValue::Literal(ch.to_string()),
            )],
            self.active_origin(TagOrigin::Character),
            SourceSpan::new(offset, end),
            location,
        );
        self.set_active_offset(end);
        Ok(RawItem::Tag(tag))
    }

    fn read_command_line(&mut self) -> Result<RawItem> {
        let start = self.active_offset();
        let (tag, next) = {
            let storage = self.current_storage_name()?.to_owned();
            let (source, line_starts) = self.active_source_parts()?;
            let line_end = line_end_in(source, start);
            let content_start = start + 1;
            let content = &source[content_start..line_end];
            let span = SourceSpan::new(start, line_end);
            let tag = parse_tag_content(
                Some(&storage),
                source,
                line_starts,
                content,
                content_start,
                span,
                TagOrigin::CommandLine,
            )?;
            (tag, line_end_with_newline_in(source, start))
        };
        self.set_active_offset(next);
        Ok(RawItem::Tag(tag))
    }

    fn read_bracket_tag(&mut self) -> Result<RawItem> {
        let start = self.active_offset();
        let (tag, next) = {
            let storage = self.current_storage_name()?.to_owned();
            let (source, line_starts) = self.active_source_parts()?;
            let content_start = start + 1;
            let mut quote = None;
            let mut escaped = false;
            let mut close = None;

            for (relative, ch) in source[content_start..].char_indices() {
                let offset = content_start + relative;
                if ch == '\r' || ch == '\n' {
                    break;
                }
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '`' {
                    escaped = true;
                    continue;
                }
                if let Some(active_quote) = quote {
                    if ch == active_quote {
                        quote = None;
                    }
                    continue;
                }

                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                } else if ch == ']' {
                    close = Some(offset);
                    break;
                }
            }

            let close = close.ok_or_else(|| {
                self.parse_error(SourceSpan::new(start, source.len()), "unterminated KAG tag")
            })?;
            let content = &source[content_start..close];
            let span = SourceSpan::new(start, close + 1);
            let tag = parse_tag_content(
                Some(&storage),
                source,
                line_starts,
                content,
                content_start,
                span,
                self.active_origin(TagOrigin::Bracket),
            )?;
            (tag, close + 1)
        };
        self.set_active_offset(next);
        Ok(RawItem::Tag(tag))
    }

    fn active_slice(&self, span: SourceSpan) -> Result<&str> {
        let (source, _) = self.active_source_parts()?;
        source
            .get(span.start..span.end)
            .ok_or_else(|| self.parse_error(span, "source span is outside the active input"))
    }

    fn push_expansion(
        &mut self,
        name: impl Into<String>,
        source: String,
        previous_params: Option<Vec<Attribute>>,
    ) -> Result<()> {
        if source.is_empty() {
            return Ok(());
        }
        if self.macro_stack.len() >= self.options.max_macro_depth {
            return Err(KagError::MacroDepthExceeded {
                limit: self.options.max_macro_depth,
            });
        }

        let name = name.into();
        self.macro_stack.push(ExpansionFrame {
            name: name.clone(),
            line_starts: line_starts_for(&source),
            source,
            offset: 0,
            origin: TagOrigin::MacroExpansion { name },
            previous_params,
        });
        Ok(())
    }

    fn handle_condition_tag<H>(&mut self, tag: &Tag, host: &mut H) -> Result<bool>
    where
        H: KagHost,
    {
        match tag.tagname.as_str() {
            "if" => {
                let parent_active = self.is_executing();
                let condition = if parent_active {
                    self.eval_bool_attr(tag, "exp", host)?
                } else {
                    false
                };
                self.condition_stack.push(ConditionFrame {
                    parent_active,
                    branch_taken: parent_active && condition,
                    current_active: parent_active && condition,
                });
                Ok(true)
            }
            "ignore" => {
                let parent_active = self.is_executing();
                let should_ignore = if parent_active {
                    self.eval_bool_attr(tag, "exp", host)?
                } else {
                    true
                };
                self.condition_stack.push(ConditionFrame {
                    parent_active,
                    branch_taken: !should_ignore,
                    current_active: parent_active && !should_ignore,
                });
                Ok(true)
            }
            "elsif" => {
                let Some(frame) = self.condition_stack.last() else {
                    return Ok(true);
                };
                let parent_active = frame.parent_active;
                let branch_taken = frame.branch_taken;
                let condition = if parent_active && !branch_taken {
                    self.eval_bool_attr(tag, "exp", host)?
                } else {
                    false
                };
                let frame = self
                    .condition_stack
                    .last_mut()
                    .expect("condition frame was checked above");
                frame.current_active = parent_active && !branch_taken && condition;
                frame.branch_taken = branch_taken || frame.current_active;
                Ok(true)
            }
            "else" => {
                if let Some(frame) = self.condition_stack.last_mut() {
                    frame.current_active = frame.parent_active && !frame.branch_taken;
                    frame.branch_taken = true;
                }
                Ok(true)
            }
            "endif" => {
                self.condition_stack.pop();
                Ok(true)
            }
            "endignore" => {
                self.condition_stack.pop();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn handle_special_tag<H>(&mut self, tag: Tag, host: &mut H) -> Result<bool>
    where
        H: KagHost,
    {
        match tag.tagname.as_str() {
            "macro" => {
                self.define_macro(tag)?;
                Ok(true)
            }
            "endmacro" => {
                Err(self.parse_error(tag.span, "endmacro tag without a matching macro tag"))
            }
            "erasemacro" => {
                let name = self.required_attr_string(&tag, "name", host)?;
                if self.macros.remove(&name).is_none() {
                    return Err(self.parse_error(tag.span, format!("unknown macro {name:?}")));
                }
                Ok(true)
            }
            "emb" => {
                let expression = self.required_attr_string(&tag, "exp", host)?;
                let text = host.eval_string(&expression)?;
                self.push_emb_text(&text)?;
                Ok(true)
            }
            "jump" => {
                self.process_jump(tag, host)?;
                Ok(true)
            }
            "call" => {
                self.process_call(tag, host)?;
                Ok(true)
            }
            "return" => {
                self.process_return(tag, host)?;
                Ok(true)
            }
            "macropop" => {
                self.pop_macro_arguments(tag.span)?;
                Ok(true)
            }
            _ => {
                if self.macros.contains_key(&tag.tagname) {
                    self.expand_macro(tag)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
        }
    }

    fn define_macro(&mut self, tag: Tag) -> Result<()> {
        let name = tag
            .literal_attr("name")
            .ok_or_else(|| KagError::MissingAttribute {
                tag: tag.tagname.clone(),
                attribute: "name".to_owned(),
            })?
            .to_lowercase();
        let source = self.capture_macro_body(&tag)?;
        self.macros.insert(name, MacroDefinition { source });
        Ok(())
    }

    fn capture_macro_body(&mut self, opening_tag: &Tag) -> Result<String> {
        let mut body = String::new();
        loop {
            let item = self.next_raw_item()?;
            let tag = match item {
                RawItem::Eof => {
                    return Err(self.parse_error(opening_tag.span, "macro tag without endmacro"));
                }
                RawItem::Label(label) => {
                    return Err(self.parse_error(label.span, "label cannot be used inside a macro"));
                }
                RawItem::ScriptStart { span, .. } => {
                    return Err(self.parse_error(span, "iscript cannot be used inside a macro"));
                }
                RawItem::Tag(tag) => tag,
            };

            match tag.tagname.as_str() {
                "endmacro" => {
                    body.push_str("[macropop]");
                    return Ok(body);
                }
                _ => body.push_str(&self.recorded_macro_fragment(&tag)?),
            }
        }
    }

    fn expand_macro(&mut self, invocation: Tag) -> Result<()> {
        let definition = self
            .macros
            .get(&invocation.tagname)
            .cloned()
            .ok_or_else(|| KagError::Parse {
                storage: self.current_storage.clone(),
                span: Some(invocation.span),
                message: format!("unknown macro {:?}", invocation.tagname),
            })?;
        let params: Vec<_> = invocation
            .attributes
            .iter()
            .filter(|attribute| attribute.name() != Some("cond"))
            .cloned()
            .collect();
        let previous_params = std::mem::replace(&mut self.macro_params, params);
        self.push_expansion(invocation.tagname, definition.source, Some(previous_params))?;
        Ok(())
    }

    fn recorded_macro_fragment(&self, tag: &Tag) -> Result<String> {
        match tag.origin {
            TagOrigin::CommandLine => {
                let raw = self.active_slice(tag.span)?;
                Ok(format!("[{}]", &raw[1..]))
            }
            TagOrigin::Newline => Ok("[r eol=true]".to_owned()),
            _ => Ok(self.active_slice(tag.span)?.to_owned()),
        }
    }

    fn push_emb_text(&mut self, text: &str) -> Result<()> {
        self.push_expansion("<emb>", escape_embedded_text(text), None)
    }

    fn pop_macro_arguments(&mut self, span: SourceSpan) -> Result<()> {
        for frame in self.macro_stack.iter_mut().rev() {
            if let Some(previous_params) = frame.previous_params.take() {
                self.macro_params = previous_params;
                return Ok(());
            }
        }

        Err(self.parse_error(span, "macropop tag without macro args"))
    }

    fn collect_script_block_from(
        &mut self,
        script_start: usize,
        start_span: SourceSpan,
    ) -> Result<(String, SourceSpan)> {
        let scenario = self.current_scenario()?;
        let mut offset = script_start;
        let mut script = String::new();

        while offset < scenario.source.len() {
            if scenario.script_end_at(offset) {
                self.cursor.offset = scenario.line_end_with_newline(offset);
                return Ok((script, SourceSpan::new(script_start, offset)));
            }
            let end = scenario.line_end(offset);
            script.push_str(&scenario.source[offset..end]);
            script.push_str("\r\n");
            offset = scenario.line_end_with_newline(offset);
        }

        Err(self.parse_error(
            SourceSpan::new(start_span.start, scenario.source.len()),
            "iscript block without endscript",
        ))
    }

    fn process_jump<H>(&mut self, tag: Tag, host: &mut H) -> Result<()>
    where
        H: KagHost,
    {
        let storage = self.optional_attr_string(&tag, "storage", host)?;
        let target = self.optional_attr_string(&tag, "target", host)?;
        if host.on_jump(&tag, storage.as_deref(), target.as_deref())? {
            self.move_to(storage.as_deref(), target.as_deref(), Some(host))?;
        }
        Ok(())
    }

    fn process_call<H>(&mut self, tag: Tag, host: &mut H) -> Result<()>
    where
        H: KagHost,
    {
        let storage = self.optional_attr_string(&tag, "storage", host)?;
        let target = self.optional_attr_string(&tag, "target", host)?;
        if host.on_call(&tag, storage.as_deref(), target.as_deref())? {
            let preserved_params = self.macro_params.clone();
            self.push_call_frame()?;
            self.move_to(storage.as_deref(), target.as_deref(), Some(host))?;
            self.macro_params = preserved_params;
        }
        Ok(())
    }

    fn process_return<H>(&mut self, tag: Tag, host: &mut H) -> Result<()>
    where
        H: KagHost,
    {
        let storage = self.optional_attr_string(&tag, "storage", host)?;
        let target = self.optional_attr_string(&tag, "target", host)?;
        host.on_call_stack_depth(self.call_stack.len())?;
        if !host.on_return(&tag, storage.as_deref(), target.as_deref())? {
            self.interrupt();
            return Ok(());
        }

        let explicit_target = storage.as_deref().is_some_and(|value| !value.is_empty())
            || target
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());

        let frame = self.call_stack.pop().ok_or(KagError::ReturnStackEmpty)?;
        if explicit_target {
            let preserved_params = frame.resume.macro_params.clone();
            self.move_to(storage.as_deref(), target.as_deref(), Some(host))?;
            self.macro_params = preserved_params;
            host.on_after_return(&frame)?;
            return Ok(());
        }

        self.restore_call_frame(&frame)?;
        host.on_after_return(&frame)?;
        Ok(())
    }

    fn push_call_frame(&mut self) -> Result<()> {
        let storage = self.current_storage_name()?.to_owned();
        let line_text = self
            .current_scenario()?
            .line_text_at(self.cursor.offset)
            .to_owned();
        self.call_stack.push(CallFrame {
            storage,
            offset: self.cursor.offset,
            line_text,
            current_label: self.current_label.clone(),
            resume: ResumeState {
                macro_stack: self.macro_stack.clone(),
                macro_params: self.macro_params.clone(),
                condition_stack: self.condition_stack.clone(),
            },
        });
        Ok(())
    }

    fn restore_call_frame(&mut self, frame: &CallFrame) -> Result<()> {
        let scenario =
            self.scenarios
                .get(&frame.storage)
                .ok_or_else(|| KagError::ScenarioNotLoaded {
                    storage: frame.storage.clone(),
                })?;
        if frame.offset > scenario.source.len() {
            return Err(KagError::ReturnLostSync {
                storage: frame.storage.clone(),
            });
        }
        if frame.offset < scenario.source.len()
            && scenario.line_text_at(frame.offset) != frame.line_text
        {
            return Err(KagError::ReturnLostSync {
                storage: frame.storage.clone(),
            });
        }
        self.current_storage = Some(frame.storage.clone());
        self.cursor.offset = frame.offset;
        self.current_label = frame.current_label.clone();
        self.macro_stack = frame.resume.macro_stack.clone();
        self.macro_params = frame.resume.macro_params.clone();
        self.condition_stack = frame.resume.condition_stack.clone();
        Ok(())
    }

    fn move_to<H>(
        &mut self,
        storage: Option<&str>,
        target: Option<&str>,
        host: Option<&mut H>,
    ) -> Result<()>
    where
        H: KagHost,
    {
        let has_storage = storage.is_some_and(|storage| !storage.is_empty());
        let normalized_target = target.and_then(normalize_target_label);
        if !has_storage && normalized_target.is_none() {
            return Ok(());
        }

        let next_storage = if has_storage {
            storage.expect("has_storage was checked above").to_owned()
        } else {
            self.current_storage_name()?.to_owned()
        };

        if has_storage {
            if !self.scenarios.contains_key(&next_storage) {
                match host {
                    Some(host) => self.load_scenario_from_host(&next_storage, host)?,
                    None => {
                        return Err(KagError::ScenarioNotLoaded {
                            storage: next_storage,
                        });
                    }
                }
            } else {
                self.current_storage = Some(next_storage.clone());
                self.cursor = Cursor::default();
                self.current_label = None;
            }
        }

        if let Some(label) = normalized_target.as_deref() {
            let scenario = self.current_scenario()?;
            let label = scenario
                .labels
                .get(label)
                .ok_or_else(|| KagError::LabelNotFound {
                    storage: scenario.storage.clone(),
                    label: label.to_owned(),
                })?;
            self.cursor.offset = label.cursor_offset;
        } else if has_storage {
            self.cursor.offset = 0;
        }

        self.condition_stack.clear();
        self.macro_stack.clear();
        self.macro_params.clear();
        Ok(())
    }

    fn process_cond_attr<H>(&self, tag: &mut Tag, host: &mut H) -> Result<bool>
    where
        H: KagHost,
    {
        let Some(value) = tag.take_attr("cond") else {
            return Ok(true);
        };
        let expression = self.attr_value_to_string(&value, host)?;
        host.eval_bool(&expression)
    }

    fn apply_macro_arguments(&self, tag: &mut Tag) {
        if self.macro_params.is_empty() {
            let mut attributes = Vec::new();
            for attribute in std::mem::take(&mut tag.attributes) {
                match attribute {
                    Attribute::Spread => {}
                    Attribute::Named {
                        name,
                        value: AttributeValue::MacroArgument(value),
                    } => attributes.push(Attribute::Named {
                        name,
                        value: AttributeValue::Literal(value),
                    }),
                    Attribute::Named { name, value } => {
                        attributes.push(Attribute::Named { name, value });
                    }
                }
            }
            tag.attributes = attributes;
            return;
        }

        let mut attributes = Vec::new();
        for attribute in std::mem::take(&mut tag.attributes) {
            match attribute {
                Attribute::Spread => attributes.extend(self.macro_params.iter().cloned()),
                Attribute::Named { name, value } => {
                    attributes.push(Attribute::Named {
                        name,
                        value: self.resolve_macro_argument_value(value),
                    });
                }
            }
        }
        tag.attributes = attributes;
    }

    fn resolve_macro_argument_value(&self, value: AttributeValue) -> AttributeValue {
        let AttributeValue::MacroArgument(spec) = value else {
            return value;
        };

        let (name, default) = spec
            .split_once('|')
            .map_or((spec.as_str(), None), |(name, default)| {
                (name, Some(default.to_owned()))
            });
        for param in &self.macro_params {
            if let Attribute::Named {
                name: param_name,
                value,
            } = param
                && param_name == name
            {
                return value.clone();
            }
        }

        AttributeValue::Literal(default.unwrap_or_default())
    }

    fn eval_bool_attr<H>(&self, tag: &Tag, attr: &str, host: &mut H) -> Result<bool>
    where
        H: KagHost,
    {
        let expression = self.required_attr_string(tag, attr, host)?;
        host.eval_bool(&expression)
    }

    fn required_attr_string<H>(&self, tag: &Tag, attr: &str, host: &mut H) -> Result<String>
    where
        H: KagHost,
    {
        let value = tag.attr(attr).ok_or_else(|| KagError::MissingAttribute {
            tag: tag.tagname.clone(),
            attribute: attr.to_owned(),
        })?;
        self.attr_value_to_string(value, host)
    }

    fn optional_attr_string<H>(&self, tag: &Tag, attr: &str, host: &mut H) -> Result<Option<String>>
    where
        H: KagHost,
    {
        tag.attr(attr)
            .map(|value| self.attr_value_to_string(value, host))
            .transpose()
    }

    fn attr_value_to_string<H>(&self, value: &AttributeValue, host: &mut H) -> Result<String>
    where
        H: KagHost,
    {
        match value {
            AttributeValue::Literal(value) => Ok(value.clone()),
            AttributeValue::Expression(expression) => host.eval_string(expression),
            AttributeValue::MacroArgument(value) => {
                let resolved =
                    self.resolve_macro_argument_value(AttributeValue::MacroArgument(value.clone()));
                self.attr_value_to_string(&resolved, host)
            }
        }
    }

    fn resolve_entities<H>(&self, tag: &mut Tag, host: &mut H) -> Result<()>
    where
        H: KagHost,
    {
        for attribute in &mut tag.attributes {
            let Attribute::Named { value, .. } = attribute else {
                continue;
            };
            if let AttributeValue::Expression(expression) = value {
                *value = AttributeValue::Literal(host.eval_string(expression)?);
            } else if let AttributeValue::MacroArgument(argument) = value {
                *value = self
                    .resolve_macro_argument_value(AttributeValue::MacroArgument(argument.clone()));
            }
        }
        Ok(())
    }

    fn is_executing(&self) -> bool {
        self.condition_stack
            .last()
            .map(|frame| frame.current_active)
            .unwrap_or(true)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Cursor {
    offset: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResumeState {
    macro_stack: Vec<ExpansionFrame>,
    macro_params: Vec<Attribute>,
    condition_stack: Vec<ConditionFrame>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MacroDefinition {
    source: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpansionFrame {
    name: String,
    source: String,
    line_starts: Vec<usize>,
    offset: usize,
    origin: TagOrigin,
    previous_params: Option<Vec<Attribute>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConditionFrame {
    parent_active: bool,
    branch_taken: bool,
    current_active: bool,
}

#[derive(Clone, Debug)]
struct Scenario {
    storage: String,
    source: String,
    line_starts: Vec<usize>,
    labels: BTreeMap<String, Label>,
    labels_by_offset: BTreeMap<usize, Label>,
}

impl Scenario {
    fn new(storage: String, source: String) -> Result<Self> {
        if source.is_empty() {
            return Err(KagError::parse_at(
                storage,
                SourceSpan::empty(0),
                "scenario has no lines",
            ));
        }

        let source = strip_leading_tabs_per_line(&source);
        let mut line_starts = vec![0];
        let mut iter = source.char_indices().peekable();
        while let Some((offset, ch)) = iter.next() {
            let next = if ch == '\r' {
                if let Some((next_offset, '\n')) = iter.peek().copied() {
                    iter.next();
                    next_offset + 1
                } else {
                    offset + ch.len_utf8()
                }
            } else if ch == '\n' {
                offset + ch.len_utf8()
            } else {
                continue;
            };

            if next < source.len() {
                line_starts.push(next);
            }
        }

        let mut scenario = Self {
            storage,
            source,
            line_starts,
            labels: BTreeMap::new(),
            labels_by_offset: BTreeMap::new(),
        };
        scenario.index_labels()?;
        Ok(scenario)
    }

    fn index_labels(&mut self) -> Result<()> {
        let mut offset = 0;
        let mut previous_label: Option<String> = None;
        let mut label_counts: BTreeMap<String, usize> = BTreeMap::new();
        while offset < self.source.len() {
            if let Some(mut label) = self.parse_label_line(offset, previous_label.as_deref())? {
                let base_name = label.name.clone();
                let count = label_counts.entry(base_name.clone()).or_insert(0);
                *count += 1;
                if *count > 1 {
                    label.name = format!("{base_name}:{}", *count);
                }
                let by_offset = label.clone();
                self.labels.insert(label.name.clone(), label);
                self.labels_by_offset.insert(offset, by_offset);
                previous_label = Some(base_name);
            }
            offset = self.line_end_with_newline(offset);
        }
        Ok(())
    }

    fn char_at(&self, offset: usize) -> Option<char> {
        self.source.get(offset..)?.chars().next()
    }

    fn location(&self, offset: usize) -> SourceLocation {
        let line_index = match self.line_starts.binary_search(&offset) {
            Ok(index) => index,
            Err(0) => 0,
            Err(index) => index - 1,
        };
        let line_start = self.line_starts.get(line_index).copied().unwrap_or(0);
        let column = self.source[line_start..offset.min(self.source.len())]
            .chars()
            .count()
            + 1;
        SourceLocation::new(offset, line_index + 1, column)
    }

    fn line_end(&self, offset: usize) -> usize {
        let mut cursor = offset.min(self.source.len());
        while cursor < self.source.len() {
            let ch = self
                .char_at(cursor)
                .expect("cursor is checked against source length");
            if ch == '\r' || ch == '\n' {
                break;
            }
            cursor += ch.len_utf8();
        }
        cursor
    }

    fn newline_end(&self, offset: usize) -> usize {
        if self.source[offset..].starts_with("\r\n") {
            offset + 2
        } else {
            offset + self.char_at(offset).map(char::len_utf8).unwrap_or(0)
        }
    }

    fn line_end_with_newline(&self, offset: usize) -> usize {
        let end = self.line_end(offset);
        if end >= self.source.len() {
            end
        } else {
            self.newline_end(end)
        }
    }

    fn line_text_at(&self, offset: usize) -> &str {
        line_text_at_in(&self.source, &self.line_starts, offset)
    }

    fn parse_label_at(&self, line_start: usize) -> Result<Label> {
        self.labels_by_offset
            .get(&line_start)
            .cloned()
            .ok_or_else(|| KagError::parse("line is not a label"))
    }

    fn parse_label_line(
        &self,
        line_start: usize,
        previous_label: Option<&str>,
    ) -> Result<Option<Label>> {
        let line_end = self.line_end(line_start);
        let line = &self.source[line_start..line_end];
        if !line.starts_with('*') {
            return Ok(None);
        }

        let pipe = line.find('|');
        let raw_name = pipe.map_or(line, |index| &line[..index]);
        let name = if raw_name == "*" {
            previous_label.ok_or_else(|| {
                KagError::parse_at(
                    self.storage.clone(),
                    SourceSpan::new(line_start, line_end),
                    "first label cannot omit its name",
                )
            })?
        } else {
            raw_name
        };

        let page_name = pipe.map(|index| line[index + 1..].to_owned());
        Ok(Some(Label {
            name: name.to_owned(),
            page_name,
            span: SourceSpan::new(line_start, line_end),
            location: self.location(line_start),
            cursor_offset: line_start,
        }))
    }

    fn script_start_at(&self, line_start: usize) -> Option<(usize, SourceSpan)> {
        let line_end = self.line_end(line_start);
        let line = &self.source[line_start..line_end];
        matches!(line, "[iscript]" | "[iscript]\\" | "@iscript").then_some((
            self.line_end_with_newline(line_start),
            SourceSpan::new(line_start, line_end),
        ))
    }

    fn script_end_at(&self, line_start: usize) -> bool {
        let line_end = self.line_end(line_start);
        matches!(
            &self.source[line_start..line_end],
            "[endscript]" | "[endscript]\\" | "@endscript"
        )
    }
}

fn line_starts_for(source: &str) -> Vec<usize> {
    let mut line_starts = vec![0];
    let mut iter = source.char_indices().peekable();
    while let Some((offset, ch)) = iter.next() {
        let next = if ch == '\r' {
            if let Some((next_offset, '\n')) = iter.peek().copied() {
                iter.next();
                next_offset + 1
            } else {
                offset + ch.len_utf8()
            }
        } else if ch == '\n' {
            offset + ch.len_utf8()
        } else {
            continue;
        };

        if next < source.len() {
            line_starts.push(next);
        }
    }
    line_starts
}

fn char_at_in(source: &str, offset: usize) -> Option<char> {
    source.get(offset..)?.chars().next()
}

fn location_in(source: &str, line_starts: &[usize], offset: usize) -> SourceLocation {
    let line_index = match line_starts.binary_search(&offset) {
        Ok(index) => index,
        Err(0) => 0,
        Err(index) => index - 1,
    };
    let line_start = line_starts.get(line_index).copied().unwrap_or(0);
    let column = source[line_start..offset.min(source.len())].chars().count() + 1;
    SourceLocation::new(offset, line_index + 1, column)
}

fn line_start_in(line_starts: &[usize], offset: usize) -> usize {
    match line_starts.binary_search(&offset) {
        Ok(index) => line_starts[index],
        Err(0) => 0,
        Err(index) => line_starts[index - 1],
    }
}

fn column_index_in(source: &str, line_starts: &[usize], offset: usize) -> usize {
    let line_start = line_start_in(line_starts, offset);
    source[line_start..offset.min(source.len())].chars().count()
}

fn line_end_in(source: &str, offset: usize) -> usize {
    let mut cursor = offset.min(source.len());
    while cursor < source.len() {
        let ch = char_at_in(source, cursor).expect("cursor is checked against source length");
        if ch == '\r' || ch == '\n' {
            break;
        }
        cursor += ch.len_utf8();
    }
    cursor
}

fn line_text_at_in<'a>(source: &'a str, line_starts: &[usize], offset: usize) -> &'a str {
    let start = line_start_in(line_starts, offset);
    let end = line_end_in(source, start);
    &source[start..end]
}

fn newline_end_in(source: &str, offset: usize) -> usize {
    if source[offset..].starts_with("\r\n") {
        offset + 2
    } else {
        offset + char_at_in(source, offset).map(char::len_utf8).unwrap_or(0)
    }
}

fn line_end_with_newline_in(source: &str, offset: usize) -> usize {
    let end = line_end_in(source, offset);
    if end >= source.len() {
        end
    } else {
        newline_end_in(source, end)
    }
}

fn only_tabs_since_line_start_in(source: &str, line_starts: &[usize], offset: usize) -> bool {
    if offset > source.len() {
        return false;
    }
    let start = line_start_in(line_starts, offset);
    source[start..offset].chars().all(|ch| ch == '\t')
}

fn is_line_continuation_in(source: &str, offset: usize) -> bool {
    source[offset..].starts_with("\\\n")
        || source[offset..].starts_with("\\\r\n")
        || source[offset..].starts_with("\\\r")
}

fn line_ends_with_page_break_in(
    source: &str,
    line_starts: &[usize],
    newline_offset: usize,
) -> bool {
    let line_start = line_start_in(line_starts, newline_offset);
    source[line_start..newline_offset].ends_with("[p]")
}

fn escape_embedded_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '[' {
            escaped.push('[');
        }
        escaped.push(ch);
    }
    escaped
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RawItem {
    Tag(Tag),
    Label(Label),
    ScriptStart {
        script_start: usize,
        span: SourceSpan,
    },
    Eof,
}

fn parse_tag_content(
    storage: Option<&str>,
    source: &str,
    line_starts: &[usize],
    content: &str,
    base_offset: usize,
    span: SourceSpan,
    origin: TagOrigin,
) -> Result<Tag> {
    let mut parser = AttributeParser {
        storage,
        content,
        base_offset,
        pos: 0,
    };
    parser.skip_space();
    let tag_start = parser.pos;
    while let Some(ch) = parser.peek() {
        if is_kag_ws(ch) {
            break;
        }
        parser.bump();
    }

    if parser.pos == tag_start {
        return Err(KagError::parse_at(
            storage.unwrap_or_default().to_owned(),
            span,
            "KAG tag name is empty",
        ));
    }

    let tagname = content[tag_start..parser.pos].to_lowercase();
    let mut attributes = Vec::new();
    loop {
        parser.skip_space();
        if parser.is_eof() {
            break;
        }
        attributes.push(parser.parse_attribute()?);
    }

    Ok(Tag::new(
        tagname,
        attributes,
        origin,
        span,
        location_in(source, line_starts, span.start),
    ))
}

struct AttributeParser<'a> {
    storage: Option<&'a str>,
    content: &'a str,
    base_offset: usize,
    pos: usize,
}

impl AttributeParser<'_> {
    fn is_eof(&self) -> bool {
        self.pos >= self.content.len()
    }

    fn peek(&self) -> Option<char> {
        self.content.get(self.pos..)?.chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(is_kag_ws) {
            self.bump();
        }
    }

    fn parse_attribute(&mut self) -> Result<Attribute> {
        if self.peek() == Some('*') {
            self.bump();
            if self.peek().is_none_or(is_kag_ws) {
                return Ok(Attribute::Spread);
            }
            return Err(self.error_here("macro spread marker must be a standalone '*'"));
        }

        let name_start = self.pos;
        while let Some(ch) = self.peek() {
            if is_kag_ws(ch) || ch == '=' {
                break;
            }
            self.bump();
        }
        let name = self.content[name_start..self.pos].to_lowercase();
        if name.is_empty() {
            return Err(self.error_here("attribute name is empty"));
        }

        self.skip_space();
        let value = if self.peek() == Some('=') {
            self.bump();
            self.skip_space();
            if self.is_eof() {
                return Err(self.error_here("attribute value is empty"));
            }
            self.parse_value()?
        } else {
            AttributeValue::Literal("true".to_owned())
        };

        Ok(Attribute::named(name, value))
    }

    fn parse_value(&mut self) -> Result<AttributeValue> {
        let mut entity = false;
        let mut macro_arg = false;

        match self.peek() {
            Some('&') => {
                entity = true;
                self.bump();
            }
            Some('%') => {
                macro_arg = true;
                self.bump();
            }
            _ => {}
        }

        let mut value = if matches!(self.peek(), Some('"') | Some('\'')) {
            self.parse_quoted_value()?
        } else {
            self.parse_unquoted_value()
        };

        if !entity && value.starts_with('&') {
            entity = true;
            value.remove(0);
        } else if !macro_arg && value.starts_with('%') {
            macro_arg = true;
            value.remove(0);
        }

        if entity {
            Ok(AttributeValue::Expression(value))
        } else if macro_arg {
            Ok(AttributeValue::MacroArgument(value))
        } else {
            Ok(AttributeValue::Literal(value))
        }
    }

    fn parse_quoted_value(&mut self) -> Result<String> {
        let quote = self
            .bump()
            .expect("parse_quoted_value requires an opening quote");
        let mut value = String::new();
        while let Some(ch) = self.bump() {
            if ch == quote {
                return Ok(value);
            }
            if ch == '`' {
                let Some(escaped) = self.bump() else {
                    return Err(self.error_here("unterminated quoted attribute value"));
                };
                value.push(escaped);
                continue;
            }
            value.push(ch);
        }

        Err(self.error_here("unterminated quoted attribute value"))
    }

    fn parse_unquoted_value(&mut self) -> String {
        let start = self.pos;
        let mut value = String::new();
        while let Some(ch) = self.peek() {
            if is_kag_ws(ch) {
                break;
            }
            self.bump();
            if ch == '`' {
                let Some(escaped) = self.bump() else {
                    break;
                };
                value.push(escaped);
            } else {
                value.push(ch);
            }
        }
        if value.is_empty() {
            self.content[start..self.pos].to_owned()
        } else {
            value
        }
    }

    fn error_here(&self, message: impl Into<String>) -> KagError {
        let offset = self.base_offset + self.pos;
        if let Some(storage) = self.storage {
            KagError::parse_at(storage.to_owned(), SourceSpan::empty(offset), message)
        } else {
            KagError::Parse {
                storage: None,
                span: Some(SourceSpan::empty(offset)),
                message: message.into(),
            }
        }
    }
}

fn tag_supports_cond(tagname: &str, process_special_tags: bool) -> bool {
    !(process_special_tags
        && matches!(
            tagname,
            "if" | "else" | "elsif" | "endif" | "ignore" | "endignore"
        ))
}

fn is_kag_ws(ch: char) -> bool {
    ch == ' ' || ch == '\t'
}

fn strip_leading_tabs_per_line(source: &str) -> String {
    let mut stripped = String::with_capacity(source.len());
    let mut at_line_start = true;

    for ch in source.chars() {
        if at_line_start && ch == '\t' {
            continue;
        }

        stripped.push(ch);
        at_line_start = ch == '\r' || ch == '\n';
    }

    stripped
}

fn normalize_target_label(target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    if target.starts_with('*') {
        Some(target.to_owned())
    } else {
        Some(format!("*{target}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestHost {
        sources: BTreeMap<String, String>,
        bools: BTreeMap<String, bool>,
        strings: BTreeMap<String, String>,
        labels: Vec<String>,
        scripts: Vec<String>,
    }

    impl KagHost for TestHost {
        fn load_scenario(&mut self, storage: &str) -> Result<String> {
            self.sources
                .get(storage)
                .cloned()
                .ok_or_else(|| KagError::ScenarioLoadUnsupported {
                    storage: storage.to_owned(),
                })
        }

        fn eval_bool(&mut self, expression: &str) -> Result<bool> {
            self.bools
                .get(expression)
                .copied()
                .ok_or_else(|| KagError::EvalUnsupported {
                    expression: expression.to_owned(),
                })
        }

        fn eval_string(&mut self, expression: &str) -> Result<String> {
            self.strings
                .get(expression)
                .cloned()
                .ok_or_else(|| KagError::EvalUnsupported {
                    expression: expression.to_owned(),
                })
        }

        fn on_label(&mut self, event: LabelEvent<'_>) -> Result<()> {
            self.labels.push(event.label.name.clone());
            Ok(())
        }

        fn on_script(&mut self, event: ScriptEvent<'_>) -> Result<()> {
            self.scripts.push(event.script.to_owned());
            Ok(())
        }
    }

    fn lit<'a>(tag: &'a Tag, name: &str) -> Option<&'a str> {
        tag.literal_attr(name)
    }

    fn next(parser: &mut KagParser) -> Tag {
        parser.next_tag().unwrap().unwrap()
    }

    fn next_with(parser: &mut KagParser, host: &mut TestHost) -> Tag {
        parser.next_tag_with(host).unwrap().unwrap()
    }

    #[test]
    fn cloned_parsers_share_loaded_scenarios_until_mutated() {
        let mut parser = KagParser::new();
        parser.load_scenario_text("first.ks", "*start\nA").unwrap();

        let cloned = parser.clone();
        assert!(Arc::ptr_eq(&parser.scenarios, &cloned.scenarios));

        parser.load_scenario_text("second.ks", "*start\nB").unwrap();

        assert!(!Arc::ptr_eq(&parser.scenarios, &cloned.scenarios));
        assert!(parser.scenarios.contains_key("second.ks"));
        assert!(!cloned.scenarios.contains_key("second.ks"));
    }

    #[test]
    fn parses_text_tags_command_lines_comments_and_labels() {
        let mut parser = KagParser::new();
        parser.set_ignore_cr(true);
        parser
            .load_scenario_text(
                "first.ks",
                "; comment\n\t*start|Start\nHello[[x[wait time=200]\n@r\n",
            )
            .unwrap();
        let mut host = TestHost::default();

        let tag = next_with(&mut parser, &mut host);
        assert_eq!(tag.tagname, "ch");
        assert_eq!(lit(&tag, "text"), Some("H"));
        assert_eq!(host.labels, vec!["*start"]);

        for expected in ["e", "l", "l", "o", "[", "x"] {
            let tag = next_with(&mut parser, &mut host);
            assert_eq!(lit(&tag, "text"), Some(expected));
        }

        let tag = next_with(&mut parser, &mut host);
        assert_eq!(tag.tagname, "wait");
        assert_eq!(lit(&tag, "time"), Some("200"));

        let tag = next_with(&mut parser, &mut host);
        assert_eq!(tag.tagname, "r");
        assert_eq!(parser.next_tag_with(&mut host).unwrap(), None);
    }

    #[test]
    fn parses_attributes_and_entities() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text(
                "first.ks",
                r#"[font face="MS Gothic" bold size=&f.size flag]"#,
            )
            .unwrap();
        let mut host = TestHost::default();
        host.strings.insert("f.size".into(), "24".into());

        let tag = next_with(&mut parser, &mut host);
        assert_eq!(tag.tagname, "font");
        assert_eq!(lit(&tag, "face"), Some("MS Gothic"));
        assert_eq!(lit(&tag, "bold"), Some("true"));
        assert_eq!(lit(&tag, "flag"), Some("true"));
        assert_eq!(lit(&tag, "size"), Some("24"));
    }

    #[test]
    fn emits_newline_tags_when_cr_is_not_ignored() {
        let mut parser = KagParser::new();
        parser.set_ignore_cr(false);
        parser.load_scenario_text("first.ks", "a\\\nb\nc").unwrap();

        assert_eq!(lit(&next(&mut parser), "text"), Some("a"));
        assert_eq!(lit(&next(&mut parser), "text"), Some("b"));
        assert_eq!(next(&mut parser).tagname, "r");
        assert_eq!(lit(&next(&mut parser), "text"), Some("c"));
        assert_eq!(parser.next_tag().unwrap(), None);
    }

    #[test]
    fn defaults_match_krkr2_parser() {
        let parser = KagParser::new();

        assert!(!parser.ignore_cr());
        assert!(parser.process_special_tags());
        assert!(parser.options().resolve_entities);
        assert_eq!(parser.debug_level(), DebugLevel::Simple);
    }

    #[test]
    fn rejects_empty_scenarios() {
        let mut parser = KagParser::new();

        assert!(parser.load_scenario_text("empty.ks", "").is_err());
    }

    #[test]
    fn lowercases_tag_and_attribute_names_and_uses_backtick_escapes() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text("first.ks", "[WaIt TIME=`] Name=\"A`\"B\"]")
            .unwrap();

        let tag = next(&mut parser);
        assert_eq!(tag.tagname, "wait");
        assert_eq!(lit(&tag, "time"), Some("]"));
        assert_eq!(lit(&tag, "name"), Some("A\"B"));
    }

    #[test]
    fn indexes_labels_with_star_aliases_pages_and_duplicates() {
        let mut parser = KagParser::new();
        parser.set_ignore_cr(true);
        parser
            .load_scenario_text("first.ks", "*start|Start page\n*|Second page\n*start\nA")
            .unwrap();
        let mut host = TestHost::default();

        let tag = next_with(&mut parser, &mut host);
        assert_eq!(lit(&tag, "text"), Some("A"));
        assert_eq!(host.labels, vec!["*start", "*start:2", "*start:3"]);

        parser.go_to_label("*start:2").unwrap();
        let tag = next_with(&mut parser, &mut host);
        assert_eq!(lit(&tag, "text"), Some("A"));
        assert_eq!(host.labels.last().map(String::as_str), Some("*start:3"));
    }

    #[test]
    fn skips_tabs_and_suppresses_newline_after_page_break() {
        let mut parser = KagParser::new();
        parser.load_scenario_text("first.ks", "A\tB[p]\nC").unwrap();

        assert_eq!(lit(&next(&mut parser), "text"), Some("A"));
        assert_eq!(lit(&next(&mut parser), "text"), Some("B"));
        assert_eq!(next(&mut parser).tagname, "p");
        assert_eq!(lit(&next(&mut parser), "text"), Some("C"));
        assert_eq!(parser.next_tag().unwrap(), None);
    }

    #[test]
    fn expands_macros_with_params_defaults_and_spread() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text(
                "first.ks",
                "[macro name=paint][font color=%color|red face=%face][trans *][endmacro][paint face=serif time=10]",
            )
            .unwrap();

        let font = next(&mut parser);
        assert_eq!(font.tagname, "font");
        assert_eq!(lit(&font, "color"), Some("red"));
        assert_eq!(lit(&font, "face"), Some("serif"));

        let trans = next(&mut parser);
        assert_eq!(trans.tagname, "trans");
        assert_eq!(lit(&trans, "face"), Some("serif"));
        assert_eq!(lit(&trans, "time"), Some("10"));
        assert_eq!(parser.next_tag().unwrap(), None);
        assert!(parser.macro_definition("paint").is_some());
    }

    #[test]
    fn records_macro_body_as_raw_kag_text() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text(
                "first.ks",
                "[macro name=paint]A\n@font face=%face\n[endmacro]",
            )
            .unwrap();

        assert_eq!(parser.next_tag().unwrap(), None);
        assert_eq!(
            parser.macro_definition("paint"),
            Some("A[r eol=true][font face=%face][macropop]")
        );
    }

    #[test]
    fn nested_macro_expansion_restores_outer_params_via_macropop() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text(
                "first.ks",
                "[macro name=inner][font face=%face][endmacro]\
                 [macro name=outer][inner face=%face][font face=%face][endmacro]\
                 [outer face=serif]",
            )
            .unwrap();

        let inner_font = next(&mut parser);
        assert_eq!(inner_font.tagname, "font");
        assert_eq!(lit(&inner_font, "face"), Some("serif"));

        let outer_font = next(&mut parser);
        assert_eq!(outer_font.tagname, "font");
        assert_eq!(lit(&outer_font, "face"), Some("serif"));
        assert_eq!(parser.next_tag().unwrap(), None);
    }

    #[test]
    fn macro_arguments_are_resolved_when_tag_is_parsed() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text("first.ks", "[font face=%face|default *]")
            .unwrap();

        let tag = next(&mut parser);
        assert_eq!(tag.tagname, "font");
        assert_eq!(lit(&tag, "face"), Some("face|default"));
        assert_eq!(tag.attributes.len(), 1);
    }

    #[test]
    fn macro_invocation_entities_are_captured_as_arguments() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text(
                "first.ks",
                "[macro name=paint][font size=%size][endmacro][paint size=&n]",
            )
            .unwrap();
        let mut host = TestHost::default();
        host.strings.insert("n".into(), "24".into());

        let tag = next_with(&mut parser, &mut host);
        assert_eq!(tag.tagname, "font");
        assert_eq!(lit(&tag, "size"), Some("24"));
    }

    #[test]
    fn process_special_tags_false_still_expands_existing_macros() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text("define.ks", "[macro name=x]A[endmacro]")
            .unwrap();
        assert_eq!(parser.next_tag().unwrap(), None);

        parser.set_process_special_tags(false);
        parser.load_scenario_text("use.ks", "[x]").unwrap();

        assert_eq!(lit(&next(&mut parser), "text"), Some("A"));
        assert_eq!(next(&mut parser).tagname, "macropop");
        assert_eq!(parser.next_tag().unwrap(), None);
    }

    #[test]
    fn process_special_tags_false_applies_cond_to_control_named_tags() {
        let mut parser = KagParser::new();
        parser.set_process_special_tags(false);
        parser
            .load_scenario_text("first.ks", "[if cond=run]A")
            .unwrap();
        let mut host = TestHost::default();
        host.bools.insert("run".into(), false);

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("A"));
        assert_eq!(parser.next_tag_with(&mut host).unwrap(), None);
    }

    #[test]
    fn processes_if_elsif_else_ignore_cond_and_emb() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text(
                "first.ks",
                "[if exp=a]A[elsif exp=b]B[else]C[endif][ignore exp=skip]X[endignore][emb exp=name][wait cond=run]",
            )
            .unwrap();
        let mut host = TestHost::default();
        host.bools.insert("a".into(), false);
        host.bools.insert("b".into(), true);
        host.bools.insert("skip".into(), true);
        host.bools.insert("run".into(), true);
        host.strings.insert("name".into(), "YZ".into());

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("B"));
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("Y"));
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("Z"));
        let wait = next_with(&mut parser, &mut host);
        assert_eq!(wait.tagname, "wait");
        assert!(wait.attr("cond").is_none());
        assert_eq!(parser.next_tag_with(&mut host).unwrap(), None);
    }

    #[test]
    fn emb_raw_newline_emits_reline_even_when_cr_is_ignored() {
        let mut parser = KagParser::new();
        parser.set_ignore_cr(true);
        parser
            .load_scenario_text("first.ks", "[emb exp=text]")
            .unwrap();
        let mut host = TestHost::default();
        host.strings.insert("text".into(), "A\nB".into());

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("A"));
        let reline = next_with(&mut parser, &mut host);
        assert_eq!(reline.tagname, "r");
        assert!(reline.attr("eol").is_none());
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("B"));
        assert_eq!(parser.next_tag_with(&mut host).unwrap(), None);
    }

    #[test]
    fn current_position_uses_active_expansion_buffer() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text("first.ks", "[emb exp=text]")
            .unwrap();
        let mut host = TestHost::default();
        host.strings.insert("text".into(), "AB".into());

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("A"));
        assert_eq!(parser.cur_line(), Some(1));
        assert_eq!(parser.cur_pos(), Some(1));
        assert_eq!(parser.cur_line_str(), Some("AB"));
    }

    #[test]
    fn processes_iscript_blocks() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text("first.ks", "[iscript]\nf.x = 1;\n[endscript]\nA")
            .unwrap();
        let mut host = TestHost::default();

        let tag = next_with(&mut parser, &mut host);
        assert_eq!(lit(&tag, "text"), Some("A"));
        assert_eq!(host.scripts, vec!["f.x = 1;\r\n"]);
    }

    #[test]
    fn processes_only_line_head_iscript_and_accepts_command_end() {
        let mut parser = KagParser::new();
        parser.set_ignore_cr(true);
        parser
            .load_scenario_text("first.ks", "A[iscript]B\n@iscript\nf.y = 2;\n@endscript\nC")
            .unwrap();
        let mut host = TestHost::default();

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("A"));
        assert_eq!(next_with(&mut parser, &mut host).tagname, "iscript");
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("B"));
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("C"));
        assert_eq!(host.scripts, vec!["f.y = 2;\r\n"]);
    }

    #[test]
    fn inline_iscript_is_normal_tag_and_honors_cond() {
        let mut parser = KagParser::new();
        parser.set_ignore_cr(true);
        parser
            .load_scenario_text("first.ks", "A[iscript cond=run]B")
            .unwrap();
        let mut host = TestHost::default();
        host.bools.insert("run".into(), false);

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("A"));
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("B"));
        assert!(host.scripts.is_empty());
    }

    #[test]
    fn skips_iscript_event_inside_excluded_blocks() {
        let mut parser = KagParser::new();
        parser.set_ignore_cr(true);
        parser
            .load_scenario_text(
                "first.ks",
                "[if exp=run]\n[iscript]\nf.hidden = true;\n[endscript]\n[endif]A",
            )
            .unwrap();
        let mut host = TestHost::default();
        host.bools.insert("run".into(), false);

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("A"));
        assert!(host.scripts.is_empty());
    }

    #[test]
    fn jumps_calls_and_returns_between_scenarios() {
        let mut host = TestHost::default();
        host.sources.insert(
            "first.ks".into(),
            "*start\nA[call storage=second.ks target=*sub]B[jump target=*end]C\n*end\nD".into(),
        );
        host.sources
            .insert("second.ks".into(), "*sub\nX[return]Y".into());

        let mut parser = KagParser::new();
        parser.load_scenario_with("first.ks", &mut host).unwrap();

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("A"));
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("X"));
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("B"));
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("D"));
        assert_eq!(parser.next_tag_with(&mut host).unwrap(), None);
    }

    #[test]
    fn cond_gates_special_tags_like_jump_return_and_emb() {
        let mut parser = KagParser::new();
        parser.set_ignore_cr(true);
        parser
            .load_scenario_text(
                "first.ks",
                "[call target=*sub]A[jump target=*end]\n*sub\n[emb exp=text cond=show][jump target=*end cond=go]B[return cond=ret][return]\n*end\nC",
            )
            .unwrap();
        let mut host = TestHost::default();
        host.bools.insert("show".into(), false);
        host.bools.insert("go".into(), false);
        host.bools.insert("ret".into(), false);
        host.strings.insert("text".into(), "X".into());

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("B"));
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("A"));
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("C"));
        assert_eq!(parser.next_tag_with(&mut host).unwrap(), None);
    }

    #[test]
    fn jump_without_storage_or_target_is_noop() {
        let mut parser = KagParser::new();
        parser.load_scenario_text("first.ks", "A[jump]B").unwrap();
        let mut host = TestHost::default();

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("A"));
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("B"));
        assert_eq!(parser.next_tag_with(&mut host).unwrap(), None);
    }

    #[test]
    fn go_to_empty_label_is_noop() {
        let mut parser = KagParser::new();
        parser.load_scenario_text("first.ks", "AB").unwrap();

        assert_eq!(lit(&next(&mut parser), "text"), Some("A"));
        parser.go_to_label("").unwrap();
        assert_eq!(lit(&next(&mut parser), "text"), Some("B"));
        assert_eq!(parser.next_tag().unwrap(), None);
    }

    #[test]
    fn call_without_storage_or_target_pushes_stack_and_continues() {
        let mut parser = KagParser::new();
        parser.load_scenario_text("first.ks", "A[call]B").unwrap();
        let mut host = TestHost::default();

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("A"));
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("B"));
        assert_eq!(parser.call_stack_depth(), 1);
    }

    #[test]
    fn call_empty_label_pushes_stack_without_moving() {
        let mut parser = KagParser::new();
        parser.load_scenario_text("first.ks", "A").unwrap();

        parser.call_label("").unwrap();
        assert_eq!(parser.call_stack_depth(), 1);
        assert_eq!(lit(&next(&mut parser), "text"), Some("A"));
    }

    #[test]
    fn return_with_empty_target_uses_saved_call_site() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text(
                "first.ks",
                "*start\n[call target=*sub]A\n*sub\n[return target=\"\"]B",
            )
            .unwrap();
        let mut host = TestHost::default();

        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("A"));
    }

    #[test]
    fn call_inside_macro_preserves_macro_params_in_callee() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text(
                "first.ks",
                "[macro name=m][call target=*sub][endmacro][m face=serif][jump target=*end]\n*sub\n[font face=%face][return]\n*end\n",
            )
            .unwrap();
        let mut host = TestHost::default();

        let tag = next_with(&mut parser, &mut host);
        assert_eq!(tag.tagname, "font");
        assert_eq!(lit(&tag, "face"), Some("serif"));
        assert_eq!(parser.next_tag_with(&mut host).unwrap(), None);
    }

    #[test]
    fn return_detects_lost_sync_when_call_site_line_changes() {
        let mut host = TestHost::default();
        host.sources.insert(
            "first.ks".into(),
            "*start\n[call storage=second.ks target=*sub]A".into(),
        );
        host.sources
            .insert("second.ks".into(), "*sub\nX[return]".into());

        let mut parser = KagParser::new();
        parser.load_scenario_with("first.ks", &mut host).unwrap();
        assert_eq!(lit(&next_with(&mut parser, &mut host), "text"), Some("X"));

        Arc::make_mut(&mut parser.scenarios).insert(
            "first.ks".into(),
            Scenario::new(
                "first.ks".into(),
                "*start\n[call storage=second.ks target=*sub]B".into(),
            )
            .unwrap(),
        );

        let error = parser.next_tag_with(&mut host).unwrap_err();
        assert!(matches!(error, KagError::ReturnLostSync { .. }));
    }

    #[test]
    fn return_with_explicit_target_still_requires_call_stack() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text("first.ks", "*start\n[return target=*start]")
            .unwrap();
        let mut host = TestHost::default();

        let error = parser.next_tag_with(&mut host).unwrap_err();
        assert!(matches!(error, KagError::ReturnStackEmpty));
    }

    #[test]
    fn unmatched_else_and_endif_are_consumed_like_krkr2() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text("first.ks", "[else]A[endif]B")
            .unwrap();

        assert_eq!(lit(&next(&mut parser), "text"), Some("A"));
        assert_eq!(lit(&next(&mut parser), "text"), Some("B"));
        assert_eq!(parser.next_tag().unwrap(), None);
    }

    #[test]
    fn stores_and_restores_parser_state() {
        let mut parser = KagParser::new();
        parser
            .load_scenario_text("first.ks", "AB[macro name=x]C[endmacro][x]")
            .unwrap();

        assert_eq!(lit(&next(&mut parser), "text"), Some("A"));
        let snapshot = parser.store();
        assert_eq!(lit(&next(&mut parser), "text"), Some("B"));
        assert_eq!(lit(&next(&mut parser), "text"), Some("C"));

        parser.restore(snapshot).unwrap();
        assert_eq!(lit(&next(&mut parser), "text"), Some("B"));
    }

    #[test]
    fn restore_does_not_overwrite_runtime_options_or_debug_level() {
        let mut parser = KagParser::new();
        parser.load_scenario_text("first.ks", "AB").unwrap();
        parser.set_ignore_cr(true);
        parser.set_debug_level(DebugLevel::Verbose);
        let snapshot = parser.store();

        parser.set_ignore_cr(false);
        parser.set_debug_level(DebugLevel::None);
        parser.restore(snapshot).unwrap();

        assert!(!parser.ignore_cr());
        assert_eq!(parser.debug_level(), DebugLevel::None);
    }

    #[test]
    fn interrupt_returns_synthetic_tag_once() {
        let mut parser = KagParser::new();
        parser.load_scenario_text("first.ks", "A").unwrap();
        parser.interrupt();

        assert_eq!(next(&mut parser).tagname, "interrupt");
        assert_eq!(lit(&next(&mut parser), "text"), Some("A"));
    }
}
