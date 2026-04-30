use krkr_tjs2::{Result, runtime::Runtime};

use crate::host::KrkrHost;

pub trait KrkrPlugin: Send + Sync {
    fn name(&self) -> &str;

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()>;

    fn unregister(&self, _runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        Ok(())
    }
}
