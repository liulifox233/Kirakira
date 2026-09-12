//! Registration shape shared by every catalog entry that has no
//! implementation yet.
//!
//! A placeholder must not pretend to support the plugin it stands for, and it
//! must not silently swallow the game's request either: `KrkrPlugin::register`
//! installs no TJS surface at all and reports itself once through the engine
//! log, so a game that loads the plugin leaves a trace in the diagnostics
//! instead of dying later on a missing member with no explanation.
//!
//! Implementing a plugin means replacing the `placeholder_plugin!` invocation
//! in its module (`src/<plugin>.rs`) with a real `KrkrPlugin` impl, then
//! flipping that module's `META.status` to [`PluginStatus::Shim`] or
//! [`PluginStatus::Implemented`] and updating `META.notes` (and
//! `META.feature` when the surface grew) beside it. `crate::catalog` holds
//! only the plugin's identity and is never part of an implementation, so
//! parallel implementations do not share a file.
//!
//! [`PluginStatus::Shim`]: crate::catalog::PluginStatus::Shim
//! [`PluginStatus::Implemented`]: crate::catalog::PluginStatus::Implemented
//! [`META`]: crate::catalog::PluginMeta

/// Declares the placeholder plugin for one catalog entry. Keeping one module
/// per plugin — with the module's `META` next to this invocation — means a
/// later mission can implement that plugin by editing that file alone.
macro_rules! placeholder_plugin {
    ($plugin:ident, $name:literal) => {
        /// Self-reporting placeholder for a plugin with no implementation yet.
        pub struct $plugin;

        impl ::krkr_engine::KrkrPlugin for $plugin {
            fn name(&self) -> &str {
                $name
            }

            fn register(
                &self,
                runtime: &mut ::krkr_tjs2::runtime::Runtime<::krkr_engine::KrkrHost>,
            ) -> ::krkr_tjs2::Result<()> {
                $crate::catalog::report_unimplemented_plugin(runtime, $name);
                Ok(())
            }
        }
    };
}

pub(crate) use placeholder_plugin;
