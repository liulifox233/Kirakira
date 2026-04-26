use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

use super::Value;

static NEXT_OBJECT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct ObjectValue {
    inner: Rc<RefCell<ObjectData>>,
}

#[derive(Debug)]
struct ObjectData {
    id: u64,
    properties: BTreeMap<String, Value>,
}

impl ObjectValue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn id(&self) -> u64 {
        self.inner.borrow().id
    }

    pub fn set_property(&mut self, name: impl Into<String>, value: Value) -> Option<Value> {
        self.inner
            .borrow_mut()
            .properties
            .insert(name.into(), value)
    }

    pub fn get_property(&self, name: &str) -> Option<Value> {
        self.inner.borrow().properties.get(name).cloned()
    }

    pub fn has_property(&self, name: &str) -> bool {
        self.inner.borrow().properties.contains_key(name)
    }

    pub fn property_count(&self) -> usize {
        self.inner.borrow().properties.len()
    }

    pub fn property_names(&self) -> impl Iterator<Item = String> {
        self.inner
            .borrow()
            .properties
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl Default for ObjectValue {
    fn default() -> Self {
        Self {
            inner: Rc::new(RefCell::new(ObjectData {
                id: NEXT_OBJECT_ID.fetch_add(1, Ordering::Relaxed),
                properties: BTreeMap::new(),
            })),
        }
    }
}

impl PartialEq for ObjectValue {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}
