use std::collections::BTreeMap;

use crate::bytecode::BytecodeContextType;
use crate::error::Result;
use crate::runtime::value::{ObjectHandle, Variant};

#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    pub kind: ObjectKind,
    pub members: BTreeMap<String, Variant>,
    pub class_infos: Vec<String>,
    pub super_class: Option<ObjectHandle>,
    pub call_missing: bool,
    pub processing_missing: bool,
    pub missing_name: String,
    pub valid: bool,
    pub invalidating: bool,
}

impl Default for Object {
    fn default() -> Self {
        Self {
            kind: ObjectKind::Ordinary,
            members: BTreeMap::new(),
            class_infos: Vec::new(),
            super_class: None,
            call_missing: false,
            processing_missing: false,
            missing_name: "missing".to_string(),
            valid: true,
            invalidating: false,
        }
    }
}

impl Object {
    pub fn new(kind: ObjectKind) -> Self {
        Self {
            kind,
            ..Self::default()
        }
    }

    pub fn array(elements: Vec<Variant>) -> Self {
        let mut object = Self::new(ObjectKind::Array { elements });
        object.sync_array_members();
        object
    }

    pub fn get_raw(&self, name: &str) -> Option<Variant> {
        match &self.kind {
            ObjectKind::Array { elements } => {
                if name == "count" || name == "length" {
                    Some(Variant::Integer(elements.len() as i64))
                } else if parse_array_index_name(name).is_some() {
                    // `tTJSArrayObject::PropGet` (`tjsArray.cpp:1601`) sends any
                    // numeric member name to `PropGetByNum`, so an array read
                    // never reaches the ordinary member table; `ARRAY_GET_VAL`
                    // (`tjsArray.cpp:1404`) answers void for an index outside
                    // the array, counting a negative index from the end.
                    Some(
                        array_index(name, elements.len())
                            .and_then(|index| elements.get(index))
                            .cloned()
                            .unwrap_or_default(),
                    )
                } else {
                    self.members.get(name).cloned()
                }
            }
            _ => self.members.get(name).cloned(),
        }
    }

    /// True when `name` addresses an element this Array has no value for.
    /// `ARRAY_GET_VAL` (`tjsArray.cpp:1407`) hands that to a
    /// `TJS_MEMBERMUSTEXIST` reader -- `typeof arr[9]` -- as member-not-found,
    /// while a plain read sees void.
    pub fn array_index_missing(&self, name: &str) -> bool {
        let ObjectKind::Array { elements } = &self.kind else {
            return false;
        };
        parse_array_index_name(name).is_some() && array_index(name, elements.len()).is_none()
    }

    pub fn get(&self, name: &str) -> Variant {
        self.get_raw(name).unwrap_or_default()
    }

    /// `tTJSArrayObject::PropSetByNum` (`tjsArray.cpp:1634`): an index still
    /// negative after wrapping from the end is member-not-found, while writing
    /// past the end is how an array grows.
    pub fn array_negative_index_after_wrap(&self, name: &str) -> bool {
        let ObjectKind::Array { elements } = &self.kind else {
            return false;
        };
        let Some(index) = parse_array_index_name(name) else {
            return false;
        };
        index < 0 && index + (elements.len() as i64) < 0
    }

    pub fn set(&mut self, name: impl Into<String>, value: Variant) {
        let name = name.into();
        if let ObjectKind::Array { elements } = &mut self.kind
            && let Some(index) = array_index_for_set(&name, elements.len())
        {
            let old_len = elements.len();
            if index >= old_len {
                elements.resize(index + 1, Variant::Void);
                for (offset, item) in elements[old_len..].iter().enumerate() {
                    self.members
                        .insert((old_len + offset).to_string(), item.clone());
                }
            }
            elements[index] = value.clone();
            let canonical_name = index.to_string();
            if name != canonical_name {
                // Full array synchronization canonicalizes numeric member
                // names, so do not leave aliases such as `01` behind.
                self.members.remove(&name);
            }
            self.members.insert(canonical_name, value);
            self.sync_array_length_members();
            return;
        }
        if let ObjectKind::Array { elements } = &mut self.kind {
            if name == "count" || name == "length" {
                let len = value.to_integer().unwrap_or(0).max(0) as usize;
                elements.resize(len, Variant::Void);
                self.sync_array_members();
                return;
            }
        }
        self.members.insert(name, value);
    }

    pub fn delete(&mut self, name: &str) -> bool {
        let removed = self.members.remove(name).is_some();
        if let ObjectKind::Array { elements } = &mut self.kind
            && let Some(index) = array_index(name, elements.len())
            && index < elements.len()
        {
            // KRKR's Array.delete is an erase operation: subsequent elements
            // shift left and the array length decreases. This intentionally
            // differs from JavaScript's sparse-array delete semantics.
            elements.remove(index);
            self.sync_array_members();
            return true;
        }
        removed
    }

