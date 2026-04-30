use std::fmt::Debug;

pub trait Snapshot {
    fn snapshot(&self) -> String;
}

impl<T> Snapshot for T
where
    T: Debug,
{
    fn snapshot(&self) -> String {
        format!("{self:#?}")
    }
}

pub fn snapshot(value: &impl Snapshot) -> String {
    value.snapshot()
}
