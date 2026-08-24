use std::collections::BTreeSet;

use proc_macro::TokenStream;
use proc_macro2::{Delimiter, Group, TokenStream as TokenStream2, TokenTree};
use quote::{ToTokens, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::token::Dot;
use syn::visit::Visit;
use syn::{
    Attribute,
    Block,
    Expr,
    Ident,
    ItemFn,
    Macro,
    Meta,
    Result,
    Token,
    parse_macro_input,
    parse_quote,
};

/// Instruments a function using canonical tracing attributes.
///
/// Field values must implement `RecordAttribute`, and their names must be registered for the value
/// type. A field whose name ends in `.count` accepts any `usize` without registration. Append
/// `#[nonstandard]` to any other field value to permit an unregistered name while retaining its
/// canonical encoding.
#[proc_macro_attribute]
pub fn miden_instrument(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = match rewrite_explicit_fields(TokenStream2::from(attr)) {
        Ok(attr) => attr,
        Err(error) => return error.into_compile_error().into(),
    };
    let mut function = parse_macro_input!(item as ItemFn);
    let fields = collect_recorded_fields(&function);
    let args = match merge_inferred_fields(attr, &fields) {
        Ok(args) => args,
        Err(error) => return error.into_compile_error().into(),
    };
    let statements = &function.block.stmts;
    let block: Block = parse_quote! {{
        #[allow(unused_macros)]
        macro_rules! __miden_span_record_must_be_used_within_miden_instrument {
            () => {};
        }

        #(#statements)*
    }};
    *function.block = block;

    let expanded = quote! {
        #[::tracing::instrument(#args)]
        #function
    };

    expanded.into()
}

fn merge_inferred_fields(attr: TokenStream2, fields: &[FieldPath]) -> Result<TokenStream2> {
    let mut args = split_top_level_args(attr);
    reject_skip_directives(&args)?;

    // Function arguments often contain large or sensitive values. Always skip them so spans only
    // contain fields explicitly declared by the caller or inferred from `miden_span_record!`.
    args.push(quote! { skip_all });

    if fields.is_empty() {
        return Ok(quote! { #(#args),* });
    }

    let inferred_fields = quote! { #(#fields = ::tracing::field::Empty),* };
    let mut merged_existing_fields = false;
    let args = args
        .into_iter()
        .map(|arg| {
            if let Some(group) = fields_group(&arg) {
                merged_existing_fields = true;
                let existing_fields = group.stream();
                let merged_fields = if existing_fields.is_empty() {
                    inferred_fields.clone()
                } else if ends_with_comma(&existing_fields) {
                    quote! { #existing_fields #inferred_fields }
                } else {
                    quote! { #existing_fields, #inferred_fields }
                };
                let mut merged_group = Group::new(Delimiter::Parenthesis, merged_fields);
                merged_group.set_span(group.span());
                quote! { fields #merged_group }
            } else {
                arg
            }
        })
        .collect::<Vec<_>>();

    if merged_existing_fields {
        Ok(quote! { #(#args),* })
    } else {
        Ok(quote! { #(#args,)* fields(#inferred_fields) })
    }
}

fn reject_skip_directives(args: &[TokenStream2]) -> Result<()> {
    for arg in args {
        let Some(TokenTree::Ident(ident)) = arg.clone().into_iter().next() else {
            continue;
        };
        if ident == "skip" || ident == "skip_all" {
            return Err(syn::Error::new_spanned(
                arg,
                format!(
                    "`{ident}` is not supported by `miden_instrument`; function arguments are \
                     always skipped, record fields explicitly with `fields(...)`"
                ),
            ));
        }
    }

    Ok(())
}

fn rewrite_explicit_fields(attr: TokenStream2) -> Result<TokenStream2> {
    let args = split_top_level_args(attr)
        .into_iter()
        .map(|arg| {
            if let Some(group) = fields_group(&arg) {
                let fields = syn::parse2::<InstrumentFields>(group.stream())?;
                let fields = fields.fields.iter().map(RecordField::instrument_tokens);
                let mut rewritten = Group::new(Delimiter::Parenthesis, quote! { #(#fields),* });
                rewritten.set_span(group.span());
                Ok(quote! { fields #rewritten })
            } else {
                Ok(arg)
            }
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(quote! { #(#args),* })
}

fn reject_formatter(input: ParseStream<'_>) -> Result<()> {
    let formatter = if input.peek(Token![%]) {
        Some(input.parse::<Token![%]>()?.span)
    } else if input.peek(Token![?]) {
        Some(input.parse::<Token![?]>()?.span)
    } else {
        None
    };

    if let Some(span) = formatter {
        Err(syn::Error::new(
            span,
            "tracing format specifiers are not supported; implement `RecordAttribute` to define \
             the type's canonical encoding",
        ))
    } else {
        Ok(())
    }
}

impl RecordField {
    fn instrument_tokens(&self) -> TokenStream2 {
        let path = &self.path;
        if let Some(value) = &self.value {
            let value = value.value_tokens(&self.path.name(), self.path.is_count());
            quote! { #path = #value }
        } else {
            quote! { #path }
        }
    }
}

fn split_top_level_args(tokens: TokenStream2) -> Vec<TokenStream2> {
    let mut args = Vec::new();
    let mut current = TokenStream2::new();

    for token in tokens {
        match &token {
            TokenTree::Punct(punct) if punct.as_char() == ',' => {
                args.push(current);
                current = TokenStream2::new();
            },
            _ => current.extend([token]),
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

fn fields_group(arg: &TokenStream2) -> Option<Group> {
    let mut tokens = arg.clone().into_iter();
    let Some(TokenTree::Ident(ident)) = tokens.next() else {
        return None;
    };
    if ident != "fields" {
        return None;
    }

    let Some(TokenTree::Group(group)) = tokens.next() else {
        return None;
    };
    if group.delimiter() != Delimiter::Parenthesis || tokens.next().is_some() {
        return None;
    }

    Some(group)
}

fn ends_with_comma(tokens: &TokenStream2) -> bool {
    matches!(
        tokens.clone().into_iter().last(),
        Some(TokenTree::Punct(punct)) if punct.as_char() == ','
    )
}

/// Records canonical attributes on the current `miden_instrument` span.
///
/// Field values must implement `RecordAttribute`, and their names must be registered for the value
/// type. A field whose name ends in `.count` accepts any `usize` without registration. Append
/// `#[nonstandard]` to any other field value to permit an unregistered name while retaining its
/// canonical encoding.
#[proc_macro]
pub fn miden_span_record(input: TokenStream) -> TokenStream {
    let records = parse_macro_input!(input as RecordFields);
    let records = records.fields.into_iter().map(|field| {
        let name = field.path.name();
        let value = field
            .value
            .expect("record fields are parsed with required values")
            .value_tokens(&name, field.path.is_count());

        quote! {
            ::tracing::Span::current().record(#name, #value);
        }
    });

    quote! {
        __miden_span_record_must_be_used_within_miden_instrument!();
        #(#records)*
    }
    .into()
}

fn collect_recorded_fields(function: &ItemFn) -> Vec<FieldPath> {
    let mut visitor = MacroVisitor::default();
    visitor.visit_block(&function.block);

    let mut names = BTreeSet::new();
    visitor.fields.into_iter().filter(|field| names.insert(field.name())).collect()
}

#[derive(Default)]
struct MacroVisitor {
    fields: Vec<FieldPath>,
}

impl<'ast> Visit<'ast> for MacroVisitor {
    fn visit_macro(&mut self, mac: &'ast Macro) {
        if mac
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "miden_span_record")
        {
            if let Ok(records) = syn::parse2::<RecordFields>(mac.tokens.clone()) {
                self.fields.extend(records.fields.into_iter().map(|field| field.path));
            }
        }

        syn::visit::visit_macro(self, mac);
    }
}

type InstrumentFields = Fields<false>;
type RecordFields = Fields<true>;

struct Fields<const VALUE_REQUIRED: bool> {
    fields: Punctuated<RecordField, Token![,]>,
}

impl<const VALUE_REQUIRED: bool> Parse for Fields<VALUE_REQUIRED> {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self {
            fields: Punctuated::parse_terminated_with(input, |input| {
                RecordField::parse(input, VALUE_REQUIRED)
            })?,
        })
    }
}

struct RecordField {
    path: FieldPath,
    value: Option<RecordValue>,
}

impl RecordField {
    fn parse(input: ParseStream<'_>, value_required: bool) -> Result<Self> {
        reject_formatter(input)?;
        let path = input.parse()?;
        let value = if value_required || input.peek(Token![=]) {
            input.parse::<Token![=]>()?;
            reject_formatter(input)?;
            Some(input.parse()?)
        } else {
            None
        };

        Ok(Self { path, value })
    }
}

struct FieldPath {
    first: Ident,
    rest: Vec<(Dot, Ident)>,
}

impl FieldPath {
    fn name(&self) -> String {
        std::iter::once(&self.first)
            .chain(self.rest.iter().map(|(_, ident)| ident))
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }

    fn is_count(&self) -> bool {
        self.rest.last().is_some_and(|(_, ident)| ident == "count")
    }
}

impl Parse for FieldPath {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let first = input.parse()?;
        let mut rest = Vec::new();

        while input.peek(Token![.]) {
            rest.push((input.parse()?, input.parse()?));
        }

        Ok(Self { first, rest })
    }
}

impl ToTokens for FieldPath {
    fn to_tokens(&self, tokens: &mut TokenStream2) {
        self.first.to_tokens(tokens);
        for (dot, ident) in &self.rest {
            dot.to_tokens(tokens);
            ident.to_tokens(tokens);
        }
    }
}

struct RecordValue {
    expr: Expr,
    nonstandard: bool,
}

impl RecordValue {
    fn value_tokens(&self, field_name: &str, is_count: bool) -> TokenStream2 {
        let expr = &self.expr;
        let assert_field_name = (!self.nonstandard && !is_count).then(|| {
            quote! {
                fn __miden_assert_field_name<T>(_: &T)
                where
                    T: ::miden_node_utils::tracing::RecordAttribute + ?Sized,
                {
                    const {
                        assert!(
                            ::miden_node_utils::tracing::field_name_allowed(
                                T::FIELD_NAMES,
                                #field_name,
                                T::PLURALIZE_FIELD_NAMES,
                            ),
                            concat!(
                                "tracing field `",
                                #field_name,
                                "` is not allowed for this attribute type",
                            ),
                        );
                    }
                }

                __miden_assert_field_name(value);
            }
        });
        let assert_count = is_count.then(|| {
            quote! {
                fn __miden_assert_count(_: &usize) {}

                __miden_assert_count(value);
            }
        });

        quote! {
            match &(#expr) {
                value => {
                    #assert_field_name
                    #assert_count
                    ::miden_node_utils::tracing::record_attribute(value)
                }
            }
        }
    }
}

impl Parse for RecordValue {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let expr = input.parse()?;
        let attributes = input.call(Attribute::parse_outer)?;
        let nonstandard = match attributes.as_slice() {
            [] => false,
            [attribute] if matches!(&attribute.meta, Meta::Path(path) if path.is_ident("nonstandard")) => {
                true
            },
            [attribute, ..] => {
                return Err(syn::Error::new_spanned(
                    attribute,
                    "only `#[nonstandard]` is supported after a tracing field value",
                ));
            },
        };

        Ok(Self { expr, nonstandard })
    }
}
