//! Debugging helpers

/// A macro that expands to a call to `log::debug!` when the feature "cdc_verbose" is enabled,
/// otherwise does nothing.
#[macro_export]
macro_rules! cdc_debug {
  ($($arg:tt)+) => (if cfg!(feature = "cdc_verbose") { ::log::debug!($($arg)+)})
}
