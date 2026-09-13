//! `httprequest.dll` — HTTP(S) requests from scripts.
//!
//! Reference: krkr2 trunk `src/plugins/win32/httprequest/` (`Main.cpp` 722
//! lines, `HttpConnection.cpp` 491, `HttpConnection.h` 245, `encoding.cpp`,
//! `match.cpp`, `Base64.cpp`); `manual.tjs` is the contract this module
//! follows. The plugin exposes one global class:
//!
//! `HttpRequest(window, cert = true, agentName = "KIRIKIRI")` with
//! `open(method, url, user, pass)` (always asynchronous in the reference),
//! `setRequestHeader`, `send`/`sendSync`, `sendStorage`/`sendStorageSync`,
//! `abort`, `getAllResponseHeaders`, `getResponseHeader`, `getResponseText`,
//! the read-only `readyState`/`response`/`responseData`/`status`/`statusText`/
//! `contentType`/`contentTypeEncoding`/`contentLength` properties, the
//! `UNINITIALIZED..LOADED` constants and the `onReadyStateChange(state)` /
//! `onProgress(upload, percent)` events.
//!
//! # HTTP client
//!
//! `ureq` is a blocking, pure-Rust client (rustls for TLS) with no async
//! runtime, which matches this engine's dependency style and the reference's
//! synchronous worker-thread design. The agent is configured the way the
//! reference configures WinINet: agent name `KIRIKIRI`, no automatic
//! content-decoding request, HTTP error statuses delivered as ordinary
//! responses (the reference reads `status`, it never receives an exception
//! for a 404), and — for `cert = false` — certificate verification disabled.
//!
//! # Deliberate divergences
//!
//! - `open()` only validates the URL and its scheme. The reference's
//!   `InternetConnect` dials the server inside `open()`; this port defers all
//!   network work to `send()` like a normal HTTP client, so a connection
//!   failure arrives through the response state (`status = 0`,
//!   `statusText = error`) instead of an exception from `open()`.
//! - The engine has no message pump for plugins, so the reference's
//!   `WM_APP+6`/`WM_APP+7` messages are delivered on engine ticks by a hidden
//!   `Timer` instance (interval 1 ms, enabled while a request is in flight):
//!   the worker thread records the state/progress records and the pump drains
//!   them in order on the script thread.
//! - A request body is sent as one sized blob, so `Content-Length` matches the
//!   reference (`_send` reads the data up front too); the upload progress
//!   event is therefore a single `onProgress(true, 100)` for a non-empty body
//!   rather than one per 16 KiB chunk. Download progress is per 16 KiB chunk
//!   like the reference.
//! - `statusText` is the status's canonical reason phrase; the `http` crate
//!   does not retain a server's custom phrase.
//! - Response header names are stored as the client canonicalizes them
//!   (lower-case), and `getResponseHeader` looks names up
//!   case-insensitively; the reference keeps the server's spelling and
//!   compares exactly. The value is returned without the single space the
//!   reference's raw-header parse keeps after the colon.
//! - `saveStorage` is written when the response finishes rather than
//!   streamed, because the engine's storage write API takes whole buffers.
//!
//! One reference behaviour worth stating because scripts depend on it: a
//! request is finished when its response is done (`HttpConnection::response`
//! closes the handles, `HttpConnection.cpp:489`), so a second `send` without a
//! new `open` throws `not open`.

// `result_large_err` is the crate-wide `TjsError` size lint every native
// callback carries, silenced here the same way `wf_basic_effect.rs` and
// `wf_typical_dsp.rs` silence it.
#![allow(clippy::result_large_err)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use base64::Engine as _;
use encoding_rs::Encoding;

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "HttpRequest class (open/send/sendStorage/sendSync, response properties, \
              onReadyStateChange/onProgress)",
    notes: "HTTP and HTTPS through ureq (rustls). Requests run on a worker thread and the \
            completion events are delivered on engine ticks; the synchronous sendSync/\
            sendStorageSync path returns the status directly. open() validates the URL only; \
            network failures surface through status/statusText as the reference's do.",
    install: |engine| engine.register_plugin(HttpRequestPlugin),
};

pub struct HttpRequestPlugin;

impl KrkrPlugin for HttpRequestPlugin {
    fn name(&self) -> &str {
        "httprequest.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_http_request_class(runtime);
        runtime
            .host_mut()
            .log("httprequest.dll registered: HttpRequest class");
        Ok(())
    }
}

/// `HttpRequest::ReadyState` (`Main.cpp:35-41`).
const READYSTATE_UNINITIALIZED: i64 = 0;
const READYSTATE_OPEN: i64 = 1;
const READYSTATE_SENT: i64 = 2;
const READYSTATE_RECEIVING: i64 = 3;
const READYSTATE_LOADED: i64 = 4;

/// The constructor's `AGENT_NAME` (`Main.cpp:12`); the manual's third
/// argument is ignored by the reference and by this port.
const AGENT_NAME: &str = "KIRIKIRI";

/// `BUFSIZE` (`HttpConnection.cpp:18`), the transfer chunk the reference posts
/// progress for.
const CHUNK_SIZE: usize = 1024 * 16;

// ---------------------------------------------------------------------------
// Instance state
// ---------------------------------------------------------------------------

/// A progress/state record the worker leaves for the script-thread pump.
enum Delivery {
    State(i64),
    Progress { upload: bool, percent: f64 },
    Finished,
}

/// What one finished worker reports back.
#[derive(Default)]
struct Outcome {
    canceled: bool,
    error: Option<String>,
    status_code: i64,
    status_text: String,
    content_type: String,
    encoding: String,
    content_length: i64,
    valid_content_length: bool,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// True when the reference's `<meta http-equiv="content-type">` sniff
    /// still has to run (`HttpConnection.cpp:451-487`).
    sniff_meta: bool,
}

#[derive(Default)]
struct Shared {
    deliveries: Vec<Delivery>,
    outcome: Option<Box<Outcome>>,
}

/// One `HttpRequest` instance.
struct RequestState {
    #[allow(dead_code)]
    window: ObjectHandle,
    cert: bool,
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    request_content_type: String,
    request_encoding: String,
    request_content_length: i64,
    opened: bool,
    input: Vec<u8>,
    output: Vec<u8>,
    save_storage: Option<String>,
    ready_state: i64,
    status_code: i64,
    status_text: String,
    content_type: String,
    encoding: String,
    content_length: i64,
    valid_content_length: bool,
    response_headers: Vec<(String, String)>,
    worker: Option<JoinHandle<()>>,
    canceled: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    timer: Option<ObjectHandle>,
}

impl RequestState {
    fn new(window: ObjectHandle, cert: bool, timer: Option<ObjectHandle>) -> Self {
        Self {
            window,
            cert,
            method: String::new(),
            url: String::new(),
            headers: Vec::new(),
            request_content_type: String::new(),
            request_encoding: String::new(),
            request_content_length: 0,
            opened: false,
            input: Vec::new(),
            output: Vec::new(),
            save_storage: None,
            ready_state: READYSTATE_UNINITIALIZED,
            status_code: 0,
            status_text: String::new(),
            content_type: String::new(),
            encoding: String::new(),
            content_length: 0,
            valid_content_length: false,
            response_headers: Vec::new(),
            worker: None,
            canceled: Arc::new(AtomicBool::new(false)),
            shared: Arc::new(Mutex::new(Shared::default())),
            timer,
        }
    }

    /// `stopThread` (`Main.cpp:654-661`): cancel and join the worker.
    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.canceled.store(true, Ordering::SeqCst);
            let _ = worker.join();
        }
    }
}

impl Drop for RequestState {
    fn drop(&mut self) {
        self.stop();
    }
}

