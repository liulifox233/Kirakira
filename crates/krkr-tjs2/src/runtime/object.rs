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
                } else if let Ok(index) = name.parse::<usize>() {
                    elements.get(index).cloned()
                } else {
                    self.members.get(name).cloned()
                }
            }
            _ => self.members.get(name).cloned(),
        }
    }

    pub fn get(&self, name: &str) -> Variant {
        self.get_raw(name).unwrap_or_default()
    }

    pub fn set(&mut self, name: impl Into<String>, value: Variant) {
        let name = name.into();
        if let ObjectKind::Array { elements } = &mut self.kind
            && let Ok(index) = name.parse::<usize>()
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
            && let Ok(index) = name.parse::<usize>()
            && index < elements.len()
        {
            elements[index] = Variant::Void;
            self.members.insert(index.to_string(), Variant::Void);
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
    },
    VmNativeFunction {
        id: usize,
    },
    NativeProperty {
        id: usize,
    },
}
