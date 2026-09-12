//! `scriptsEx.dll` — extra `Scripts` helpers.
//!
//! Reference: krkrz `src/plugins/win32/scriptsEx/Main.cpp` (628 lines),
//! `NCB_ATTACH_CLASS(ScriptsAdd, Scripts)` plus
//! `NCB_ATTACH_FUNCTION(rehash, Scripts, TJSDoRehash)`. PARQUET links
//! `scriptsEx.dll` by name although the same group is compiled into
//! `PackinOne.dll`, so this module installs the surface under that DLL's own
//! name and takes over the members the engine pre-installs on `Scripts` with
//! the reference's semantics.
//!
//! Behaviour carried over from the reference:
//!
//! - `getObjectKeys`/`getObjectCount` skip hidden members and require an
//!   argument (`TJS_E_BADPARAMCOUNT`).
//! - `getObjectContext` answers the closure's `ObjThis` (null for a plain
//!   object), `isNullContext` tests exactly that.
//! - `equalStruct` compares arrays element-wise with an equal length, and
//!   dictionaries by comparing every member of the left object against the
//!   right one **while skipping members the right object does not have**;
//!   everything else is a strict (type-aware) compare.
//! - `equalStructNumericLoose` differs only in comparing two numbers loosely,
//!   so `1` and `1.0` are equal.
//! - `foreach(collection, func, ...)` calls `func(key, value, ...)` in member
//!   order, returns the first non-void callback result, and runs an unbound
//!   callback in the `foreach` receiver's context.
//! - `getMD5HashString` answers the 32 lowercase hex digits of an octet's
//!   MD5 digest.
//! - `clone` deep-copies arrays and dictionaries.
//!
//! Deliberate divergences:
//!
//! - Member enumeration follows this engine's object model, which keeps
//!   members in sorted order; the reference enumerates them in hash order.
//! - `rehash` is a no-op: the engine's object model has no hash table to
//!   rebuild, and the reference's only observable effect is that rebuild.
//! - `clone` delegates to an object's own `clone` member when it has one
//!   (the reference's `FuncCall(...) == TJS_S_TRUE` test can never be true, a
//!   dead branch in the original; delegating is what the sibling `PackinOne`
//!   implementation does, and it is what the code intends).
//! - `clone` tracks already-copied objects, so a self-referential structure
//!   terminates instead of recursing forever.
//! - `getMD5HashString` also accepts a String (hashed as its UTF-8 bytes);
//!   the reference requires an octet.

use std::collections::{BTreeMap, BTreeSet};

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{Closure, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Scripts.getObjectKeys/getObjectCount/getObjectContext/isNullContext/equalStruct/\
              equalStructNumericLoose/foreach/getMD5HashString/clone/rehash",
    notes: "All ten reference members are functional, including MD5 over octets and the \
            right-side-skipping equalStruct rule; rehash is a no-op because the engine's object \
            model has no hash table, and enumeration order is the engine's sorted member order.",
    install: |engine| engine.register_plugin(ScriptsExPlugin),
};

pub struct ScriptsExPlugin;

impl KrkrPlugin for ScriptsExPlugin {
    fn name(&self) -> &str {
        "scriptsEx.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_scripts_ex(runtime);
        runtime
            .host_mut()
            .log("scriptsEx.dll registered: Scripts object/member/struct helpers");
        Ok(())
    }
}

/// The ten members the reference attaches to `Scripts`. Shared with the
/// module's surface test.
#[cfg(test)]
const SCRIPTS_MEMBERS: &[&str] = &[
    "getObjectKeys",
    "getObjectCount",
    "getObjectContext",
    "isNullContext",
    "equalStruct",
    "equalStructNumericLoose",
    "foreach",
    "getMD5HashString",
    "clone",
    "rehash",
];