thread_local! {
    static REQUESTS: RefCell<BTreeMap<ObjectHandle, RequestState>> =
        const { RefCell::new(BTreeMap::new()) };
}

// ---------------------------------------------------------------------------
// Shared plugin helpers
// ---------------------------------------------------------------------------

/// The object a native acts on; `None` for the class object itself.
fn instance_handle(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    let handle = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))?;
    (handle != runtime.global_handle()).then_some(handle)
}

fn bound_instance(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    class_name: &'static str,
) -> ObjectHandle {
    let instance = this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or_else(|| runtime.alloc_ordinary_object());
    runtime.add_object_class_info(instance, class_name);
    runtime.set_object_member(
        instance,
        "__className",
        Variant::String(class_name.to_string()),
    );
    if let Variant::Object(class) = runtime.global_member(class_name)
        && runtime.object_super_class(instance).is_none()
    {
        runtime.set_object_super_class(instance, class);
    }
    instance
}

/// `IsInstanceOf(0, NULL, NULL, name, obj)` over the class chain
/// (`Main.cpp:379-382`).
fn object_is_instance_of(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    class_name: &str,
) -> bool {
    let mut current = Some(object);
    while let Some(handle) = current {
        if runtime
            .object_class_infos(handle)
            .iter()
            .any(|info| info == class_name)
        {
            return true;
        }
        current = runtime.object_super_class(handle);
    }
    false
}

/// `setReceiver(true)` (`Main.cpp:430-438`) is the reference's message-pump
/// registration; this engine's pump is a hidden `Timer` whose action is the
/// owning object's `__httpTick` member, built through the engine's
/// class-name constructor idiom.
fn create_tick_timer(
    runtime: &mut Runtime<KrkrHost>,
    owner: ObjectHandle,
    action: &str,
) -> Option<ObjectHandle> {
    let Variant::Object(class) = runtime.global_member("Timer") else {
        return None;
    };
    let timer = runtime.alloc_ordinary_object();
    let constructor = runtime.object_member(class, "Timer");
    runtime.set_object_member(timer, "Timer", constructor);
    let args = vec![Variant::Object(owner), Variant::String(action.to_string())];
    if runtime
        .call_variant_method(Variant::Object(timer), "Timer", args)
        .is_err()
    {
        return None;
    }
    runtime.set_object_member(timer, "interval", Variant::Integer(1));
    runtime.set_object_member(timer, "enabled", Variant::Integer(0));
    runtime.set_object_member(owner, "__timer", Variant::Object(timer));
    Some(timer)
}

fn set_timer_enabled(runtime: &mut Runtime<KrkrHost>, timer: Option<ObjectHandle>, enabled: bool) {
    if let Some(timer) = timer {
        runtime.set_object_member(timer, "enabled", Variant::Integer(i64::from(enabled)));
    }
}

/// Delivers one script event like `TVPPostEvent`: a missing handler is a
/// silent drop and an escaping script error goes through
/// `System.exceptionHandler` (`EventIntf.cpp:622`).
fn deliver_event(
    runtime: &mut Runtime<KrkrHost>,
    target: ObjectHandle,
    name: &str,
    args: Vec<Variant>,
) -> Result<()> {
    if !runtime.object_valid(target) {
        return Ok(());
    }
    if matches!(runtime.object_member(target, name), Variant::Void) {
        return Ok(());
    }
    match runtime.call_object_method(target, name, args) {
        Ok(_) => Ok(()),
        Err(error) => {
            if runtime.process_unhandled_exception(&error)? {
                runtime.host_mut().log(&format!(
                    "handled httprequest event `{name}` error: {}",
                    error.message
                ));
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

fn deny_write(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _value: Variant,
) -> Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Content-Type parsing (`HttpConnection.cpp:60-95`, `match.cpp`)
// ---------------------------------------------------------------------------

/// `parseContentType`: the MIME type up to `;` (or whitespace), then a
/// `charset=` parameter when it directly follows.
fn parse_content_type(value: &str) -> (String, String) {
    let bytes = value.as_bytes();
    let mut start = 0;
    while start < bytes.len() && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    let rest = &value[start..];
    let Some(semicolon) = rest.find(';') else {
        let end = rest
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(rest.len());
        return (rest[..end].to_string(), String::new());
    };
    let head = &rest[..semicolon];
    let end = head
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(head.len());
    let content_type = head[..end].to_string();
    let tail = rest[semicolon + 1..].trim_start();
    let mut encoding = String::new();
    // `get(..7)` keeps a multibyte tail from splitting a character: a
    // server's `<meta … content="text/html; あああ">` and a script's
    // `setRequestHeader("Content-Type", …)` both reach this parser, and a
    // byte-indexed slice would panic the process.
    if tail
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("charset"))
    {
        let after = tail[7..].trim_start();
        if let Some(value) = after.strip_prefix('=') {
            let value = value.trim_start();
            let end = value
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(value.len());
            encoding = value[..end].to_string();
        }
    }
    (content_type, encoding)
}

/// ASCII case-insensitive search for `needle` in `haystack`, starting at
/// byte offset `from`.
fn find_ascii_ci(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    (from..=last).find(|start| haystack[*start..*start + needle.len()].eq_ignore_ascii_case(needle))
}

/// `matchContentType` (`match.cpp:16-36`): the `content=` value of a
/// `<meta http-equiv="content-type" ...>` tag, case-insensitive, quotes
/// stripped. The reference uses a regex; this is the same match by hand. All
/// scanning is done on byte offsets located at ASCII delimiters and every
/// slice goes through `str::get`, so a multibyte body can never split a
/// character (matching the reference, which reads UTF-16 code units).
fn sniff_meta_content_type(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut offset = 0;
    while let Some(found) = find_ascii_ci(bytes, offset, b"<meta") {
        let start = found + "<meta".len();
        // The tag ends at the next '>'.
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == b'>')
            .map(|end| start + end)?;
        let tag = text.get(start..end)?;
        if meta_tag_matches(tag)
            && let Some(value) = meta_content_value(tag)
        {
            return Some(value);
        }
        offset = end + 1;
        if offset >= bytes.len() {
            break;
        }
    }
    None
}

fn meta_tag_matches(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    let mut index = 0;
    let mut has_http_equiv = false;
    let mut has_content = false;
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index] == b' ' || bytes[index] == b'\t') {
            index += 1;
        }
        let name_start = index;
        while index < bytes.len()
            && bytes[index] != b'='
            && bytes[index] != b' '
            && bytes[index] != b'\t'
        {
            index += 1;
        }
        let Some(name) = tag.get(name_start..index) else {
            return false;
        };
        while index < bytes.len() && (bytes[index] == b' ' || bytes[index] == b'\t') {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            continue;
        }
        index += 1;
        while index < bytes.len() && (bytes[index] == b' ' || bytes[index] == b'\t') {
            index += 1;
        }
        if name.eq_ignore_ascii_case("http-equiv") {
            has_http_equiv = true;
        } else if name.eq_ignore_ascii_case("content") {
            has_content = true;
        }
        // Skip the value.
        if index < bytes.len() && (bytes[index] == b'"' || bytes[index] == b'\'') {
            let quote = bytes[index];
            index += 1;
            while index < bytes.len() && bytes[index] != quote {
                index += 1;
            }
            index += 1;
        } else {
            while index < bytes.len() && bytes[index] != b' ' && bytes[index] != b'\t' {
                index += 1;
            }
        }
    }
    has_http_equiv && has_content
}

