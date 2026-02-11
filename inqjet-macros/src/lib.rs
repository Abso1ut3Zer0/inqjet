//! Proc macros for InqJet hot-path logging.
//!
//! Provides the `__hot_log` proc macro that parses format strings and
//! generates per-arg encoding/decoding with correct Display/Debug bounds.
//!
//! Not meant for direct use — invoked through `inqjet::info!()` etc.

use proc_macro::TokenStream;

/// Internal proc macro for hot-path log expansion.
///
/// Parses the format string to determine per-arg formatting traits
/// (Display vs Debug), then generates:
/// - Producer-side encoding via autoref dispatch (`HotArg`)
/// - Consumer-side formatter function with per-arg `HotDecode` + correct bounds
/// - Level gate and logbuf claim/commit
///
/// Not public API — called through declarative wrapper macros in `inqjet`.
#[proc_macro]
pub fn __hot_log(_input: TokenStream) -> TokenStream {
    todo!("format string parsing + codegen")
}