fn install_scripts_ex(runtime: &mut Runtime<KrkrHost>) {
    let scripts = match runtime.global_member("Scripts") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "Scripts");
            runtime.set_global_member("Scripts", Variant::Object(handle));
            handle
        }
    };
    for (name, handler) in [
        (
            "getObjectKeys",
            scripts_get_object_keys
                as fn(
                    &mut Runtime<KrkrHost>,
                    Option<ObjectHandle>,
                    Vec<Variant>,
                ) -> Result<Variant>,
        ),
        ("getObjectCount", scripts_get_object_count),
        ("getObjectContext", scripts_get_object_context),
        ("isNullContext", scripts_is_null_context),
        ("equalStruct", scripts_equal_struct),
        (
            "equalStructNumericLoose",
            scripts_equal_struct_numeric_loose,
        ),
        ("foreach", scripts_foreach),
        ("getMD5HashString", scripts_get_md5_hash_string),
        ("clone", scripts_clone),
        ("rehash", scripts_rehash),
    ] {
        runtime.register_object_native(scripts, name, handler);
    }
}

// ---------------------------------------------------------------------------
// Shared helpers

/// Members the reference's `TJS_IGNOREPROP` enumeration leaves out: the
/// object's own bookkeeping (`__`-prefixed) and the built-in Array/Dictionary
/// methods (including the ones `savestruct.dll` installs), which TJS registers
/// as hidden. Shared with the `savestruct.dll` writer.
pub(crate) fn is_hidden_member_name(key: &str) -> bool {
    key.starts_with("__")
        || matches!(
            key,
            "clear"
                | "assign"
                | "assignStruct"
                | "saveStruct"
                | "loadStruct"
                | "load"
                | "save"
                | "add"
                | "push"
                | "split"
                | "insert"
                | "erase"
                | "remove"
                | "pop"
                | "shift"
                | "unshift"
                | "join"
                | "sort"
                | "reverse"
                | "find"
                | "count"
                | "length"
                | "save2"
                | "saveStruct2"
                | "toStructString"
        )
}

/// The object a value refers to, with the reference's conversion error when
/// the argument is a scalar.
fn required_object(args: &[Variant], index: usize, what: &str) -> Result<ObjectHandle> {
    let Some(value) = args.get(index) else {
        return Err(TjsError::bad_param_count());
    };
    value.object_handle().ok_or_else(|| {
        TjsError::runtime(format!(
            "{what} requires an object argument, not {:?}",
            value
        ))
    })
}

fn visible_members(runtime: &Runtime<KrkrHost>, object: ObjectHandle) -> Vec<(String, Variant)> {
    runtime
        .object_members(object)
        .into_iter()
        .filter(|(key, _)| !is_hidden_member_name(key))
        .collect()
}

// ---------------------------------------------------------------------------
// Object member helpers

fn scripts_get_object_keys(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let object = required_object(&args, 0, "Scripts.getObjectKeys")?;
    let keys = visible_members(runtime, object)
        .into_iter()
        .map(|(key, _)| Variant::String(key))
        .collect();
    Ok(Variant::Object(runtime.alloc_array_object(keys)))
}

fn scripts_get_object_count(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let object = required_object(&args, 0, "Scripts.getObjectCount")?;
    // The reference asks the object for `GetCount`, which counts the array's
    // elements and the object's members (hidden ones included).
    let count = match runtime.array_elements(object) {
        Some(elements) => elements.len(),
        None => runtime.object_members(object).len(),
    };
    Ok(Variant::Integer(count as i64))
}

fn scripts_get_object_context(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(value) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    match value {
        // `getObjectContext` answers the closure's `ObjThis`; a plain object
        // has none, and the reference's null-object variant reads as null.
        Variant::Closure(closure) => Ok(match closure.this_obj {
            Some(handle) => Variant::Object(handle),
            None => Variant::Null,
        }),
        Variant::Object(_) | Variant::Null | Variant::Void => Ok(Variant::Null),
        other => Err(TjsError::runtime(format!(
            "Scripts.getObjectContext requires an object argument, not {other:?}"
        ))),
    }
}

fn scripts_is_null_context(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(value) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    let is_null = match value {
        Variant::Closure(closure) => closure.this_obj.is_none(),
        Variant::Object(_) | Variant::Null | Variant::Void => true,
        other => {
            return Err(TjsError::runtime(format!(
                "Scripts.isNullContext requires an object argument, not {other:?}"
            )));
        }
    };
    Ok(Variant::Integer(i64::from(is_null)))
}

// ---------------------------------------------------------------------------
// Struct comparison

