use proc_macro::TokenStream;

/// Procedural macro to wrap a main function returning `anyhow::Result<()>` so as to catch and
/// handle panics.
#[proc_macro_attribute]
pub fn main(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut func = syn::parse_macro_input!(item as syn::ItemFn);
    let original_body = func.block;

    func.block = Box::new(syn::parse_quote!({
        miden_node_utils::panic::handle_panic(async move {
            #original_body
        }).await?
    }));

    quote::quote!(#func).into()
}