fn meta_content_value(tag: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index] == b' ' || bytes[index] == b'\t') {
            index += 1;
        }
        let name_start = index;
        while index < bytes.len()
            && bytes[index] != b'='
            && bytes[index] != b' '
            && bytes[index] != b'\t'
        {
            index += 1;
        }
        let name = tag.get(name_start..index)?;
        while index < bytes.len() && (bytes[index] == b' ' || bytes[index] == b'\t') {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            continue;
        }
        index += 1;
        while index < bytes.len() && (bytes[index] == b' ' || bytes[index] == b'\t') {
            index += 1;
        }
        let value = if index < bytes.len() && (bytes[index] == b'"' || bytes[index] == b'\'') {
            let quote = bytes[index];
            index += 1;
            let start = index;
            while index < bytes.len() && bytes[index] != quote {
                index += 1;
            }
            let value = tag.get(start..index)?.to_string();
            index += 1;
            value
        } else {
            let start = index;
            while index < bytes.len() && bytes[index] != b' ' && bytes[index] != b'\t' {
                index += 1;
            }
            tag.get(start..index)?.to_string()
        };
        if name.eq_ignore_ascii_case("content") {
            return Some(value);
        }
    }
    None
}

/// `getEncoding` (`encoding.cpp:29-38`): an IANA name to a character encoding,
/// defaulting to UTF-8 for an empty or unknown name.
fn encoding_for_name(name: &str) -> &'static Encoding {
    if name.is_empty() {
        return encoding_rs::UTF_8;
    }
    Encoding::for_label(name.as_bytes()).unwrap_or(encoding_rs::UTF_8)
}

// ---------------------------------------------------------------------------
// The HTTP worker
// ---------------------------------------------------------------------------

/// Runs one request and reports its outcome. `emit` receives the intermediate
/// state/progress records; the synchronous path passes a sink that drops them
/// (the reference's `*Sync` callbacks skip the posts the same way).
fn perform(
    cert: bool,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    canceled: &AtomicBool,
    emit: &mut impl FnMut(Delivery),
) -> Outcome {
    let mut outcome = Outcome::default();
    let config = ureq::config::Config::builder()
        .user_agent(AGENT_NAME)
        // The reference never throws for an HTTP error status; it reads
        // `status` after the response arrives.
        .http_status_as_error(false)
        // A method the reference passes through to WinINet verbatim.
        .allow_non_standard_methods(true)
        .max_redirects(10)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .disable_verification(!cert)
                .build(),
        )
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let mut request = match ureq::http::Request::builder()
        .method(method)
        .uri(url)
        .body(body.to_vec())
    {
        Ok(request) => request,
        Err(error) => {
            outcome.error = Some(error.to_string());
            return outcome;
        }
    };
    for (name, value) in headers {
        // The reference sets its own `Content-Length` when a body is sent;
        // ureq derives the framing from the body length, so a script's
        // duplicate would only confuse the server.
        if name.eq_ignore_ascii_case("Content-Length") {
            continue;
        }
        let (Ok(name), Ok(value)) = (
            ureq::http::HeaderName::try_from(name.as_str()),
            ureq::http::HeaderValue::try_from(value.as_str()),
        ) else {
            // The reference hands the header string to WinINet and lets it
            // complain; an unencodable header is a request failure here.
            outcome.error = Some(format!("invalid request header `{name}: {value}`"));
            return outcome;
        };
        request.headers_mut().append(name, value);
    }

    let response = match agent.run(request) {
        Ok(response) => response,
        Err(error) => {
            outcome.canceled = canceled.load(Ordering::SeqCst);
            if !outcome.canceled {
                outcome.error = Some(error.to_string());
            }
            return outcome;
        }
    };

    let status = response.status();
    outcome.status_code = i64::from(status.as_u16());
    outcome.status_text = status.canonical_reason().unwrap_or_default().to_string();
    outcome.headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    if let Some(value) = response.headers().get(ureq::http::header::CONTENT_LENGTH)
        && let Some(length) = value
            .to_str()
            .ok()
            .and_then(|value| value.trim().parse::<i64>().ok())
    {
        outcome.content_length = length;
        outcome.valid_content_length = true;
    }
    if let Some(value) = response.headers().get(ureq::http::header::CONTENT_TYPE) {
        let value = value.to_str().unwrap_or_default();
        let (content_type, encoding) = parse_content_type(value);
        outcome.content_type = content_type;
        outcome.encoding = encoding;
    }

    emit(Delivery::State(READYSTATE_RECEIVING));

    // `HttpConnection::response` reads the body only for a 200
    // (`HttpConnection.cpp:452-488`).
    if outcome.status_code == 200 {
        let mut reader = response.into_body().into_reader();
        let mut buffer = vec![0u8; CHUNK_SIZE];
        loop {
            if canceled.load(Ordering::SeqCst) {
                outcome.canceled = true;
                return outcome;
            }
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    outcome.body.extend_from_slice(&buffer[..read]);
                    let bp = if outcome.content_length > 0 {
                        (outcome.body.len() as i64) * 10000 / outcome.content_length
                    } else {
                        0
                    };
                    emit(Delivery::Progress {
                        upload: false,
                        percent: bp as f64 / 100.0,
                    });
                }
                Err(error) => {
                    outcome.canceled = canceled.load(Ordering::SeqCst);
                    if !outcome.canceled {
                        outcome.error = Some(error.to_string());
                    }
                    return outcome;
                }
            }
        }
        if !outcome.valid_content_length {
            outcome.content_length = outcome.body.len() as i64;
        }
        outcome.sniff_meta =
            outcome.content_type.eq_ignore_ascii_case("text/html") && outcome.encoding.is_empty();
    }
    outcome
}

// ---------------------------------------------------------------------------
// `HttpRequest` class
// ---------------------------------------------------------------------------

fn install_http_request_class(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let Some(window_arg) = args.first() else {
                return Err(TjsError::bad_param_count());
            };
            let Some(window) = window_arg.object_handle() else {
                // The reference dereferences `AsObjectNoAddRef()`
                // unconditionally; a non-object argument is the same invalid
                // window.
                return Err(TjsError::runtime("InvalidObject"));
            };
            if !object_is_instance_of(runtime, window, "Window") {
                return Err(TjsError::runtime("InvalidObject"));
            }
            let cert = match args.get(1) {
                Some(value) => value.to_integer()? != 0,
                None => true,
            };
            // The manual's third argument (`agentName`) is ignored by the
            // reference: the agent string is the `AGENT_NAME` constant.
            let instance = bound_instance(runtime, this_obj, "HttpRequest");
            install_http_request_members(runtime, instance);
            let timer = create_tick_timer(runtime, instance, "__httpTick");
            REQUESTS.with(|map| {
                map.borrow_mut()
                    .insert(instance, RequestState::new(window, cert, timer));
            });
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "HttpRequest");
    install_http_request_members(runtime, class);
    for (name, value) in [
        ("UNINITIALIZED", READYSTATE_UNINITIALIZED),
        ("OPEN", READYSTATE_OPEN),
        ("SENT", READYSTATE_SENT),
        ("RECEIVING", READYSTATE_RECEIVING),
        ("LOADED", READYSTATE_LOADED),
    ] {
        runtime.set_object_member(class, name, Variant::Integer(value));
    }
    runtime.register_object_native(class, "finalize", http_finalize);
    runtime.set_global_member("HttpRequest", Variant::Object(class));
}

