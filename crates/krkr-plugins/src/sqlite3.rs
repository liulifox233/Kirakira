//! `sqlite3.dll` — SQLite database access from scripts.
//!
//! Reference: krkr2 trunk `src/plugins/win32/sqlite3/` (`Main.cpp` 1052 lines
//! and `extend.cpp` 103, over the bundled SQLite 3.7.14.1 amalgamation);
//! `manual.tjs` is the contract this module follows. The plugin exposes three
//! global classes:
//!
//! - `Sqlite(database, readonly = false)` — one connection, `exec`,
//!   `execValue`, `begin`/`commit`/`rollback`, `lastInsertRowId`,
//!   `errorCode`, `errorMessage` and the `SQLITE_*` result codes.
//! - `SqliteStatement(sqlite, sql = void, params = void)` — a prepared
//!   statement: `open`/`close`/`reset`/`bind`/`bindAt`/`exec`/`step`,
//!   `sql`/`count`/`columnCount`, `isNull`/`getType`/`getName`/`get`, the
//!   `SQLITE_INTEGER..SQLITE_NULL` column types, and direct `stmt.columnName`
//!   reads.
//! - `SqliteThread(window, sqlite)` — background `select`/`update` with
//!   `onStateChange`/`onProgress` events and `state`/`errorCode`/
//!   `selectResult`/`progressUpdateCount`; `selectResult` is one array per
//!   row, each holding one entry per column (`Main.cpp:884-895`).
//!
//! # SQLite binding
//!
//! `libsqlite3-sys` with the `bundled` feature compiles the official
//! amalgamation (3.x — this build is 3.53.2 against the reference's
//! 3.7.14.1; major-version file format, SQL semantics and error codes are
//! what the scripts see) through the host C compiler, exactly like the
//! reference's `sqlite3.c`. The plugin uses the raw C API because the
//! reference's statement model keeps a prepared `sqlite3_stmt` alive across
//! script calls, which the safe wrappers cannot express without
//! self-referential lifetimes. The reference calls SQLite's UTF-16 entry
//! points because `tjs_char` is UTF-16; Rust strings are UTF-8, so this port
//! uses the UTF-8 entry points — the same text in the same database encoding
//! (SQLite stores text as UTF-8 internally either way).
//!
//! # Deliberate divergences
//!
//! - `readonly = true` opens through the engine storage layer like the
//!   reference's `xp3` VFS: a name that resolves to a real file is opened
//!   read-only in place; an archive/memory member is read whole and opened
//!   read-only from a temporary copy that is deleted with the connection.
//!   The copy is created exclusively (`create_new`) under a per-open random
//!   name with owner-only permissions, so a pre-existing path is never
//!   written through. The reference reads the member through
//!   `TVPCreateIStream` directly; both refuse writes with `SQLITE_READONLY`
//!   and see the same bytes.
//! - `readonly = false` needs a locally accessible file, like the reference:
//!   an existing local name resolves through the storage layer, a new plain
//!   name resolves under the engine's executable path (the reference's
//!   `TVPGetLocallyAccessibleName`), and a name only an archive provides
//!   throws the reference's `Unable to open the database file, try readonly
//!   if exists: %1`.
//! - The engine has no message pump for plugins, so `SqliteThread`'s
//!   `TVPPostEvent`s are delivered on engine ticks by a hidden `Timer`
//!   instance (interval 1 ms, enabled only while an operation is in flight):
//!   the worker thread records state/progress/rows, and the pump drains them
//!   in order on the script thread, calling `onStateChange`/`onProgress`
//!   exactly like the reference's window messages would.
//! - The reference's `SqliteStatement` direct-column reads go through the
//!   class's `missing` handler, which only answers names the class chain
//!   misses. This engine's missing handler cannot write the value property it
//!   receives, so the plugin instead registers one native read-only property
//!   per result column when a statement is opened (plus a lowercase alias
//!   when the column has upper-case letters — the reference resolves column
//!   names case-insensitively), skipping any name that already resolves on
//!   the statement so a column called `count`/`step`/`sql` cannot shadow the
//!   class member. The values are live reads of the current row, so
//!   `stmt.columnName` behaves the same.
//! - `progressUpdateCount` is sampled when an operation starts; the reference
//!   re-reads the member at each progress milestone.
//! - `errorCode`/`errorMessage` report the connection's last error through
//!   `sqlite3_errcode`/`sqlite3_errmsg`; when `sqlite3_open` itself fails the
//!   handle SQLite returned is kept, so the codes match the reference.

// `result_large_err` is the crate-wide `TjsError` size lint every native
// callback carries, silenced here the same way `wf_basic_effect.rs` and
// `wf_typical_dsp.rs` silence it.
#![allow(clippy::result_large_err)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::io::Write;
use std::os::raw::{c_char, c_int};
use std::path::PathBuf;
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use libsqlite3_sys as ffi;

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Sqlite / SqliteStatement / SqliteThread classes, cnt/ncnt SQL functions",
    notes: "SQLite 3.x through the bundled C amalgamation. Connections, prepared statements \
            (including direct column reads) and background select/update with \
            onStateChange/onProgress work; readonly opens read through the engine storage layer, \
            an archive-hosted database is served from a temporary read-only copy, and thread \
            events are delivered on engine ticks by a hidden timer.",
    install: |engine| engine.register_plugin(Sqlite3Plugin),
};

pub struct Sqlite3Plugin;

impl KrkrPlugin for Sqlite3Plugin {
    fn name(&self) -> &str {
        "sqlite3.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_sqlite_class(runtime);
        install_statement_class(runtime);
        install_thread_class(runtime);
        runtime
            .host_mut()
            .log("sqlite3.dll registered: Sqlite / SqliteStatement / SqliteThread classes");
        Ok(())
    }
}

/// The constants the reference sets on `Sqlite` (`Main.cpp:338-367`) and
/// `SqliteStatement` (`Main.cpp:634-639`); SQLite's own values.
const SQLITE_RESULT_CODES: &[(&str, i64)] = &[
    ("SQLITE_OK", 0),
    ("SQLITE_ERROR", 1),
    ("SQLITE_INTERNAL", 2),
    ("SQLITE_PERM", 3),
    ("SQLITE_ABORT", 4),
    ("SQLITE_BUSY", 5),
    ("SQLITE_LOCKED", 6),
    ("SQLITE_NOMEM", 7),
    ("SQLITE_READONLY", 8),
    ("SQLITE_INTERRUPT", 9),
    ("SQLITE_IOERR", 10),
    ("SQLITE_CORRUPT", 11),
    ("SQLITE_NOTFOUND", 12),
    ("SQLITE_FULL", 13),
    ("SQLITE_CANTOPEN", 14),
    ("SQLITE_PROTOCOL", 15),
    ("SQLITE_EMPTY", 16),
    ("SQLITE_SCHEMA", 17),
    ("SQLITE_TOOBIG", 18),
    ("SQLITE_CONSTRAINT", 19),
    ("SQLITE_MISMATCH", 20),
    ("SQLITE_MISUSE", 21),
    ("SQLITE_NOLFS", 22),
    ("SQLITE_AUTH", 23),
    ("SQLITE_FORMAT", 24),
    ("SQLITE_RANGE", 25),
    ("SQLITE_NOTADB", 26),
    ("SQLITE_ROW", 100),
    ("SQLITE_DONE", 101),
];

const SQLITE_INTEGER: i64 = 1;
const SQLITE_FLOAT: i64 = 2;
const SQLITE_TEXT: i64 = 3;
const SQLITE_BLOB: i64 = 4;
const SQLITE_NULL: i64 = 5;

/// `SqliteThread::State` (`Main.cpp:667-671`).
const THREAD_INIT: i64 = 0;
const THREAD_WORKING: i64 = 1;
const THREAD_DONE: i64 = 2;

/// `PROGRESS_COUNT` (`Main.cpp:12`).
const DEFAULT_PROGRESS_COUNT: i64 = 100;

// ---------------------------------------------------------------------------
// Raw connection / statement plumbing
// ---------------------------------------------------------------------------

/// `sqlite3*` handed to the `SqliteThread` worker as well as used on the
/// script thread.
///
/// SAFETY: SQLite is compiled in its default serialized threading mode
/// (`SQLITE_THREADSAFE=1`), so one connection may be used from several threads
/// at once; the reference shares a single `sqlite3*` between the script thread
/// and its `_beginthreadex` worker in exactly the same way.
#[derive(Clone, Copy)]
struct Db(*mut ffi::sqlite3);

unsafe impl Send for Db {}

/// A prepared statement pointer handed to a worker thread.
#[derive(Clone, Copy)]
struct Stmt(*mut ffi::sqlite3_stmt);

unsafe impl Send for Stmt {}

/// One open database connection, kept alive by every statement and worker that
/// still points at it (the reference closes it in `~Sqlite` and lets its
/// statements dangle; keeping the handle alive is strictly safer and invisible
/// to scripts).
struct Connection {
    db: Db,
    /// The temporary copy a readonly archive/memory database was materialized
    /// into, removed with the connection.
    temp_path: Option<PathBuf>,
}

impl Connection {
    fn ptr(&self) -> *mut ffi::sqlite3 {
        self.db.0
    }

    /// `db ? sqlite3_errcode(db) : -1` (`Main.cpp:308-310`); a handle SQLite
    /// never produced answers -1 and `errorMessage` its `database open
    /// failed` (`:312-314`).
    fn error_code(&self) -> i64 {
        if self.db.0.is_null() {
            return -1;
        }
        i64::from(unsafe { ffi::sqlite3_errcode(self.db.0) })
    }

    fn error_message(&self) -> String {
        if self.db.0.is_null() {
            return "database open failed".to_string();
        }
        let message = unsafe { ffi::sqlite3_errmsg(self.db.0) };
        if message.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    }

