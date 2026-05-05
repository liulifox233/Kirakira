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
        let mut sync_array = false;
        if let ObjectKind::Array { elements } = &mut self.kind {
            if name == "count" || name == "length" {
                let len = value.to_integer().unwrap_or(0).max(0) as usize;
                elements.resize(len, Variant::Void);
                self.sync_array_members();
                return;
            }
            if let Ok(index) = name.parse::<usize>() {
                if index >= elements.len() {
                    elements.resize(index + 1, Variant::Void);
                }
                elements[index] = value.clone();
                sync_array = true;
            }
        }
        self.members.insert(name, value);
        if sync_array {
            self.sync_array_members();
        }
    }

    pub fn delete(&mut self, name: &str) -> bool {
        let removed = self.members.remove(name).is_some();
        if let ObjectKind::Array { elements } = &mut self.kind
            && let Ok(index) = name.parse::<usize>()
            && index < elements.len()
        {
            elements[index] = Variant::Void;
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
        let ObjectKind::Array { elements } = &mut self.kind else {
            return false;
        };
        elements.push(value);
        self.sync_array_members();
        true
    }

    pub fn array_insert(&mut self, index: usize, value: Variant) -> bool {
        let ObjectKind::Array { elements } = &mut self.kind else {
            return false;
        };
        let index = index.min(elements.len());
        elements.insert(index, value);
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
        let value = elements.pop().unwrap_or_default();
        self.sync_array_members();
        Some(value)
    }

    pub fn array_clear(&mut self) -> bool {
        let ObjectKind::Array { elements } = &mut self.kind else {
            return false;
        };
        elements.clear();
        self.sync_array_members();
        true
    }

    fn sync_array_members(&mut self) {
        if let ObjectKind::Array { elements } = &self.kind {
            self.members.retain(|key, _| key.parse::<usize>().is_err());
            for (index, value) in elements.iter().enumerate() {
                self.members.insert(index.to_string(), value.clone());
            }
            self.members
                .insert("count".to_string(), Variant::Integer(elements.len() as i64));
            self.members.insert(
                "length".to_string(),
                Variant::Integer(elements.len() as i64),
            );
        }
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