    pub fn array_elements(&self) -> Option<&[Variant]> {
        match &self.kind {
            ObjectKind::Array { elements } => Some(elements),
            _ => None,
        }
    }

    pub fn array_push(&mut self, value: Variant) -> bool {
        self.array_extend(std::iter::once(value))
    }

    /// Appends a batch of values without rebuilding every existing numeric
    /// member for each element.  Array construction and binary-struct decode
    /// commonly append thousands of values in a tight loop; doing a full
    /// `sync_array_members` per value made those paths quadratic.
    pub fn array_extend<I>(&mut self, values: I) -> bool
    where
        I: IntoIterator<Item = Variant>,
    {
        let ObjectKind::Array { elements } = &mut self.kind else {
            return false;
        };
        let start = elements.len();
        elements.extend(values);
        for (index, value) in elements[start..].iter().enumerate() {
            self.members
                .insert((start + index).to_string(), value.clone());
        }
        self.sync_array_length_members();
        true
    }

    pub fn array_insert(&mut self, index: usize, value: Variant) -> bool {
        self.array_insert_values(index, std::iter::once(value))
    }

    pub fn array_insert_values<I>(&mut self, index: usize, values: I) -> bool
    where
        I: IntoIterator<Item = Variant>,
    {
        let ObjectKind::Array { elements } = &mut self.kind else {
            return false;
        };
        let index = index.min(elements.len());
        let values = values.into_iter().collect::<Vec<_>>();
        if values.is_empty() {
            return true;
        }
        elements.splice(index..index, values);
        self.sync_array_members();
        true
    }

    pub fn array_prepend<I>(&mut self, values: I) -> bool
    where
        I: IntoIterator<Item = Variant>,
    {
        let ObjectKind::Array { elements } = &mut self.kind else {
            return false;
        };
        let values = values.into_iter().collect::<Vec<_>>();
        if values.is_empty() {
            return true;
        }
        elements.splice(0..0, values);
        self.sync_array_members();
        true
    }

    pub fn array_erase(&mut self, index: usize) -> Option<Variant> {
        let ObjectKind::Array { elements } = &mut self.kind else {
            return None;
        };
        if index >= elements.len() {
            return None;
        }
        let value = elements.remove(index);
        self.sync_array_members();
        Some(value)
    }

    pub fn array_remove_value(&mut self, value: &Variant) -> bool {
        self.array_remove_values(value, false)
            .is_some_and(|count| count > 0)
    }

    pub fn array_remove_values(&mut self, value: &Variant, remove_all: bool) -> Option<usize> {
        let ObjectKind::Array { elements } = &mut self.kind else {
            return None;
        };
        let mut count = 0;
        let mut index = 0;
        while index < elements.len() {
            if value.discern_eq(&elements[index]) {
                elements.remove(index);
                count += 1;
                if !remove_all {
                    break;
                }
            } else {
                index += 1;
            }
        }
        if count > 0 {
            self.sync_array_members();
        }
        Some(count)
    }

    pub fn array_sort_by<F>(&mut self, mut less: F) -> Result<bool>
    where
        F: FnMut(&Variant, &Variant) -> Result<bool>,
    {
        let ObjectKind::Array { elements } = &mut self.kind else {
            return Ok(false);
        };
        for index in 1..elements.len() {
            let mut current = index;
            while current > 0 && less(&elements[current], &elements[current - 1])? {
                elements.swap(current, current - 1);
                current -= 1;
            }
        }
        self.sync_array_members();
        Ok(true)
    }

    pub fn array_pop(&mut self) -> Option<Variant> {
        let ObjectKind::Array { elements } = &mut self.kind else {
            return None;
        };
        let Some(index) = elements.len().checked_sub(1) else {
            return Some(Variant::Void);
        };
        let value = elements.pop().expect("array length checked above");
        self.members.remove(&index.to_string());
        self.sync_array_length_members();
        Some(value)
    }

    pub fn array_clear(&mut self) -> bool {
        let ObjectKind::Array { elements } = &mut self.kind else {
            return false;
        };
        elements.clear();
        self.members.retain(|key, _| key.parse::<usize>().is_err());
        self.sync_array_length_members();
        true
    }

    fn sync_array_members(&mut self) {
        if let ObjectKind::Array { elements } = &self.kind {
            self.members.retain(|key, _| key.parse::<usize>().is_err());
            for (index, value) in elements.iter().enumerate() {
                self.members.insert(index.to_string(), value.clone());
            }
            self.sync_array_length_members();
        }
    }

    fn sync_array_length_members(&mut self) {
        let ObjectKind::Array { elements } = &self.kind else {
            return;
        };
        let length = Variant::Integer(elements.len() as i64);
        self.members.insert("count".to_string(), length.clone());
        self.members.insert("length".to_string(), length);
    }
}