fn scripts_equal_struct(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let lhs = args.first().cloned().unwrap_or_default();
    let rhs = args.get(1).cloned().unwrap_or_default();
    let mut compared = BTreeSet::new();
    Ok(Variant::Integer(i64::from(equal_struct(
        runtime,
        &lhs,
        &rhs,
        false,
        &mut compared,
    ))))
}

fn scripts_equal_struct_numeric_loose(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let lhs = args.first().cloned().unwrap_or_default();
    let rhs = args.get(1).cloned().unwrap_or_default();
    let mut compared = BTreeSet::new();
    Ok(Variant::Integer(i64::from(equal_struct(
        runtime,
        &lhs,
        &rhs,
        true,
        &mut compared,
    ))))
}

fn is_number(value: &Variant) -> bool {
    matches!(value, Variant::Integer(_) | Variant::Real(_))
}

/// `equalStruct` / `equalStructNumericLoose`. `compared` holds the object
/// pairs already being compared, so a self-referential structure terminates
/// (the reference recurses forever on one).
fn equal_struct(
    runtime: &mut Runtime<KrkrHost>,
    lhs: &Variant,
    rhs: &Variant,
    numeric_loose: bool,
    compared: &mut BTreeSet<(usize, usize)>,
) -> bool {
    if let (Some(left), Some(right)) = (lhs.object_handle(), rhs.object_handle()) {
        if left == right {
            return true;
        }
        if !compared.insert((left.0, right.0)) {
            return true;
        }
        return equal_struct_objects(runtime, left, right, numeric_loose, compared);
    }
    if numeric_loose && is_number(lhs) && is_number(rhs) {
        return lhs.normal_eq(rhs);
    }
    // Everything that is not a pair of objects (and not two loosely compared
    // numbers) is the reference's `DiscernCompare`: type-aware equality.
    lhs.discern_eq(rhs)
}

fn equal_struct_objects(
    runtime: &mut Runtime<KrkrHost>,
    left: ObjectHandle,
    right: ObjectHandle,
    numeric_loose: bool,
    compared: &mut BTreeSet<(usize, usize)>,
) -> bool {
    // Two different functions compare by identity, which the caller already
    // rejected.
    if runtime.object_is_callable(left) && runtime.object_is_callable(right) {
        return false;
    }
    if let (Some(left_items), Some(right_items)) = (
        runtime.array_elements(left).map(Vec::from),
        runtime.array_elements(right).map(Vec::from),
    ) {
        if left_items.len() != right_items.len() {
            return false;
        }
        return left_items
            .iter()
            .zip(right_items.iter())
            .all(|(left, right)| equal_struct(runtime, left, right, numeric_loose, compared));
    }
    if runtime.is_dictionary_instance(left) && runtime.is_dictionary_instance(right) {
        let left_members = visible_members(runtime, left);
        let right_members = visible_members(runtime, right);
        if left_members.len() != right_members.len() {
            return false;
        }
        return left_members.iter().all(|(key, value)| {
            match right_members.iter().find(|(other, _)| other == key) {
                Some((_, other)) => equal_struct(runtime, value, other, numeric_loose, compared),
                // A member the right object does not have is skipped, exactly
                // as `DictMemberCompareCaller` does when `PropGet` fails.
                None => true,
            }
        });
    }
    false
}

// ---------------------------------------------------------------------------
// foreach

fn scripts_foreach(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    if args.len() < 2 {
        return Err(TjsError::bad_param_count());
    }
    let receiver = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle));
    let Some(collection) = args[0].object_handle() else {
        return Ok(Variant::Void);
    };
    // An unbound callback runs in the receiver's context, which is what the
    // reference does with `functhis` (`Main.cpp:455-460`).
    let callback = match args[1].clone() {
        Variant::Closure(closure) => {
            Variant::Closure(Closure::new(closure.object, closure.this_obj.or(receiver)))
        }
        Variant::Object(handle) => Variant::Closure(Closure::new(handle, receiver)),
        _ => return Ok(Variant::Void),
    };
    let entries: Vec<(Variant, Variant)> = match runtime.array_elements(collection) {
        Some(elements) => elements
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, value)| (Variant::Integer(index as i64), value))
            .collect(),
        None => visible_members(runtime, collection)
            .into_iter()
            .map(|(key, value)| (Variant::String(key), value))
            .collect(),
    };
    for (key, value) in entries {
        let mut call_args = vec![key, value];
        call_args.extend(args.iter().skip(2).cloned());
        let result = runtime.call_function(callback.clone(), call_args)?;
        if !matches!(result, Variant::Void) {
            return Ok(result);
        }
    }
    Ok(Variant::Void)
}