fn install_http_request_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native_with_arg_count(
        handle,
        "open",
        NativeArgCount::AtLeast(2),
        http_open,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "setRequestHeader",
        NativeArgCount::AtLeast(2),
        http_set_request_header,
    );
    runtime.register_object_native(handle, "send", http_send);
    runtime.register_object_native(handle, "sendSync", http_send_sync);
    runtime.register_object_native_with_arg_count(
        handle,
        "sendStorage",
        NativeArgCount::AtLeast(1),
        http_send_storage,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "sendStorageSync",
        NativeArgCount::AtLeast(1),
        http_send_storage_sync,
    );
    runtime.register_object_native(handle, "abort", http_abort);
    runtime.register_object_native(handle, "getAllResponseHeaders", http_all_response_headers);
    runtime.register_object_native_with_arg_count(
        handle,
        "getResponseHeader",
        NativeArgCount::AtLeast(1),
        http_response_header,
    );
    runtime.register_object_native(handle, "getResponseText", http_response_text);
    runtime.register_object_native(handle, "__httpTick", http_tick);
    runtime.register_object_native(handle, "finalize", http_finalize);
    runtime.register_object_native_property_with_access(
        handle,
        "readyState",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            request_property(runtime, this_obj, |state| {
                Variant::Integer(state.ready_state)
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "response",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            request_property(runtime, this_obj, |state| {
                // `get(..5)` keeps a multibyte content type (a meta-sniffed
                // `<meta … content="ああ">`) from splitting a character.
                if state
                    .content_type
                    .get(..5)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("text/"))
                {
                    return Variant::String(decode_text(&state.output, &state.encoding));
                }
                if state.output.is_empty() {
                    Variant::Void
                } else {
                    Variant::Octet(state.output.clone())
                }
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "responseData",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            request_property(runtime, this_obj, |state| {
                if state.output.is_empty() {
                    Variant::Void
                } else {
                    Variant::Octet(state.output.clone())
                }
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "status",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            request_property(runtime, this_obj, |state| {
                Variant::Integer(state.status_code)
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "statusText",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            request_property(runtime, this_obj, |state| {
                Variant::String(state.status_text.clone())
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "contentType",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            request_property(runtime, this_obj, |state| {
                Variant::String(state.content_type.clone())
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "contentTypeEncoding",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            request_property(runtime, this_obj, |state| {
                Variant::String(state.encoding.clone())
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "contentLength",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            request_property(runtime, this_obj, |state| {
                Variant::Integer(state.content_length)
            })
        },
        deny_write,
    );
}

fn with_request<R>(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    action: impl FnOnce(&RequestState) -> R,
) -> Result<R> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    REQUESTS.with(|map| {
        map.borrow()
            .get(&instance)
            .map(action)
            .ok_or_else(TjsError::native_class_crash)
    })
}

/// A property read tolerant of a class-object receiver: the class object is
/// not an instance, so its read-only properties answer void.
fn request_property(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    action: impl FnOnce(&RequestState) -> Variant,
) -> Result<Variant> {
    let Some(instance) = instance_handle(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    REQUESTS.with(|map| match map.borrow().get(&instance) {
        Some(state) => Ok(action(state)),
        None => Ok(Variant::Void),
    })
}

fn with_request_mut<R>(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    action: impl FnOnce(&mut RequestState) -> R,
) -> Result<R> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    REQUESTS.with(|map| {
        map.borrow_mut()
            .get_mut(&instance)
            .map(action)
            .ok_or_else(TjsError::native_class_crash)
    })
}

/// `checkRunning` (`Main.cpp:390-394`): a worker is still going.
fn check_running(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Result<()> {
    let running = with_request(runtime, this_obj, |state| state.worker.is_some())?;
    if running {
        return Err(TjsError::runtime("already running"));
    }
    Ok(())
}

/// `checkOpen` (`Main.cpp:396-400`).
fn check_open(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Result<()> {
    let opened = with_request(runtime, this_obj, |state| state.opened)?;
    if !opened {
        return Err(TjsError::runtime("not open"));
    }
    Ok(())
}

fn http_finalize(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(instance) = this_obj {
        let timer = REQUESTS
            .with(|map| map.borrow_mut().remove(&instance))
            .and_then(|state| state.timer);
        set_timer_enabled(runtime, timer, false);
    }
    Ok(Variant::Void)
}

/// `HttpRequest::_open` (`Main.cpp:81-96`) with `HttpConnection::open`
/// (`HttpConnection.cpp:124-228`): validate the URL, remember the method and
/// basic-auth credentials, clear the previous request, then report the OPEN
/// state.
fn http_open(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let method = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let url = args
        .get(1)
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let user = args.get(2).map(Variant::to_tjs_string).transpose()?;
    let pass = args.get(3).map(Variant::to_tjs_string).transpose()?;

    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    // `abort()` runs first, then `clearParam` resets the request headers.
    let timer = REQUESTS.with(|map| {
        let mut map = map.borrow_mut();
        let state = map
            .get_mut(&instance)
            .ok_or_else(TjsError::native_class_crash)?;
        state.stop();
        state.canceled.store(false, Ordering::SeqCst);
        state.input.clear();
        state.output.clear();
        state.save_storage = None;
        state.headers.clear();
        state.request_content_type.clear();
        state.request_encoding.clear();
        state.request_content_length = 0;
        Ok::<_, TjsError>(state.timer)
    })?;
    set_timer_enabled(runtime, timer, false);

    let uri: ureq::http::Uri = url
        .parse()
        .map_err(|error: ureq::http::uri::InvalidUri| TjsError::runtime(error.to_string()))?;
    match uri.scheme_str() {
        Some("http") | Some("https") => {}
        _ => return Err(TjsError::runtime("invalid protocol")),
    }

    // Credentials in the URL win over the arguments (`HttpConnection.cpp:157-172`).
    let mut user = user.unwrap_or_default();
    let mut pass = pass.unwrap_or_default();
    if let Some(authority) = uri.authority() {
        let authority = authority.as_str();
        if let Some(at) = authority.rfind('@') {
            let credentials = &authority[..at];
            if let Some((url_user, url_pass)) = credentials.split_once(':') {
                user = url_user.to_string();
                pass = url_pass.to_string();
            } else {
                user = credentials.to_string();
            }
        }
    }

    REQUESTS.with(|map| {
        if let Some(state) = map.borrow_mut().get_mut(&instance) {
            state.method = method;
            state.url = url;
            state.opened = true;
            if !user.is_empty() && !pass.is_empty() {
                let credentials = base64::engine::general_purpose::STANDARD
                    .encode(format!("{user}:{pass}").as_bytes());
                state
                    .headers
                    .push(("Authorization".to_string(), format!("Basic {credentials}")));
            }
        }
    });
    // `onReadyStateChange(READYSTATE_OPEN)` (`Main.cpp:84`).
    with_request_mut(runtime, this_obj, |state| {
        state.ready_state = READYSTATE_OPEN;
    })?;
    queue_state(runtime, instance, READYSTATE_OPEN);
    Ok(Variant::Void)
}

/// `setRequestHeader` (`Main.cpp:103-106`) over `addHeader`
/// (`HttpConnection.cpp:107-119`).
fn http_set_request_header(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    check_running(runtime, this_obj)?;
    let name = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let value = args
        .get(1)
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    with_request_mut(runtime, this_obj, |state| {
        if name.eq_ignore_ascii_case("Content-Type") {
            let (content_type, encoding) = parse_content_type(&value);
            state.request_content_type = content_type;
            state.request_encoding = encoding;
        } else if name.eq_ignore_ascii_case("Content-Length") {
            state.request_content_length = value.trim().parse().unwrap_or(0);
        }
        state.headers.push((name, value));
    })?;
    Ok(Variant::Void)
}

/// One `_send` (`Main.cpp:114-169`).
fn prepare_send(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: &[Variant],
    storage_name: Option<String>,
) -> Result<()> {
    check_running(runtime, this_obj)?;
    check_open(runtime, this_obj)?;
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    let save_storage = args
        .get(1)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_tjs_string)
        .transpose()?;

    // The output stream is opened up front in the reference
    // (`Main.cpp:117-122`); the engine's write API has no open, so a
    // zero-length write creates/truncates the target, which is what the
    // create/write stream does.
    if let Some(target) = &save_storage
        && runtime
            .host_mut()
            .write_binary_storage(target, "", &[])
            .is_err()
    {
        return Err(TjsError::runtime("saveStorage open failed"));
    }

    let input = if let Some(storage) = storage_name {
        match runtime.host().read_binary_storage(&storage) {
            Ok(bytes) => bytes,
            Err(_) => return Err(TjsError::runtime("sendStorage open failed")),
        }
    } else {
        match args.first() {
            Some(Variant::String(text)) => {
                let encoding =
                    with_request(runtime, this_obj, |state| state.request_encoding.clone())?;
                let (bytes, _, _) = encoding_for_name(&encoding).encode(text);
                bytes.into_owned()
            }
            Some(Variant::Octet(bytes)) => bytes.clone(),
            _ => Vec::new(),
        }
    };

    let length = input.len();
    REQUESTS.with(|map| {
        if let Some(state) = map.borrow_mut().get_mut(&instance) {
            state.input = input;
            state.save_storage = save_storage;
            if length > 0 {
                // `_send` keeps the reference's Content-Length header
                // bookkeeping; ureq derives the framing from the body length.
                state.request_content_length = length as i64;
            }
        }
    });
    Ok(())
}

fn http_send(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    prepare_send(runtime, this_obj, &args, None)?;
    start_request(runtime, this_obj)?;
    Ok(Variant::Void)
}

fn http_send_sync(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    prepare_send(runtime, this_obj, &args, None)?;
    let status = run_request_now(runtime, this_obj)?;
    Ok(Variant::Integer(status))
}

fn http_send_storage(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let storage = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    prepare_send(runtime, this_obj, &args, Some(storage))?;
    start_request(runtime, this_obj)?;
    Ok(Variant::Void)
}

fn http_send_storage_sync(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let storage = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    prepare_send(runtime, this_obj, &args, Some(storage))?;
    let status = run_request_now(runtime, this_obj)?;
    Ok(Variant::Integer(status))
}

/// The request definition handed to the worker (or the inline sync path).
struct Job {
    cert: bool,
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    canceled: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
}

fn take_job(instance: ObjectHandle) -> Result<Job> {
    REQUESTS.with(|map| {
        let mut map = map.borrow_mut();
        let state = map
            .get_mut(&instance)
            .ok_or_else(TjsError::native_class_crash)?;
        // The event queue is kept: an `open()` that queued OPEN before the
        // same script block calls `send()` still delivers it.
        state.canceled = Arc::new(AtomicBool::new(false));
        Ok::<_, TjsError>(Job {
            cert: state.cert,
            method: state.method.clone(),
            url: state.url.clone(),
            headers: state.headers.clone(),
            body: state.input.clone(),
            canceled: Arc::clone(&state.canceled),
            shared: Arc::clone(&state.shared),
        })
    })
}

/// `startThread` (`Main.cpp:647-651`).
fn start_request(runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Result<()> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    let job = take_job(instance)?;
    let timer = with_request(runtime, this_obj, |state| state.timer)?;
    let handle = std::thread::spawn(move || {
        let mut emit = |delivery: Delivery| push_delivery(&job.shared, delivery);
        let outcome = perform_worker(&job, &mut emit);
        publish_outcome(&job.shared, outcome);
    });
    REQUESTS.with(|map| {
        if let Some(state) = map.borrow_mut().get_mut(&instance) {
            state.worker = Some(handle);
        }
    });
    set_timer_enabled(runtime, timer, true);
    Ok(())
}

fn perform_worker(job: &Job, emit: &mut impl FnMut(Delivery)) -> Outcome {
    emit(Delivery::State(READYSTATE_SENT));
    if !job.body.is_empty() {
        // A sized body is sent as one blob, so the upload finishes before the
        // response (`_send` also reads the whole data up front).
        emit(Delivery::Progress {
            upload: true,
            percent: 100.0,
        });
    }
    if job.canceled.load(Ordering::SeqCst) {
        return Outcome {
            canceled: true,
            ..Outcome::default()
        };
    }
    let outcome = perform(
        job.cert,
        &job.method,
        &job.url,
        &job.headers,
        &job.body,
        &job.canceled,
        emit,
    );
    if job.canceled.load(Ordering::SeqCst) {
        let mut canceled = outcome;
        canceled.canceled = true;
        canceled.error = None;
        return canceled;
    }
    outcome
}

fn push_delivery(shared: &Arc<Mutex<Shared>>, delivery: Delivery) {
    if let Ok(mut shared) = shared.lock() {
        shared.deliveries.push(delivery);
    }
}

fn publish_outcome(shared: &Arc<Mutex<Shared>>, outcome: Outcome) {
    if let Ok(mut shared) = shared.lock() {
        shared.outcome = Some(Box::new(outcome));
    }
    push_delivery(shared, Delivery::Finished);
}

/// The synchronous path: the request runs inline and no events are posted
/// (`threadMain(false)`, `Main.cpp:587-637`).
fn run_request_now(runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Result<i64> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    let job = take_job(instance)?;
    let mut emit = |_delivery: Delivery| {};
    let outcome = perform(
        job.cert,
        &job.method,
        &job.url,
        &job.headers,
        &job.body,
        &job.canceled,
        &mut emit,
    );
    apply_outcome(runtime, instance, outcome)
}

/// Copies a worker outcome into the instance state, writing a `saveStorage`
/// body (`threadMain`, `Main.cpp:622-636`).
fn apply_outcome(
    runtime: &mut Runtime<KrkrHost>,
    instance: ObjectHandle,
    mut outcome: Outcome,
) -> Result<i64> {
    let save_storage = REQUESTS.with(|map| {
        let map = map.borrow();
        map.get(&instance)
            .and_then(|state| state.save_storage.clone())
    });

    if outcome.sniff_meta {
        sniff_outcome_meta(&mut outcome);
    }

    if let Some(target) = save_storage {
        if outcome.error.is_none() && !outcome.canceled && outcome.status_code == 200 {
            let _ = runtime
                .host_mut()
                .write_binary_storage(&target, "", &outcome.body);
        }
        outcome.body.clear();
    } else {
        let body = std::mem::take(&mut outcome.body);
        REQUESTS.with(|map| {
            if let Some(state) = map.borrow_mut().get_mut(&instance) {
                state.output = body;
            }
        });
    }

    let (status, status_text) = if outcome.canceled {
        (-1, "aborted".to_string())
    } else if let Some(error) = &outcome.error {
        (0, error.clone())
    } else {
        (outcome.status_code, outcome.status_text.clone())
    };

    REQUESTS.with(|map| {
        if let Some(state) = map.borrow_mut().get_mut(&instance) {
            state.status_code = status;
            state.status_text = status_text;
            state.save_storage = None;
            // `HttpConnection::response` closes its handles when the response
            // is done (`HttpConnection.cpp:489`), so `isValid()` is false
            // afterwards and a second `send()` without `open()` throws
            // "not open" (`Main.cpp:114-116`, `:396-400`).
            state.opened = false;
            if outcome.error.is_none() && !outcome.canceled {
                state.content_type = outcome.content_type.clone();
                state.encoding = outcome.encoding.clone();
                state.content_length = outcome.content_length;
                state.valid_content_length = outcome.valid_content_length;
                state.response_headers = outcome.headers.clone();
            }
        }
    });
    Ok(status)
}

fn sniff_outcome_meta(outcome: &mut Outcome) {
    let head_len = outcome.body.len().min(CHUNK_SIZE);
    let head: String = String::from_utf8_lossy(&outcome.body[..head_len]).into_owned();
    if let Some(value) = sniff_meta_content_type(&head) {
        let (content_type, encoding) = parse_content_type(&value);
        if !content_type.is_empty() {
            outcome.content_type = content_type;
        }
        if !encoding.is_empty() {
            outcome.encoding = encoding;
        }
    }
}

/// `abort` (`Main.cpp:241-245`).
fn http_abort(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    let timer = REQUESTS.with(|map| {
        let mut map = map.borrow_mut();
        let state = map
            .get_mut(&instance)
            .ok_or_else(TjsError::native_class_crash)?;
        state.stop();
        if let Ok(mut shared) = state.shared.lock() {
            shared.deliveries.clear();
        }
        state.input.clear();
        state.output.clear();
        state.save_storage = None;
        Ok::<_, TjsError>(state.timer)
    })?;
    set_timer_enabled(runtime, timer, false);
    Ok(Variant::Void)
}

/// `getAllResponseHeaders` (`Main.cpp:251-263`).
fn http_all_response_headers(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let headers = with_request(runtime, this_obj, |state| state.response_headers.clone())?;
    let dictionary = runtime.alloc_dictionary_object();
    for (name, value) in headers {
        runtime.set_object_member(dictionary, name, Variant::String(value));
    }
    Ok(Variant::Object(dictionary))
}

/// `getResponseHeader` (`Main.cpp:270-272`); the reference compares names
/// exactly against the server's spelling, this port searches the canonical
/// (lower-case) names case-insensitively.
fn http_response_header(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let name = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let value = with_request(runtime, this_obj, |state| {
        state
            .response_headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(&name))
            .map(|(_, value)| value.clone())
    })?;
    Ok(match value {
        Some(value) => Variant::String(value),
        None => Variant::Void,
    })
}

/// `getResponseText` (`Main.cpp:310-315`).
fn http_response_text(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let encoding = match args.first() {
        Some(value) if !matches!(value, Variant::Void) => Some(value.to_tjs_string()?),
        _ => None,
    };
    let text = with_request(runtime, this_obj, |state| {
        let encoding = encoding.as_deref().unwrap_or(&state.encoding);
        decode_text(&state.output, encoding)
    })?;
    Ok(Variant::String(text))
}

fn decode_text(bytes: &[u8], encoding: &str) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let (text, _, _) = encoding_for_name(encoding).decode(bytes);
    text.into_owned()
}

/// The pump: drain the worker's records in order and fire the handlers.
fn http_tick(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(instance) = instance_handle(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let Some((deliveries, timer)) = REQUESTS.with(|map| {
        let map = map.borrow();
        let state = map.get(&instance)?;
        let deliveries = state
            .shared
            .lock()
            .map(|mut shared| std::mem::take(&mut shared.deliveries))
            .unwrap_or_default();
        Some((deliveries, state.timer))
    }) else {
        return Ok(Variant::Void);
    };
    let mut finished = false;
    for delivery in deliveries {
        match delivery {
            Delivery::State(value) => {
                if value == READYSTATE_LOADED {
                    // `onReadyStateChange` stops the thread for LOADED
                    // (`Main.cpp:406-414`).
                    REQUESTS.with(|map| {
                        if let Some(state) = map.borrow_mut().get_mut(&instance) {
                            state.stop();
                            state.ready_state = value;
                        }
                    });
                    finished = true;
                } else {
                    REQUESTS.with(|map| {
                        if let Some(state) = map.borrow_mut().get_mut(&instance) {
                            state.ready_state = value;
                        }
                    });
                }
                deliver_event(
                    runtime,
                    instance,
                    "onReadyStateChange",
                    vec![Variant::Integer(value)],
                )?;
            }
            Delivery::Progress { upload, percent } => {
                deliver_event(
                    runtime,
                    instance,
                    "onProgress",
                    vec![Variant::Integer(i64::from(upload)), Variant::Real(percent)],
                )?;
            }
            Delivery::Finished => {
                let outcome = REQUESTS.with(|map| {
                    let mut map = map.borrow_mut();
                    let state = map.get_mut(&instance)?;
                    state
                        .shared
                        .lock()
                        .ok()
                        .and_then(|mut shared| shared.outcome.take())
                        .map(|outcome| *outcome)
                });
                if let Some(outcome) = outcome {
                    apply_outcome(runtime, instance, outcome)?;
                }
                // `threadMain` posts LOADED once the status and body are in
                // place (`Main.cpp:636`); `onReadyStateChange` then stops the
                // thread (`:406-414`).
                REQUESTS.with(|map| {
                    if let Some(state) = map.borrow_mut().get_mut(&instance) {
                        state.stop();
                        state.ready_state = READYSTATE_LOADED;
                    }
                });
                finished = true;
                deliver_event(
                    runtime,
                    instance,
                    "onReadyStateChange",
                    vec![Variant::Integer(READYSTATE_LOADED)],
                )?;
            }
        }
    }
    if finished {
        set_timer_enabled(runtime, timer, false);
    }
    let idle = REQUESTS.with(|map| {
        map.borrow()
            .get(&instance)
            .map(|state| state.worker.is_none())
            .unwrap_or(true)
    });
    if idle {
        set_timer_enabled(runtime, timer, false);
    }
    Ok(Variant::Void)
}

/// Queues one state record and wakes the pump.
fn queue_state(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle, value: i64) {
    let timer = REQUESTS
        .with(|map| {
            let mut map = map.borrow_mut();
            let state = map.get_mut(&instance)?;
            if let Ok(mut shared) = state.shared.lock() {
                shared.deliveries.push(Delivery::State(value));
            }
            Some(state.timer)
        })
        .flatten();
    set_timer_enabled(runtime, timer, true);
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use krkr_core::{FrameInput, Size};
    use krkr_engine::{EngineConfig, EngineInput, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::HttpRequestPlugin;

    /// A minimal local HTTP server speaking one canned response per
    /// connection; requests are recorded for assertions.
    struct TestServer {
        port: u16,
        shutdown: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<String>>>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn start(response: impl Fn(&str) -> String + Send + Sync + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = listener.local_addr().expect("addr").port();
            listener.set_nonblocking(true).expect("nonblocking");
            let shutdown = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_shutdown = Arc::clone(&shutdown);
            let thread_requests = Arc::clone(&requests);
            let handle = std::thread::spawn(move || {
                while !thread_shutdown.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let request = read_request(&stream);
                            let response = response(&request);
                            thread_requests.lock().expect("requests").push(request);
                            let _ = write_response(&stream, &response);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                port,
                shutdown,
                requests,
                handle: Some(handle),
            }
        }

        fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().expect("requests").clone()
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn read_request(stream: &TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .expect("timeout");
        let mut data = Vec::new();
        let mut buffer = [0u8; 1024];
        let mut reader = stream;
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    data.extend_from_slice(&buffer[..read]);
                    if let Some(headers_end) = find_headers_end(&data) {
                        let head = String::from_utf8_lossy(&data[..headers_end]).into_owned();
                        let length = content_length(&head);
                        if data.len() >= headers_end + length {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    fn find_headers_end(data: &[u8]) -> Option<usize> {
        data.windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4)
    }

    fn content_length(head: &str) -> usize {
        head.lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0)
    }

    fn write_response(stream: &TcpStream, response: &str) -> std::io::Result<()> {
        let mut writer = stream;
        writer.write_all(response.as_bytes())?;
        writer.flush()
    }

    fn ok_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Length: {}\r\n\
             X-Kira: probe\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn not_found_response() -> String {
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/html\r\nContent-Length: 9\r\n\
         Connection: close\r\n\r\nnot found"
            .to_string()
    }

    fn test_engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(HttpRequestPlugin).expect("plugin");
        engine
    }

    fn tick(engine: &mut KrkrEngine, millis: u64) {
        // The engine's native clock is wall time, so a timer needs real
        // milliseconds to pass between frames.
        std::thread::sleep(Duration::from_millis(2));
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::from_millis(millis),
            )
            .expect("update");
    }

    /// Ticks until `expression` answers `done`, or the budget runs out.
    fn tick_until(engine: &mut KrkrEngine, expression: &str, done: i64) -> bool {
        for _ in 0..400 {
            let value = engine
                .execute_expression("inline.tjs", expression)
                .expect("probe");
            if value == Variant::Integer(done) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
            tick(engine, 4);
        }
        false
    }

    #[test]
    fn the_surface_matches_the_reference() {
        let mut engine = test_engine();
        for member in [
            "open",
            "setRequestHeader",
            "send",
            "sendSync",
            "sendStorage",
            "sendStorageSync",
            "abort",
            "getAllResponseHeaders",
            "getResponseHeader",
            "getResponseText",
            "readyState",
            "response",
            "responseData",
            "status",
            "statusText",
            "contentType",
            "contentTypeEncoding",
            "contentLength",
        ] {
            assert_ne!(
                engine
                    .execute_expression("inline.tjs", &format!("typeof HttpRequest.{member}"))
                    .expect("member probe"),
                Variant::String("undefined".to_string()),
                "HttpRequest.{member} is not installed"
            );
        }
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "HttpRequest.UNINITIALIZED + ':' + HttpRequest.OPEN + ':' + \
                     HttpRequest.SENT + ':' + HttpRequest.RECEIVING + ':' + HttpRequest.LOADED"
                )
                .expect("constants"),
            Variant::String("0:1:2:3:4".to_string())
        );
    }

    #[test]
    fn the_constructor_requires_a_window() {
        let mut engine = test_engine();
        let error = engine
            .execute_expression("inline.tjs", "new HttpRequest(5)")
            .expect_err("the first argument must be a Window");
        assert!(
            error.message.contains("InvalidObject"),
            "unexpected message: {}",
            error.message
        );
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "(function() { var r = new HttpRequest(new Window()); return r.readyState; })()"
                )
                .expect("window constructor"),
            Variant::Integer(0)
        );
    }

    #[test]
    fn open_validates_the_url_like_the_reference() {
        let mut engine = test_engine();
        let error = engine
            .execute_script(
                "inline.tjs",
                "var r = new HttpRequest(new Window());\n\
                 return r.open(\"GET\", \"ftp://example.com/x\");",
            )
            .expect_err("a non-HTTP scheme is invalid");
        assert!(
            error.message.contains("invalid protocol"),
            "unexpected message: {}",
            error.message
        );
        // A valid open flips readyState to OPEN and posts the event.
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                global.r = new HttpRequest(new Window());
                r.onReadyStateChange = function(s) { global.events += "R" + s + ";"; };
                r.open("GET", "http://127.0.0.1:1/");
                "#,
            )
            .expect("open");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "global.r.readyState")
                .expect("readyState"),
            Variant::Integer(1)
        );
        assert!(
            tick_until(&mut engine, "global.events.length", 3),
            "the OPEN event never arrived: {}",
            engine
                .execute_expression("inline.tjs", "global.events")
                .expect("events")
        );
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "global.events")
                .expect("events"),
            Variant::String("R1;".to_string())
        );
    }

    #[test]
    fn a_successful_synchronous_request_fills_the_response_properties() {
        let server = TestServer::start(|_| ok_response("hello kira"));
        let mut engine = test_engine();
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("GET", "{}");
                    var status = r.sendSync();
                    return status + "|" + r.statusText + "|" + r.response + "|" +
                        r.contentType + "|" + r.contentTypeEncoding + "|" +
                        r.contentLength + "|" + r.getResponseHeader("X-Kira") + "|" +
                        r.readyState;
                    "#,
                    server.url("/hello")
                ),
            )
            .expect("sync request");
        assert_eq!(
            value,
            Variant::String("200|OK|hello kira|text/plain|UTF-8|10|probe|1".to_string()),
            "the sync path leaves readyState at OPEN like the reference's threadMain(false)"
        );
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /hello HTTP/1.1"));
        assert!(
            requests[0].to_lowercase().contains("user-agent: kirikiri"),
            "the agent name is the reference's constant: {}",
            requests[0]
        );
    }

    #[test]
    fn a_not_found_response_reports_its_status_without_a_body() {
        let server = TestServer::start(|_| not_found_response());
        let mut engine = test_engine();
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("GET", "{}");
                    var status = r.sendSync();
                    return status + "|" + r.statusText + "|" + (typeof r.response) + "|" +
                        (typeof r.responseData) + "|" + r.contentType + "|" + r.contentLength;
                    "#,
                    server.url("/missing")
                ),
            )
            .expect("404 request");
        assert_eq!(
            value,
            Variant::String("404|Not Found|String|void|text/html|9".to_string()),
            "the reference only reads the body for status 200; a text/* response \
             still answers the (empty) decoded text while octet data stays void"
        );
    }

    #[test]
    fn a_connection_failure_reports_status_zero_and_the_message() {
        // Bind, learn the port, then drop the listener: the port is closed.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let mut engine = test_engine();
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("GET", "http://127.0.0.1:{port}/down");
                    var status = r.sendSync();
                    return status + "|" + (r.statusText.length > 0) + "|" + (typeof r.response);
                    "#
                ),
            )
            .expect("failing request");
        assert_eq!(
            value,
            Variant::String("0|1|void".to_string()),
            "a transport failure answers status 0 with the error as statusText"
        );
    }

    #[test]
    fn the_async_path_delivers_ready_state_and_progress_in_order() {
        let server = TestServer::start(|_| ok_response("payload"));
        let mut engine = test_engine();
        engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    global.events = "";
                    global.r = new HttpRequest(new Window());
                    r.onReadyStateChange = function(s) {{ global.events += "R" + s + ";"; }};
                    r.onProgress = function(upload, percent) {{ global.events += "P" + (upload ? "u" : "d") + Math.round(percent) + ";"; }};
                    r.open("GET", "{}");
                    r.send();
                    "#,
                    server.url("/async")
                ),
            )
            .expect("async request");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "global.r.readyState")
                .expect("readyState right after send"),
            Variant::Integer(1),
            "the reference's readyState only moves when the events are delivered"
        );
        assert!(
            tick_until(&mut engine, "global.r.readyState", 4),
            "the request never reached LOADED"
        );
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                return global.events + "|" + r.status + "|" + r.responseData.length;
                "#,
            )
            .expect("probe");
        assert_eq!(
            value,
            Variant::String("R1;R2;R3;Pd100;R4;|200|7".to_string()),
            "OPEN at open(), then SENT/RECEIVING/LOADED from the worker with the \
             download progress the reference posts per chunk"
        );
    }

    #[test]
    fn a_post_carries_its_body_headers_and_content_length() {
        let server = TestServer::start(|_| ok_response("done"));
        let mut engine = test_engine();
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("POST", "{}");
                    r.setRequestHeader("Content-Type", "application/x-www-form-urlencoded");
                    r.setRequestHeader("X-Probe", "42");
                    var status = r.sendSync("name=kira");
                    return status;
                    "#,
                    server.url("/post")
                ),
            )
            .expect("post request");
        assert_eq!(value, Variant::Integer(200));
        let requests = server.requests();
        let request = &requests[0];
        assert!(request.starts_with("POST /post HTTP/1.1"), "{request}");
        assert!(
            request
                .to_lowercase()
                .contains("content-type: application/x-www-form-urlencoded"),
            "{request}"
        );
        assert!(request.to_lowercase().contains("x-probe: 42"), "{request}");
        assert!(
            request.to_lowercase().contains("content-length: 9"),
            "{request}"
        );
        assert!(request.ends_with("name=kira"), "{request}");
    }

    #[test]
    fn basic_auth_uses_the_reference_header_shape() {
        let server = TestServer::start(|_| ok_response("ok"));
        let mut engine = test_engine();
        engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("GET", "{}", "user", "pass");
                    r.sendSync();
                    "#,
                    server.url("/auth")
                ),
            )
            .expect("auth request");
        let requests = server.requests();
        assert!(
            requests[0]
                .to_lowercase()
                .contains("authorization: basic dxnlcjpwyxnz"),
            "unexpected request: {}",
            requests[0]
        );
    }

    #[test]
    fn send_storage_reads_the_body_from_the_storage_layer() {
        let root = std::env::temp_dir().join(format!(
            "kirakira-httprequest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(root.join("body.txt"), b"from storage").expect("body");
        let storage = krkr_assets::ProjectStorage::for_root(&root).expect("storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(HttpRequestPlugin).expect("plugin");
        let server = TestServer::start(|_| ok_response("stored"));
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("POST", "{}");
                    var status = r.sendStorageSync("body.txt");
                    return status;
                    "#,
                    server.url("/storage")
                ),
            )
            .expect("sendStorageSync");
        assert_eq!(value, Variant::Integer(200));
        let requests = server.requests();
        assert!(requests[0].ends_with("from storage"), "{}", requests[0]);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn save_storage_receives_the_body_and_keeps_response_empty() {
        let root = std::env::temp_dir().join(format!(
            "kirakira-httprequest-save-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("root");
        let storage = krkr_assets::ProjectStorage::for_root(&root).expect("storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(HttpRequestPlugin).expect("plugin");
        let server = TestServer::start(|_| ok_response("saved body"));
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("GET", "{}");
                    var status = r.sendSync(void, "out.bin");
                    return status + "|" + (typeof r.response);
                    "#,
                    server.url("/save")
                ),
            )
            .expect("saveStorage");
        assert_eq!(
            value,
            Variant::String("200|String".to_string()),
            "a saved body leaves `response` as the empty decoded text of a \
             text/* content type, like the reference's `getResponse`"
        );
        assert_eq!(
            std::fs::read(root.join("out.bin")).expect("saved file"),
            b"saved body"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn the_meta_tag_supplies_the_encoding_of_html_without_a_charset() {
        let server = TestServer::start(|_| {
            let body = "<html><head><meta http-equiv=\"content-type\" \
                        content=\"text/html; charset=Shift_JIS\"></head><body>x</body></html>";
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            )
        });
        let mut engine = test_engine();
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("GET", "{}");
                    r.sendSync();
                    return r.contentType + "|" + r.contentTypeEncoding;
                    "#,
                    server.url("/meta")
                ),
            )
            .expect("meta sniff");
        assert_eq!(value, Variant::String("text/html|Shift_JIS".to_string()));
    }

    #[test]
    fn abort_stops_a_loading_request() {
        let server = TestServer::start(|_| ok_response("slow"));
        let mut engine = test_engine();
        engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    global.r = new HttpRequest(new Window());
                    r.open("GET", "{}");
                    r.send();
                    r.abort();
                    "#,
                    server.url("/abort")
                ),
            )
            .expect("abort");
        // The worker is joined by abort; a later tick delivers nothing new.
        tick(&mut engine, 5);
        let value = engine
            .execute_script(
                "inline.tjs",
                "return (typeof global.r.responseData) + ':' + global.r.readyState;",
            )
            .expect("probe");
        assert_eq!(
            value,
            Variant::String("void:1".to_string()),
            "abort clears the body and joins the worker before the loaded state"
        );
    }

    #[test]
    fn send_without_open_reports_the_reference_message() {
        let mut engine = test_engine();
        let error = engine
            .execute_script(
                "inline.tjs",
                "var r = new HttpRequest(new Window());\nreturn r.send();",
            )
            .expect_err("send requires open");
        assert!(
            error.message.contains("not open"),
            "unexpected message: {}",
            error.message
        );
    }

    #[test]
    fn a_second_send_while_running_reports_already_running() {
        let server = TestServer::start(|_| ok_response("x"));
        let mut engine = test_engine();
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("GET", "{}");
                    r.send();
                    var message = "";
                    try {{
                        r.send();
                    }} catch (e) {{
                        message = e.message;
                    }}
                    return message;
                    "#,
                    server.url("/twice")
                ),
            )
            .expect("second send");
        assert_eq!(value, Variant::String("already running".to_string()));
    }

    #[test]
    fn a_completed_request_is_closed_until_the_next_open() {
        let server = TestServer::start(|_| ok_response("once"));
        let mut engine = test_engine();
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("GET", "{}");
                    var first = r.sendSync();
                    // `HttpConnection::response` closes its handles when the
                    // response is done (`HttpConnection.cpp:489`), so the
                    // reference refuses a second send (`Main.cpp:114-116`).
                    var syncAgain = "";
                    try {{ r.sendSync(); }} catch (e) {{ syncAgain = e.message; }}
                    var asyncAgain = "";
                    try {{ r.send(); }} catch (e) {{ asyncAgain = e.message; }}
                    r.open("GET", "{}");
                    var third = r.sendSync();
                    return first + ":" + syncAgain + ":" + asyncAgain + ":" + third;
                    "#,
                    server.url("/once"),
                    server.url("/twice")
                ),
            )
            .expect("completed request");
        assert_eq!(
            value,
            Variant::String("200:not open:not open:200".to_string()),
            "a finished request has to be opened again, like the reference's closed handles"
        );
        assert_eq!(server.requests().len(), 2, "no request was re-issued");
    }

    #[test]
    fn multibyte_content_types_never_panic() {
        // The meta-sniff path (server-controlled): the 7-byte `charset` probe
        // used to slice inside a UTF-8 character of the parameter tail.
        let server = TestServer::start(|_| {
            let body = "<html><head><meta http-equiv=\"content-type\" \
                        content=\"text/html; あああ\"></head></html>";
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            )
        });
        let mut engine = test_engine();
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("GET", "{}");
                    r.sendSync();
                    return r.status + ":" + r.contentType + ":" + (r.contentTypeEncoding === "");
                    "#,
                    server.url("/multibyte-tail")
                ),
            )
            .expect("multibyte charset tail");
        assert_eq!(value, Variant::String("200:text/html:1".to_string()));

        // A multibyte content type, which the `response` property's 5-byte
        // `text/` probe then has to survive.
        let server = TestServer::start(|_| {
            let body = "<meta http-equiv=\"content-type\" content=\"ああ\">";
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            )
        });
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("GET", "{}");
                    r.sendSync();
                    var read = typeof r.response;
                    return r.status + ":" + r.contentType + ":" + (read != "undefined");
                    "#,
                    server.url("/multibyte-content-type")
                ),
            )
            .expect("multibyte content type");
        assert_eq!(value, Variant::String("200:ああ:1".to_string()));

        // A script's multibyte Content-Type: the parse must not panic, and the
        // header is passed through like the reference hands it to WinINet.
        let server = TestServer::start(|_| ok_response("x"));
        let value = engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    var r = new HttpRequest(new Window());
                    r.open("GET", "{}");
                    r.setRequestHeader("Content-Type", "text/x; あああ");
                    var status = r.sendSync();
                    return status + ":" + (r.statusText.length > 0);
                    "#,
                    server.url("/multibyte-header")
                ),
            )
            .expect("multibyte header");
        assert_eq!(value, Variant::String("200:1".to_string()));
        assert!(
            server.requests()[0].contains("text/x; あああ"),
            "the multibyte header reached the server intact: {}",
            server.requests()[0]
        );
    }
}