fn array_index(name: &str, len: usize) -> Option<usize> {
    let index = parse_array_index_name(name)?;
    let index = if index < 0 { len as i64 + index } else { index };
    (index >= 0 && index < len as i64).then_some(index as usize)
}

fn array_index_for_set(name: &str, len: usize) -> Option<usize> {
    let index = parse_array_index_name(name)?;
    if index < 0 {
        return array_index(name, len);
    }
    usize::try_from(index).ok()
}

fn parse_array_index_name(name: &str) -> Option<i64> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if let Ok(index) = name.parse::<i64>() {
        return Some(index);
    }
    let value = name.parse::<f64>().ok()?;
    value.is_finite().then_some(value as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_bulk_append_and_index_assignment_keep_visible_members_in_sync() {
        let mut array = Object::array(Vec::new());
        assert!(array.array_extend((0..4096).map(Variant::Integer)));
        assert_eq!(array.array_elements().unwrap().len(), 4096);
        assert_eq!(array.get("0"), Variant::Integer(0));
        assert_eq!(array.get("4095"), Variant::Integer(4095));
        assert_eq!(array.get("count"), Variant::Integer(4096));
        assert_eq!(array.get("length"), Variant::Integer(4096));

        array.set("8191", Variant::Integer(8191));
        assert_eq!(array.array_elements().unwrap().len(), 8192);
        assert_eq!(array.get("4096"), Variant::Void);
        assert_eq!(array.get("8191"), Variant::Integer(8191));
        assert_eq!(array.get("count"), Variant::Integer(8192));

        array.set("01", Variant::Integer(1));
        assert_eq!(array.get("1"), Variant::Integer(1));
        assert!(!array.members.contains_key("01"));
    }

    #[test]
    fn array_reads_resolve_numeric_names_as_indices_like_krkr() {
        // `tTJSArrayObject::PropGet` (`tjsArray.cpp:1601`): a numeric member
        // name is an index, never an entry of the member table. `ARRAY_GET_VAL`
        // (`tjsArray.cpp:1404`) yields void for an index the array does not
        // have -- with a negative index counted from the end -- and
        // `tjsArray.cpp:1407` reports that to a TJS_MEMBERMUSTEXIST reader as
        // member-not-found.
        let array = Object::array(vec![Variant::Integer(7), Variant::Integer(8)]);
        assert_eq!(array.get_raw("0"), Some(Variant::Integer(7)));
        assert_eq!(array.get_raw("1"), Some(Variant::Integer(8)));
        assert_eq!(array.get_raw("2"), Some(Variant::Void));
        assert_eq!(array.get_raw("-1"), Some(Variant::Integer(8)));
        assert_eq!(array.get_raw("-3"), Some(Variant::Void));
        assert!(!array.array_index_missing("1"));
        assert!(array.array_index_missing("2"));
        assert!(array.array_index_missing("-3"));
        assert!(!array.array_index_missing("count"));
        assert!(!array.array_index_missing("push"));

        let mut empty = Object::array(Vec::new());
        assert_eq!(empty.get_raw("0"), Some(Variant::Void));
        assert!(empty.array_negative_index_after_wrap("-1"));
        empty.set("0", Variant::Integer(1));
        assert!(!empty.array_negative_index_after_wrap("-1"));
        assert!(empty.array_negative_index_after_wrap("-2"));
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ObjectKind {
    Ordinary,
    Proxy {
        primary: Option<ObjectHandle>,
        fallback: ObjectHandle,
        bind_this: Option<ObjectHandle>,
    },
    Array {
        elements: Vec<Variant>,
    },
    InterCode {
        file_id: usize,
        object_index: usize,
        context: BytecodeContextType,
    },
    NativeFunction {
        id: usize,
        constructable: bool,
        /// Argument-count contract declared at registration; a call that
        /// breaks it fails with `TJS_E_BADPARAMCOUNT` (-1004) before the
        /// handler runs.
        arg_count: NativeArgCount,
    },
    VmNativeFunction {
        id: usize,
        /// See [`ObjectKind::NativeFunction`].
        arg_count: NativeArgCount,
    },
    NativeProperty {
        id: usize,
        /// Which halves of the property script may use. A denied direction
        /// fails with `TJS_E_ACCESSDENYED` (-1007) before the accessor runs,
        /// which is how the official `TJS_DENY_NATIVE_PROP_SETTER`
        /// read-only properties are declared.
        access: NativePropertyAccess,
    },
}

/// Which halves of a native property script code may use.
///
/// Only the denied-*setter* half has a reference counterpart: krkrz registers
/// the official read-only properties with a setter that returns
/// `TJS_E_ACCESSDENYED` (the `TJS_DENY_NATIVE_PROP_SETTER` macro,
/// `tjsNative.h:463`), and a denied direction answers that code instead of
/// running the accessor. No property *getter* in the reference reports
/// `TJS_E_ACCESSDENYED`, so [`WriteOnly`](Self::WriteOnly) and
/// [`NoAccess`](Self::NoAccess) are engine extensions rather than a modelling
/// of reference behaviour.
///
/// The engine keeps its own write path for storage-backed properties (host
/// writes go through [`crate::runtime::Runtime::set_object_member`], not
/// through TJS dispatch), so marking a property read-only only takes the
/// direction away from script.
///
/// Known gap, deferred to the self-bound value model mission: a script store
/// that carries `TJS_IGNOREPROP` (a regmember-style `&obj.prop = v` or VM
/// member store) still reaches
/// [`crate::runtime::Runtime::register_object_native_property`]'s setter,
/// because `prop_set_handle` keeps invoking native property setters under that
/// flag. It therefore fails with -1007 on a denied property where the
/// reference's `TJSDefaultPropSet` (`tjsObject.cpp:1435-1466`) skips the
/// property and overwrites the member. The gap is unobservable while every
/// property stays `ReadWrite`; see
/// [`crate::runtime::Runtime::deny_native_property_writes`] for what a caller
/// of the deny-list has to check, and for why the skip waits for `this` and
/// `new` results to be self-bound.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NativePropertyAccess {
    /// Script may read and write (the default).
    #[default]
    ReadWrite,
    /// Script may read; a script write fails with `TJS_E_ACCESSDENYED`.
    ReadOnly,
    /// Script may write; a script read fails with `TJS_E_ACCESSDENYED`.
    WriteOnly,
    /// Neither direction is available to script.
    NoAccess,
}

impl NativePropertyAccess {
    /// Whether a script read is allowed.
    pub const fn allows_get(self) -> bool {
        matches!(self, Self::ReadWrite | Self::ReadOnly)
    }

    /// Whether a script write is allowed.
    pub const fn allows_set(self) -> bool {
        matches!(self, Self::ReadWrite | Self::WriteOnly)
    }
}

/// Argument-count contract of a native method, declared at registration.
///
/// Official natives validate `numparams` themselves and return
/// `TJS_E_BADPARAMCOUNT` (-1004) before looking at any argument
/// (`LayerIntf.cpp:7835` for `assignImages`, `tjsInterCodeExec.cpp:2592` for
/// the string methods), so the count is part of the method's declaration
/// rather than something a handler has to re-derive from `args.len()`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NativeArgCount {
    /// No declaration-level check; the handler validates what it needs.
    #[default]
    Any,
    /// At least N arguments: too few fails with `TJS_E_BADPARAMCOUNT`.
    AtLeast(usize),
    /// Exactly N arguments.
    Exactly(usize),
}

