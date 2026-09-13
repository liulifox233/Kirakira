//! `expat.dll` — the global `XMLParser` class over a SAX-style XML parser.
//!
//! Reference: the krkr2 trunk `plugins/win32/expat/Main.cpp` (696 lines; the
//! dossier's `docs/plugins/system-storage.md`, expat section, records the
//! older krkrz snapshot at the same size). The DLL installs one global
//! (`V2Link` `:648-661`, removed again by `V2Unlink` `:674-696`):
//! `XMLParser`, a native class wrapping the expat library's SAX API.
//!
//! Surface, verified line by line against the source:
//!
//! * `XMLParser(target = void)` — the constructor stores `param[0]->AsObject()`
//!   as the handler target (`:339-355`); a non-object argument (or none) leaves
//!   it NULL.
//! * `parse(text)` / `parseStorage(filename)` (`:485-511`): both call the
//!   instance's `init` (`:352-372`), which resets the parser with the
//!   **UTF-8** protocol encoding and registers a handler for every member
//!   the target answers for; the second argument of either method (or `this`
//!   when absent) is the target `init` receives (`:503-505`, `:511-518`,
//!   `:415`). `parse` converts the TJS text to UTF-8 and feeds the whole
//!   document as one buffer (`:413-421`); `parseStorage` streams the
//!   storage's raw bytes in 8 KiB chunks, the last one flagged final
//!   (`:427-455`). A missing storage throws `cannot open : <filename>`
//!   (`:438-440`) — the only exception either method raises itself.
//! * Parse errors **never throw**: `parse` returns the `XML_Parse` status
//!   (`:420`, `:444`) and the caller inspects the properties.
//! * Read-only properties (getters only, `TJS_DENY_NATIVE_PROP_SETTER`
//!   `:555-589`): `errorCode` (`XML_GetErrorCode`), `errorString`
//!   (`XML_ErrorString`), `currentByteIndex`, `currentLineNumber`,
//!   `currentColumnNumber`, `currentByteCount` (`:472-491`).
//! * Handler members (`:152-292`, registered only when the target has such a
//!   member, `:359-371`): `startElement(name, attrs)` (attrs is a TJS
//!   `Dictionary` built in document order, `:158-163`), `endElement(name)`,
//!   `characterData(data)`, `processingInstruction(target, data)`,
//!   `comment(data)`, `startCdataSection()`, `endCdataSection()`,
//!   `defaultHandler(data)`, `defaultHandlerExpand(data)`. The two default
//!   handlers are registered through `XML_SetDefaultHandler` /
//!   `XML_SetDefaultHandlerExpand` (`:365-366`); every dispatch fetches the
//!   member by name and a vanished member raises `can't get member:<name>`
//!   (`:89-127`).
//!
//! # How the port parses
//!
//! The backing engine is [`quick_xml`] (already in the workspace lockfile),
//! driven as a pull parser; the expat layer on top owns the observable
//! semantics: handler registration and dispatch, the default-handler rule
//! ("a token without its own handler is reported raw", expat's
//! `reportDefault`), text splitting, entity references, and the position
//! model. The pieces the reference gets from expat itself are mirrored as
//! follows:
//!
//! * **Events and their order** — start/end/empty tags (an empty tag reports
//!   `startElement` then `endElement`, as expat's content loop does for
//!   `XML_TOK_EMPTY_ELEMENT_*`), character data, CDATA sections (the
//!   `startCdataSection`/`endCdataSection` pair brackets the content, which
//!   is reported through `characterData`), comments, processing
//!   instructions, and the document type declaration (only the default
//!   handler sees it).
//! * **Text splitting** matches expat's tokenizer: a run of ordinary
//!   characters is one call, every line end is its own `characterData("\n")`
//!   call, and an entity reference splits the text around it. Line ends are
//!   normalized (`\r\n` and `\r` → `\n`).
//! * **Entity references** — the five predefined entities and numeric
//!   character references are expanded; an undefined entity is
//!   `XML_ERROR_UNDEFINED_ENTITY` (11) and an invalid character number is
//!   `XML_ERROR_BAD_CHAR_REF` (14), at expat's positions.
//! * **Errors** are never thrown. The code/`errorString` pair is expat
//!   2.0.0's `XML_Error` table (`xmlparse.c`, the `errorStrings` array), and
//!   the positions follow `XML_GetCurrentByteIndex`/`LineNumber`/
//!   `ColumnNumber`/`ByteCount` (`xmlparse.c:1755-1801`): the byte offset of
//!   the current token, 1-based line and 0-based column counted in
//!   **characters** with `\r\n` as one line break, and the token's byte
//!   length (0 at an error and at end of input). After a successful parse the
//!   position is the end of the input, exactly like expat's end-of-buffer
//!   event pointer.
//! * `errorString` for code 0 (and for codes outside expat 2.0.0's table) is
//!   `void`: `XML_ErrorString(0)` returns NULL (`xmlparse.c:1842-1886`), and
//!   the reference assigns that NULL straight into the variant.
//!
//! # Documented divergences
//!
//! * **DTD-declared entities are not expanded.** The reference links expat
//!   with DTD support, so `<!DOCTYPE a [<!ENTITY e "hi">]><a>&e;</a>` yields
//!   `characterData("hi")`; here the internal subset is not interpreted and
//!   `&e;` is `XML_ERROR_UNDEFINED_ENTITY` (11), the code expat itself
//!   reports when no declaration is in force. External entities were never
//!   supported by the plugin either (no `XML_SetExternalEntityRefHandler`).
//! * **Encoding switching is not reproduced.** `init` pins the protocol
//!   encoding to UTF-8 (`XML_ParserReset(parser, L"UTF-8")`), which expat
//!   applies to the byte stream regardless of the declaration; the port
//!   reads the bytes as UTF-8, so a declaration naming another encoding is
//!   accepted-and-ignored exactly like the reference, and non-UTF-8 bytes
//!   fail with `XML_ERROR_INVALID_TOKEN` (4) at the offending byte.
//! * **Buffer-boundary text splits**: the reference feeds `parseStorage` in
//!   8 KiB reads and expat flushes accumulated character data at each read
//!   boundary, so a long text node can arrive as several calls. The port
//!   parses the storage's bytes as one buffer, so text splits only at
//!   tokenizer boundaries (line ends, entity references, CDATA edges), not
//!   at 8 KiB offsets. The concatenation is identical.
//! * **Position details on malformed entity references** are the port's own
//!   reading of expat's tokenizer (`syntax error` / `invalid token` at the
//!   character that broke the reference); everything else in the error table
//!   matches the reference's own reported `lineno`/`offset`/code for the
//!   cases the tests pin.
//! * A `void` argument to `parse` is an empty document here; the reference
//!   dereferences the NULL string variant and crashes.
//! * Namespace processing is off on both sides (`XML_ParserCreate(NULL)`, no
//!   `XML_SetNamespaceDeclHandler`), so `a:b` is a literal name in both.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError, TjsErrorKind,
    runtime::{Closure, NativeArgCount, NativePropertyAccess, ObjectHandle, Runtime, Variant},
};
use quick_xml::{
    Reader,
    errors::{Error as XmlError, IllFormedError, SyntaxError},
    events::Event,
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "XMLParser (expat SAX binding)",
    notes: "A port of expat/Main.cpp: the global XMLParser class with \
            parse/parseStorage, the six read-only error/position properties and \
            all nine handler members, dispatched over quick-xml with expat's \
            event order, text splitting, entity expansion and never-throwing \
            error codes/strings/positions. Documented divergences: \
            DTD-declared entities are not expanded (undefined entity, code 11), \
            non-UTF-8 declared encodings are ignored (invalid token, code 4), \
            and the 8 KiB read-boundary character-data splits of parseStorage \
            are not reproduced.",
    install: |engine| engine.register_plugin(ExpatPlugin),
};

pub struct ExpatPlugin;

impl KrkrPlugin for ExpatPlugin {
    fn name(&self) -> &str {
        "expat.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // `V2Link` re-runs after `Plugins.link`; a global a script replaced
        // keeps its own object, but a re-link installs ours again.
        if global_is_ours(runtime) {
            return Ok(());
        }
        let class = runtime.alloc_native_constructor(parser_constructor);
        runtime.add_object_class_info(class, "XMLParser");
        install_members(runtime, class);
        // `tTJSNativeClass` carries a member named after the class, so the
        // shipped `XML.tjs` `DOMDocument` constructor's
        // `global.XMLParser.XMLParser()` (`:63`) resolves, bound to the class
        // object itself.
        runtime.set_object_member(
            class,
            "XMLParser",
            Variant::Closure(Closure::new(class, Some(class))),
        );
        runtime.set_global_member("XMLParser", Variant::Object(class));
        Ok(())
    }

    fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        // `V2Unlink` deletes the global member (`:674-696`).
        if global_is_ours(runtime) {
            runtime.delete_object_member(runtime.global_handle(), "XMLParser");
        }
        Ok(())
    }
}

fn global_is_ours(runtime: &Runtime<KrkrHost>) -> bool {
    runtime
        .global_member("XMLParser")
        .object_handle()
        .is_some_and(|handle| {
            runtime
                .object_class_infos(handle)
                .iter()
                .any(|info| info == "XMLParser")
        })
}

fn parser_constructor(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_for(runtime, this_obj);
    runtime.add_object_class_info(instance, "XMLParser");
    install_members(runtime, instance);
    // `Construct` keeps `param[0]->AsObject()`; only an object counts.
    let target = args
        .first()
        .and_then(Variant::object_handle)
        .map(Variant::Object)
        .unwrap_or(Variant::Void);
    state_set(runtime, instance, TARGET, target);
    Ok(Variant::Object(instance))
}

/// `new Class(...)` (a fresh ordinary object), `super.Class(...)` (the
/// instance being built) and `Class(...)` (the class object) all land here;
/// the global object itself is never a receiver.
fn instance_for(runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> ObjectHandle {
    this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object())
}

fn install_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    // The reference keeps the parser in a native instance (`NI_XMLParser`);
    // this port keeps the same state in one hidden member of the object it
    // belongs to, the idiom `window_ex`/`k2compat` use for native state.
    ensure_state(runtime, handle);
    for (name, key) in [
        ("errorCode", ERROR_CODE),
        ("errorString", ERROR_STRING),
        ("currentByteIndex", CURRENT_BYTE_INDEX),
        ("currentLineNumber", CURRENT_LINE_NUMBER),
        ("currentColumnNumber", CURRENT_COLUMN_NUMBER),
        ("currentByteCount", CURRENT_BYTE_COUNT),
    ] {
        runtime.register_object_native_property_with_access(
            handle,
            name,
            NativePropertyAccess::ReadOnly,
            move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
                let Some(object) =
                    this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
                else {
                    return Ok(Variant::Void);
                };
                if key == ERROR_STRING {
                    let code = state_int(runtime, object, ERROR_CODE);
                    return Ok(if code == 0 {
                        Variant::Void
                    } else {
                        error_string(code)
                    });
                }
                Ok(Variant::Integer(state_int(runtime, object, key)))
            },
            |_runtime: &mut Runtime<KrkrHost>, _this_obj, _value| Err(TjsError::access_denied()),
        );
    }
    // `numparams < 1` is `TJS_E_BADPARAMCOUNT` (`:503`, `:515`), declared at
    // the registration site.
    runtime.register_object_native_with_arg_count(
        handle,
        "parse",
        NativeArgCount::AtLeast(1),
        parse,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "parseStorage",
        NativeArgCount::AtLeast(1),
        parse_storage,
    );
}

/// `parse(text [, target])` (`:485-499`).
fn parse(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = plugin_this(runtime, this_obj) else {
        return Err(TjsError::native_class_crash());
    };
    let text = match args.first() {
        Some(value) => value.to_tjs_string()?,
        None => return Err(TjsError::bad_param_count()),
    };
    // `AsStringNoAddRef` then `WideCharToMultiByte(CP_UTF8, ...)`: the
    // document reaches the parser as UTF-8 bytes (`:171-175`).
    let target = object_argument(&args, 1);
    Ok(Variant::Integer(i64::from(run_parse(
        runtime,
        this,
        text.into_bytes(),
        target,
    )?)))
}

/// `parseStorage(filename [, target])` (`:501-518`).
fn parse_storage(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = plugin_this(runtime, this_obj) else {
        return Err(TjsError::native_class_crash());
    };
    let filename = match args.first() {
        Some(value) => value.to_tjs_string()?,
        None => return Err(TjsError::bad_param_count()),
    };
    let bytes = runtime
        .host()
        .read_binary_storage(&filename)
        // `TVPCreateIStream` failing throws (`:438-440`); nothing else in
        // either method raises.
        .map_err(|_| TjsError::runtime(format!("cannot open : {filename}")))?;
    let target = object_argument(&args, 1);
    Ok(Variant::Integer(i64::from(run_parse(
        runtime, this, bytes, target,
    )?)))
}

fn plugin_this(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
}

fn object_argument(args: &[Variant], index: usize) -> Option<ObjectHandle> {
    args.get(index).and_then(Variant::object_handle)
}

// ---------------------------------------------------------------------------
// Parser state
// ---------------------------------------------------------------------------

const STATE_MEMBER: &str = "__expatParserState";
const TARGET: &str = "target";
const ERROR_CODE: &str = "errorCode";
const ERROR_STRING: &str = "errorString";
const CURRENT_BYTE_INDEX: &str = "currentByteIndex";
const CURRENT_LINE_NUMBER: &str = "currentLineNumber";
const CURRENT_COLUMN_NUMBER: &str = "currentColumnNumber";
const CURRENT_BYTE_COUNT: &str = "currentByteCount";

/// The state object of `object`, created on first use.
///
/// A script subclass (`class Doc extends XMLParser`, the shipped `XML.tjs`
/// pattern) never runs this module's constructor, so its instances start
/// without the member and pick one up on first use — the counterpart of
/// TJS2 resolving `TJS_GET_NATIVE_INSTANCE` through a subclass chain.
fn state_object(runtime: &mut Runtime<KrkrHost>, object: ObjectHandle) -> ObjectHandle {
    if let Variant::Object(state) = runtime.object_member(object, STATE_MEMBER) {
        return state;
    }
    let state = runtime.alloc_dictionary_object();
    for (key, initial) in [
        (TARGET, Variant::Void),
        (ERROR_CODE, Variant::Integer(0)),
        (CURRENT_BYTE_INDEX, Variant::Integer(-1)),
        (CURRENT_LINE_NUMBER, Variant::Integer(1)),
        (CURRENT_COLUMN_NUMBER, Variant::Integer(0)),
        (CURRENT_BYTE_COUNT, Variant::Integer(0)),
    ] {
        runtime.set_object_member(state, key, initial);
    }
    runtime.set_object_member(object, STATE_MEMBER, Variant::Object(state));
    state
}

fn ensure_state(runtime: &mut Runtime<KrkrHost>, object: ObjectHandle) {
    state_object(runtime, object);
}

fn state_get(runtime: &Runtime<KrkrHost>, object: ObjectHandle, key: &str) -> Variant {
    match runtime.object_member(object, STATE_MEMBER) {
        Variant::Object(state) => runtime.object_member(state, key),
        _ => Variant::Void,
    }
}

fn state_int(runtime: &Runtime<KrkrHost>, object: ObjectHandle, key: &str) -> i64 {
    state_get(runtime, object, key).to_integer().unwrap_or(0)
}