    fn exec(&self, sql: &str) -> bool {
        if self.db.0.is_null() {
            return false;
        }
        let Ok(sql) = std::ffi::CString::new(sql) else {
            return false;
        };
        unsafe {
            ffi::sqlite3_exec(
                self.db.0,
                sql.as_ptr(),
                None,
                ptr::null_mut(),
                ptr::null_mut(),
            ) == ffi::SQLITE_OK
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if !self.db.0.is_null() {
            unsafe {
                ffi::sqlite3_close(self.db.0);
            }
        }
        if let Some(path) = self.temp_path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// One `Sqlite` instance's state: the connection every statement shares. The
/// handle is single-threaded (`Rc`): only the raw `sqlite3*` travels to a
/// `SqliteThread` worker, not this value.
struct SqliteState {
    connection: Rc<Connection>,
}

impl SqliteState {
    fn error_code(&self) -> i64 {
        self.connection.error_code()
    }

    fn error_message(&self) -> String {
        self.connection.error_message()
    }
}

/// A parameter value snapshotted from a TJS variant (`bindParam`,
/// `Main.cpp:21-42`: Integer → int64, Real → double, String → text,
/// Octet → blob, everything else NULL).
#[derive(Clone, Debug, PartialEq)]
enum BindValue {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl BindValue {
    fn from_variant(value: &Variant) -> Result<Self> {
        Ok(match value {
            Variant::Integer(v) => Self::Int(*v),
            Variant::Real(v) => Self::Real(*v),
            Variant::String(v) => Self::Text(v.clone()),
            Variant::Octet(v) => Self::Blob(v.clone()),
            // Booleans are integers in TJS; closures/objects bind as NULL.
            _ => Self::Null,
        })
    }
}

/// Bindings snapshotted for the `SqliteThread` worker: an Array binds
/// positionally (`bindParams`, `Main.cpp:120-133`), any other object binds by
/// member name through `sqlite3_bind_parameter_index` (`Main.cpp:135-140`).
#[derive(Clone, Debug, PartialEq)]
enum BindSpec {
    Positional(Vec<BindValue>),
    Named(Vec<(String, BindValue)>),
}

/// A column value read from a statement (`getColumnData`, `Main.cpp:150-170`).
#[derive(Clone, Debug, PartialEq)]
enum ColumnValue {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl ColumnValue {
    fn to_variant(&self) -> Variant {
        match self {
            Self::Null => Variant::Void,
            Self::Int(v) => Variant::Integer(*v),
            Self::Real(v) => Variant::Real(*v),
            Self::Text(v) => Variant::String(v.clone()),
            Self::Blob(v) => Variant::Octet(v.clone()),
        }
    }
}

/// The state a `SqliteStatement` instance owns.
struct StatementState {
    connection: Rc<Connection>,
    stmt: *mut ffi::sqlite3_stmt,
    /// `bindPos` (`Main.cpp:630`), the position `bindAt` binds at next.
    bind_pos: c_int,
    /// Member names the current statement's column properties were registered
    /// under, removed when the statement is closed or reopened.
    column_members: Vec<String>,
}

impl Drop for StatementState {
    fn drop(&mut self) {
        if !self.stmt.is_null() {
            unsafe {
                ffi::sqlite3_finalize(self.stmt);
            }
        }
    }
}

/// One record the `SqliteThread` worker leaves for the script-thread pump.
enum Delivery {
    /// A result row, appended to `selectResult` on delivery.
    Row(Vec<ColumnValue>),
    /// `onProgress(n)`.
    Progress(i64),
    /// `onStateChange(state)`.
    State(i64),
}

/// State shared with the `SqliteThread` worker.
#[derive(Default)]
struct Shared {
    deliveries: Vec<Delivery>,
    /// The worker's final `errorCode`.
    final_error: i64,
    finished: bool,
}

struct ThreadState {
    #[allow(dead_code)]
    window: ObjectHandle,
    #[allow(dead_code)]
    sqlite: ObjectHandle,
    connection: Option<Rc<Connection>>,
    worker: Option<std::thread::JoinHandle<()>>,
    canceled: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    select_result: Variant,
    state: i64,
    error_code: i64,
    progress_update_count: i64,
    timer: Option<ObjectHandle>,
}

impl ThreadState {
    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.canceled.store(true, Ordering::SeqCst);
            let _ = worker.join();
        }
    }
}

impl Drop for ThreadState {
    fn drop(&mut self) {
        self.stop();
    }
}

thread_local! {
    static SQLITES: RefCell<BTreeMap<ObjectHandle, SqliteState>> =
        const { RefCell::new(BTreeMap::new()) };
    static STATEMENTS: RefCell<BTreeMap<ObjectHandle, StatementState>> =
        const { RefCell::new(BTreeMap::new()) };
    static THREADS: RefCell<BTreeMap<ObjectHandle, ThreadState>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// The object a native acts on; `None` when called on the class object or with
/// no receiver at all (the reference would crash on a null `self`).
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

/// `IsInstanceOf(0, 0, 0, name, obj)`: the name is looked up on the object and
/// every class above it (`Main.cpp:557`, `:764-769`).
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

fn sqlite_state(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Result<Rc<Connection>> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    SQLITES.with(|map| {
        map.borrow()
            .get(&instance)
            .map(|state| state.connection.clone())
            .ok_or_else(TjsError::native_class_crash)
    })
}

fn with_sqlite_state<R>(
    this_obj: Option<ObjectHandle>,
    runtime: &Runtime<KrkrHost>,
    action: impl FnOnce(&SqliteState) -> R,
) -> Result<R> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    SQLITES.with(|map| {
        map.borrow()
            .get(&instance)
            .map(action)
            .ok_or_else(TjsError::native_class_crash)
    })
}

// ---------------------------------------------------------------------------
// SQLite helpers
// ---------------------------------------------------------------------------

unsafe fn c_string(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

/// `sqlite3_prepare_v2` over a UTF-8 SQL string.
fn prepare(db: *mut ffi::sqlite3, sql: &str) -> (*mut ffi::sqlite3_stmt, c_int) {
    let Ok(sql_c) = std::ffi::CString::new(sql) else {
        return (ptr::null_mut(), ffi::SQLITE_ERROR);
    };
    let mut stmt: *mut ffi::sqlite3_stmt = ptr::null_mut();
    let rc = unsafe { ffi::sqlite3_prepare_v2(db, sql_c.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
    (stmt, rc)
}

fn bind_value(stmt: *mut ffi::sqlite3_stmt, pos: c_int, value: &BindValue) -> c_int {
    unsafe {
        match value {
            BindValue::Null => ffi::sqlite3_bind_null(stmt, pos),
            BindValue::Int(v) => ffi::sqlite3_bind_int64(stmt, pos, *v),
            BindValue::Real(v) => ffi::sqlite3_bind_double(stmt, pos, *v),
            BindValue::Text(text) => ffi::sqlite3_bind_text(
                stmt,
                pos,
                text.as_ptr().cast(),
                text.len() as c_int,
                ffi::SQLITE_TRANSIENT(),
            ),
            BindValue::Blob(bytes) => ffi::sqlite3_bind_blob(
                stmt,
                pos,
                bytes.as_ptr().cast(),
                bytes.len() as c_int,
                ffi::SQLITE_TRANSIENT(),
            ),
        }
    }
}

/// `getBindPos` (`Main.cpp:49-73`): a number is a 0-based script position
/// (`pos + 1` as the SQLite index) and a string is a named-parameter lookup.
fn bind_pos(stmt: *mut ffi::sqlite3_stmt, name: &Variant) -> c_int {
    match name {
        Variant::Integer(v) => (*v as c_int).saturating_add(1),
        Variant::Real(v) => (*v as c_int).saturating_add(1),
        Variant::String(text) => named_bind_pos(stmt, text),
        _ => 0,
    }
}

fn named_bind_pos(stmt: *mut ffi::sqlite3_stmt, name: &str) -> c_int {
    let Ok(name) = std::ffi::CString::new(name) else {
        return 0;
    };
    if stmt.is_null() {
        return 0;
    }
    unsafe { ffi::sqlite3_bind_parameter_index(stmt, name.as_ptr()) }
}

fn column_value(stmt: *mut ffi::sqlite3_stmt, index: c_int) -> ColumnValue {
    if stmt.is_null() || index < 0 {
        return ColumnValue::Null;
    }
    unsafe {
        match ffi::sqlite3_column_type(stmt, index) {
            ffi::SQLITE_INTEGER => ColumnValue::Int(ffi::sqlite3_column_int64(stmt, index)),
            ffi::SQLITE_FLOAT => ColumnValue::Real(ffi::sqlite3_column_double(stmt, index)),
            ffi::SQLITE_TEXT => {
                ColumnValue::Text(c_string(ffi::sqlite3_column_text(stmt, index).cast()))
            }
            ffi::SQLITE_BLOB => {
                let len = ffi::sqlite3_column_bytes(stmt, index);
                let data = ffi::sqlite3_column_blob(stmt, index).cast::<u8>();
                if data.is_null() || len <= 0 {
                    ColumnValue::Blob(Vec::new())
                } else {
                    ColumnValue::Blob(std::slice::from_raw_parts(data, len as usize).to_vec())
                }
            }
            _ => ColumnValue::Null,
        }
    }
}

fn column_name(stmt: *mut ffi::sqlite3_stmt, index: c_int) -> String {
    if stmt.is_null() || index < 0 {
        return String::new();
    }
    unsafe { c_string(ffi::sqlite3_column_name(stmt, index)) }
}

fn column_count(stmt: *mut ffi::sqlite3_stmt) -> c_int {
    if stmt.is_null() {
        return 0;
    }
    unsafe { ffi::sqlite3_column_count(stmt) }
}

/// Snapshots a parameter list for the worker thread. Arrays bind
/// positionally; every other object enumerates its members and binds each by
/// name (`bindParams`, `Main.cpp:116-142`).
fn snapshot_params(runtime: &Runtime<KrkrHost>, params: &Variant) -> Result<BindSpec> {
    let Some(handle) = params.object_handle() else {
        return Ok(BindSpec::Positional(Vec::new()));
    };
    if let Some(elements) = runtime.array_elements(handle) {
        let mut values = Vec::with_capacity(elements.len());
        for element in elements {
            values.push(BindValue::from_variant(element)?);
        }
        return Ok(BindSpec::Positional(values));
    }
    let mut named = Vec::new();
    for (name, value) in runtime.object_members(handle) {
        named.push((name, BindValue::from_variant(&value)?));
    }
    Ok(BindSpec::Named(named))
}

fn apply_bind_spec(stmt: *mut ffi::sqlite3_stmt, spec: &BindSpec) -> c_int {
    match spec {
        BindSpec::Positional(values) => {
            let mut rc = ffi::SQLITE_OK;
            for (index, value) in values.iter().enumerate() {
                rc = bind_value(stmt, (index + 1) as c_int, value);
                if rc != ffi::SQLITE_OK {
                    break;
                }
            }
            rc
        }
        BindSpec::Named(entries) => {
            let mut rc = ffi::SQLITE_OK;
            for (name, value) in entries {
                rc = bind_value(stmt, named_bind_pos(stmt, name), value);
                if rc != ffi::SQLITE_OK {
                    break;
                }
            }
            rc
        }
    }
}

/// Binds a TJS parameter list directly (`bindParams` on the script thread).
fn bind_params(
    runtime: &Runtime<KrkrHost>,
    stmt: *mut ffi::sqlite3_stmt,
    params: &Variant,
) -> Result<c_int> {
    // An Array instance binds positionally; everything else enumerates
    // members and binds each by the member name.
    if let Some(handle) = params.object_handle() {
        if let Some(elements) = runtime.array_elements(handle) {
            let mut rc = ffi::SQLITE_OK;
            for (index, element) in elements.iter().enumerate() {
                rc = bind_value(
                    stmt,
                    (index + 1) as c_int,
                    &BindValue::from_variant(element)?,
                );
                if rc != ffi::SQLITE_OK {
                    break;
                }
            }
            return Ok(rc);
        }
        let mut rc = ffi::SQLITE_OK;
        for (name, value) in runtime.object_members(handle) {
            rc = bind_value(
                stmt,
                named_bind_pos(stmt, &name),
                &BindValue::from_variant(&value)?,
            );
            if rc != ffi::SQLITE_OK {
                break;
            }
        }
        return Ok(rc);
    }
    Ok(ffi::SQLITE_OK)
}

/// The `exec()` step loop: with a callback object every row calls it with one
/// argument per column (`Main.cpp:238-258`).
fn exec_statement(
    runtime: &mut Runtime<KrkrHost>,
    stmt: *mut ffi::sqlite3_stmt,
    callback: Option<&Variant>,
) -> Result<c_int> {
    let count = column_count(stmt);
    loop {
        let rc = unsafe { ffi::sqlite3_step(stmt) };
        if rc != ffi::SQLITE_ROW {
            return Ok(rc);
        }
        if let Some(callback) = callback {
            let mut args = Vec::with_capacity(count.max(0) as usize);
            for index in 0..count {
                args.push(column_value(stmt, index).to_variant());
            }
            runtime.call_function(callback.clone(), args)?;
        }
    }
}

// ---------------------------------------------------------------------------
// `Sqlite` class
// ---------------------------------------------------------------------------

fn install_sqlite_class(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let Some(database) = args.first() else {
                return Err(TjsError::bad_param_count());
            };
            let database = database.to_tjs_string()?;
            let readonly = match args.get(1) {
                Some(value) => value.to_integer()? != 0,
                None => false,
            };
            let instance = bound_instance(runtime, this_obj, "Sqlite");
            install_sqlite_members(runtime, instance);
            let connection = open_connection(runtime, &database, readonly)?;
            SQLITES.with(|map| {
                map.borrow_mut()
                    .insert(instance, SqliteState { connection });
            });
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "Sqlite");
    install_sqlite_members(runtime, class);
    for (name, value) in SQLITE_RESULT_CODES {
        runtime.set_object_member(class, *name, Variant::Integer(*value));
    }
    runtime.register_object_native(class, "finalize", sqlite_finalize);
    runtime.set_global_member("Sqlite", Variant::Object(class));
}

fn install_sqlite_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native_with_arg_count(
        handle,
        "exec",
        NativeArgCount::AtLeast(1),
        sqlite_exec,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "execValue",
        NativeArgCount::AtLeast(1),
        sqlite_exec_value,
    );
    runtime.register_object_native(handle, "begin", sqlite_begin);
    runtime.register_object_native(handle, "commit", sqlite_commit);
    runtime.register_object_native(handle, "rollback", sqlite_rollback);
    runtime.register_object_native(handle, "finalize", sqlite_finalize);
    runtime.register_object_native_property_with_access(
        handle,
        "lastInsertRowId",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            sqlite_property(runtime, this_obj, |state| {
                Variant::Integer(last_insert_row_id(state))
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "errorCode",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            sqlite_property(runtime, this_obj, |state| {
                Variant::Integer(state.error_code())
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "errorMessage",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            sqlite_property(runtime, this_obj, |state| {
                Variant::String(state.error_message())
            })
        },
        deny_write,
    );
}

/// A property read tolerant of a class-object receiver: the class object is
/// not an instance, so its read-only properties answer void.
fn sqlite_property(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    action: impl FnOnce(&SqliteState) -> Variant,
) -> Result<Variant> {
    let Some(instance) = instance_handle(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    SQLITES.with(|map| match map.borrow().get(&instance) {
        Some(state) => Ok(action(state)),
        None => Ok(Variant::Void),
    })
}

fn last_insert_row_id(state: &SqliteState) -> i64 {
    let db = state.connection.ptr();
    if db.is_null() {
        0
    } else {
        unsafe { ffi::sqlite3_last_insert_rowid(db) }
    }
}

fn deny_write(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _value: Variant,
) -> Result<()> {
    Ok(())
}

/// `open_connection` mirrors the reference constructor (`Main.cpp:185-215`).
/// The connection is always returned: SQLite hands back a handle even when the
/// open itself fails, and the failure then shows through `errorCode` /
/// `errorMessage` exactly like the reference.
fn open_connection(
    runtime: &mut Runtime<KrkrHost>,
    database: &str,
    readonly: bool,
) -> Result<Rc<Connection>> {
    if readonly {
        // Read-only opens use the engine storage layer, the equivalent of the
        // reference's `xp3` VFS (`sqlite3_open_v2(..., "xp3")`).
        if let Some(path) = runtime.host().placed_path(database) {
            return Ok(Rc::new(open_path(&path, true)));
        }
        let bytes = match runtime.host().read_binary_storage(database) {
            Ok(bytes) => bytes,
            // Nothing readable behind the name: hand SQLite a missing path so
            // the connection answers SQLITE_CANTOPEN like the reference's VFS.
            Err(_) => {
                let missing = virtual_path(runtime, database);
                return Ok(Rc::new(open_path(&missing, true)));
            }
        };
        // The copy is created exclusively (no pre-existing path, symlink or
        // otherwise, is ever written through) and carries a per-open random
        // name; it is deleted with the connection.
        let Some(temp) = write_temp_copy(&bytes) else {
            let missing = virtual_path(runtime, database);
            return Ok(Rc::new(open_path(&missing, true)));
        };
        let mut connection = open_path(&temp, true);
        connection.temp_path = Some(temp);
        return Ok(Rc::new(connection));
    }

    if database.is_empty() || database.starts_with(':') {
        // SQLite's own special names (`:memory:` and friends) go through
        // unchanged (`Main.cpp:199-202`).
        return Ok(Rc::new(open_special(database)));
    }

    if let Some(path) = runtime.host().placed_path(database) {
        return Ok(Rc::new(open_path(&path, false)));
    }
    if runtime.host().placed_storage_name(database).is_some() {
        // The name exists but only inside an archive or a media: the
        // reference's `TVPGetLocallyAccessibleName` answers empty here.
        return Err(unopenable(database));
    }
    // A plain relative name that does not exist yet: sqlite creates the file,
    // so the path resolves under the engine's executable path the way the
    // reference's `file` media resolves it.
    let path = virtual_path(runtime, database);
    Ok(Rc::new(open_path(&path, false)))
}

fn open_special(name: &str) -> Connection {
    let name = std::ffi::CString::new(name).unwrap_or_default();
    let mut db: *mut ffi::sqlite3 = ptr::null_mut();
    unsafe {
        ffi::sqlite3_open(name.as_ptr(), &mut db);
    }
    if !db.is_null() {
        init_contain_functions(db);
    }
    Connection {
        db: Db(db),
        temp_path: None,
    }
}

fn open_path(path: &std::path::Path, readonly: bool) -> Connection {
    let name = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default();
    let mut db: *mut ffi::sqlite3 = ptr::null_mut();
    let flags = if readonly {
        ffi::SQLITE_OPEN_READONLY
    } else {
        ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE
    };
    unsafe {
        ffi::sqlite3_open_v2(name.as_ptr(), &mut db, flags, ptr::null());
    }
    let connection = Connection {
        db: Db(db),
        temp_path: None,
    };
    if !db.is_null() {
        init_contain_functions(db);
    }
    connection
}

/// Resolves a storage name to a local path under the engine's executable
/// path; used when the name has no placed file yet and for the reference's
/// not-locally-accessible failure cases.
fn virtual_path(runtime: &Runtime<KrkrHost>, database: &str) -> PathBuf {
    let normalized = database.replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
    let base = runtime.host().system_paths().exe_path.clone();
    let path = PathBuf::from(normalized);
    if path.is_absolute() {
        path
    } else {
        PathBuf::from(base).join(path)
    }
}

/// Materializes a read-only archive/memory database into a private temp file.
///
/// The name carries per-open entropy (clock nanos plus a counter) and the file
/// is created with `create_new` (`O_CREAT|O_EXCL`), so a pre-created path — a
/// symlink included — is never written through; the caller records the path on
/// the connection and removes it when the connection drops.
fn write_temp_copy(bytes: &[u8]) -> Option<PathBuf> {
    for attempt in 0..16u32 {
        let path = temp_copy_path(attempt);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                if file.write_all(bytes).is_err() {
                    let _ = std::fs::remove_file(&path);
                    return None;
                }
                return Some(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

fn temp_copy_path(attempt: u32) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "kirakira-sqlite3-{}-{unique}-{nanos}-{attempt}.db",
        std::process::id()
    ))
}

fn unopenable(database: &str) -> TjsError {
    TjsError::runtime(format!(
        "Unable to open the database file, try readonly if exists: {database}"
    ))
}

fn sqlite_finalize(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(instance) = this_obj {
        SQLITES.with(|map| {
            map.borrow_mut().remove(&instance);
        });
    }
    Ok(Variant::Void)
}

fn sqlite_exec(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let connection = sqlite_state(runtime, this_obj)?;
    let sql = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let (stmt, rc) = prepare(connection.ptr(), &sql);
    if rc != ffi::SQLITE_OK || stmt.is_null() {
        if !stmt.is_null() {
            unsafe {
                ffi::sqlite3_finalize(stmt);
            }
        }
        return Ok(Variant::Integer(0));
    }
    // `ret` starts at the prepare result, is replaced by the bind result when
    // parameters are given, and by the final step result (`Main.cpp:233-267`).
    let mut rc = ffi::SQLITE_OK;
    unsafe {
        ffi::sqlite3_reset(stmt);
    }
    if args.len() > 1 {
        rc = bind_params(runtime, stmt, &args[1])?;
    }
    if rc == ffi::SQLITE_OK {
        let callback = args.get(2).filter(|value| value.object_handle().is_some());
        rc = exec_statement(runtime, stmt, callback)?;
    }
    unsafe {
        ffi::sqlite3_finalize(stmt);
    }
    Ok(Variant::Integer(i64::from(
        rc == ffi::SQLITE_OK || rc == ffi::SQLITE_DONE,
    )))
}

fn sqlite_exec_value(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let connection = sqlite_state(runtime, this_obj)?;
    let sql = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let (stmt, rc) = prepare(connection.ptr(), &sql);
    if rc != ffi::SQLITE_OK || stmt.is_null() {
        if !stmt.is_null() {
            unsafe {
                ffi::sqlite3_finalize(stmt);
            }
        }
        return Ok(Variant::Void);
    }
    let mut result = Variant::Void;
    unsafe {
        ffi::sqlite3_reset(stmt);
    }
    let bound = if args.len() <= 1 {
        ffi::SQLITE_OK
    } else {
        bind_params(runtime, stmt, &args[1])?
    };
    if bound == ffi::SQLITE_OK {
        let count = column_count(stmt);
        if unsafe { ffi::sqlite3_step(stmt) } == ffi::SQLITE_ROW && count > 0 {
            result = column_value(stmt, 0).to_variant();
        }
    }
    unsafe {
        ffi::sqlite3_finalize(stmt);
    }
    Ok(result)
}

fn sqlite_transaction(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    sql: &str,
) -> Result<Variant> {
    with_sqlite_state(this_obj, runtime, |state| {
        Variant::Integer(i64::from(state.connection.exec(sql)))
    })
}

fn sqlite_begin(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    sqlite_transaction(runtime, this_obj, "BEGIN TRANSACTION;")
}

fn sqlite_commit(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    sqlite_transaction(runtime, this_obj, "COMMIT;")
}

fn sqlite_rollback(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    sqlite_transaction(runtime, this_obj, "ROLLBACK;")
}

// ---------------------------------------------------------------------------
// `SqliteStatement` class
// ---------------------------------------------------------------------------

fn install_statement_class(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            let Some(sqlite) = args.first() else {
                return Err(TjsError::bad_param_count());
            };
            let Some(sqlite_handle) = sqlite.object_handle() else {
                return Err(TjsError::runtime("use Sqlite class Object"));
            };
            if !object_is_instance_of(runtime, sqlite_handle, "Sqlite") {
                return Err(TjsError::runtime("use Sqlite class Object"));
            }
            let connection = sqlite_state(runtime, Some(sqlite_handle))?;
            let instance = bound_instance(runtime, this_obj, "SqliteStatement");
            install_statement_members(runtime, instance);
            STATEMENTS.with(|map| {
                map.borrow_mut().insert(
                    instance,
                    StatementState {
                        connection,
                        stmt: ptr::null_mut(),
                        bind_pos: 1,
                        column_members: Vec::new(),
                    },
                );
            });
            if let Some(sql) = args.get(1) {
                let rc = statement_open(runtime, instance, sql, args.get(2))?;
                if rc != ffi::SQLITE_OK {
                    STATEMENTS.with(|map| {
                        map.borrow_mut().remove(&instance);
                    });
                    return Err(TjsError::runtime("failed to open state"));
                }
            }
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "SqliteStatement");
    install_statement_members(runtime, class);
    for (name, value) in [
        ("SQLITE_INTEGER", SQLITE_INTEGER),
        ("SQLITE_FLOAT", SQLITE_FLOAT),
        ("SQLITE_TEXT", SQLITE_TEXT),
        ("SQLITE_BLOB", SQLITE_BLOB),
        ("SQLITE_NULL", SQLITE_NULL),
    ] {
        runtime.set_object_member(class, name, Variant::Integer(value));
    }
    runtime.register_object_native(class, "finalize", statement_finalize);
    runtime.set_global_member("SqliteStatement", Variant::Object(class));
}

fn install_statement_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native_with_arg_count(
        handle,
        "open",
        NativeArgCount::AtLeast(1),
        statement_open_native,
    );
    runtime.register_object_native(handle, "close", statement_close);
    runtime.register_object_native(handle, "reset", statement_reset);
    runtime.register_object_native_with_arg_count(
        handle,
        "bind",
        NativeArgCount::AtLeast(1),
        statement_bind,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "bindAt",
        NativeArgCount::AtLeast(1),
        statement_bind_at,
    );
    runtime.register_object_native(handle, "exec", statement_exec);
    runtime.register_object_native(handle, "step", statement_step);
    runtime.register_object_native_with_arg_count(
        handle,
        "isNull",
        NativeArgCount::AtLeast(1),
        statement_is_null,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getType",
        NativeArgCount::AtLeast(1),
        statement_get_type,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "getName",
        NativeArgCount::AtLeast(1),
        statement_get_name,
    );
    runtime.register_object_native(handle, "get", statement_get);
    runtime.register_object_native(handle, "finalize", statement_finalize);
    runtime.register_object_native_property_with_access(
        handle,
        "sql",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            statement_property(runtime, this_obj, |state| {
                if state.stmt.is_null() {
                    Variant::String(String::new())
                } else {
                    Variant::String(unsafe { c_string(ffi::sqlite3_sql(state.stmt)) })
                }
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "count",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            statement_property(runtime, this_obj, |state| {
                let count = if state.stmt.is_null() {
                    0
                } else {
                    unsafe { ffi::sqlite3_data_count(state.stmt) }
                };
                Variant::Integer(i64::from(count))
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "columnCount",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            statement_property(runtime, this_obj, |state| {
                Variant::Integer(i64::from(column_count(state.stmt)))
            })
        },
        deny_write,
    );
}

/// A property read tolerant of a class-object receiver.
fn statement_property(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    action: impl FnOnce(&StatementState) -> Variant,
) -> Result<Variant> {
    let Some(instance) = instance_handle(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    STATEMENTS.with(|map| match map.borrow().get(&instance) {
        Some(state) => Ok(action(state)),
        None => Ok(Variant::Void),
    })
}

/// The column a column argument names (`getColumnNo`, `Main.cpp:581-599`):
/// numbers are column indexes, strings compare case-insensitively against the
/// statement's column names, anything else is -1.
fn statement_column_no(state: &StatementState, column: &Variant) -> c_int {
    match column {
        Variant::Integer(value) => *value as c_int,
        Variant::Real(value) => *value as c_int,
        Variant::String(name) => {
            let count = column_count(state.stmt);
            for index in 0..count {
                let candidate = column_name(state.stmt, index);
                if candidate.eq_ignore_ascii_case(name)
                    || candidate.to_lowercase() == name.to_lowercase()
                {
                    return index;
                }
            }
            -1
        }
        _ => -1,
    }
}

/// Opens `sql` on the instance, replacing any previous statement: prepare,
/// reset, bind (`_open`, `Main.cpp:607-617`). Also (re)registers the column
/// properties the reference's missing handler would answer.
fn statement_open(
    runtime: &mut Runtime<KrkrHost>,
    instance: ObjectHandle,
    sql: &Variant,
    params: Option<&Variant>,
) -> Result<c_int> {
    let sql = sql.to_tjs_string()?;
    let (rc, column_names) = STATEMENTS.with(|map| {
        let mut map = map.borrow_mut();
        let state = map
            .get_mut(&instance)
            .ok_or_else(TjsError::native_class_crash)?;
        if !state.stmt.is_null() {
            unsafe {
                ffi::sqlite3_finalize(state.stmt);
            }
            state.stmt = ptr::null_mut();
        }
        for member in state.column_members.drain(..) {
            let _ = runtime.delete_object_member(instance, &member);
        }
        let (stmt, rc) = prepare(state.connection.ptr(), &sql);
        state.bind_pos = 1;
        if rc == ffi::SQLITE_OK && !stmt.is_null() {
            unsafe {
                ffi::sqlite3_reset(stmt);
            }
            state.stmt = stmt;
        } else if !stmt.is_null() {
            unsafe {
                ffi::sqlite3_finalize(stmt);
            }
        }
        let mut rc = rc;
        if let (Some(params), false) = (params, state.stmt.is_null())
            && let Ok(bind_rc) = bind_params(runtime, state.stmt, params)
            && bind_rc != ffi::SQLITE_OK
        {
            rc = bind_rc;
        }
        let names = if state.stmt.is_null() {
            Vec::new()
        } else {
            (0..column_count(state.stmt))
                .map(|index| column_name(state.stmt, index))
                .collect::<Vec<_>>()
        };
        Ok::<_, TjsError>((rc, names))
    })?;
    register_column_members(runtime, instance, &column_names)?;
    Ok(rc)
}

/// Registers one read-only native property per result column so
/// `stmt.columnName` reads the current row's value, the engine-side
/// equivalent of the reference's `missing` handler (`Main.cpp:532-548`).
/// A name that already resolves on the statement — its methods, `count`,
/// `sql`, `columnCount` — is skipped: the reference's handler only answers
/// members the class chain misses, so `SELECT count(*) AS count` must leave
/// `stmt.count` alone (`manual.tjs`: only columns that do not collide with
/// class member names are directly readable).
fn register_column_members(
    runtime: &mut Runtime<KrkrHost>,
    instance: ObjectHandle,
    names: &[String],
) -> Result<()> {
    let mut registered = Vec::new();
    for (index, name) in names.iter().enumerate() {
        if name.is_empty() {
            continue;
        }
        let mut members = vec![name.clone()];
        let lower = name.to_lowercase();
        if lower != *name {
            members.push(lower);
        }
        for member in members {
            if statement_member_resolves(runtime, instance, &member) {
                continue;
            }
            let row = index;
            runtime.register_object_native_property_with_access(
                instance,
                member.clone(),
                NativePropertyAccess::ReadOnly,
                move |runtime, this_obj| {
                    let instance = instance_handle(runtime, this_obj)
                        .ok_or_else(TjsError::native_class_crash)?;
                    let value = STATEMENTS.with(|map| {
                        map.borrow()
                            .get(&instance)
                            .map(|state| column_value(state.stmt, i32::try_from(row).unwrap_or(-1)))
                    });
                    Ok(match value {
                        Some(ColumnValue::Null) | None => Variant::Void,
                        Some(value) => value.to_variant(),
                    })
                },
                deny_write,
            );
            registered.push(member);
        }
    }
    STATEMENTS.with(|map| {
        if let Some(state) = map.borrow_mut().get_mut(&instance) {
            state.column_members = registered;
        }
    });
    Ok(())
}

/// Whether `name` already resolves on the instance or anywhere up its class
/// chain (the shape the reference's `PropGet` consults before `missing`).
fn statement_member_resolves(
    runtime: &Runtime<KrkrHost>,
    instance: ObjectHandle,
    name: &str,
) -> bool {
    let mut current = Some(instance);
    while let Some(handle) = current {
        if !matches!(runtime.object_member(handle, name), Variant::Void) {
            return true;
        }
        current = runtime.object_super_class(handle);
    }
    false
}

fn statement_state_mut<R>(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    action: impl FnOnce(&mut StatementState) -> Result<R>,
) -> Result<R> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    STATEMENTS.with(|map| {
        let mut map = map.borrow_mut();
        let state = map
            .get_mut(&instance)
            .ok_or_else(TjsError::native_class_crash)?;
        action(state)
    })
}

fn statement_open_native(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    let sql = args.first().ok_or_else(TjsError::bad_param_count)?;
    let rc = statement_open(runtime, instance, sql, args.get(1))?;
    Ok(Variant::Integer(i64::from(rc)))
}

fn statement_close(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    STATEMENTS.with(|map| {
        if let Some(state) = map.borrow_mut().get_mut(&instance) {
            if !state.stmt.is_null() {
                unsafe {
                    ffi::sqlite3_finalize(state.stmt);
                }
                state.stmt = ptr::null_mut();
            }
            for member in state.column_members.drain(..) {
                let _ = runtime.delete_object_member(instance, &member);
            }
        }
    });
    Ok(Variant::Void)
}

fn statement_reset(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    statement_state_mut(runtime, this_obj, |state| {
        state.bind_pos = 1;
        Ok(Variant::Integer(i64::from(unsafe {
            ffi::sqlite3_reset(state.stmt)
        })))
    })
}

fn statement_bind(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let params = args.first().ok_or_else(TjsError::bad_param_count)?.clone();
    statement_state_mut(runtime, this_obj, |state| {
        let rc = if state.stmt.is_null() {
            ffi::SQLITE_MISUSE
        } else {
            bind_params(runtime, state.stmt, &params)?
        };
        Ok(Variant::Integer(i64::from(rc)))
    })
}

fn statement_bind_at(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let value = BindValue::from_variant(args.first().ok_or_else(TjsError::bad_param_count)?)?;
    statement_state_mut(runtime, this_obj, |state| {
        if let Some(pos) = args.get(1)
            && !matches!(pos, Variant::Void)
        {
            state.bind_pos = bind_pos(state.stmt, pos);
        }
        let pos = state.bind_pos;
        state.bind_pos = state.bind_pos.saturating_add(1);
        Ok(Variant::Integer(i64::from(bind_value(
            state.stmt, pos, &value,
        ))))
    })
}

fn statement_exec(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    statement_state_mut(runtime, this_obj, |state| {
        let rc = unsafe { ffi::sqlite3_step(state.stmt) };
        if rc != ffi::SQLITE_ROW {
            state.bind_pos = 1;
            unsafe {
                ffi::sqlite3_reset(state.stmt);
            }
        }
        Ok(Variant::Integer(i64::from(rc)))
    })
}

fn statement_step(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    statement_state_mut(runtime, this_obj, |state| {
        if unsafe { ffi::sqlite3_step(state.stmt) } == ffi::SQLITE_ROW {
            return Ok(Variant::Integer(1));
        }
        state.bind_pos = 1;
        unsafe {
            ffi::sqlite3_reset(state.stmt);
        }
        Ok(Variant::Integer(0))
    })
}

fn statement_is_null(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let column = args.first().ok_or_else(TjsError::bad_param_count)?.clone();
    statement_state_mut(runtime, this_obj, |state| {
        let index = statement_column_no(state, &column);
        let is_null = index >= 0
            && unsafe { ffi::sqlite3_column_type(state.stmt, index) } == ffi::SQLITE_NULL;
        Ok(Variant::Integer(i64::from(is_null)))
    })
}

fn statement_get_type(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let column = args.first().ok_or_else(TjsError::bad_param_count)?.clone();
    statement_state_mut(runtime, this_obj, |state| {
        let index = statement_column_no(state, &column);
        let ty = if index < 0 {
            ffi::SQLITE_NULL
        } else {
            unsafe { ffi::sqlite3_column_type(state.stmt, index) }
        };
        Ok(Variant::Integer(i64::from(ty)))
    })
}

fn statement_get_name(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let column = args.first().ok_or_else(TjsError::bad_param_count)?.clone();
    statement_state_mut(runtime, this_obj, |state| {
        let index = statement_column_no(state, &column);
        Ok(Variant::String(column_name(state.stmt, index)))
    })
}

fn statement_get(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if args.is_empty() {
        // No argument: an Array with every column of the current row
        // (`Main.cpp:505-514`).
        let values = statement_state_mut(runtime, this_obj, |state| {
            Ok((0..column_count(state.stmt))
                .map(|index| column_value(state.stmt, index).to_variant())
                .collect::<Vec<_>>())
        })?;
        return Ok(Variant::Object(runtime.alloc_array_object(values)));
    }
    let column = args[0].clone();
    let default = args.get(1).cloned();
    statement_state_mut(runtime, this_obj, |state| {
        let index = statement_column_no(state, &column);
        // `sqlite3_column_type(stmt, -1)` is `SQLITE_NULL`, so the reference
        // answers the default value for a name that is not a column
        // (`Main.cpp:519-530`).
        if index < 0 {
            return Ok(default.unwrap_or(Variant::Void));
        }
        let value = column_value(state.stmt, index);
        Ok(match value {
            ColumnValue::Null => default.unwrap_or(Variant::Void),
            value => value.to_variant(),
        })
    })
}

fn statement_finalize(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(instance) = this_obj {
        STATEMENTS.with(|map| map.borrow_mut().remove(&instance));
    }
    Ok(Variant::Void)
}

// ---------------------------------------------------------------------------
// `SqliteThread` class
// ---------------------------------------------------------------------------

fn install_thread_class(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
            if args.len() < 2 {
                return Err(TjsError::bad_param_count());
            }
            let Some(window) = args[0].object_handle() else {
                return Err(TjsError::runtime("use Window class Object"));
            };
            if !object_is_instance_of(runtime, window, "Window") {
                return Err(TjsError::runtime("use Window class Object"));
            }
            let Some(sqlite) = args[1].object_handle() else {
                return Err(TjsError::runtime("use Sqlite class Object"));
            };
            if !object_is_instance_of(runtime, sqlite, "Sqlite") {
                return Err(TjsError::runtime("use Sqlite class Object"));
            }
            let connection = SQLITES.with(|map| {
                map.borrow()
                    .get(&sqlite)
                    .map(|state| state.connection.clone())
            });
            let instance = bound_instance(runtime, this_obj, "SqliteThread");
            install_thread_members(runtime, instance);
            let timer = create_tick_timer(runtime, instance, "__sqliteTick");
            THREADS.with(|map| {
                map.borrow_mut().insert(
                    instance,
                    ThreadState {
                        window,
                        sqlite,
                        connection,
                        worker: None,
                        canceled: Arc::new(AtomicBool::new(false)),
                        shared: Arc::new(Mutex::new(Shared::default())),
                        select_result: Variant::Void,
                        state: THREAD_INIT,
                        error_code: 0,
                        progress_update_count: DEFAULT_PROGRESS_COUNT,
                        timer,
                    },
                );
            });
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "SqliteThread");
    install_thread_members(runtime, class);
    for (name, value) in [
        ("INIT", THREAD_INIT),
        ("WORKING", THREAD_WORKING),
        ("DONE", THREAD_DONE),
    ] {
        runtime.set_object_member(class, name, Variant::Integer(value));
    }
    runtime.register_object_native(class, "finalize", thread_finalize);
    runtime.set_global_member("SqliteThread", Variant::Object(class));
}

fn install_thread_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native_with_arg_count(
        handle,
        "select",
        NativeArgCount::AtLeast(1),
        thread_select,
    );
    runtime.register_object_native_with_arg_count(
        handle,
        "update",
        NativeArgCount::AtLeast(2),
        thread_update,
    );
    runtime.register_object_native(handle, "abort", thread_abort);
    runtime.register_object_native(handle, "__sqliteTick", thread_tick);
    runtime.register_object_native(handle, "finalize", thread_finalize);
    runtime.register_object_native_property_with_access(
        handle,
        "state",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            thread_property(runtime, this_obj, |state| Variant::Integer(state.state))
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "errorCode",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| {
            thread_property(runtime, this_obj, |state| {
                Variant::Integer(state.error_code)
            })
        },
        deny_write,
    );
    runtime.register_object_native_property_with_access(
        handle,
        "selectResult",
        NativePropertyAccess::ReadOnly,
        |runtime, this_obj| thread_property(runtime, this_obj, |state| state.select_result.clone()),
        deny_write,
    );
    runtime.register_object_native_property(
        handle,
        "progressUpdateCount",
        |runtime, this_obj| {
            thread_property(runtime, this_obj, |state| {
                Variant::Integer(state.progress_update_count)
            })
        },
        |runtime, this_obj, value| {
            let count = value.to_integer()?;
            let Some(instance) = instance_handle(runtime, this_obj) else {
                return Ok(());
            };
            THREADS.with(|map| {
                if let Some(state) = map.borrow_mut().get_mut(&instance) {
                    state.progress_update_count = count;
                }
            });
            Ok(())
        },
    );
}

/// A property read tolerant of a class-object receiver.
fn thread_property(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    action: impl FnOnce(&ThreadState) -> Variant,
) -> Result<Variant> {
    let Some(instance) = instance_handle(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    THREADS.with(|map| match map.borrow().get(&instance) {
        Some(state) => Ok(action(state)),
        None => Ok(Variant::Void),
    })
}

fn with_thread_state<R>(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    action: impl FnOnce(&ThreadState) -> R,
) -> Result<R> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    THREADS.with(|map| {
        map.borrow()
            .get(&instance)
            .map(action)
            .ok_or_else(TjsError::native_class_crash)
    })
}

fn with_thread_state_mut<R>(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    action: impl FnOnce(&mut ThreadState) -> R,
) -> Result<R> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    THREADS.with(|map| {
        map.borrow_mut()
            .get_mut(&instance)
            .map(action)
            .ok_or_else(TjsError::native_class_crash)
    })
}

/// `setReceiver(true)` (`Main.cpp:823-831`) is the reference's message-pump
/// registration; this engine's pump is a hidden `Timer` whose action is the
/// owning object's `__sqliteTick` member.
///
/// The Timer is built through the engine's class-name constructor idiom
/// (`instance.Timer(owner, action)`), the same path `_Layer.Layer(win, this)`
/// takes: the constructor runs on the object it is called on.
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

/// The reference's `open()` refuses while a worker runs (`Main.cpp:782-783`),
/// before it closes the old statement and prepares a new one. Preparing first
/// would leak the statement on the error path — and with it the connection,
/// because `sqlite3_close` answers `SQLITE_BUSY` for unfinalized statements.
/// The check also has to precede the per-operation state reset, or a refused
/// call would throw away the running operation's queued events (and with them
/// its DONE delivery).
fn check_thread_idle(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Result<()> {
    let running = with_thread_state(runtime, this_obj, |state| state.worker.is_some())?;
    if running {
        return Err(TjsError::runtime("already running"));
    }
    Ok(())
}

/// `onStateChange` sets the visible state synchronously, before the event it
/// posts is delivered (`Main.cpp:804-813`); a refused `select` still leaves
/// the state at INIT in the reference. The event itself is only queued once
/// the operation starts, because the queue still belongs to the previous
/// operation until `check_thread_idle` passes.
fn set_thread_state(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    value: i64,
) -> Result<()> {
    with_thread_state_mut(runtime, this_obj, |state| state.state = value)
}

fn thread_select(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    let sql = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let params = args.get(1).cloned();
    let (connection, spec) = {
        let connection = with_thread_state(runtime, this_obj, |state| state.connection.clone())?;
        let Some(connection) = connection else {
            return Ok(Variant::Integer(0));
        };
        let spec = match &params {
            Some(params) => snapshot_params(runtime, params)?,
            None => BindSpec::Positional(Vec::new()),
        };
        (connection, spec)
    };
    // `select` publishes INIT before `open` can refuse it (`Main.cpp:700`),
    // and the refusal must not disturb the running operation: state first,
    // then the running check, then the per-operation reset (`startSelectThread`
    // replaces the result array only once the open has succeeded,
    // `Main.cpp:920-927`).
    set_thread_state(runtime, this_obj, THREAD_INIT)?;
    check_thread_idle(runtime, this_obj)?;
    let results = runtime.alloc_array_object(Vec::new());
    let (shared, canceled, progress) = reset_operation(runtime, this_obj, Some(results))?;
    begin_operation(runtime, instance, THREAD_INIT)?;
    let (stmt, rc) = prepare(connection.ptr(), &sql);
    if rc != ffi::SQLITE_OK || stmt.is_null() {
        if !stmt.is_null() {
            unsafe {
                ffi::sqlite3_finalize(stmt);
            }
        }
        set_error_code(runtime, instance, i64::from(rc));
        return Ok(Variant::Integer(0));
    }
    unsafe {
        ffi::sqlite3_reset(stmt);
    }
    let bind_rc = apply_bind_spec(stmt, &spec);
    if bind_rc != ffi::SQLITE_OK {
        unsafe {
            ffi::sqlite3_finalize(stmt);
        }
        set_error_code(runtime, instance, i64::from(bind_rc));
        return Ok(Variant::Integer(0));
    }
    start_worker(
        runtime,
        this_obj,
        instance,
        WorkerJob::Select {
            statement: Stmt(stmt),
            shared,
            canceled,
            progress,
        },
    )?;
    Ok(Variant::Integer(1))
}

fn thread_update(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    let sql = args
        .first()
        .ok_or_else(TjsError::bad_param_count)?
        .to_tjs_string()?;
    let Some(datas) = args.get(1) else {
        return Err(TjsError::bad_param_count());
    };
    let rows = snapshot_rows(runtime, datas)?;
    let connection = with_thread_state(runtime, this_obj, |state| state.connection.clone())?;
    let Some(connection) = connection else {
        return Ok(Variant::Integer(0));
    };
    // `update` publishes INIT before `open` can refuse it (`Main.cpp:716`);
    // state first, then the running check, then the per-operation reset
    // (`startUpdateThread`, `Main.cpp:979-984`).
    set_thread_state(runtime, this_obj, THREAD_INIT)?;
    check_thread_idle(runtime, this_obj)?;
    let (shared, canceled, progress) = reset_operation(runtime, this_obj, None)?;
    begin_operation(runtime, instance, THREAD_INIT)?;
    let (stmt, rc) = prepare(connection.ptr(), &sql);
    if rc != ffi::SQLITE_OK || stmt.is_null() {
        if !stmt.is_null() {
            unsafe {
                ffi::sqlite3_finalize(stmt);
            }
        }
        set_error_code(runtime, instance, i64::from(rc));
        return Ok(Variant::Integer(0));
    }
    unsafe {
        ffi::sqlite3_reset(stmt);
    }
    start_worker(
        runtime,
        this_obj,
        instance,
        WorkerJob::Update {
            statement: Stmt(stmt),
            connection: connection.db,
            rows,
            shared,
            canceled,
            progress,
        },
    )?;
    Ok(Variant::Integer(1))
}

fn thread_abort(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let instance = instance_handle(runtime, this_obj).ok_or_else(TjsError::native_class_crash)?;
    THREADS.with(|map| {
        let mut map = map.borrow_mut();
        if let Some(state) = map.get_mut(&instance) {
            state.stop();
            if let Ok(mut shared) = state.shared.lock() {
                shared.deliveries.clear();
            }
            state.select_result = Variant::Void;
        }
        set_timer_enabled(
            runtime,
            map.get(&instance).and_then(|state| state.timer),
            false,
        );
    });
    Ok(Variant::Void)
}

fn thread_finalize(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(instance) = this_obj {
        THREADS.with(|map| {
            if let Some(state) = map.borrow_mut().remove(&instance) {
                state.timer.inspect(|timer| {
                    runtime.set_object_member(*timer, "enabled", Variant::Integer(0));
                });
            }
        });
    }
    Ok(Variant::Void)
}

/// Records the operation start: state INIT immediately, plus the queued
/// `onStateChange(INIT)` event (`Main.cpp:700`, `:716`).
/// Resets the per-operation worker state (`startSelectThread` /
/// `startUpdateThread`, `Main.cpp:920-927`, `:979-984`): a fresh result array
/// for a select and a fresh event queue plus cancel flag for both. Queues must
/// be replaced before `begin_operation` puts the INIT record in them.
fn reset_operation(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    results: Option<ObjectHandle>,
) -> Result<(Arc<Mutex<Shared>>, Arc<AtomicBool>, i64)> {
    with_thread_state_mut(runtime, this_obj, |state| {
        if let Some(results) = results {
            state.select_result = Variant::Object(results);
        }
        state.error_code = 0;
        state.canceled = Arc::new(AtomicBool::new(false));
        state.shared = Arc::new(Mutex::new(Shared::default()));
        (
            Arc::clone(&state.shared),
            Arc::clone(&state.canceled),
            state.progress_update_count,
        )
    })
}

fn begin_operation(
    runtime: &mut Runtime<KrkrHost>,
    instance: ObjectHandle,
    state_value: i64,
) -> Result<()> {
    let timer = THREADS.with(|map| {
        let mut map = map.borrow_mut();
        let state = map
            .get_mut(&instance)
            .ok_or_else(TjsError::native_class_crash)?;
        state.state = state_value;
        if let Ok(mut shared) = state.shared.lock() {
            shared.deliveries.push(Delivery::State(state_value));
        }
        Ok::<_, TjsError>(state.timer)
    })?;
    set_timer_enabled(runtime, timer, true);
    Ok(())
}

fn set_error_code(runtime: &mut Runtime<KrkrHost>, instance: ObjectHandle, error: i64) {
    THREADS.with(|map| {
        if let Some(state) = map.borrow_mut().get_mut(&instance) {
            state.error_code = error;
        }
    });
    set_timer_enabled(
        runtime,
        THREADS.with(|map| map.borrow().get(&instance).and_then(|state| state.timer)),
        true,
    );
}

enum WorkerJob {
    Select {
        statement: Stmt,
        shared: Arc<Mutex<Shared>>,
        canceled: Arc<AtomicBool>,
        progress: i64,
    },
    Update {
        statement: Stmt,
        connection: Db,
        rows: Vec<BindSpec>,
        shared: Arc<Mutex<Shared>>,
        canceled: Arc<AtomicBool>,
        progress: i64,
    },
}

fn start_worker(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    instance: ObjectHandle,
    job: WorkerJob,
) -> Result<()> {
    let timer = THREADS.with(|map| {
        let mut map = map.borrow_mut();
        let state = map
            .get_mut(&instance)
            .ok_or_else(TjsError::native_class_crash)?;
        if state.worker.is_some() {
            return Err(TjsError::runtime("already running"));
        }
        Ok::<_, TjsError>(state.timer)
    })?;
    let handle = std::thread::spawn(move || match job {
        WorkerJob::Select {
            statement,
            shared,
            canceled,
            progress,
        } => run_select(statement, shared, canceled, progress),
        WorkerJob::Update {
            statement,
            connection,
            rows,
            shared,
            canceled,
            progress,
        } => run_update(statement, connection, rows, shared, canceled, progress),
    });
    let _ = this_obj;
    THREADS.with(|map| {
        if let Some(state) = map.borrow_mut().get_mut(&instance) {
            state.worker = Some(handle);
        }
    });
    set_timer_enabled(runtime, timer, true);
    Ok(())
}

fn push_delivery(shared: &Arc<Mutex<Shared>>, delivery: Delivery) {
    if let Ok(mut shared) = shared.lock() {
        shared.deliveries.push(delivery);
    }
}

fn finish_worker(shared: &Arc<Mutex<Shared>>, error: i64) {
    if let Ok(mut shared) = shared.lock() {
        shared.final_error = error;
        shared.finished = true;
    }
}

/// The reference's `selectThreadMain` (`Main.cpp:879-910`).
fn run_select(
    statement: Stmt,
    shared: Arc<Mutex<Shared>>,
    canceled: Arc<AtomicBool>,
    progress: i64,
) {
    push_delivery(&shared, Delivery::State(THREAD_WORKING));
    let mut rows = 0i64;
    let mut milestone = progress;
    let mut error = ffi::SQLITE_OK;
    while !canceled.load(Ordering::SeqCst) {
        error = unsafe { ffi::sqlite3_step(statement.0) };
        if error != ffi::SQLITE_ROW {
            break;
        }
        let count = unsafe { ffi::sqlite3_data_count(statement.0) };
        let mut row = Vec::with_capacity(count.max(0) as usize);
        for index in 0..count {
            row.push(column_value(statement.0, index));
        }
        push_delivery(&shared, Delivery::Row(row));
        rows += 1;
        if progress > 0 && rows == milestone {
            push_delivery(&shared, Delivery::Progress(milestone));
            milestone += progress;
        }
    }
    if canceled.load(Ordering::SeqCst) {
        error = ffi::SQLITE_ABORT;
    }
    if error == ffi::SQLITE_OK || error == ffi::SQLITE_DONE {
        push_delivery(&shared, Delivery::Progress(rows));
    }
    finish_worker(&shared, i64::from(error));
    unsafe {
        ffi::sqlite3_finalize(statement.0);
    }
    push_delivery(&shared, Delivery::State(THREAD_DONE));
}

/// The reference's `updateThreadMain` (`Main.cpp:934-969`).
fn run_update(
    statement: Stmt,
    connection: Db,
    rows: Vec<BindSpec>,
    shared: Arc<Mutex<Shared>>,
    canceled: Arc<AtomicBool>,
    progress: i64,
) {
    push_delivery(&shared, Delivery::State(THREAD_WORKING));
    let mut done = 0i64;
    let mut milestone = progress;
    let mut error = ffi::SQLITE_OK;
    let exec = |sql: &str| {
        let Ok(sql) = std::ffi::CString::new(sql) else {
            return;
        };
        unsafe {
            ffi::sqlite3_exec(
                connection.0,
                sql.as_ptr(),
                None,
                ptr::null_mut(),
                ptr::null_mut(),
            );
        }
    };
    exec("BEGIN TRANSACTION;");
    while !canceled.load(Ordering::SeqCst)
        && done < rows.len() as i64
        && (error == ffi::SQLITE_OK || error == ffi::SQLITE_DONE)
    {
        error = apply_bind_spec(statement.0, &rows[done as usize]);
        if error == ffi::SQLITE_OK {
            loop {
                error = unsafe { ffi::sqlite3_step(statement.0) };
                if error != ffi::SQLITE_ROW {
                    break;
                }
            }
            unsafe {
                ffi::sqlite3_reset(statement.0);
            }
        }
        done += 1;
        if progress > 0 && done == milestone {
            push_delivery(&shared, Delivery::Progress(done));
            milestone += progress;
        }
    }
    if canceled.load(Ordering::SeqCst) {
        error = ffi::SQLITE_ABORT;
    }
    if error == ffi::SQLITE_OK || error == ffi::SQLITE_DONE {
        exec("COMMIT;");
        push_delivery(&shared, Delivery::Progress(done));
    } else {
        exec("ROLLBACK;");
    }
    finish_worker(&shared, i64::from(error));
    unsafe {
        ffi::sqlite3_finalize(statement.0);
    }
    push_delivery(&shared, Delivery::State(THREAD_DONE));
}

/// Snapshots the `update` data Array of parameter objects (`Main.cpp:979-984`).
fn snapshot_rows(runtime: &Runtime<KrkrHost>, datas: &Variant) -> Result<Vec<BindSpec>> {
    let Some(handle) = datas.object_handle() else {
        return Ok(Vec::new());
    };
    let Some(elements) = runtime.array_elements(handle) else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::with_capacity(elements.len());
    for element in elements {
        rows.push(snapshot_params(runtime, element)?);
    }
    Ok(rows)
}

/// The script-thread pump: drains the worker's queued events in order and
/// calls the handlers, the plugin-side equivalent of the reference's
/// `WM_APP+8`/`WM_APP+9` messages.
fn thread_tick(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(instance) = instance_handle(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let Some((deliveries, timer)) = THREADS.with(|map| {
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
    for delivery in deliveries {
        match delivery {
            Delivery::State(value) => {
                THREADS.with(|map| {
                    if let Some(state) = map.borrow_mut().get_mut(&instance) {
                        state.state = value;
                    }
                });
                if value == THREAD_DONE {
                    THREADS.with(|map| {
                        if let Some(state) = map.borrow_mut().get_mut(&instance) {
                            state.stop();
                        }
                    });
                }
                deliver_event(
                    runtime,
                    instance,
                    "onStateChange",
                    vec![Variant::Integer(value)],
                )?;
            }
            Delivery::Progress(value) => {
                deliver_event(
                    runtime,
                    instance,
                    "onProgress",
                    vec![Variant::Integer(value)],
                )?;
            }
            Delivery::Row(row) => {
                // `selectThreadMain` builds a fresh array per row and adds
                // *that* to `selectResult` (`Main.cpp:884-895`), so scripts
                // read `selectResult[row][column]`.
                let values = row.iter().map(ColumnValue::to_variant).collect::<Vec<_>>();
                THREADS.with(|map| {
                    if let Some(state) = map.borrow_mut().get_mut(&instance)
                        && let Variant::Object(results) = &state.select_result
                    {
                        let line = runtime.alloc_array_object(values);
                        runtime.array_push(*results, Variant::Object(line));
                    }
                });
            }
        }
    }
    // The worker's final error code becomes the script-visible one, and the
    // tick timer stops once nothing is queued and no worker runs.
    let (finished, final_error, idle) = THREADS.with(|map| {
        let map = map.borrow();
        let Some(state) = map.get(&instance) else {
            return (false, 0, true);
        };
        let (finished, final_error) = state
            .shared
            .lock()
            .map(|shared| (shared.finished, shared.final_error))
            .unwrap_or((false, state.error_code));
        let idle = state.worker.is_none();
        (finished, final_error, idle)
    });
    if finished {
        THREADS.with(|map| {
            if let Some(state) = map.borrow_mut().get_mut(&instance) {
                state.error_code = final_error;
            }
        });
    }
    if idle {
        set_timer_enabled(runtime, timer, false);
    }
    Ok(Variant::Void)
}

/// Delivers one script event like `TVPPostEvent`'s handler dispatch: a
/// missing handler is a silent drop.
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
                    "handled sqlite3 event `{name}` error: {}",
                    error.message
                ));
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `cnt` / `ncnt` SQL functions (`extend.cpp`)
// ---------------------------------------------------------------------------

const NORMALIZE_BEFORE: &str = concat!(
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    "ＡＢＣＤＥＦＧＨＩＪＫＬＭＮＯＰＱＲＳＴＵＶＷＸＹＺ",
    "ａｂｃｄｅｆｇｈｉｊｋｌｍｎｏｐｑｒｓｔｕｖｗｘｙｚ",
    "１２３４５６７８９０",
    "あいうえおかきくけこさしすせそたちつてとなにぬねの",
    "はひふへほまみむめもやゆよらりるれろわゐゑをんぁぃぅぇぉっゃゅょ",
    "がぎぐげござじずぜぞだぢづでどばびぶべぼぱぴぷぺぽ",
    "アイウエオカキクケコサシスセソタチツテトナニヌネノ",
    "ハヒフヘホマミムメモヤユヨラリルレロワヰヱヲンァィゥェォッャュョ",
    "ガギグゲゴザジズゼゾダヂヅデドバビブベボパピプペポ",
    "ｧｨｩｪｫｯｬｭｮ",
    "ー・、。ｰ",
    "[]{}",
    "，．：；？！´｀＾￣＿〇ー―‐／＼～",
    "｜‘’“”（）〔〕［］｛｝〈〉《》「」『』【】＋－×＝",
    "＜＞￥＄％＃＆＊＠★●◎◆■▲▼※",
);

const NORMALIZE_AFTER: &str = concat!(
    "abcdefghijklmnopqrstuvwxyz",
    "abcdefghijklmnopqrstuvwxyz",
    "abcdefghijklmnopqrstuvwxyz",
    "1234567890",
    "ｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉ",
    "ﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜｲｴｦﾝｱｲｳｴｵﾂﾔﾕﾖ",
    "ｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾊﾋﾌﾍﾎﾊﾋﾌﾍﾎ",
    "ｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉ",
    "ﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜｲｴｦﾝｱｲｳｴｵﾂﾔﾕﾖ",
    "ｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾊﾋﾌﾍﾎﾊﾋﾌﾍﾎ",
    "ｱｲｳｴｵﾂﾔﾕﾖ",
    "-･,.-",
    "()()",
    ",.:;?!'`^~_◯---/＼-",
    "|`'\"\"()()()()()()｢｣｢｣()+-x=",
    "<>\\$%#&*@☆○○◇□△▽*",
);

const NORMALIZE_CLEAR: &str = "ﾞ゛゜";

/// `initNormalize` (`extend.cpp:60-77`): a 65536-entry character map, with
/// the combining dakuten marks mapping to zero (removed).
fn normalize_char(ch: char) -> Option<char> {
    if NORMALIZE_CLEAR.contains(ch) {
        return None;
    }
    let mut before = NORMALIZE_BEFORE.chars();
    let mut after = NORMALIZE_AFTER.chars();
    loop {
        match (before.next(), after.next()) {
            (Some(source), Some(target)) => {
                if source == ch {
                    return Some(target);
                }
            }
            _ => return Some(ch),
        }
    }
}

fn normalize_text(text: &str) -> String {
    text.chars().filter_map(normalize_char).collect()
}

fn contains_units(haystack: &[u16], needle: &[u16]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

unsafe extern "C" fn cnt_function(
    context: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    if argc < 2 {
        return;
    }
    let left = unsafe { value_text(*argv) };
    let right = unsafe { value_text(*argv.add(1)) };
    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();
    unsafe {
        ffi::sqlite3_result_int(context, c_int::from(contains_units(&left, &right)));
    }
}

unsafe extern "C" fn ncnt_function(
    context: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    if argc < 2 {
        return;
    }
    let left = normalize_text(&unsafe { value_text(*argv) });
    let right = normalize_text(&unsafe { value_text(*argv.add(1)) });
    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();
    unsafe {
        ffi::sqlite3_result_int(context, c_int::from(contains_units(&left, &right)));
    }
}

unsafe fn value_text(value: *mut ffi::sqlite3_value) -> String {
    let pointer = unsafe { ffi::sqlite3_value_text(value) };
    if pointer.is_null() {
        return String::new();
    }
    let length = unsafe { ffi::sqlite3_value_bytes(value) };
    if length <= 0 {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(pointer, length as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// `initContainFunc` (`extend.cpp:97-103`), called for every connection.
fn init_contain_functions(db: *mut ffi::sqlite3) {
    let cnt = std::ffi::CString::new("cnt").expect("static name");
    let ncnt = std::ffi::CString::new("ncnt").expect("static name");
    unsafe {
        ffi::sqlite3_create_function_v2(
            db,
            cnt.as_ptr(),
            2,
            ffi::SQLITE_UTF8,
            ptr::null_mut(),
            Some(cnt_function),
            None,
            None,
            None,
        );
        ffi::sqlite3_create_function_v2(
            db,
            ncnt.as_ptr(),
            2,
            ffi::SQLITE_UTF8,
            ptr::null_mut(),
            Some(ncnt_function),
            None,
            None,
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, sync::Arc, time::Duration};

    use krkr_assets::ProjectStorage;
    use krkr_core::{FrameInput, Size};
    use krkr_engine::{EngineConfig, EngineInput, KrkrEngine, SystemPaths};
    use krkr_tjs2::runtime::Variant;

    use super::{Connection, Db, SQLITES, Sqlite3Plugin, ffi, write_temp_copy};

    fn test_root(name: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-sqlite3-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project root");
        root
    }

    fn test_engine(root: &Path) -> KrkrEngine {
        let storage = ProjectStorage::for_root(root).expect("storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            system_paths: SystemPaths {
                exe_path: format!("{}/", root.display()),
                ..SystemPaths::default()
            },
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        engine
    }

    fn memory_engine(
        name: &str,
        members: impl IntoIterator<Item = (&'static str, Vec<u8>)>,
    ) -> KrkrEngine {
        let _ = name;
        let storage = ProjectStorage::from_memory(members);
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        engine
    }

    /// Drives a few engine frames, which is what delivers the plugin's tick
    /// timer (and with it the background-thread events).
    fn tick(engine: &mut KrkrEngine, millis: u64) {
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::from_millis(millis),
            )
            .expect("update");
    }

    /// Ticks until `expression` answers `done` or the budget runs out.
    fn tick_until(engine: &mut KrkrEngine, expression: &str, done: i64) -> bool {
        for _ in 0..300 {
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
    fn the_surface_and_constants_match_the_reference() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        for class in ["Sqlite", "SqliteStatement", "SqliteThread"] {
            assert_ne!(
                engine
                    .execute_expression("inline.tjs", &format!("typeof {class}"))
                    .expect("class probe"),
                Variant::String("undefined".to_string()),
                "{class} is not installed"
            );
        }
        for member in [
            "exec",
            "execValue",
            "begin",
            "commit",
            "rollback",
            "lastInsertRowId",
            "errorCode",
            "errorMessage",
        ] {
            assert_ne!(
                engine
                    .execute_expression("inline.tjs", &format!("typeof Sqlite.{member}"))
                    .expect("member probe"),
                Variant::String("undefined".to_string()),
                "Sqlite.{member} is not installed"
            );
        }
        for member in [
            "open",
            "close",
            "reset",
            "bind",
            "bindAt",
            "exec",
            "step",
            "count",
            "columnCount",
            "isNull",
            "getType",
            "getName",
            "get",
            "sql",
        ] {
            assert_ne!(
                engine
                    .execute_expression("inline.tjs", &format!("typeof SqliteStatement.{member}"))
                    .expect("member probe"),
                Variant::String("undefined".to_string()),
                "SqliteStatement.{member} is not installed"
            );
        }
        for member in [
            "select",
            "update",
            "abort",
            "state",
            "errorCode",
            "selectResult",
            "progressUpdateCount",
        ] {
            assert_ne!(
                engine
                    .execute_expression("inline.tjs", &format!("typeof SqliteThread.{member}"))
                    .expect("member probe"),
                Variant::String("undefined".to_string()),
                "SqliteThread.{member} is not installed"
            );
        }
        // The result/type/state constants (`Main.cpp:338-367`, `:634-639`,
        // `:1020-1023`).
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "Sqlite.SQLITE_OK + ':' + Sqlite.SQLITE_CANTOPEN + ':' + \
                     Sqlite.SQLITE_CONSTRAINT + ':' + Sqlite.SQLITE_ROW + ':' + Sqlite.SQLITE_DONE"
                )
                .expect("Sqlite constants"),
            Variant::String("0:14:19:100:101".to_string())
        );
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "SqliteStatement.SQLITE_INTEGER + ':' + SqliteStatement.SQLITE_FLOAT + ':' + \
                     SqliteStatement.SQLITE_TEXT + ':' + SqliteStatement.SQLITE_BLOB + ':' + \
                     SqliteStatement.SQLITE_NULL"
                )
                .expect("column types"),
            Variant::String("1:2:3:4:5".to_string())
        );
        assert_eq!(
            engine
                .execute_expression(
                    "inline.tjs",
                    "SqliteThread.INIT + ':' + SqliteThread.WORKING + ':' + SqliteThread.DONE"
                )
                .expect("thread states"),
            Variant::String("0:1:2".to_string())
        );
    }

    #[test]
    fn a_memory_database_round_trips_values_and_reports_errors() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var db = new Sqlite(":memory:");
                var created = db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, score REAL, data OCTET)");
                var inserted = db.exec("INSERT INTO t (name, score, data) VALUES (?, ?, ?)", ["kira", 1.5, <% 01 02 %>]);
                var id = db.lastInsertRowId;
                var name = db.execValue("SELECT name FROM t WHERE id = ?", [id]);
                var score = db.execValue("SELECT score FROM t WHERE id = ?", [id]);
                var count = db.execValue("SELECT count(*) FROM t");
                var named = db.execValue("SELECT name FROM t WHERE id = :id", %[":id" => id]);
                var broken = db.exec("SELECT * FROM nope");
                return created + ":" + inserted + ":" + id + ":" + name + ":" + score + ":" +
                    count + ":" + named + ":" + broken + ":" + db.errorCode + ":" +
                    (db.errorMessage.length > 0);
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("1:1:1:kira:1.5:1:kira:0:1:1".to_string())
        );
    }

    #[test]
    fn exec_calls_back_with_one_argument_per_column() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var db = new Sqlite(":memory:");
                db.exec("CREATE TABLE t (a INTEGER, b TEXT)");
                db.exec("INSERT INTO t VALUES (1, 'x')");
                db.exec("INSERT INTO t VALUES (2, 'y')");
                var seen = [];
                db.exec("SELECT a, b FROM t ORDER BY a", void, function(a, b) {
                    seen.add(a + "=" + b);
                });
                return seen.join(",");
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("1=x,2=y".to_string()));
    }

    #[test]
    fn a_real_file_database_is_created_reopened_and_read_only() {
        let root = test_root("file");
        fs::create_dir_all(root.join("savedata")).expect("create savedata");
        let mut engine = test_engine(&root);
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var db = new Sqlite("savedata/scores.db");
                var created = db.exec("CREATE TABLE s (v TEXT)");
                var inserted = db.exec("INSERT INTO s VALUES ('x')");
                var again = new Sqlite("savedata/scores.db");
                var value = again.execValue("SELECT v FROM s");
                var ro = new Sqlite("savedata/scores.db", true);
                var read = ro.execValue("SELECT v FROM s");
                var denied = ro.exec("INSERT INTO s VALUES ('nope')");
                var code = ro.errorCode;
                return created + ":" + inserted + ":" + value + ":" + read + ":" + denied + ":" + code;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("1:1:x:x:0:8".to_string()),
            "SQLITE_READONLY (8) is the write to a readonly connection"
        );
        assert!(
            root.join("savedata/scores.db").is_file(),
            "the database was created under the project root"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn an_archive_hosted_database_opens_read_only_and_never_locally() {
        // A database image in a memory storage stands in for an XP3 member:
        // the reference's xp3 VFS reads it through `TVPCreateIStream`, this
        // engine through the storage layer.
        let root = test_root("archive-source");
        let path = root.join("member.db");
        {
            let mut engine = test_engine(&root);
            engine
                .execute_script(
                    "inline.tjs",
                    r#"
                    var db = new Sqlite("member.db");
                    db.exec("CREATE TABLE t (v TEXT)");
                    db.exec("INSERT INTO t VALUES ('inside')");
                    "#,
                )
                .expect("create database");
        }
        let bytes = fs::read(&path).expect("read database image");
        fs::remove_file(&path).expect("remove local copy");
        let mut engine = memory_engine("archive", [("member.db", bytes)]);
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var ro = new Sqlite("member.db", true);
                var read = ro.execValue("SELECT v FROM t");
                var denied = ro.exec("INSERT INTO t VALUES ('nope')");
                return read + ":" + denied + ":" + ro.errorCode;
                "#,
            )
            .expect("readonly open");
        assert_eq!(value, Variant::String("inside:0:8".to_string()));

        let error = engine
            .execute_expression("inline.tjs", "new Sqlite(\"member.db\")")
            .expect_err("a read-write open cannot reach an archive member");
        assert!(
            error
                .message
                .contains("Unable to open the database file, try readonly if exists: member.db"),
            "unexpected message: {}",
            error.message
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn the_readonly_temp_copy_is_private_and_dies_with_the_connection() {
        let first = write_temp_copy(b"copy one").expect("first copy");
        let second = write_temp_copy(b"copy two").expect("second copy");
        assert_ne!(
            first, second,
            "every archive-backed open gets its own copy name"
        );
        assert_eq!(fs::read(&first).expect("read first"), b"copy one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&second)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the copy must not be world readable");
        }

        // The copy belongs to the connection: dropping it removes the file.
        // (The null handle keeps the sqlite3_close path out of this test.)
        let connection = Connection {
            db: Db(std::ptr::null_mut()),
            temp_path: Some(first.clone()),
        };
        drop(connection);
        assert!(
            !first.exists(),
            "the temp copy was removed with the connection"
        );
        fs::remove_file(&second).expect("cleanup");
    }

    #[test]
    fn statements_step_read_and_reset() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var db = new Sqlite(":memory:");
                db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)");
                db.exec("INSERT INTO t (name) VALUES ('a')");
                db.exec("INSERT INTO t (name) VALUES ('b')");
                var st = new SqliteStatement(db, "SELECT id, name FROM t ORDER BY id");
                var rows = [];
                while (st.step()) {
                    rows.add(st.get(0) + ":" + st.get("name") + ":" + st.name + ":" +
                        st.getName(1) + ":" + st.getType(0) + ":" + st.isNull(1));
                }
                var countAfter = st.count;
                var sql = st.sql;
                var columns = st.columnCount;
                var all = new SqliteStatement(db, "SELECT id, name FROM t ORDER BY id DESC");
                all.step();
                var row = all.get();
                all.close();
                return rows.join(";") + "|" + countAfter + ":" + columns + ":" + sql + "|" +
                    row.join(",");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String(
                "1:a:a:name:1:0;2:b:b:name:1:0|0:2:SELECT id, name FROM t ORDER BY id|2,b"
                    .to_string()
            )
        );
    }

    #[test]
    fn a_column_named_like_a_statement_member_does_not_shadow_it() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var db = new Sqlite(":memory:");
                db.exec("CREATE TABLE t (v TEXT)");
                db.exec("INSERT INTO t VALUES ('x')");
                // Columns named after statement members: the reference's
                // `missing` handler only answers names the class chain misses,
                // so `st.count` stays `sqlite3_data_count` and `st.step()`
                // stays a method.
                var st = new SqliteStatement(db,
                    "SELECT 'zzz' AS step, 'yyy' AS plain, count(*) AS count, v AS sql FROM t");
                var has_row = st.step();
                var countValue = st.count;
                var plain = st.plain;
                var sqlProperty = st.sql;
                var more = st.step();
                return has_row + ":" + countValue + ":" + plain + ":" + sqlProperty + ":" + more;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String(
                "1:4:yyy:SELECT 'zzz' AS step, 'yyy' AS plain, count(*) AS count, v AS sql FROM t:0"
                    .to_string()
            ),
            "a colliding column keeps the class member; a free name reads the column"
        );
    }

    #[test]
    fn get_answers_the_default_for_a_name_that_is_not_a_column() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var db = new Sqlite(":memory:");
                var st = new SqliteStatement(db, "SELECT 1 AS one");
                st.step();
                // `sqlite3_column_type(stmt, -1)` is SQLITE_NULL, so the
                // reference answers the default (`Main.cpp:519-530`).
                return (st.get("absent") === void) + ":" + st.get("absent", "fallback");
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("1:fallback".to_string()));
    }

    #[test]
    fn statement_bind_and_bind_at_use_the_reference_positions() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var db = new Sqlite(":memory:");
                db.exec("CREATE TABLE t (a INTEGER, b TEXT)");
                var st = new SqliteStatement(db, "INSERT INTO t VALUES (:a, :b)");
                // A named parameter is bound by its full SQL spelling, like
                // `sqlite3_bind_parameter_index` (`Main.cpp:57-67`).
                var bound = st.bind(%[":a" => 7, ":b" => "seven"]);
                var at = st.bindAt(8, ":a");
                var at2 = st.bindAt("eight", ":b");
                var ran = st.exec();
                st.reset();
                var read = db.execValue("SELECT a FROM t");
                var read2 = db.execValue("SELECT b FROM t");
                return bound + ":" + at + ":" + at2 + ":" + ran + ":" + read + ":" + read2;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("0:0:0:101:8:eight".to_string()),
            "exec answers SQLITE_DONE (101) when the INSERT finishes"
        );
    }

    #[test]
    fn statement_errors_match_the_reference_messages() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        let error = engine
            .execute_expression("inline.tjs", "new SqliteStatement(5, \"SELECT 1\")")
            .expect_err("first argument must be a Sqlite");
        assert!(
            error.message.contains("use Sqlite class Object"),
            "unexpected message: {}",
            error.message
        );
        let error = engine
            .execute_script(
                "inline.tjs",
                "var db = new Sqlite(\":memory:\");\n\
                 return new SqliteStatement(db, \"SELECT FROM nope\");",
            )
            .expect_err("a bad statement cannot open");
        assert!(
            error.message.contains("failed to open state"),
            "unexpected message: {}",
            error.message
        );
    }

    #[test]
    fn transactions_commit_and_roll_back() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var db = new Sqlite(":memory:");
                db.exec("CREATE TABLE t (v TEXT)");
                db.exec("INSERT INTO t VALUES ('keep')");
                var began = db.begin();
                db.exec("INSERT INTO t VALUES ('undo')");
                var rolled = db.rollback();
                var afterRollback = db.execValue("SELECT count(*) FROM t");
                db.begin();
                db.exec("INSERT INTO t VALUES ('add')");
                var committed = db.commit();
                var afterCommit = db.execValue("SELECT count(*) FROM t");
                return began + ":" + rolled + ":" + afterRollback + ":" + committed + ":" + afterCommit;
                "#,
            )
            .expect("script");
        assert_eq!(value, Variant::String("1:1:1:1:2".to_string()));
    }

    #[test]
    fn cnt_and_ncnt_are_case_insensitive_search_functions() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var db = new Sqlite(":memory:");
                var plain = db.execValue("SELECT cnt('KiraKira', 'kira')");
                var normalized = db.execValue("SELECT ncnt('キラキラ', 'きらきら')");
                var width = db.execValue("SELECT ncnt('ABC', 'ＡＢＣ')");
                var missing = db.execValue("SELECT ncnt('abc', 'zzz')");
                return plain + ":" + normalized + ":" + width + ":" + missing;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("0:1:1:0".to_string()),
            "cnt is a plain substring test, ncnt normalizes width and kana"
        );
    }

    #[test]
    fn the_background_thread_answers_state_progress_and_rows() {
        let root = test_root("thread");
        let mut engine = test_engine(&root);
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                global.progress = [];
                global.rows = [];
                var win = new Window();
                global.db = new Sqlite(":memory:");
                db.exec("CREATE TABLE t (v TEXT)");
                db.exec("INSERT INTO t VALUES ('a')");
                db.exec("INSERT INTO t VALUES ('b')");
                global.th = new SqliteThread(win, db);
                th.onStateChange = function(s) { global.events += "S" + s; };
                th.onProgress = function(n) { global.progress.add(n); };
                global.started = th.select("SELECT v, upper(v) FROM t ORDER BY v");
                "#,
            )
            .expect("script");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "global.started")
                .expect("started"),
            Variant::Integer(1)
        );
        assert!(
            tick_until(&mut engine, "global.th.state", 2),
            "the background select never reached DONE"
        );
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var result = th.selectResult;
                // One array per row, each holding one entry per column
                // (`sqlite3/Main.cpp:884-895`).
                var shape = result.count + ":" + result[0].count + ":" + result[0][0] + ":" +
                    result[0][1] + ":" + result[1][0] + ":" + result[1][1];
                return global.events + "|" + shape + "|" + th.errorCode + "|" +
                    global.progress.join(",");
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("S0S1S2|2:2:a:A:b:B|101|2".to_string()),
            "INIT, WORKING and DONE in order (progress arrives through its own \
             handler); selectResult is an array of row arrays; a successful select \
             leaves the last step's SQLITE_DONE in errorCode like the reference"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn the_background_update_commits_its_rows() {
        let root = test_root("thread-update");
        let mut engine = test_engine(&root);
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.events = "";
                var win = new Window();
                global.db = new Sqlite(":memory:");
                db.exec("CREATE TABLE t (v TEXT)");
                global.th = new SqliteThread(win, db);
                th.progressUpdateCount = 2;
                th.onStateChange = function(s) { global.events += "S" + s; };
                th.onProgress = function(n) { global.events += "P" + n; };
                global.started = th.update("INSERT INTO t VALUES (:v)", [
                    %[":v" => "a"], %[":v" => "b"], %[":v" => "c"], %[":v" => "d"]
                ]);
                "#,
            )
            .expect("script");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "global.started")
                .expect("started"),
            Variant::Integer(1)
        );
        assert!(
            tick_until(&mut engine, "global.th.state", 2),
            "the background update never reached DONE"
        );
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                return global.events + "|" + db.execValue("SELECT count(*) FROM t") + "|" +
                    th.errorCode;
                "#,
            )
            .expect("script");
        assert_eq!(
            value,
            Variant::String("S0S1P2P4P4S2|4|101".to_string()),
            "progress at the 2-row milestone, the final count twice (the reference \
             posts the closing progress even when it coincides with a milestone) \
             and DONE; errorCode keeps the last step's SQLITE_DONE"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn sqlite_thread_validates_its_arguments() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(Sqlite3Plugin).expect("plugin");
        let error = engine
            .execute_script(
                "inline.tjs",
                "var db = new Sqlite(\":memory:\");\nreturn new SqliteThread(5, db);",
            )
            .expect_err("the first argument must be a Window");
        assert!(
            error.message.contains("use Window class Object"),
            "unexpected message: {}",
            error.message
        );
        let error = engine
            .execute_script(
                "inline.tjs",
                "var win = new Window();\nreturn new SqliteThread(win, 5);",
            )
            .expect_err("the second argument must be a Sqlite");
        assert!(
            error.message.contains("use Sqlite class Object"),
            "unexpected message: {}",
            error.message
        );
    }

    #[test]
    fn a_finished_thread_locks_out_a_second_start_until_done_arrives() {
        let root = test_root("thread-running");
        let mut engine = test_engine(&root);
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.win = new Window();
                global.db = new Sqlite(":memory:");
                db.exec("CREATE TABLE t (v TEXT)");
                db.exec("INSERT INTO t VALUES ('a')");
                global.th = new SqliteThread(win, db);
                global.first = th.select("SELECT v FROM t");
                global.second = "";
                try {
                    th.select("SELECT v FROM t");
                } catch (e) {
                    global.second = e.message;
                }
                global.state_after_refusal = th.state;
                "#,
            )
            .expect("script");
        // The worker for the first select has finished by the time the second
        // call runs in the same script, but DONE has not been delivered, so
        // the reference's `already running` check still fires — after the
        // state has moved to INIT (`Main.cpp:700`, `:782-783`).
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "global.first + ':' + global.second")
                .expect("first/second"),
            Variant::String("1:already running".to_string())
        );
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "global.state_after_refusal")
                .expect("state"),
            Variant::Integer(0),
            "a refused select still leaves the state at INIT"
        );
        let db = engine
            .execute_expression("inline.tjs", "global.db")
            .expect("db handle")
            .object_handle()
            .expect("object handle");
        assert!(
            open_statement_count(db) <= 1,
            "the refused select leaked its prepared statement"
        );
        // The first operation still completes: its queued events survived the
        // refused call.
        assert!(
            tick_until(&mut engine, "global.th.state", 2),
            "the first select never reached DONE after the refusal"
        );
        assert_eq!(
            open_statement_count(db),
            0,
            "the statement of the finished select was not finalized"
        );
        // And the thread is usable again, which also proves the connection was
        // not left busy by a leaked statement.
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var started = th.select("SELECT v FROM t");
                return started + ":" + th.state;
                "#,
            )
            .expect("third select");
        assert_eq!(value, Variant::String("1:0".to_string()));
        assert!(tick_until(&mut engine, "global.th.state", 2));
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "th.selectResult[0][0]")
                .expect("row"),
            Variant::String("a".to_string()),
            "the third select still reads the table through the same connection"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    /// Open `sqlite3_stmt`s on a connection, so a test can see a leaked
    /// prepared statement (and the `SQLITE_BUSY` close it would cause).
    fn open_statement_count(instance: krkr_tjs2::runtime::ObjectHandle) -> usize {
        SQLITES.with(|map| {
            let map = map.borrow();
            let state = map.get(&instance).expect("sqlite state");
            let db = state.connection.ptr();
            let mut count = 0;
            let mut stmt = std::ptr::null_mut();
            while {
                stmt = unsafe { ffi::sqlite3_next_stmt(db, stmt) };
                !stmt.is_null()
            } {
                count += 1;
            }
            count
        })
    }

    #[test]
    fn abort_stops_the_worker_and_clears_the_result_array() {
        let root = test_root("thread-abort");
        let mut engine = test_engine(&root);
        engine
            .execute_script(
                "inline.tjs",
                r#"
                var win = new Window();
                var db = new Sqlite(":memory:");
                db.exec("CREATE TABLE t (v TEXT)");
                db.exec("INSERT INTO t VALUES ('a')");
                global.th = new SqliteThread(win, db);
                global.th.select("SELECT v FROM t");
                global.th.abort();
                "#,
            )
            .expect("script");
        assert_eq!(
            engine
                .execute_expression("inline.tjs", "typeof global.th.selectResult")
                .expect("selectResult"),
            Variant::String("void".to_string()),
            "`abort` clears the result array to void like the reference's Clear()"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn readonly_names_that_do_not_exist_report_the_reference_message() {
        let root = test_root("missing");
        let mut engine = test_engine(&root);
        // A name that is not locally accessible and not resolvable at all:
        // SQLite answers CANTOPEN through the connection instead of throwing
        // (`Main.cpp:200-210` runs `sqlite3_open16` and ignores its result).
        let value = engine
            .execute_script(
                "inline.tjs",
                r#"
                var db = new Sqlite("data.xp3>member.db", true);
                return db.errorCode + ":" + (db.errorMessage.length > 0);
                "#,
            )
            .expect("readonly open of a missing archive member");
        assert_eq!(value, Variant::String("14:1".to_string()));
        fs::remove_dir_all(root).expect("cleanup");
    }
}
