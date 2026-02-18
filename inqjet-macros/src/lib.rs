//! Proc macros for InqJet hot-path logging.
//!
//! Provides the `__hot_log` proc macro that parses format strings and
//! generates per-arg encoding/decoding with correct trait bounds.
//!
//! Not meant for direct use — invoked through `inqjet::info!()` etc.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Expr, LitStr, Token,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

// ---------------------------------------------------------------------------
// Input parsing
// ---------------------------------------------------------------------------

/// Parsed input to the `__hot_log` proc macro.
///
/// Expected syntax: `level_expr, target_expr, line_expr, "format string", arg0, arg1, ...`
struct HotLogInput {
    level: Expr,
    target: Expr,
    line: Expr,
    fmt_str: LitStr,
    args: Vec<Expr>,
}

impl Parse for HotLogInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let level: Expr = input.parse()?;
        input.parse::<Token![,]>()?;
        let target: Expr = input.parse()?;
        input.parse::<Token![,]>()?;
        let line: Expr = input.parse()?;
        input.parse::<Token![,]>()?;
        let fmt_str: LitStr = input.parse()?;

        let mut args = Vec::new();
        while input.parse::<Token![,]>().is_ok() {
            if input.is_empty() {
                break;
            }
            args.push(input.parse()?);
        }

        Ok(HotLogInput {
            level,
            target,
            line,
            fmt_str,
            args,
        })
    }
}

// ---------------------------------------------------------------------------
// Format string analysis
// ---------------------------------------------------------------------------

/// Which formatting trait a placeholder requires.
#[derive(Clone, Copy, Debug, PartialEq)]
enum FormatTrait {
    Display,
    Debug,
    LowerHex,
    UpperHex,
    Octal,
    Binary,
    LowerExp,
    UpperExp,
    Pointer,
}

/// Parsed info about a single format placeholder.
struct PlaceholderInfo {
    format_trait: FormatTrait,
    /// Content between `{` and `}`, e.g. `""`, `":?"`, `":.2"`, `":#010x"`.
    inner: String,
}

/// Determines the formatting trait from a format spec string.
///
/// The type character is always the last character of the spec (after `:`).
/// If absent or not a recognized type, defaults to Display.
fn format_trait_from_spec(spec: &str) -> FormatTrait {
    match spec.chars().last() {
        Some('?') => FormatTrait::Debug,
        Some('x') => FormatTrait::LowerHex,
        Some('X') => FormatTrait::UpperHex,
        Some('o') => FormatTrait::Octal,
        Some('b') => FormatTrait::Binary,
        Some('e') => FormatTrait::LowerExp,
        Some('E') => FormatTrait::UpperExp,
        Some('p') => FormatTrait::Pointer,
        _ => FormatTrait::Display,
    }
}

/// Parses a format string, returning per-placeholder info.
///
/// Handles `{{`/`}}` escapes. Rejects explicit indices and named arguments
/// (these can be added later if needed).
fn parse_format_placeholders(fmt: &str) -> Result<Vec<PlaceholderInfo>, String> {
    let mut placeholders = Vec::new();
    let mut chars = fmt.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '{' {
            if chars.peek() == Some(&'{') {
                chars.next(); // escaped {{
            } else {
                // Collect everything between { and }
                let mut inner = String::new();
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                    inner.push(c);
                }

                let spec_start = inner.find(':').unwrap_or(inner.len());
                let arg_part = &inner[..spec_start];

                if arg_part.is_empty() {
                    // Positional arg — determine trait from format spec
                    let spec = &inner[spec_start..]; // includes ':' if present
                    placeholders.push(PlaceholderInfo {
                        format_trait: format_trait_from_spec(spec),
                        inner: inner.clone(),
                    });
                } else if arg_part.chars().all(|c| c.is_ascii_digit()) {
                    return Err(format!(
                        "explicit indices ({{{inner}}}) not supported in hot-path \
                         logging; use positional arguments"
                    ));
                } else {
                    return Err(format!(
                        "named arguments ({{{inner}}}) not supported in hot-path \
                         logging; use positional arguments"
                    ));
                }
            }
        } else if c == '}' && chars.peek() == Some(&'}') {
            chars.next(); // escaped }}
        }
    }

    Ok(placeholders)
}

