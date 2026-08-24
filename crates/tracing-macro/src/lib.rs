use std::collections::BTreeSet;

use proc_macro::TokenStream;
use proc_macro2::{Delimiter, Group, TokenStream as TokenStream2, TokenTree};
use quote::{ToTokens, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::token::Dot;
use syn::visit::Visit;
use syn::{Block, Expr, Ident, ItemFn, Macro, Result, Stmt, Token, parse_macro_input, parse_quote};

const ALLOWED_FIELD_NAMES: &[&str] = &[
    "account.id",
    "account.id.network_prefix",
    "account.ids",
    "account.ids.count",
    "account.updated",
    "batch.id",
    "batch.account_updates.count",
    "batch.expires_at",
    "batch.expiration_height",
    "batch.input_notes.count",
    "batch.output_notes.count",
    "batch.reference_block.commitment",
    "batch.reference_block.number",
    "block.batch.ids",
    "block.batches.count",
    "block.batches.output_notes.count",
    "block.commitment",
    "block.commitments.account",
    "block.commitments.chain",
    "block.commitments.kernel",
    "block.commitments.note",
    "block.commitments.nullifier",
    "block.commitments.transaction",
    "block.erased_note_proofs.count",
    "block.erased_notes.count",
    "block.from",
    "block.nullifiers.count",
    "block.number",
    "block.output_notes.count",
    "block.prev_block_commitment",
    "block.protocol.version",
    "block.size",
    "block.sub_commitment",
    "block.timestamp",
    "block.transactions.ids",
    "block.transactions.count",
    "block.updated_accounts.count",
    "block_range.from",
    "block_range.to",
    "current_client_block_height",
    "cutoff_block",
    "db.account_state_forest.size",
    "db.account_tree.size",
    "db.block_store.size",
    "db.nullifier_tree.size",
    "db.sqlite.size",
    "db.sqlite.wal.size",
    "dice_roll",
    "failure_rate",
    "finality_level",
    "inputs_size",
    "mempool.accounts",
    "mempool.batches.proposed",
    "mempool.batches.proven",
    "mempool.nullifiers",
    "mempool.output_notes",
    "mempool.transactions.unbatched",
    "mempool.transactions.uncommitted",
    "note.id",
    "notes.count",
    "nullifiers",
    "path",
    "port",
    "prefix_len",
    "prefixes",
    "proof_size",
    "prover",
    "prover.kind",
    "reference_block.number",
    "request.kind",
    "script.root",
    "snapshot.block_num",
    "snapshot.lifetime_ms",
    "snapshots.live",
    "transaction.id",
    "transaction.expires_at",
    "transaction.input_notes.count",
    "transaction.output_notes.count",
    "transaction.reference_block.commitment",
    "transaction.reference_block.number",
    "tip.number",
    "transactions.count",
    "transactions.ids",
    "transactions.input_notes.count",
    "transactions.output_notes.count",
    "transactions.unauthenticated_notes.count",
    "workers.active",
    "workers.capacity",
    "workers.count",
];

/// A drop-in replacement for `tracing::instrument` enforcing the node's telemetry conventions.
///
/// Differences from `tracing::instrument`:
///
/// - Function arguments are never recorded: `skip_all` is always applied, and explicit `skip` /
///   `skip_all` directives are rejected. Span fields must instead be declared with `fields(...)`
///   or recorded later in the body with `miden_span_record!`; either way the field names are
///   validated against the node's allowed span field names, and recorded fields are inferred and
///   pre-declared as empty on the span.
/// - The `err` directive gains a `fault_only` mode for request handlers:
///
///   ```ignore
///   #[miden_instrument(
///       target = COMPONENT,
///       name = "block_producer.api.submit_proven_tx",
///       err(fault_only),
///   )]
///   ```
///
///   Where plain `err` unconditionally reports a returned `Err`, `err(fault_only)` classifies it
///   via the `GrpcFault` trait (`miden_node_utils::tracing`): node faults are logged at `ERROR`
///   with the full error report and mark the span with `OTel` error status, while client-caused
///   failures are logged at debug level and leave the span status untouched — rejecting a bad
///   request is the node behaving correctly, not an application error. An optional
///   `level = "..."` (`"trace"` through `"error"`) tunes the level of the fault-side event only,
///   e.g. `err(fault_only, level = "warn")`; span error status always follows the classification
///   regardless of level.
///
/// All other arguments are forwarded to `tracing::instrument` unchanged.
#[proc_macro_attribute]
pub fn miden_instrument(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = TokenStream2::from(attr);
    let mut function = parse_macro_input!(item as ItemFn);
    let fields = collect_recorded_fields(&function);
    let (args, fault_level) = match merge_inferred_fields(attr, &fields) {
        Ok(args) => args,
        Err(error) => return error.into_compile_error().into(),
    };
    if let Some(level) = fault_level {
        apply_fault_only_err(&mut function, &level);
    }
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

fn merge_inferred_fields(
    attr: TokenStream2,
    fields: &[FieldPath],
) -> Result<(TokenStream2, Option<TokenStream2>)> {
    validate_explicit_fields(&attr)?;

    let mut args = split_top_level_args(attr);
    reject_skip_directives(&args)?;
    let fault_level = extract_err_directive(&mut args)?;

    // Function arguments often contain large or sensitive values. Always skip them so spans only
    // contain fields explicitly declared by the caller or inferred from `miden_span_record!`.
    args.push(quote! { skip_all });

    if fields.is_empty() {
        return Ok((quote! { #(#args),* }, fault_level));
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
        Ok((quote! { #(#args),* }, fault_level))
    } else {
        Ok((quote! { #(#args,)* fields(#inferred_fields) }, fault_level))
    }
}

/// Rewrites the function body so a returned `Err` is classified via
/// `miden_node_utils::tracing::record_classified_error` from inside the instrumented span: node
/// faults are logged at `level` (`ERROR` unless overridden) and mark the span with `OTel` error
/// status (like `err` would), while client-caused failures are logged at debug level and do not —
/// rejecting a bad request is the node behaving correctly, not an application error.
///
/// `#[async_trait]` methods are expanded before this macro runs, leaving a non-async fn whose
/// body is `Box::pin(async move { ... })`; the classification is applied inside that async block
/// so it runs within the instrumented span.
fn apply_fault_only_err(function: &mut ItemFn, level: &TokenStream2) {
    fn classified(body: &TokenStream2, level: &TokenStream2) -> Block {
        parse_quote! {{
            #[allow(clippy::redundant_async_block, clippy::redundant_closure_call)]
            let __miden_instrument_result = #body;
            if let Err(err) = &__miden_instrument_result {
                ::miden_node_utils::tracing::record_classified_error(err, #level);
            }
            __miden_instrument_result
        }}
    }

    fn wrap_async(statements: &[Stmt], level: &TokenStream2) -> Block {
        classified(&quote! { async move { #(#statements)* }.await }, level)
    }

    if function.sig.asyncness.is_some() {
        let wrapped = wrap_async(&function.block.stmts, level);
        *function.block = wrapped;
        return;
    }

    if let Some(Stmt::Expr(Expr::Call(call), _)) = function.block.stmts.last_mut()
        && call.args.len() == 1
        && let Some(Expr::Async(async_block)) = call.args.first_mut()
    {
        let wrapped = wrap_async(&async_block.block.stmts, level);
        async_block.block = wrapped;
        return;
    }

    let statements = &function.block.stmts;
    let wrapped = classified(&quote! { (move || { #(#statements)* })() }, level);
    *function.block = wrapped;
}

/// Handles the `err` directive, extracting `fault_only` mode when present.
///
/// `fault_only` is a `miden_instrument` extension to `tracing::instrument`'s `err` directive: the
/// returned `Err` is classified instead of unconditionally reported, so only node faults mark the
/// span as failed. An optional `level = "..."` tunes the level of the fault-side event (`ERROR` by
/// default); client-caused failures are always logged at debug level regardless.
///
/// Since `tracing::instrument` would reject `fault_only`, the whole `err(...)` argument is removed
/// from the forwarded list and this returns the level tokens (e.g. `::tracing::Level::WARN`) to
/// emit fault events at. Plain `err` and tracing's own modes are forwarded untouched, but their
/// option idents are validated here so a typo like `err(faultonly)` fails with a targeted message
/// rather than a tracing error pointing at expanded code.
fn extract_err_directive(args: &mut Vec<TokenStream2>) -> Result<Option<TokenStream2>> {
    let mut fault_level = None;
    let mut seen_err: Option<TokenStream2> = None;
    let mut retained = Vec::with_capacity(args.len());

    for arg in args.drain(..) {
        let Some(directive) = err_directive_options(&arg) else {
            retained.push(arg);
            continue;
        };
        if let Some(previous) = &seen_err {
            let mut error =
                syn::Error::new_spanned(&arg, "duplicate `err` directive; only one is allowed");
            error.combine(syn::Error::new_spanned(previous, "first `err` directive here"));
            return Err(error);
        }
        seen_err = Some(arg.clone());

        match directive {
            // Bare `err`: tracing's unconditional error reporting, forwarded as-is.
            ErrDirective::Bare => retained.push(arg),
            ErrDirective::Options(options) => {
                if options.iter().any(|option| is_bare_ident(option, "fault_only")) {
                    fault_level = Some(parse_fault_only_options(&options)?);
                } else {
                    validate_forwarded_err_options(&options)?;
                    retained.push(arg);
                }
            },
        }
    }

    *args = retained;
    Ok(fault_level)
}

/// An `err` directive argument: bare `err` or `err(...)` with its comma-separated options.
enum ErrDirective {
    Bare,
    Options(Vec<TokenStream2>),
}

/// Parses the argument as an `err` directive, returning `None` if it is some other argument.
fn err_directive_options(arg: &TokenStream2) -> Option<ErrDirective> {
    let mut tokens = arg.clone().into_iter();
    match tokens.next() {
        Some(TokenTree::Ident(ident)) if ident == "err" => {},
        _ => return None,
    }

    match tokens.next() {
        None => Some(ErrDirective::Bare),
        Some(TokenTree::Group(group))
            if group.delimiter() == Delimiter::Parenthesis && tokens.next().is_none() =>
        {
            Some(ErrDirective::Options(split_top_level_args(group.stream())))
        },
        _ => None,
    }
}

fn is_bare_ident(arg: &TokenStream2, name: &str) -> bool {
    let mut tokens = arg.clone().into_iter();
    matches!(tokens.next(), Some(TokenTree::Ident(ident)) if ident == name)
        && tokens.next().is_none()
}

/// Parses the options of an `err(fault_only, ...)` directive into the fault event's level tokens.
fn parse_fault_only_options(options: &[TokenStream2]) -> Result<TokenStream2> {
    let mut level = quote! { ::tracing::Level::ERROR };

    for option in options {
        if is_bare_ident(option, "fault_only") {
            continue;
        }

        let name_value: syn::MetaNameValue = syn::parse2(option.clone()).map_err(|_| {
            syn::Error::new_spanned(
                option,
                "unsupported `err(fault_only)` option; only `level = \"...\"` can be combined \
                 with `fault_only`",
            )
        })?;
        if !name_value.path.is_ident("level") {
            return Err(syn::Error::new_spanned(
                option,
                "unsupported `err(fault_only)` option; only `level = \"...\"` can be combined \
                 with `fault_only`",
            ));
        }

        let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(value), .. }) = &name_value.value
        else {
            return Err(syn::Error::new_spanned(
                &name_value.value,
                "`level` must be a string literal: one of \"trace\", \"debug\", \"info\", \
                 \"warn\" or \"error\"",
            ));
        };
        level = match value.value().to_ascii_lowercase().as_str() {
            "trace" => quote! { ::tracing::Level::TRACE },
            "debug" => quote! { ::tracing::Level::DEBUG },
            "info" => quote! { ::tracing::Level::INFO },
            "warn" => quote! { ::tracing::Level::WARN },
            "error" => quote! { ::tracing::Level::ERROR },
            unknown => {
                return Err(syn::Error::new_spanned(
                    value,
                    format!(
                        "unknown level \"{unknown}\"; expected one of \"trace\", \"debug\", \
                         \"info\", \"warn\" or \"error\""
                    ),
                ));
            },
        };
    }

    Ok(level)
}

/// Validates the options of an `err(...)` directive that is forwarded to `tracing::instrument`.
///
/// Forwarded options are parsed by `tracing::instrument` itself; this only rejects idents outside
/// its `err` grammar (`Debug`, `Display`, `level = ...`) so near-misses of `fault_only` fail here
/// with a message that mentions it.
fn validate_forwarded_err_options(options: &[TokenStream2]) -> Result<()> {
    for option in options {
        let mut tokens = option.clone().into_iter();
        let first = tokens.next();
        let is_mode = matches!(
            &first,
            Some(TokenTree::Ident(ident)) if ident == "Debug" || ident == "Display"
        ) && tokens.next().is_none();
        let is_level = matches!(&first, Some(TokenTree::Ident(ident)) if ident == "level");

        if !is_mode && !is_level {
            return Err(syn::Error::new_spanned(
                option,
                "unsupported `err` option; expected `fault_only`, `Debug`, `Display` or `level = \
                 \"...\"`",
            ));
        }
    }

    Ok(())
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

fn validate_explicit_fields(attr: &TokenStream2) -> Result<()> {
    for arg in split_top_level_args(attr.clone()) {
        if let Some(group) = fields_group(&arg) {
            syn::parse2::<InstrumentFields>(group.stream())?;
        }
    }

    Ok(())
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

#[proc_macro]
pub fn miden_span_record(input: TokenStream) -> TokenStream {
    let records = parse_macro_input!(input as RecordFields);
    let records = records.fields.into_iter().map(|field| {
        let name = field.path.name();
        let value = field
            .value
            .expect("record fields are parsed with required values")
            .value_tokens();

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

fn validate_field_name(path: &FieldPath) -> Result<()> {
    let name = path.name();

    if ALLOWED_FIELD_NAMES.contains(&name.as_str()) {
        Ok(())
    } else {
        Err(syn::Error::new_spanned(
            path,
            format!(
                "unsupported tracing field `{name}`; use one of: {}",
                ALLOWED_FIELD_NAMES.join(", "),
            ),
        ))
    }
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
        let shorthand_formatter = if value_required {
            None
        } else {
            Formatter::parse_optional(input)?
        };
        let path = input.parse()?;
        validate_field_name(&path)?;
        let value = if value_required || shorthand_formatter.is_none() && input.peek(Token![=]) {
            input.parse::<Token![=]>()?;
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
    formatter: Formatter,
    expr: Expr,
}

impl RecordValue {
    fn value_tokens(&self) -> TokenStream2 {
        let expr = &self.expr;

        match self.formatter {
            Formatter::Display => quote! { &::tracing::field::display(#expr) },
            Formatter::Debug => quote! { &::tracing::field::debug(#expr) },
            Formatter::Plain => quote! { &#expr },
        }
    }
}

impl Parse for RecordValue {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let formatter = Formatter::parse_optional(input)?.unwrap_or(Formatter::Plain);
        let expr = input.parse()?;

        Ok(Self { formatter, expr })
    }
}

enum Formatter {
    Display,
    Debug,
    Plain,
}

impl Formatter {
    fn parse_optional(input: ParseStream<'_>) -> Result<Option<Self>> {
        if input.peek(Token![%]) {
            input.parse::<Token![%]>()?;
            Ok(Some(Self::Display))
        } else if input.peek(Token![?]) {
            input.parse::<Token![?]>()?;
            Ok(Some(Self::Debug))
        } else {
            Ok(None)
        }
    }
}