impl NativeArgCount {
    /// Whether a call with `count` arguments satisfies the declaration.
    pub const fn accepts(self, count: usize) -> bool {
        match self {
            Self::Any => true,
            Self::AtLeast(min) => count >= min,
            Self::Exactly(expected) => count == expected,
        }
    }
}

#[cfg(test)]
mod declaration_tests {
    use super::{NativeArgCount, NativePropertyAccess};

    #[test]
    fn property_access_allows_only_the_declared_directions() {
        assert!(NativePropertyAccess::ReadWrite.allows_get());
        assert!(NativePropertyAccess::ReadWrite.allows_set());
        assert!(NativePropertyAccess::ReadOnly.allows_get());
        assert!(!NativePropertyAccess::ReadOnly.allows_set());
        assert!(!NativePropertyAccess::WriteOnly.allows_get());
        assert!(NativePropertyAccess::WriteOnly.allows_set());
        assert!(!NativePropertyAccess::NoAccess.allows_get());
        assert!(!NativePropertyAccess::NoAccess.allows_set());
        assert_eq!(
            NativePropertyAccess::default(),
            NativePropertyAccess::ReadWrite
        );
    }

    #[test]
    fn argument_counts_accept_what_the_declaration_allows() {
        assert!(NativeArgCount::Any.accepts(0));
        assert!(NativeArgCount::Any.accepts(9));
        assert!(!NativeArgCount::AtLeast(1).accepts(0));
        assert!(NativeArgCount::AtLeast(1).accepts(1));
        assert!(NativeArgCount::AtLeast(1).accepts(4));
        assert!(!NativeArgCount::Exactly(0).accepts(1));
        assert!(NativeArgCount::Exactly(0).accepts(0));
        assert!(!NativeArgCount::Exactly(2).accepts(1));
        assert!(NativeArgCount::Exactly(2).accepts(2));
        assert_eq!(NativeArgCount::default(), NativeArgCount::Any);
    }
}