// ---------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------

/// Returns the trait path tokens for a non-Debug format trait, or None for
/// Debug (which is already guaranteed by the `HotDecode` trait bound).
fn extra_trait_bound(ft: FormatTrait, type_param: &proc_macro2::Ident) -> Option<TokenStream2> {
    if ft == FormatTrait::Debug {
        return None; // Debug is on HotDecode::Decoded, no extra bound needed
    }
    let trait_path = match ft {
        FormatTrait::Display => quote!(::std::fmt::Display),
        FormatTrait::LowerHex => quote!(::std::fmt::LowerHex),
        FormatTrait::UpperHex => quote!(::std::fmt::UpperHex),
        FormatTrait::Octal => quote!(::std::fmt::Octal),
        FormatTrait::Binary => quote!(::std::fmt::Binary),
        FormatTrait::LowerExp => quote!(::std::fmt::LowerExp),
        FormatTrait::UpperExp => quote!(::std::fmt::UpperExp),
        FormatTrait::Pointer => quote!(::std::fmt::Pointer),
        FormatTrait::Debug => unreachable!(),
    };
    Some(quote! { <#type_param as ::inqjet::__private::HotDecode>::Decoded: #trait_path })
}

fn generate(input: &HotLogInput) -> TokenStream2 {
    let level = &input.level;
    let target = &input.target;
    let line = &input.line;
    let fmt_str = &input.fmt_str;
    let n = input.args.len();

    // Parse format string for per-arg info
    let placeholders = match parse_format_placeholders(&fmt_str.value()) {
        Ok(p) => p,
        Err(msg) => return syn::Error::new_spanned(fmt_str, msg).to_compile_error(),
    };

    if placeholders.len() != n {
        return syn::Error::new_spanned(
            fmt_str,
            format!(
                "format string expects {} arguments but {} were provided",
                placeholders.len(),
                n
            ),
        )
        .to_compile_error();
    }

    // Identifier lists
    let vn: Vec<_> = (0..n).map(|i| format_ident!("__v{}", i)).collect();
    let sn: Vec<_> = (0..n).map(|i| format_ident!("__s{}", i)).collect();
    let wn: Vec<_> = (0..n).map(|i| format_ident!("__w{}", i)).collect();
    let dn: Vec<_> = (0..n).map(|i| format_ident!("__d{}", i)).collect();
    let tp: Vec<_> = (0..n).map(|i| format_ident!("__T{}", i)).collect();
    let args = &input.args;

    // Per-arg format strings for stash closures: "{}", "{:?}", "{:.2}", etc.
    let closure_fmts: Vec<LitStr> = placeholders
        .iter()
        .map(|p| LitStr::new(&format!("{{{}}}", p.inner), fmt_str.span()))
        .collect();

    let arg_size_expr = if n == 0 {
        quote!(0usize)
    } else {
        quote!(#(#sn)+*)
    };

    // Extra trait bounds (Display, LowerHex, etc.) — Debug is free
    let bounds: Vec<TokenStream2> = placeholders
        .iter()
        .enumerate()
        .filter_map(|(i, p)| extra_trait_bound(p.format_trait, &tp[i]))
        .collect();

    let where_clause = if bounds.is_empty() {
        quote! {}
    } else {
        quote! { where #(#bounds),* }
    };
    let where_clause2 = where_clause.clone();

    // Payload prefix decode (consumer-side): reads line, target_len, target
    let prefix_decode = quote! {
        let __line = u32::from_le_bytes(
            __payload[0..4].try_into().unwrap()
        );
        let __tl = u16::from_le_bytes(
            __payload[4..6].try_into().unwrap()
        ) as usize;
        let __target = ::std::str::from_utf8(
            &__payload[6..6 + __tl]
        ).unwrap();
    };

    // Witness function + formatter
    let witness_block = if n == 0 {
        quote! {
            fn __make_fmt() -> for<'__a> fn(::inqjet::__private::FormatContext<'__a>) {
                fn __fmt(__ctx: ::inqjet::__private::FormatContext<'_>) {
                    let _ts = __ctx.timestamp_ns;
                    let _level = __ctx.level;
                    let __payload = __ctx.payload;
                    let out = __ctx.out;
                    #prefix_decode
                    ::inqjet::__private::write_log_prefix(_ts, _level, __target, __line, out);
                    writeln!(out, #fmt_str).ok();
                }
                __fmt
            }
            let __fmt_fn = __make_fmt();
        }
    } else {
        quote! {
            fn __make_fmt<#(#tp: ::inqjet::__private::HotDecode),*>(
                #(_: ::inqjet::__private::Witness<#tp>),*
            ) -> for<'__a> fn(::inqjet::__private::FormatContext<'__a>)
            #where_clause
            {
                fn __fmt<#(#tp: ::inqjet::__private::HotDecode),*>(
                    __ctx: ::inqjet::__private::FormatContext<'_>,
                )
                #where_clause2
                {
                    let _ts = __ctx.timestamp_ns;
                    let _level = __ctx.level;
                    let __payload = __ctx.payload;
                    let out = __ctx.out;
                    #prefix_decode
                    let mut __off = 6usize + __tl;
                    #(
                        let (#dn, __n) =
                            <#tp as ::inqjet::__private::HotDecode>::hot_decode(
                                &__payload[__off..]
                            );
                        __off += __n;
                    )*
                    let _ = __off;
                    ::inqjet::__private::write_log_prefix(_ts, _level, __target, __line, out);
                    writeln!(out, #fmt_str, #(#dn),*).ok();
                }
                __fmt::<#(#tp),*>
            }
            #(let #wn = (&::inqjet::__private::HotArg(#vn)).hot_witness();)*
            let __fmt_fn = __make_fmt(#(#wn),*);
        }
    };

    // Payload prefix encode (producer-side): writes line, target_len, target
    let prefix_encode = quote! {
        __buf[0..4].copy_from_slice(&__line.to_le_bytes());
        __buf[4..6].copy_from_slice(&(__target_bytes.len() as u16).to_le_bytes());
        __buf[6..6 + __target_bytes.len()].copy_from_slice(__target_bytes);
    };

    // Encode closure
    let encode_block = if n == 0 {
        quote! {
            |__buf: &mut [u8]| {
                #prefix_encode
            }
        }
    } else {
        quote! {
            |__buf: &mut [u8]| {
                #prefix_encode
                let mut __off = 6usize + __target_bytes.len();
                #(
                    (&::inqjet::__private::HotArg(#vn)).hot_encode(&mut __buf[__off..]);
                    __off += #sn;
                )*
                let _ = __off;
            }
        }
    };

    // Common prefix setup (producer-side)
    let prefix_setup = quote! {
        let __target: &str = #target;
        let __line: u32 = #line;
        let __target_bytes = __target.as_bytes();
        let __prefix_size: usize = 4 + 2 + __target_bytes.len();
    };

    // Full block
    if n == 0 {
        quote! {
            {
                #prefix_setup
                let __total = __prefix_size;
                #witness_block
                ::inqjet::__private::hot_log_submit(#level, __total, __fmt_fn, #encode_block);
            }
        }
    } else {
        quote! {
            {
                use ::inqjet::__private::HotEncode as _;
                ::inqjet::__private::fallback_stash_clear();
                #prefix_setup
                #(let #vn = &(#args);)*
                #(let #sn = (&::inqjet::__private::HotArg(#vn)).hot_size_with(
                    |__stash: &mut ::std::string::String| {
                        use ::std::fmt::Write;
                        write!(__stash, #closure_fmts, #vn).ok();
                    }
                );)*
                let __total = __prefix_size + #arg_size_expr;
                #witness_block
                ::inqjet::__private::hot_log_submit(#level, __total, __fmt_fn, #encode_block);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Proc macro entry point
// ---------------------------------------------------------------------------

/// Internal proc macro for hot-path log expansion.
///
/// Input: `level_expr, target_expr, line_expr, "format string", arg0, arg1, ...`
///
/// Generates:
/// - Producer-side encoding via autoref dispatch (`HotArg`)
/// - Consumer-side formatter via witness pattern (`HotDecode`)
/// - Per-arg trait bounds from the format spec (`{}` → Display, `{:?}` →
///   Debug, `{:x}` → LowerHex, etc.)
/// - Logbuf submission via `__hot_log_submit`
///
/// Not public API — called through declarative wrapper macros in `inqjet`.
#[proc_macro]
pub fn __hot_log(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as HotLogInput);
    generate(&input).into()
}

// ---------------------------------------------------------------------------
// Derive macro for Pod
// ---------------------------------------------------------------------------

/// Derive macro for `inqjet::Pod`.
///
/// Marks a type as safe for hot-path logging via raw byte memcpy.
/// Generates a compile-time assertion that the type does not require drop.
///
/// # Example
///
/// ```rust,ignore
/// use inqjet::Pod;
///
/// #[derive(Pod)]
/// struct OrderInfo {
///     id: u64,
///     price: f64,
/// }
/// ```
#[proc_macro_derive(Pod)]
pub fn derive_pod(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as syn::DeriveInput);
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let assertion = if input.generics.params.is_empty() {
        // Concrete type: assert at definition site.
        let msg = format!("Pod: `{}` must not require drop", name);
        quote! {
            const _: () = assert!(
                !::std::mem::needs_drop::<#name>(),
                #msg
            );
        }
    } else {
        // Generic type: defer assertion to usage site (HotArg inherent methods).
        quote! {}
    };

    let expanded = quote! {
        impl #impl_generics ::inqjet::Pod for #name #ty_generics #where_clause {}
        #assertion
    };

    expanded.into()
}

// ---------------------------------------------------------------------------
// Unit tests for format string parsing
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse and extract just the FormatTrait per placeholder.
    fn parse_traits(fmt: &str) -> Vec<FormatTrait> {
        parse_format_placeholders(fmt)
            .unwrap()
            .iter()
            .map(|p| p.format_trait)
            .collect()
    }

    #[test]
    fn simple_positional() {
        assert_eq!(
            parse_traits("hello {} world {}"),
            vec![FormatTrait::Display, FormatTrait::Display]
        );
    }

    #[test]
    fn debug_format() {
        assert_eq!(
            parse_traits("{:?} and {:#?}"),
            vec![FormatTrait::Debug, FormatTrait::Debug]
        );
    }

    #[test]
    fn mixed_display_debug() {
        assert_eq!(
            parse_traits("id {} state {:?} price {:.2}"),
            vec![
                FormatTrait::Display,
                FormatTrait::Debug,
                FormatTrait::Display
            ]
        );
    }

    #[test]
    fn escaped_braces() {
        assert_eq!(
            parse_traits("literal {{}} and {}"),
            vec![FormatTrait::Display]
        );
    }

    #[test]
    fn no_args() {
        assert!(parse_traits("static message").is_empty());
    }

    #[test]
    fn reject_explicit_index() {
        assert!(parse_format_placeholders("{0} and {1}").is_err());
    }

    #[test]
    fn reject_named_arg() {
        assert!(parse_format_placeholders("{name}").is_err());
    }

    #[test]
    fn display_spec_variants() {
        let result = parse_traits("{:>10} {:.<5} {:.2}");
        assert!(result.iter().all(|t| *t == FormatTrait::Display));
    }

    #[test]
    fn hex_octal_binary() {
        assert_eq!(
            parse_traits("{:x} {:X} {:o} {:b}"),
            vec![
                FormatTrait::LowerHex,
                FormatTrait::UpperHex,
                FormatTrait::Octal,
                FormatTrait::Binary,
            ]
        );
    }

    #[test]
    fn scientific_and_pointer() {
        assert_eq!(
            parse_traits("{:e} {:E} {:p}"),
            vec![
                FormatTrait::LowerExp,
                FormatTrait::UpperExp,
                FormatTrait::Pointer,
            ]
        );
    }

    #[test]
    fn hex_with_prefix_and_width() {
        assert_eq!(parse_traits("{:#010x}"), vec![FormatTrait::LowerHex]);
    }

    #[test]
    fn inner_content_preserved() {
        let result = parse_format_placeholders("{} {:?} {:.2} {:#010x}").unwrap();
        assert_eq!(result[0].inner, "");
        assert_eq!(result[1].inner, ":?");
        assert_eq!(result[2].inner, ":.2");
        assert_eq!(result[3].inner, ":#010x");
    }
}