fn state_set(runtime: &mut Runtime<KrkrHost>, object: ObjectHandle, key: &str, value: Variant) {
    let state = state_object(runtime, object);
    runtime.set_object_member(state, key, value);
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// `init()` (`:352-372`): fresh error state, then a parse run over `input`.
fn run_parse(
    runtime: &mut Runtime<KrkrHost>,
    this: ObjectHandle,
    input: Vec<u8>,
    argument_target: Option<ObjectHandle>,
) -> Result<bool> {
    let state = state_object(runtime, this);
    for (key, initial) in [
        (ERROR_CODE, Variant::Integer(0)),
        (CURRENT_BYTE_INDEX, Variant::Integer(-1)),
        (CURRENT_LINE_NUMBER, Variant::Integer(1)),
        (CURRENT_COLUMN_NUMBER, Variant::Integer(0)),
        (CURRENT_BYTE_COUNT, Variant::Integer(0)),
    ] {
        runtime.set_object_member(state, key, initial);
    }
    // `init` uses the constructor's target when one was given, then the
    // method's own target argument, then `objthis` (`:110-111`, `:415-416`).
    let target = match state_get(runtime, this, TARGET).object_handle() {
        Some(target) => target,
        None => argument_target.unwrap_or(this),
    };
    let handlers = handlers_for(runtime, target);
    let mut cursor = Cursor::new();
    let mut guard = DocumentGuard::default();

    let mut reader = Reader::from_reader(input.as_slice());
    reader.config_mut().check_end_names = false;
    let mut buffer = Vec::new();
    loop {
        let start = reader.buffer_position() as usize;
        let event = match reader.read_event_into(&mut buffer) {
            Ok(event) => event,
            Err(error) => {
                let (code, offset) = parse_error(&error, &input, start);
                record_error(runtime, state, &mut cursor, &input, code, offset);
                return Ok(false);
            }
        };
        let end = reader.buffer_position() as usize;
        let frame = Frame {
            start,
            end,
            input: &input,
        };
        let outcome: std::result::Result<(), Issue> = match event {
            Event::Eof => break,
            Event::Decl(_) => {
                if start != 0 {
                    Err(Issue::at(XML_ERROR_MISPLACED_XML_PI, start))
                } else {
                    Ok(())
                }
            }
            Event::DocType(_) => {
                if guard.root_closed {
                    Err(Issue::at(XML_ERROR_JUNK_AFTER_DOC_ELEMENT, start))
                } else if let Some(name) = handlers.default_name {
                    let raw = input[start..end].to_vec();
                    set_position(runtime, state, &mut cursor, &frame, start, end - start);
                    report_default(runtime, target, name, &raw)
                } else {
                    Ok(())
                }
            }
            Event::Comment(data) => process_comment(
                runtime,
                state,
                target,
                &handlers,
                &mut cursor,
                &frame,
                data.into_inner().as_ref(),
            ),
            Event::PI(data) => process_pi(
                runtime,
                state,
                target,
                &handlers,
                &mut cursor,
                &frame,
                data.target(),
                data.content(),
            ),
            Event::Start(tag) => {
                let empty = false;
                process_start(
                    runtime,
                    state,
                    target,
                    &handlers,
                    &mut cursor,
                    &frame,
                    &tag,
                    &mut guard,
                    empty,
                )
            }
            Event::Empty(tag) => process_start(
                runtime,
                state,
                target,
                &handlers,
                &mut cursor,
                &frame,
                &tag,
                &mut guard,
                true,
            ),
            Event::End(tag) => process_end(
                runtime,
                state,
                target,
                &handlers,
                &mut cursor,
                &frame,
                tag.name().as_ref(),
                &mut guard,
            ),
            Event::Text(data) => {
                let raw = data.into_inner().into_owned();
                process_text(
                    runtime,
                    state,
                    target,
                    &handlers,
                    &mut cursor,
                    &frame,
                    &raw,
                    &mut guard,
                )
            }
            Event::CData(data) => {
                let content = data.into_inner().into_owned();
                process_cdata(
                    runtime,
                    state,
                    target,
                    &handlers,
                    &mut cursor,
                    &frame,
                    &content,
                    &mut guard,
                )
            }
            Event::GeneralRef(reference) => {
                let name = reference.into_inner().into_owned();
                process_reference(
                    runtime,
                    state,
                    target,
                    &handlers,
                    &mut cursor,
                    &frame,
                    &name,
                    &mut guard,
                )
            }
        };
        buffer.clear();
        if let Err(issue) = outcome {
            if let Some(error) = issue.thrown {
                return Err(error);
            }
            record_error(
                runtime,
                state,
                &mut cursor,
                &input,
                issue.code,
                issue.offset,
            );
            return Ok(false);
        }
    }
    if !guard.saw_root || guard.depth != 0 {
        // No root element at all, or one left open at EOF: expat's
        // `XML_ERROR_NO_ELEMENTS` at the end of the buffer.
        record_error(
            runtime,
            state,
            &mut cursor,
            &input,
            XML_ERROR_NO_ELEMENTS,
            input.len(),
        );
        return Ok(false);
    }
    let frame = Frame {
        start: input.len(),
        end: input.len(),
        input: &input,
    };
    set_position(runtime, state, &mut cursor, &frame, input.len(), 0);
    Ok(true)
}

/// One event's byte span in the input.
struct Frame<'a> {
    start: usize,
    #[allow(dead_code)]
    end: usize,
    input: &'a [u8],
}

