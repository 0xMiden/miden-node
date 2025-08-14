use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn, ReturnType, Type};

/// Procedural macro to wrap a main function returning `anyhow::Result<()>` so as to catch and
/// handle panics.
///
/// This macro should be applied to async main functions that return `anyhow::Result<()>`.
/// It wraps the function body in panic handling logic provided by `miden_node_utils::panic::handle_panic`.
///
/// This macro will produce a compile-time error if:
/// - Applied to a non-async function.
/// - Applied to a function that doesn't return `Result<(), _>`.
/// - Applied to a function that isn't named `main`.
#[proc_macro_attribute]
pub fn main(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut func = parse_macro_input!(item as ItemFn);

    // Validate function name.
    if func.sig.ident != "main" {
        return syn::Error::new_spanned(
            func.sig.ident,
            "panic::main can only be applied to functions named 'main'",
        )
        .to_compile_error()
        .into();
    }

    // Validate that function is async.
    if func.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            func.sig.fn_token,
            "panic::main can only be applied to async functions",
        )
        .to_compile_error()
        .into();
    }

    // Validate return type is Result<(), _>.
    if let ReturnType::Type(_, return_type) = &func.sig.output {
        if !is_result_type_compatible(return_type) {
            return syn::Error::new_spanned(
                return_type,
                "panic::main requires the function to return a Result type, typically anyhow::Result<()>",
            )
            .to_compile_error()
            .into();
        }
    } else {
        return syn::Error::new_spanned(
            func.sig,
            "panic::main requires the function to return a Result type, typically anyhow::Result<()>",
        )
        .to_compile_error()
        .into();
    }

    // Validate function has no parameters.
    if !func.sig.inputs.is_empty() {
        return syn::Error::new_spanned(
            func.sig.inputs,
            "main function should not have parameters",
        )
        .to_compile_error()
        .into();
    }

    // Store the original function body
    let original_body = func.block;

    // Replace the function body with panic-handled version
    func.block = Box::new(syn::parse_quote!({
        ::miden_node_utils::panic::handle_panic(async move {
            #original_body
        }).await?
    }));

    // Generate the modified function
    quote!(#func).into()
}

/// Checks if the return type is compatible with Result<(), _>.
fn is_result_type_compatible(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            return segment.ident == "Result";
        }
    }
    false
}
