use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn handle_panic_fn(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut func = syn::parse_macro_input!(item as syn::ItemFn);
    let original_body = func.block;

    func.block = Box::new(syn::parse_quote!({
        miden_node_utils::panic::handle_panic(async move {
            #original_body
        }).await?
    }));

    quote::quote!(#func).into()
}
