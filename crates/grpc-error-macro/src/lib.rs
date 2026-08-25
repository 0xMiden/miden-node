//! Procedural macro for deriving the `GrpcError` trait on error enums.
//!
//! This macro simplifies the creation of gRPC-compatible error enums by automatically:
//! - Generating a companion error enum for gRPC serialization
//! - Implementing the `GrpcError` trait
//! - Providing proper error code mappings
//! - Generating `From<Error> for tonic::Status` conversion
//!
//! # Example
//!
//! ```rust,ignore
//! use miden_node_grpc_error_macro::GrpcError;
//! use thiserror::Error;
//!
//! #[derive(Debug, Error, GrpcError)]
//! pub enum GetNoteScriptByRootError {
//!     #[error("database error")]
//!     #[grpc(internal)]
//!     DatabaseError(#[from] DatabaseError),
//!     
//!     #[error("malformed script root")]
//!     DeserializationFailed,
//!     
//!     #[error("script with given root doesn't exist")]
//!     ScriptNotFound,
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Ident, parse_macro_input};

/// Derives the `GrpcError` trait for an error enum.
///
/// # Attributes
///
/// - `#[grpc(<code>)]` - Sets the variant's gRPC status code: `internal`, `invalid_argument`,
///   `not_found`, `failed_precondition` or `resource_exhausted`. Variants without the attribute
///   map to `invalid_argument`. Internal variants collapse into a shared `Internal` companion
///   variant and their message is masked as `"Internal error"`.
///
/// # Generated Code
///
/// This macro generates:
/// 1. A companion `*GrpcError` enum with `#[repr(u8)]` for wire serialization
/// 2. An implementation of the `GrpcError` trait for the companion enum
/// 3. A method `api_error()` on the original enum that maps to the companion enum
/// 4. An implementation of `From<Error> for tonic::Status` for automatic error conversion
#[proc_macro_derive(GrpcError, attributes(grpc))]
pub fn derive_grpc_error(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let name = &input.ident;
    let vis = &input.vis;
    let grpc_name = Ident::new(&format!("{name}GrpcError"), name.span());

    let variants = match &input.data {
        Data::Enum(data) => &data.variants,
        _ => {
            return syn::Error::new_spanned(name, "GrpcError can only be derived for enums")
                .to_compile_error()
                .into();
        },
    };

    // Build the GrpcError enum variants
    let mut grpc_variants = Vec::new();
    let mut api_error_arms = Vec::new();
    let mut tonic_code_arms = Vec::new();

    // Always add Internal variant (standard practice for gRPC errors)
    grpc_variants.push(quote! {
        /// Internal server error
        Internal = 0
    });
    let mut discriminant = 1u8;

    for variant in variants {
        let variant_name = &variant.ident;

        // Parse the variant's `#[grpc(<code>)]` attribute; absent means `invalid_argument`.
        let code = match variant_grpc_code(variant) {
            Ok(code) => code,
            Err(error) => return error.to_compile_error().into(),
        };

        // Extract doc comments
        let docs: Vec<_> =
            variant.attrs.iter().filter(|attr| attr.path().is_ident("doc")).collect();

        let pattern = match &variant.fields {
            Fields::Unit => quote! { #name::#variant_name },
            Fields::Unnamed(_) => quote! { #name::#variant_name(..) },
            Fields::Named(_) => quote! { #name::#variant_name { .. } },
        };

        if let Some(tonic_code) = code.tonic_code_ident() {
            // Create a corresponding variant in GrpcError enum
            grpc_variants.push(quote! {
                #(#docs)*
                #variant_name = #discriminant
            });

            api_error_arms.push(quote! {
                #pattern => #grpc_name::#variant_name
            });

            tonic_code_arms.push(quote! {
                Self::#variant_name => tonic::Code::#tonic_code
            });

            discriminant += 1;
        } else {
            // Map to Internal variant
            api_error_arms.push(quote! {
                #pattern => #grpc_name::Internal
            });
        }
    }

    let expanded = quote! {
        #[derive(Debug, Copy, Clone, PartialEq, Eq)]
        #[repr(u8)]
        #vis enum #grpc_name {
            #(#grpc_variants,)*
        }

        impl #grpc_name {
            /// Returns the error code for this gRPC error.
            pub fn api_code(self) -> u8 {
                self as u8
            }

            /// Returns true if this is an internal server error.
            pub fn is_internal(&self) -> bool {
                matches!(self, Self::Internal)
            }

            /// Returns the appropriate tonic code for this error.
            pub fn tonic_code(&self) -> tonic::Code {
                match self {
                    Self::Internal => tonic::Code::Internal,
                    #(#tonic_code_arms,)*
                }
            }
        }

        impl #name {
            /// Maps this error to its gRPC error code representation.
            pub fn api_error(&self) -> #grpc_name {
                match self {
                    #(#api_error_arms,)*
                }
            }
        }

        // Automatically implement From<Error> for tonic::Status
        impl From<#name> for tonic::Status {
            fn from(value: #name) -> Self {
                let api_error = value.api_error();

                let message = if api_error.is_internal() {
                    "Internal error".to_owned()
                } else {
                    // Use ErrorReport trait to get detailed error message
                    use miden_node_utils::ErrorReport as _;
                    value.as_report()
                };

                tonic::Status::with_details(
                    api_error.tonic_code(),
                    message,
                    vec![api_error.api_code()].into(),
                )
            }
        }

        impl ::miden_node_utils::tracing::GrpcFault for #name {
            fn is_server_fault(&self) -> bool {
                ::miden_node_utils::tracing::is_server_fault_code(self.api_error().tonic_code())
            }
        }
    };

    TokenStream::from(expanded)
}

/// The gRPC code assigned to an error variant via `#[grpc(<code>)]`.
#[derive(Clone, Copy)]
enum GrpcVariantCode {
    Internal,
    InvalidArgument,
    NotFound,
    FailedPrecondition,
    ResourceExhausted,
}

impl GrpcVariantCode {
    /// The identifier of the corresponding `tonic::Code` variant.
    ///
    /// `None` for internal errors, which collapse into the companion enum's `Internal` variant.
    fn tonic_code_ident(self) -> Option<Ident> {
        let name = match self {
            Self::Internal => return None,
            Self::InvalidArgument => "InvalidArgument",
            Self::NotFound => "NotFound",
            Self::FailedPrecondition => "FailedPrecondition",
            Self::ResourceExhausted => "ResourceExhausted",
        };
        Some(Ident::new(name, proc_macro2::Span::call_site()))
    }
}

/// Parses a variant's `#[grpc(<code>)]` attribute; a variant without one maps to
/// `invalid_argument`.
fn variant_grpc_code(variant: &syn::Variant) -> syn::Result<GrpcVariantCode> {
    let mut code = GrpcVariantCode::InvalidArgument;
    for attr in &variant.attrs {
        if !attr.path().is_ident("grpc") {
            continue;
        }
        let ident: Ident = attr.parse_args()?;
        code = match ident.to_string().as_str() {
            "internal" => GrpcVariantCode::Internal,
            "invalid_argument" => GrpcVariantCode::InvalidArgument,
            "not_found" => GrpcVariantCode::NotFound,
            "failed_precondition" => GrpcVariantCode::FailedPrecondition,
            "resource_exhausted" => GrpcVariantCode::ResourceExhausted,
            other => {
                return Err(syn::Error::new_spanned(
                    &ident,
                    format!(
                        "unsupported gRPC code `{other}`; use one of: internal, \
                         invalid_argument, not_found, failed_precondition, resource_exhausted"
                    ),
                ));
            },
        };
    }
    Ok(code)
}