// ---------------------------------------------------------------------------
// MD5

fn scripts_get_md5_hash_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(value) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    // The reference hashes an octet; a String is accepted as its UTF-8 bytes.
    let bytes = match value {
        Variant::Octet(bytes) => bytes.clone(),
        Variant::String(text) => text.clone().into_bytes(),
        other => {
            return Err(TjsError::runtime(format!(
                "Scripts.getMD5HashString requires an octet, not {other:?}"
            )));
        }
    };
    Ok(Variant::String(md5_hex(&bytes)))
}

/// MD5 (RFC 1321) as `TVP_md5_*` computes it, lower-case hex.
pub(crate) fn md5_hex(input: &[u8]) -> String {
    let digest = md5(input);
    let mut out = String::with_capacity(32);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub(crate) fn md5(input: &[u8]) -> [u8; 16] {
    const SHIFT: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const SINE: [u32; 64] = [
        0xd76a_a478,
        0xe8c7_b756,
        0x2420_70db,
        0xc1bd_ceee,
        0xf57c_0faf,
        0x4787_c62a,
        0xa830_4613,
        0xfd46_9501,
        0x6980_98d8,
        0x8b44_f7af,
        0xffff_5bb1,
        0x895c_d7be,
        0x6b90_1122,
        0xfd98_7193,
        0xa679_438e,
        0x49b4_0821,
        0xf61e_2562,
        0xc040_b340,
        0x265e_5a51,
        0xe9b6_c7aa,
        0xd62f_105d,
        0x0244_1453,
        0xd8a1_e681,
        0xe7d3_fbc8,
        0x21e1_cde6,
        0xc337_07d6,
        0xf4d5_0d87,
        0x455a_14ed,
        0xa9e3_e905,
        0xfcef_a3f8,
        0x676f_02d9,
        0x8d2a_4c8a,
        0xfffa_3942,
        0x8771_f681,
        0x6d9d_6122,
        0xfde5_380c,
        0xa4be_ea44,
        0x4bde_cfa9,
        0xf6bb_4b60,
        0xbebf_bc70,
        0x289b_7ec6,
        0xeaa1_27fa,
        0xd4ef_3085,
        0x0488_1d05,
        0xd9d4_d039,
        0xe6db_99e5,
        0x1fa2_7cf8,
        0xc4ac_5665,
        0xf429_2244,
        0x432a_ff97,
        0xab94_23a7,
        0xfc93_a039,
        0x655b_59c3,
        0x8f0c_cc92,
        0xffef_f47d,
        0x8584_5dd1,
        0x6fa8_7e4f,
        0xfe2c_e6e0,
        0xa301_4314,
        0x4e08_11a1,
        0xf753_7e82,
        0xbd3a_f235,
        0x2ad7_d2bb,
        0xeb86_d391,
    ];

    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;

    let mut message = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in message.chunks_exact(64) {
        let mut words = [0u32; 16];
        for (index, word) in words.iter_mut().enumerate() {
            let offset = index * 4;
            *word = u32::from_le_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for index in 0..64 {
            let (mix, word) = match index {
                0..=15 => ((b & c) | (!b & d), index),
                16..=31 => ((d & b) | (!d & c), (5 * index + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a
                .wrapping_add(mix)
                .wrapping_add(SINE[index])
                .wrapping_add(words[word]);
            b = b.wrapping_add(sum.rotate_left(SHIFT[index]));
            a = temp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = [0u8; 16];
    for (index, value) in [a0, b0, c0, d0].into_iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    digest
}

// ---------------------------------------------------------------------------
// clone / rehash

fn scripts_clone(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let value = args.first().cloned().unwrap_or_default();
    clone_scripts_value(runtime, &value, &mut BTreeMap::new())
}

fn clone_scripts_value(
    runtime: &mut Runtime<KrkrHost>,
    value: &Variant,
    cloned: &mut BTreeMap<ObjectHandle, ObjectHandle>,
) -> Result<Variant> {
    // `this`-born arguments arrive as self-bound closures; clone the object
    // behind the binding.
    let Some(source) = value.object_handle() else {
        return Ok(value.clone());
    };
    if let Some(destination) = cloned.get(&source) {
        return Ok(Variant::Object(*destination));
    }
    if let Some(elements) = runtime.array_elements(source).map(Vec::from) {
        let destination = runtime.alloc_array_object(Vec::new());
        cloned.insert(source, destination);
        for element in elements {
            let element = clone_scripts_value(runtime, &element, cloned)?;
            runtime.array_push(destination, element);
        }
        return Ok(Variant::Object(destination));
    }
    if runtime.is_dictionary_instance(source) {
        let Some(destination) = new_dictionary(runtime) else {
            return Ok(value.clone());
        };
        cloned.insert(source, destination);
        for (name, member) in visible_members(runtime, source) {
            let member = clone_scripts_value(runtime, &member, cloned)?;
            runtime.set_object_member(destination, name, member);
        }
        return Ok(Variant::Object(destination));
    }
    // An object with its own `clone` member copies through it.
    if !matches!(runtime.object_member(source, "clone"), Variant::Void)
        && let Ok(result) = runtime.call_object_method(source, "clone", Vec::new())
    {
        return Ok(result);
    }
    Ok(value.clone())
}

fn new_dictionary(runtime: &mut Runtime<KrkrHost>) -> Option<ObjectHandle> {
    runtime
        .call_function(runtime.global_member("Dictionary"), Vec::new())
        .ok()?
        .object_handle()
}

/// `TJSDoRehash` only bumps the engine's global "rebuild hash magic"
/// (`tjsObject.cpp:362`); this object model keeps members in a B-tree with no
/// hash table to rebuild, so the member exists and does nothing observable.
fn scripts_rehash(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::*;

    #[test]
    fn every_reference_member_is_installed() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ScriptsExPlugin).expect("plugin");
        for name in SCRIPTS_MEMBERS {
            let value = engine
                .execute_expression("inline.tjs", &format!("typeof Scripts.{name}"))
                .expect("member probe");
            assert_ne!(
                value,
                Variant::String("undefined".to_string()),
                "Scripts.{name} is not installed"
            );
        }
    }

    #[test]
    fn object_keys_count_and_context_match_the_reference() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ScriptsExPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var data = %[first => 1, second => 2];\n\
                     var keys = Scripts.getObjectKeys(data);\n\
                     keys.sort();\n\
                     var plain = function() {};\n\
                     var bound = (Scripts.getObjectContext incontextof Scripts);\n\
                     return keys.join(\",\") + \":\" + Scripts.getObjectCount(data) + \":\" +\n\
                         Scripts.isNullContext(plain) + \":\" + Scripts.isNullContext(bound) + \":\" +\n\
                         (Scripts.getObjectContext(data) === null);\n\
                 })()",
            )
            .expect("object member helpers");

        assert_eq!(value, Variant::String("first,second:2:1:0:1".to_string()));
    }

    #[test]
    fn scalar_arguments_are_rejected_where_the_reference_needs_an_object() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ScriptsExPlugin).expect("plugin");
        let error = engine
            .execute_expression("inline.tjs", "Scripts.getObjectKeys(5)")
            .expect_err("a scalar has no members to enumerate");
        assert!(
            error.message.contains("requires an object argument"),
            "unexpected message: {}",
            error.message
        );

        let error = engine
            .execute_expression("inline.tjs", "Scripts.getMD5HashString(%[a => 1])")
            .expect_err("a dictionary has no digest");
        assert!(
            error.message.contains("requires an octet"),
            "unexpected message: {}",
            error.message
        );
    }

    #[test]
    fn object_helpers_require_arguments() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ScriptsExPlugin).expect("plugin");
        for call in [
            "Scripts.getObjectKeys()",
            "Scripts.getObjectCount()",
            "Scripts.getObjectContext()",
            "Scripts.isNullContext()",
            "Scripts.foreach(%[a => 1])",
            "Scripts.getMD5HashString()",
        ] {
            let error = engine
                .execute_expression("inline.tjs", call)
                .expect_err("a missing argument must fail");
            assert_eq!(error.message, "Invalid argument count", "{call}");
        }
    }

    #[test]
    fn equal_struct_skips_members_the_right_object_lacks() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ScriptsExPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var out = \"\";\n\
                     out += Scripts.equalStruct(%[a => 1, b => 2], %[a => 1, c => 3]);\n\
                     out += \":\" + Scripts.equalStruct(%[a => 1], %[]);\n\
                     out += \":\" + Scripts.equalStruct([1, 2], [1, 2]);\n\
                     out += \":\" + Scripts.equalStruct([1, 2], [1, 3]);\n\
                     out += \":\" + Scripts.equalStruct(1, 1.0);\n\
                     out += \":\" + Scripts.equalStructNumericLoose(1, 1.0);\n\
                     out += \":\" + Scripts.equalStructNumericLoose(1.5, 2);\n\
                     out += \":\" + Scripts.equalStruct(\"text\", \"text\");\n\
                     return out;\n\
                 })()",
            )
            .expect("struct comparison");

        assert_eq!(value, Variant::String("1:0:1:0:0:1:0:1".to_string()));
    }

    #[test]
    fn foreach_walks_arrays_and_dictionaries_and_aborts_on_a_result() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ScriptsExPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     global.__seen = \"\";\n\
                     global.__keys = \"\";\n\
                     Scripts.foreach([\"a\", \"b\"], function(key, value) {\n\
                         global.__seen += key + \"=\" + value + \";\";\n\
                     });\n\
                     var data = %[x => 1, y => 2];\n\
                     var stop = Scripts.foreach(data, function(key, value) {\n\
                         global.__keys += key;\n\
                         if (key == \"x\") return \"stopped\";\n\
                     });\n\
                     return global.__seen + \":\" + global.__keys + \":\" + stop;\n\
                 })()",
            )
            .expect("foreach");

        assert_eq!(value, Variant::String("0=a;1=b;:x:stopped".to_string()));
    }

    #[test]
    fn md5_hash_string_matches_known_digests() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );

        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ScriptsExPlugin).expect("plugin");
        let value = engine
            .execute_expression("inline.tjs", "Scripts.getMD5HashString(<%61 62 63 %>)")
            .expect("digest an octet");
        assert_eq!(
            value,
            Variant::String("900150983cd24fb0d6963f7d28e17f72".to_string())
        );
    }

    #[test]
    fn clone_deep_copies_arrays_and_dictionaries() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ScriptsExPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var source = %[nested => [1, %[value => 2]]];\n\
                     var copy = Scripts.clone(source);\n\
                     copy.nested[0] = 9;\n\
                     copy.nested[1].value = 7;\n\
                     return source.nested[0] + \":\" + source.nested[1].value + \":\" +\n\
                         copy.nested[0] + \":\" + copy.nested[1].value;\n\
                 })()",
            )
            .expect("clone a structure");

        assert_eq!(value, Variant::String("1:2:9:7".to_string()));
    }

    #[test]
    fn clone_delegates_to_an_object_clone_member() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ScriptsExPlugin).expect("plugin");
        engine
            .execute_script(
                "inline.tjs",
                "class Copyable {\n\
                     var value;\n\
                     function Copyable(value) { this.value = value; }\n\
                     function clone() { return new Copyable(this.value + 1); }\n\
                 }",
            )
            .expect("define the class");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() {\n\
                     var copy = Scripts.clone(new Copyable(4));\n\
                     return copy.value;\n\
                 })()",
            )
            .expect("clone through a clone member");

        assert_eq!(value, Variant::Integer(5));
    }

    #[test]
    fn rehash_is_installed_and_returns_void() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(ScriptsExPlugin).expect("plugin");
        let value = engine
            .execute_expression(
                "inline.tjs",
                "(function() { Scripts.rehash(%[a => 1]); return \"ok\"; })()",
            )
            .expect("rehash");
        assert_eq!(value, Variant::String("ok".to_string()));
    }
}
