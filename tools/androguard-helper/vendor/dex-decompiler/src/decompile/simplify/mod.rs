//! Post-process decompiled method body to simplify invoke + move-result + return patterns.

mod cleanup;
mod conditions;
mod expr;
mod format;
mod loops;
mod pipeline;
mod repair;
mod string_switch;
mod try_catch;
mod util;

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;

pub use format::simplify_synchronized_blocks;
pub use pipeline::simplify_method_body;
pub use string_switch::restore_string_switch;
pub(crate) use expr::wrap_postinc_div_try;

pub(crate) use util::is_temp_like_name;

// Re-exports used by unit tests (and external callers).
#[cfg(test)]
pub use format::normalize_java_indent;
#[cfg(test)]
pub use string_switch::java_string_hash_code;
#[cfg(test)]
pub use try_catch::merge_duplicate_finally;