#[allow(clippy::too_many_arguments)]
fn process_start(
    runtime: &mut Runtime<KrkrHost>,
    state: ObjectHandle,
    target: ObjectHandle,
    handlers: &Handlers,
    cursor: &mut Cursor,
    frame: &Frame<'_>,
    tag: &quick_xml::events::BytesStart<'_>,
    guard: &mut DocumentGuard,
    empty: bool,
) -> std::result::Result<(), Issue> {
    if guard.root_closed {
        return Err(Issue::at(XML_ERROR_JUNK_AFTER_DOC_ELEMENT, frame.start));
    }
    let name = decode_slice(tag.name().as_ref(), frame, frame.start + 1)?;
    let attributes = if handlers.start_element {
        Some(attribute_dictionary(runtime, tag, frame)?)
    } else {
        None
    };
    guard.saw_root = true;
    if empty {
        if guard.depth == 0 {
            guard.root_closed = true;
        }
    } else {
        guard.depth += 1;
        guard.stack.push(name.clone());
    }
    if handlers.start_element {
        set_position(
            runtime,
            state,
            cursor,
            frame,
            frame.start,
            frame.end - frame.start,
        );
        call_handler(
            runtime,
            target,
            "startElement",
            vec![
                Variant::String(name.clone()),
                Variant::Object(attributes.expect("handler has a dictionary")),
            ],
        )?;
    } else if let Some(default) = handlers.default_name {
        let raw = frame.input[frame.start..frame.end].to_vec();
        set_position(
            runtime,
            state,
            cursor,
            frame,
            frame.start,
            frame.end - frame.start,
        );
        report_default(runtime, target, default, &raw)?;
    }
    if empty {
        if handlers.end_element {
            set_position(
                runtime,
                state,
                cursor,
                frame,
                frame.start,
                frame.end - frame.start,
            );
            call_handler(runtime, target, "endElement", vec![Variant::String(name)])?;
        } else if let Some(default) = handlers.default_name {
            let raw = frame.input[frame.start..frame.end].to_vec();
            set_position(
                runtime,
                state,
                cursor,
                frame,
                frame.start,
                frame.end - frame.start,
            );
            report_default(runtime, target, default, &raw)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_text(
    runtime: &mut Runtime<KrkrHost>,
    state: ObjectHandle,
    target: ObjectHandle,
    handlers: &Handlers,
    cursor: &mut Cursor,
    frame: &Frame<'_>,
    raw: &[u8],
    guard: &mut DocumentGuard,
) -> std::result::Result<(), Issue> {
    if guard.root_closed {
        // Only whitespace may follow the document element; the error points
        // at the first non-whitespace character.
        if let Some(index) = raw.iter().position(|byte| !byte.is_ascii_whitespace()) {
            return Err(Issue::at(
                XML_ERROR_JUNK_AFTER_DOC_ELEMENT,
                frame.start + index,
            ));
        }
        if let Some(default) = handlers.default_name {
            let raw = raw.to_vec();
            set_position(
                runtime,
                state,
                cursor,
                frame,
                frame.start,
                frame.end - frame.start,
            );
            return report_default(runtime, target, default, &raw);
        }
        return Ok(());
    }
    if !guard.saw_root {
        // Character data in the prolog: expat's syntax error at the first
        // non-whitespace character.
        if let Some(index) = raw.iter().position(|byte| !byte.is_ascii_whitespace()) {
            return Err(Issue::at(XML_ERROR_SYNTAX, frame.start + index));
        }
    }
    split_character_data(
        runtime,
        state,
        target,
        handlers,
        cursor,
        frame,
        raw,
        frame.start,
    )
}

#[allow(clippy::too_many_arguments)]
fn process_cdata(
    runtime: &mut Runtime<KrkrHost>,
    state: ObjectHandle,
    target: ObjectHandle,
    handlers: &Handlers,
    cursor: &mut Cursor,
    frame: &Frame<'_>,
    content: &[u8],
    guard: &mut DocumentGuard,
) -> std::result::Result<(), Issue> {
    if !guard.saw_root || guard.root_closed {
        return Err(Issue::at(XML_ERROR_SYNTAX, frame.start));
    }
    // `<![CDATA[` is 9 bytes in; the content begins there.
    let content_start = frame.start + 9;
    if handlers.start_cdata_section {
        set_position(runtime, state, cursor, frame, frame.start, 9);
        call_handler(runtime, target, "startCdataSection", vec![])?;
    } else if let Some(default) = handlers.default_name {
        set_position(runtime, state, cursor, frame, frame.start, 9);
        report_default(runtime, target, default, b"<![CDATA[")?;
    }
    split_character_data(
        runtime,
        state,
        target,
        handlers,
        cursor,
        frame,
        content,
        content_start,
    )?;
    let close = content_start + content.len();
    if handlers.end_cdata_section {
        set_position(runtime, state, cursor, frame, close, 3);
        call_handler(runtime, target, "endCdataSection", vec![])?;
    } else if let Some(default) = handlers.default_name {
        set_position(runtime, state, cursor, frame, close, 3);
        report_default(runtime, target, default, b"]]>")?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_reference(
    runtime: &mut Runtime<KrkrHost>,
    state: ObjectHandle,
    target: ObjectHandle,
    handlers: &Handlers,
    cursor: &mut Cursor,
    frame: &Frame<'_>,
    name: &[u8],
    guard: &mut DocumentGuard,
) -> std::result::Result<(), Issue> {
    if guard.root_closed {
        return Err(Issue::at(XML_ERROR_JUNK_AFTER_DOC_ELEMENT, frame.start));
    }
    if !guard.saw_root {
        return Err(Issue::at(XML_ERROR_SYNTAX, frame.start));
    }
    let text = resolve_reference(name, frame.start)?;
    if handlers.character_data {
        set_position(
            runtime,
            state,
            cursor,
            frame,
            frame.start,
            frame.end - frame.start,
        );
        call_handler(
            runtime,
            target,
            "characterData",
            vec![Variant::String(text)],
        )
    } else if let Some(default) = handlers.default_name {
        let raw = frame.input[frame.start..frame.end].to_vec();
        set_position(
            runtime,
            state,
            cursor,
            frame,
            frame.start,
            frame.end - frame.start,
        );
        report_default(runtime, target, default, &raw)
    } else {
        Ok(())
    }
}

/// `comment(data)` or, without that member, the raw token to the default
/// handler (`doContent`'s `reportDefault`).
#[allow(clippy::too_many_arguments)]
fn process_comment(
    runtime: &mut Runtime<KrkrHost>,
    state: ObjectHandle,
    target: ObjectHandle,
    handlers: &Handlers,
    cursor: &mut Cursor,
    frame: &Frame<'_>,
    content: &[u8],
) -> std::result::Result<(), Issue> {
    if handlers.comment {
        let text = decode_slice(content, frame, frame.start + 4)?;
        set_position(
            runtime,
            state,
            cursor,
            frame,
            frame.start,
            frame.end - frame.start,
        );
        return call_handler(runtime, target, "comment", vec![Variant::String(text)]);
    }
    if let Some(default) = handlers.default_name {
        let raw = frame.input[frame.start..frame.end].to_vec();
        set_position(
            runtime,
            state,
            cursor,
            frame,
            frame.start,
            frame.end - frame.start,
        );
        return report_default(runtime, target, default, &raw);
    }
    Ok(())
}

/// `processingInstruction(target, data)`: the data is everything after the
/// target with the separating whitespace skipped (expat's
/// `reportProcessingInstruction`).
#[allow(clippy::too_many_arguments)]
fn process_pi(
    runtime: &mut Runtime<KrkrHost>,
    state: ObjectHandle,
    target: ObjectHandle,
    handlers: &Handlers,
    cursor: &mut Cursor,
    frame: &Frame<'_>,
    pi_target: &[u8],
    pi_data: &[u8],
) -> std::result::Result<(), Issue> {
    if handlers.processing_instruction {
        let target_text = String::from_utf8_lossy(pi_target).into_owned();
        let data_text = normalize_eols(String::from_utf8_lossy(pi_data).trim_start());
        set_position(
            runtime,
            state,
            cursor,
            frame,
            frame.start,
            frame.end - frame.start,
        );
        return call_handler(
            runtime,
            target,
            "processingInstruction",
            vec![Variant::String(target_text), Variant::String(data_text)],
        );
    }
    if let Some(default) = handlers.default_name {
        let raw = frame.input[frame.start..frame.end].to_vec();
        set_position(
            runtime,
            state,
            cursor,
            frame,
            frame.start,
            frame.end - frame.start,
        );
        return report_default(runtime, target, default, &raw);
    }
    Ok(())
}

/// `endElement(name)` with the well-formedness checks expat's content loop
/// makes: a stray end tag is an invalid token at the `/`, a mismatched one a
/// tag mismatch at the name.
#[allow(clippy::too_many_arguments)]
fn process_end(
    runtime: &mut Runtime<KrkrHost>,
    state: ObjectHandle,
    target: ObjectHandle,
    handlers: &Handlers,
    cursor: &mut Cursor,
    frame: &Frame<'_>,
    raw_name: &[u8],
    guard: &mut DocumentGuard,
) -> std::result::Result<(), Issue> {
    let name = decode_slice(raw_name, frame, frame.start + 2)?;
    if guard.depth == 0 {
        return Err(Issue::at(XML_ERROR_INVALID_TOKEN, frame.start + 1));
    }
    let expected = guard.stack.pop().unwrap_or_default();
    if expected != name {
        return Err(Issue::at(XML_ERROR_TAG_MISMATCH, frame.start + 2));
    }
    guard.depth -= 1;
    if guard.depth == 0 {
        guard.root_closed = true;
    }
    if handlers.end_element {
        set_position(
            runtime,
            state,
            cursor,
            frame,
            frame.start,
            frame.end - frame.start,
        );
        return call_handler(runtime, target, "endElement", vec![Variant::String(name)]);
    }
    if let Some(default) = handlers.default_name {
        let raw = frame.input[frame.start..frame.end].to_vec();
        set_position(
            runtime,
            state,
            cursor,
            frame,
            frame.start,
            frame.end - frame.start,
        );
        return report_default(runtime, target, default, &raw);
    }
    Ok(())
}

/// Character data delivery, shared by text and CDATA content: one call per
/// stretch of ordinary characters, one call per line end (expat's
/// `XML_TOK_DATA_NEWLINE`), raw bytes to the default handler when no
/// `characterData` member exists.
#[allow(clippy::too_many_arguments)]
fn split_character_data(
    runtime: &mut Runtime<KrkrHost>,
    state: ObjectHandle,
    target: ObjectHandle,
    handlers: &Handlers,
    cursor: &mut Cursor,
    frame: &Frame<'_>,
    raw: &[u8],
    start: usize,
) -> std::result::Result<(), Issue> {
    let mut index = 0usize;
    while index < raw.len() {
        let byte = raw[index];
        if byte == b'\n' || byte == b'\r' {
            let length = if byte == b'\r' && raw.get(index + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
            if handlers.character_data {
                set_position(runtime, state, cursor, frame, start + index, length);
                call_handler(
                    runtime,
                    target,
                    "characterData",
                    vec![Variant::String("\n".to_string())],
                )?;
            } else if let Some(default) = handlers.default_name {
                let piece = raw[index..index + length].to_vec();
                set_position(runtime, state, cursor, frame, start + index, length);
                report_default(runtime, target, default, &piece)?;
            }
            index += length;
            continue;
        }
        let mut end = index;
        while end < raw.len() && raw[end] != b'\n' && raw[end] != b'\r' {
            end += 1;
        }
        let run = &raw[index..end];
        let text = decode_slice(run, frame, start + index)?;
        if handlers.character_data {
            set_position(runtime, state, cursor, frame, start + index, run.len());
            call_handler(
                runtime,
                target,
                "characterData",
                vec![Variant::String(text)],
            )?;
        } else if let Some(default) = handlers.default_name {
            let piece = run.to_vec();
            set_position(runtime, state, cursor, frame, start + index, run.len());
            report_default(runtime, target, default, &piece)?;
        }
        index = end;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Handler set and dispatch
// ---------------------------------------------------------------------------

/// Which of the nine handlers `init` registered for a target (`:359-371`).
#[derive(Default, Clone, Copy)]
struct Handlers {
    start_element: bool,
    end_element: bool,
    character_data: bool,
    processing_instruction: bool,
    comment: bool,
    start_cdata_section: bool,
    end_cdata_section: bool,
    /// `Some` when `defaultHandler` or `defaultHandlerExpand` was registered
    /// (`:365-366`); the two differ only in internal-entity expansion, which
    /// this port does not implement (see the module docs). The name is the
    /// member every raw token is dispatched to.
    default_name: Option<&'static str>,
}

fn handlers_for(runtime: &mut Runtime<KrkrHost>, target: ObjectHandle) -> Handlers {
    // `isValidMember` (`:105-112`) asks the target's dispatch; the VM's
    // lenient member read answers the same question for script and native
    // members alike (a member that reads back as `void` counts as absent,
    // and a throwing getter does not break the parse setup).
    let present = |runtime: &mut Runtime<KrkrHost>, name: &str| {
        !matches!(
            runtime.resolve_object_member(target, name),
            Ok(Variant::Void) | Err(_)
        )
    };
    let mut handlers = Handlers {
        start_element: present(runtime, "startElement"),
        end_element: present(runtime, "endElement"),
        character_data: present(runtime, "characterData"),
        processing_instruction: present(runtime, "processingInstruction"),
        comment: present(runtime, "comment"),
        start_cdata_section: present(runtime, "startCdataSection"),
        end_cdata_section: present(runtime, "endCdataSection"),
        default_name: None,
    };
    if present(runtime, "defaultHandler") {
        handlers.default_name = Some("defaultHandler");
    }
    if present(runtime, "defaultHandlerExpand") {
        handlers.default_name = Some("defaultHandlerExpand");
    }
    handlers
}

fn call_handler(
    runtime: &mut Runtime<KrkrHost>,
    target: ObjectHandle,
    name: &str,
    args: Vec<Variant>,
) -> std::result::Result<(), Issue> {
    match runtime.call_object_method(target, name, args) {
        Ok(_) => Ok(()),
        Err(error) if error.kind == TjsErrorKind::MemberNotFound => Err(Issue::thrown(
            TjsError::runtime(format!("can't get member:{name}")),
        )),
        Err(error) => Err(Issue::thrown(error)),
    }
}

fn report_default(
    runtime: &mut Runtime<KrkrHost>,
    target: ObjectHandle,
    name: &str,
    raw: &[u8],
) -> std::result::Result<(), Issue> {
    let text = String::from_utf8_lossy(raw).into_owned();
    call_handler(runtime, target, name, vec![Variant::String(text)])
}

// ---------------------------------------------------------------------------
// expat error codes
// ---------------------------------------------------------------------------

const XML_ERROR_SYNTAX: i64 = 2;
const XML_ERROR_NO_ELEMENTS: i64 = 3;
const XML_ERROR_INVALID_TOKEN: i64 = 4;
const XML_ERROR_UNCLOSED_TOKEN: i64 = 5;
const XML_ERROR_TAG_MISMATCH: i64 = 7;
const XML_ERROR_DUPLICATE_ATTRIBUTE: i64 = 8;
const XML_ERROR_JUNK_AFTER_DOC_ELEMENT: i64 = 9;
const XML_ERROR_UNDEFINED_ENTITY: i64 = 11;
const XML_ERROR_BAD_CHAR_REF: i64 = 14;
const XML_ERROR_MISPLACED_XML_PI: i64 = 17;
const XML_ERROR_UNCLOSED_CDATA_SECTION: i64 = 20;
const XML_ERROR_XML_DECL: i64 = 30;

/// expat 2.0.0's `XML_ErrorString` table (`xmlparse.c`, `errorStrings`).
fn error_string(code: i64) -> Variant {
    let text = match code {
        1 => "out of memory",
        XML_ERROR_SYNTAX => "syntax error",
        XML_ERROR_NO_ELEMENTS => "no element found",
        XML_ERROR_INVALID_TOKEN => "not well-formed (invalid token)",
        XML_ERROR_UNCLOSED_TOKEN => "unclosed token",
        6 => "partial character",
        XML_ERROR_TAG_MISMATCH => "mismatched tag",
        XML_ERROR_DUPLICATE_ATTRIBUTE => "duplicate attribute",
        XML_ERROR_JUNK_AFTER_DOC_ELEMENT => "junk after document element",
        10 => "illegal parameter entity reference",
        XML_ERROR_UNDEFINED_ENTITY => "undefined entity",
        12 => "recursive entity reference",
        13 => "asynchronous entity",
        XML_ERROR_BAD_CHAR_REF => "reference to invalid character number",
        15 => "reference to binary entity",
        16 => "reference to external entity in attribute",
        XML_ERROR_MISPLACED_XML_PI => "XML or text declaration not at start of entity",
        18 => "unknown encoding",
        19 => "encoding specified in XML declaration is incorrect",
        XML_ERROR_UNCLOSED_CDATA_SECTION => "unclosed CDATA section",
        21 => "error in processing external entity reference",
        22 => "document is not standalone",
        23 => "unexpected parser state - please send a bug report",
        24 => "entity declared in parameter entity",
        25 => "requested feature requires XML_DTD support in Expat",
        26 => "cannot change setting once parsing has begun",
        27 => "unbound prefix",
        28 => "must not undeclare prefix",
        29 => "incomplete markup in parameter entity",
        XML_ERROR_XML_DECL => "XML declaration not well-formed",
        31 => "text declaration not well-formed",
        32 => "illegal character(s) in public id",
        33 => "parser suspended",
        34 => "parser not suspended",
        35 => "parsing aborted",
        36 => "parsing finished",
        37 => "cannot suspend in external parameter entity",
        38 => "reserved prefix (xml) must not be undeclared or bound to another namespace name",
        39 => "reserved prefix (xmlns) must not be declared or undeclared",
        40 => "prefix must not be bound to one of the reserved namespace names",
        _ => return Variant::Void,
    };
    Variant::String(text.to_string())
}

/// Resolves `&name;` the way `doContent` does with no DTD in force
/// (`xmlparse.c:2215-2280`): predefined entities and character references
/// expand, anything else is `XML_ERROR_UNDEFINED_ENTITY`.
fn resolve_reference(name: &[u8], start: usize) -> std::result::Result<String, Issue> {
    match name {
        b"amp" => Ok("&".to_string()),
        b"lt" => Ok("<".to_string()),
        b"gt" => Ok(">".to_string()),
        b"apos" => Ok("'".to_string()),
        b"quot" => Ok("\"".to_string()),
        _ if name.first() == Some(&b'#') && name.len() >= 2 => {
            let hexadecimal = name[1] == b'x';
            let digits = if hexadecimal { &name[2..] } else { &name[1..] };
            if digits.is_empty() {
                return Err(Issue::at(XML_ERROR_INVALID_TOKEN, start + 2));
            }
            let radix = if hexadecimal { 16 } else { 10 };
            let mut value: u32 = 0;
            for (index, byte) in digits.iter().enumerate() {
                let digit = match (byte, radix) {
                    (b'0'..=b'9', _) => u32::from(byte - b'0'),
                    (b'a'..=b'f', 16) => u32::from(byte - b'a') + 10,
                    (b'A'..=b'F', 16) => u32::from(byte - b'A') + 10,
                    // `XmlParseCharRef` errors at the offending character.
                    _ => {
                        let offset = start + 1 + usize::from(hexadecimal) + 1 + index;
                        return Err(Issue::at(XML_ERROR_INVALID_TOKEN, offset));
                    }
                };
                value = value.saturating_mul(radix).saturating_add(digit);
            }
            match char::from_u32(value) {
                Some(ch) if is_xml_char(ch) => Ok(ch.to_string()),
                // `&#0;`, surrogates and out-of-range numbers: expat's invalid
                // character number, reported at the reference start.
                _ => Err(Issue::at(XML_ERROR_BAD_CHAR_REF, start)),
            }
        }
        _ => {
            if let Some(index) = name.iter().position(|byte| !is_name_byte(*byte)) {
                return Err(Issue::at(XML_ERROR_INVALID_TOKEN, start + 1 + index));
            }
            Err(Issue::at(XML_ERROR_UNDEFINED_ENTITY, start))
        }
    }
}

fn is_xml_char(ch: char) -> bool {
    matches!(ch as u32,
        0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
}

// ---------------------------------------------------------------------------
// Attributes and text decoding
// ---------------------------------------------------------------------------

fn attribute_dictionary(
    runtime: &mut Runtime<KrkrHost>,
    tag: &quick_xml::events::BytesStart<'_>,
    frame: &Frame<'_>,
) -> std::result::Result<ObjectHandle, Issue> {
    let dictionary = runtime.alloc_dictionary_object();
    let mut seen: Vec<String> = Vec::new();
    for attribute in tag.attributes().with_checks(false) {
        let attribute = attribute.map_err(|_| Issue::at(XML_ERROR_INVALID_TOKEN, frame.start))?;
        let name = decode_slice(attribute.key.as_ref(), frame, frame.start)?;
        if seen.iter().any(|previous| previous == &name) {
            // expat reports the duplicate at the second attribute's name
            // (`xmlparse.c:2361`); quick-xml's own duplicate check reports no
            // position, so the offset is found in the raw tag bytes.
            let haystack = &frame.input[frame.start..frame.end];
            let offset = second_occurrence(haystack, name.as_bytes())
                .map(|offset| frame.start + offset)
                .unwrap_or(frame.start);
            return Err(Issue::at(XML_ERROR_DUPLICATE_ATTRIBUTE, offset));
        }
        let value = decode_attribute_value(attribute.value.as_ref(), frame)?;
        seen.push(name.clone());
        runtime.set_object_member(dictionary, name, Variant::String(value));
    }
    Ok(dictionary)
}

/// XML attribute-value normalization (`Extensible Markup Language 1.0`,
/// 3.3.3): literal line ends and tabs become spaces, references expand;
/// character references keep the character they name.
fn decode_attribute_value(value: &[u8], frame: &Frame<'_>) -> std::result::Result<String, Issue> {
    let mut text = String::new();
    let mut index = 0usize;
    while index < value.len() {
        let byte = value[index];
        if byte == b'&' {
            let Some(semicolon) = value[index + 1..].iter().position(|b| *b == b';') else {
                return Err(Issue::at(XML_ERROR_INVALID_TOKEN, frame.start));
            };
            let end = index + 1 + semicolon;
            // Attribute-value entity failures report at the tag start, the
            // way expat's tag-level token does.
            let resolved = resolve_reference(&value[index + 1..end], frame.start)
                .map_err(|issue| Issue::at(issue.code, frame.start))?;
            text.push_str(&resolved);
            index = end + 1;
            continue;
        }
        if byte == b'\n' || byte == b'\r' || byte == b'\t' {
            if byte == b'\r' && value.get(index + 1) == Some(&b'\n') {
                index += 2;
            } else {
                index += 1;
            }
            text.push(' ');
            continue;
        }
        let start = index;
        while index < value.len()
            && value[index] != b'&'
            && value[index] != b'\n'
            && value[index] != b'\r'
            && value[index] != b'\t'
        {
            index += 1;
        }
        text.push_str(&decode_slice(&value[start..index], frame, frame.start)?);
    }
    Ok(text)
}

/// The offset of the second occurrence of `needle` in `haystack`, for the
/// duplicate-attribute position.
fn second_occurrence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let first = haystack
        .windows(needle.len())
        .position(|window| window == needle)?;
    let rest = &haystack[first + 1..];
    rest.windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| first + 1 + offset)
}

fn decode_slice(
    raw: &[u8],
    frame: &Frame<'_>,
    offset: usize,
) -> std::result::Result<String, Issue> {
    match std::str::from_utf8(raw) {
        Ok(text) => Ok(text.to_string()),
        Err(error) => {
            let _ = frame;
            Err(Issue::at(
                XML_ERROR_INVALID_TOKEN,
                offset + error.valid_up_to(),
            ))
        }
    }
}

/// Line-end normalization (`Extensible Markup Language 1.0`, 2.11).
fn normalize_eols(raw: &str) -> String {
    if !raw.contains('\r') {
        return raw.to_string();
    }
    let mut text = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            text.push('\n');
        } else {
            text.push(ch);
        }
    }
    text
}

// ---------------------------------------------------------------------------
// Positions and errors
// ---------------------------------------------------------------------------

/// expat's position tracking (`XmlUpdatePosition`, `xmlparse.c:1784-1801`):
/// 1-based lines, 0-based columns counted in characters, `\r\n` as one
/// break.
struct Cursor {
    offset: usize,
    line: i64,
    column: i64,
    pending_cr: bool,
}

impl Cursor {
    fn new() -> Self {
        Cursor {
            offset: 0,
            line: 1,
            column: 0,
            pending_cr: false,
        }
    }

    fn advance_to(&mut self, input: &[u8], target: usize) -> (i64, i64) {
        let target = target.min(input.len());
        if target <= self.offset {
            return (self.line, self.column);
        }
        let text = String::from_utf8_lossy(&input[self.offset..target]);
        for ch in text.chars() {
            if self.pending_cr {
                self.pending_cr = false;
                if ch == '\n' {
                    continue;
                }
            }
            match ch {
                '\n' => {
                    self.line += 1;
                    self.column = 0;
                }
                '\r' => {
                    self.line += 1;
                    self.column = 0;
                    self.pending_cr = true;
                }
                _ => self.column += 1,
            }
        }
        self.offset = target;
        (self.line, self.column)
    }
}

fn set_position(
    runtime: &mut Runtime<KrkrHost>,
    state: ObjectHandle,
    cursor: &mut Cursor,
    frame: &Frame<'_>,
    offset: usize,
    byte_count: usize,
) {
    let (line, column) = cursor.advance_to(frame.input, offset);
    runtime.set_object_member(state, CURRENT_BYTE_INDEX, Variant::Integer(offset as i64));
    runtime.set_object_member(state, CURRENT_LINE_NUMBER, Variant::Integer(line));
    runtime.set_object_member(state, CURRENT_COLUMN_NUMBER, Variant::Integer(column));
    runtime.set_object_member(
        state,
        CURRENT_BYTE_COUNT,
        Variant::Integer(byte_count as i64),
    );
}

fn record_error(
    runtime: &mut Runtime<KrkrHost>,
    state: ObjectHandle,
    cursor: &mut Cursor,
    input: &[u8],
    code: i64,
    offset: usize,
) {
    let offset = offset.min(input.len());
    let (line, column) = cursor.advance_to(input, offset);
    runtime.set_object_member(state, ERROR_CODE, Variant::Integer(code));
    runtime.set_object_member(state, CURRENT_BYTE_INDEX, Variant::Integer(offset as i64));
    runtime.set_object_member(state, CURRENT_LINE_NUMBER, Variant::Integer(line));
    runtime.set_object_member(state, CURRENT_COLUMN_NUMBER, Variant::Integer(column));
    runtime.set_object_member(state, CURRENT_BYTE_COUNT, Variant::Integer(0));
}

/// An expat-level failure: a well-formedness error to record, or a script
/// exception that escaped a handler and must propagate unchanged.
struct Issue {
    code: i64,
    offset: usize,
    thrown: Option<TjsError>,
}

impl Issue {
    fn at(code: i64, offset: usize) -> Self {
        Issue {
            code,
            offset,
            thrown: None,
        }
    }

    fn thrown(error: TjsError) -> Self {
        Issue {
            code: 0,
            offset: 0,
            thrown: Some(error),
        }
    }
}

/// Maps a quick-xml failure onto expat's code and position.
fn parse_error(error: &XmlError, input: &[u8], event_start: usize) -> (i64, usize) {
    match error {
        XmlError::Syntax(error) => match error {
            SyntaxError::UnclosedCData => (XML_ERROR_UNCLOSED_CDATA_SECTION, input.len()),
            SyntaxError::UnclosedTag
            | SyntaxError::UnclosedSingleQuotedAttributeValue
            | SyntaxError::UnclosedDoubleQuotedAttributeValue => {
                (XML_ERROR_UNCLOSED_TOKEN, event_start)
            }
            SyntaxError::UnclosedComment
            | SyntaxError::UnclosedPI
            | SyntaxError::UnclosedXmlDecl
            | SyntaxError::UnclosedDoctype
            | SyntaxError::InvalidBangMarkup => (XML_ERROR_INVALID_TOKEN, event_start),
        },
        XmlError::IllFormed(error) => match error {
            IllFormedError::UnclosedReference => {
                // expat's own tokenizer stops at the offending character: a
                // `<` before any `;` is an invalid token there, EOF is an
                // unclosed token at the `&`.
                match unterminated_reference(input, event_start) {
                    Some(offset) => (XML_ERROR_INVALID_TOKEN, offset),
                    None => (XML_ERROR_UNCLOSED_TOKEN, event_start),
                }
            }
            IllFormedError::MismatchedEndTag { .. } => (XML_ERROR_TAG_MISMATCH, event_start + 2),
            // A `</x>` with nothing open is expat's invalid token at the `/`
            // (`<a/></a>` reports column 5).
            IllFormedError::UnmatchedEndTag(_) => (XML_ERROR_INVALID_TOKEN, event_start + 1),
            IllFormedError::MissingDeclVersion(_) => (XML_ERROR_XML_DECL, event_start),
            IllFormedError::MissingDoctypeName => (XML_ERROR_SYNTAX, event_start),
            _ => (XML_ERROR_INVALID_TOKEN, event_start),
        },
        XmlError::Encoding(_) => (XML_ERROR_INVALID_TOKEN, event_start),
        _ => (XML_ERROR_INVALID_TOKEN, event_start),
    }
}

/// The offset of the `<` that terminates an unterminated `&…` reference, if
/// one exists before the end of input.
fn unterminated_reference(input: &[u8], from: usize) -> Option<usize> {
    input[from..]
        .iter()
        .position(|byte| *byte == b'<')
        .map(|index| from + index)
}

/// The document structure expat tracks in `doContent`/`doProlog`.
#[derive(Default)]
struct DocumentGuard {
    saw_root: bool,
    root_closed: bool,
    depth: i32,
    stack: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, time::SystemTime};

    use krkr_assets::ProjectStorage;
    use krkr_engine::{EngineConfig, KrkrEngine, SystemPaths};
    use krkr_tjs2::TjsErrorKind;
    use krkr_tjs2::runtime::Variant;

    use super::ExpatPlugin;

    fn test_root(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-expat-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        root
    }

    fn test_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(ExpatPlugin).expect("plugin");
        engine
    }

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ExpatPlugin).expect("plugin");
        engine
    }

    /// A capturing handler object and the driver around it. The result string
    /// is `ok|errorCode|byteIndex|line|column|byteCount|` plus one `;`-ended
    /// entry per dispatched handler call.
    const HARNESS: &str = r#"
    class Capture {
        var trace = [];
        var parser;
        function Capture() { }
        function startElement(name, attrs) {
            var text = "s:" + name;
            if (attrs["b"] != void) { text += "[b=" + attrs["b"] + "]"; }
            if (attrs["x"] != void) { text += "[x=" + attrs["x"] + "]"; }
            trace.add(text);
        }
        function endElement(name) { trace.add("e:" + name); }
        function characterData(data) { trace.add("c:" + data); }
        function processingInstruction(target, data) { trace.add("p:" + target + ":" + data); }
        function comment(data) { trace.add("m:" + data); }
        function startCdataSection() { trace.add("cs"); }
        function endCdataSection() { trace.add("ce"); }
    }
    "#;

    const DRIVER: &str = r#"(function() {
        var capture = new Capture();
        capture.parser = new XMLParser(capture);
        var ok = capture.parser.parse(DOCUMENT);
        var text = ok + "|" + capture.parser.errorCode + "|" + capture.parser.currentByteIndex +
            "|" + capture.parser.currentLineNumber + "|" + capture.parser.currentColumnNumber +
            "|" + capture.parser.currentByteCount + "|";
        for (var i = 0; i < capture.trace.count; i++) { text += capture.trace[i] + ";"; }
        return text; })()"#;

    fn expression_string(engine: &mut KrkrEngine, script: &str) -> String {
        match engine
            .execute_expression("inline.tjs", script)
            .expect("expression")
        {
            Variant::String(text) => text,
            other => panic!("expected a string, got {other:?}"),
        }
    }

    fn parse_trace(engine: &mut KrkrEngine, document: &str) -> String {
        engine
            .execute_script("harness.tjs", HARNESS)
            .expect("harness");
        let script = DRIVER.replace("DOCUMENT", &format!("{document:?}"));
        match engine
            .execute_expression("inline.tjs", &script)
            .expect("parse")
        {
            Variant::String(text) => text,
            other => panic!("unexpected value {other:?}"),
        }
    }

    /// The event sequence of `Main.cpp`'s handlers, asserted exactly:
    /// document order, the attribute dictionary, entity expansion splitting
    /// text runs, CDATA bracketed by its handlers, comments and processing
    /// instructions.
    #[test]
    fn events_match_the_reference_dispatch() {
        let mut engine = engine();
        assert_eq!(
            parse_trace(
                &mut engine,
                "<a b=\"c\">t1&amp;t2<![CDATA[cd]]><!--c--><?pi dat?></a>",
            ),
            "1|0|54|1|54|0|s:a[b=c];c:t1;c:&;c:t2;cs;c:cd;ce;m:c;p:pi:dat;e:a;"
        );
        // Nested elements keep document order: an empty child reports its
        // own start/end pair around the parent's other children.
        assert_eq!(
            parse_trace(&mut engine, "<a><b x=\"1\"/><c>t</c></a>"),
            "1|0|25|1|25|0|s:a;s:b[x=1];e:b;s:c;c:t;e:c;e:a;"
        );
    }

    /// expat's tokenizer reports every line end as its own `characterData`
    /// call and normalizes `\r\n`/`\r` to `\n`; CDATA content is split the
    /// same way and brackets its own two handler calls.
    #[test]
    fn text_splits_at_line_ends() {
        let mut engine = engine();
        assert_eq!(
            parse_trace(&mut engine, "<a>x\ny</a>"),
            "1|0|10|2|5|0|s:a;c:x;c:\n;c:y;e:a;"
        );
        assert_eq!(
            parse_trace(&mut engine, "<a>x\r\n\ry</a>"),
            "1|0|12|3|5|0|s:a;c:x;c:\n;c:\n;c:y;e:a;"
        );
        assert_eq!(
            parse_trace(&mut engine, "<a><![CDATA[a\r\nb]]></a>"),
            "1|0|23|2|8|0|s:a;cs;c:a;c:\n;c:b;ce;e:a;"
        );
    }

    /// An empty element reports `startElement` then `endElement`, exactly as
    /// expat's `XML_TOK_EMPTY_ELEMENT_*` path does.
    #[test]
    fn empty_elements_report_start_then_end() {
        let mut engine = engine();
        assert_eq!(
            parse_trace(&mut engine, "<a x=\"1\"/>"),
            "1|0|10|1|10|0|s:a[x=1];e:a;"
        );
    }

    /// Parse failures do not throw: the code, string and position properties
    /// carry expat's answers (the codes, lines and columns are the values
    /// expat itself reports for these documents).
    #[test]
    fn errors_are_reported_through_the_properties() {
        let mut engine = engine();
        for (document, expected) in [
            ("", "0|3|0|1|0|0|"),
            ("<a>", "0|3|3|1|3|0|s:a;"),
            ("<a><b>", "0|3|6|1|6|0|s:a;s:b;"),
            ("<a></b>", "0|7|5|1|5|0|s:a;"),
            ("<a/></a>", "0|4|5|1|5|0|s:a;e:a;"),
            ("</a>", "0|4|1|1|1|0|"),
            ("<a>&foo;</a>", "0|11|3|1|3|0|s:a;"),
            ("<a/>tail", "0|9|4|1|4|0|s:a;e:a;"),
            ("<a/>  tail", "0|9|6|1|6|0|s:a;e:a;"),
            ("<a b=\"1\" b=\"2\"/>", "0|8|9|1|9|0|"),
            ("<a>&#xZZ;</a>", "0|4|6|1|6|0|s:a;"),
            ("<a>&#xD800;</a>", "0|14|3|1|3|0|s:a;"),
            ("<a></a", "0|5|3|1|3|0|s:a;"),
            ("<a b=\"c\"", "0|5|0|1|0|0|"),
            ("<a>&", "0|5|3|1|3|0|s:a;"),
            ("<a>&</a>", "0|4|4|1|4|0|s:a;"),
            ("<a>&foo</a>", "0|4|7|1|7|0|s:a;"),
            ("<a><?xml version=\"1.0\"?></a>", "0|17|3|1|3|0|s:a;"),
            ("<a b=\"&foo;\"/>", "0|11|0|1|0|0|"),
            ("<a>t</a>tail", "0|9|8|1|8|0|s:a;c:t;e:a;"),
            ("<a>\n\n  &foo;</a>", "0|11|7|3|2|0|s:a;c:\n;c:\n;c:  ;"),
        ] {
            assert_eq!(parse_trace(&mut engine, document), expected, "{document}");
        }
    }

    /// The error string is `XML_ErrorString`'s text; with no error it is
    /// `void` because `XML_ErrorString(0)` returns NULL.
    #[test]
    fn error_strings_are_expat_text() {
        let mut engine = engine();
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() { var parser = new XMLParser();\n\
                 var fresh = typeof parser.errorString;\n\
                 parser.parse(\"<a></b>\");\n\
                 return fresh + \"|\" + parser.errorString + \"|\" + parser.errorCode; })()",
            )
            .expect("errors");
        assert_eq!(value, Variant::String("void|mismatched tag|7".to_string()));
    }

    /// A successful parse leaves the position at the end of the input.
    #[test]
    fn success_reports_the_end_position() {
        let mut engine = engine();
        assert_eq!(
            parse_trace(&mut engine, "<a>text</a>"),
            "1|0|11|1|11|0|s:a;c:text;e:a;"
        );
    }

    /// `parse()` with no argument is `TJS_E_BADPARAMCOUNT` (`:503`).
    #[test]
    fn parse_requires_an_argument() {
        let mut engine = engine();
        let error = engine
            .execute_expression("inline.tjs", "(new XMLParser()).parse()")
            .expect_err("missing argument");
        assert_eq!(error.kind, TjsErrorKind::BadParamCount);
        assert_eq!(error.message, "Invalid argument count");
    }

    /// `parseStorage` reads through the engine's storage and streams the raw
    /// bytes (`:427-455`); a missing file is the reference's
    /// `cannot open : <name>` (`:438-440`).
    #[test]
    fn parse_storage_reads_the_storage() {
        let root = test_root("storage");
        fs::write(root.join("doc.xml"), b"<a>one</a>").expect("write fixture");
        let mut engine = test_engine(&root);
        engine
            .execute_script("harness.tjs", HARNESS)
            .expect("harness");
        let script = "(function() { var capture = new Capture();\n\
             var parser = new XMLParser(capture);\n\
             var ok = parser.parseStorage(\"doc.xml\");\n\
             return ok + \"|\" + capture.trace[0] + \";\" + capture.trace[1] + \";\" + capture.trace[2]; })()";
        let value = engine
            .execute_expression("inline.tjs", script)
            .expect("parseStorage");
        assert_eq!(value, Variant::String("1|s:a;c:one;e:a".to_string()));

        let message = expression_string(
            &mut engine,
            "(function() { var parser = new XMLParser();\n\
             try { parser.parseStorage(\"absent.xml\"); return \"no error\"; }\n\
             catch (e) { return e.message; } })()",
        );
        assert_eq!(message, "cannot open : absent.xml");
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// The handler target may be given per call instead of per constructor
    /// (`:503-505`, `:511-518`).
    #[test]
    fn the_target_argument_is_honoured() {
        let mut engine = engine();
        engine
            .execute_script(
                "harness.tjs",
                r#"class Sink {
                    var trace = [];
                    function Sink() { }
                    function startElement(name, attrs) { trace.add("s:" + name); }
                }"#,
            )
            .expect("harness");
        let script = r#"(function() {
        var sink = new Sink();
        var parser = new XMLParser();
        var ok = parser.parse("<a/>", sink);
        return ok + "|" + sink.trace.count + "|" + parser.errorCode; })()"#;
        assert_eq!(
            engine
                .execute_expression("inline.tjs", script)
                .expect("parse"),
            Variant::String("1|1|0".to_string())
        );
    }

    /// A script subclass of `XMLParser` (the shipped `XML.tjs` pattern,
    /// `class DOMDocument extends DOMNode, XMLParser`) dispatches to its own
    /// handler members through the class chain.
    #[test]
    fn subclass_handlers_are_dispatched() {
        let mut engine = engine();
        engine
            .execute_script(
                "harness.tjs",
                r#"class Document extends XMLParser {
                    var trace = [];
                    function Document() {
                        global.XMLParser.XMLParser(this);
                    }
                    function startElement(name, attrs) { trace.add("s:" + name); }
                    function endElement(name) { trace.add("e:" + name); }
                    function characterData(data) { trace.add("c:" + data); }
                }"#,
            )
            .expect("harness");
        let script = r#"(function() {
        var doc = new Document();
        var ok = doc.parse("<root attr='1'>text</root>");
        return ok + "|" + doc.trace.count + "|" + doc.trace[0] + ";" + doc.trace[1] + ";" +
            doc.trace[2] + ";" + doc.errorCode; })()"#;
        assert_eq!(
            engine
                .execute_expression("inline.tjs", script)
                .expect("parse"),
            Variant::String("1|3|s:root;c:text;e:root;0".to_string())
        );
    }

    /// With only a `defaultHandler`, the parser hands every token to it as
    /// raw text — entity references unexpanded, expat's `reportDefault` rule
    /// (`xmlparse.c:2225-2227`).
    #[test]
    fn the_default_handler_receives_raw_tokens() {
        let mut engine = engine();
        engine
            .execute_script(
                "harness.tjs",
                r#"class Defaults {
                    var trace = [];
                    function Defaults() { }
                    function defaultHandler(data) { trace.add(data); }
                }"#,
            )
            .expect("harness");
        let script = r#"(function() {
        var sink = new Defaults();
        var parser = new XMLParser(sink);
        var ok = parser.parse("<a x=\"1\">x&amp;y</a>");
        var text = ok + "|";
        for (var i = 0; i < sink.trace.count; i++) { text += sink.trace[i] + ";"; }
        return text; })()"#;
        assert_eq!(
            engine
                .execute_expression("inline.tjs", script)
                .expect("parse"),
            Variant::String("1|<a x=\"1\">;x;&amp;;y;</a>;".to_string())
        );
    }

    /// An exception thrown by a handler reaches the `parse` caller as a
    /// catchable exception, the way the reference lets it cross `XML_Parse`.
    ///
    /// The message is the engine's event-boundary conversion of the thrown
    /// value (`uncaught exception "boom"`, `vm/mod.rs:1338`): the handler runs
    /// through the runtime's callback entry point, which turns an escaped
    /// throw into that text, where the reference's C++ call chain carried the
    /// original value through. The exception is still thrown and catchable at
    /// the `parse` call site, which is what a script relies on.
    #[test]
    fn handler_exceptions_propagate() {
        let mut engine = engine();
        engine
            .execute_script(
                "harness.tjs",
                r#"class Boom {
                    function Boom() { }
                    function startElement(name, attrs) { throw "boom"; }
                }"#,
            )
            .expect("harness");
        let script = r#"(function() {
            var parser = new XMLParser(new Boom());
            try { parser.parse("<a/>"); return "no throw"; }
            catch (e) { return e.message; }
        })()"#;
        assert_eq!(
            expression_string(&mut engine, script),
            "uncaught exception \"boom\""
        );
    }

    /// `V2Unlink` removes the global (`:674-696`) and `Plugins.link`
    /// installs it again.
    #[test]
    fn unlink_removes_the_global_and_relink_restores_it() {
        let mut engine = engine();
        engine
            .execute_script("unlink.tjs", "Plugins.unlink(\"expat.dll\");")
            .expect("unlink");
        let error = engine
            .execute_expression("inline.tjs", "new XMLParser()")
            .expect_err("global gone");
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);

        engine
            .execute_script("relink.tjs", "Plugins.link(\"expat.dll\");")
            .expect("relink");
        let value = engine
            .execute_expression("inline.tjs", "typeof XMLParser")
            .expect("relink");
        assert_eq!(value, Variant::String("Object".to_string()));
    }

    /// The constructible class object works without an instance, exactly as
    /// `Class.method()` on a native class does in TJS2.
    #[test]
    fn the_class_object_parses() {
        let mut engine = engine();
        let value = engine
            .execute_expression(
                "inline.tjs",
                "XMLParser.parse(\"<a/>\") + \":\" + XMLParser.errorCode",
            )
            .expect("class object");
        assert_eq!(value, Variant::String("1:0".to_string()));
    }

    /// The fresh state of a parser that never parsed: `currentByteIndex` is
    /// -1 (`XML_GetCurrentByteIndex` with no event pointer) and the error
    /// code is 0.
    #[test]
    fn fresh_parsers_report_no_error() {
        let mut engine = engine();
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() { var p = new XMLParser(); return p.errorCode + \":\" + \
                 typeof p.errorString + \":\" + p.currentByteIndex + \":\" + p.currentLineNumber + \
                 \":\" + p.currentColumnNumber + \":\" + p.currentByteCount; })()",
            )
            .expect("fresh");
        assert_eq!(value, Variant::String("0:void:-1:1:0:0".to_string()));
    }
}
