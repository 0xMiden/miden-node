//! Generated protobuf code with conditional compilation
//!
//! This module contains both client and server generated code based on enabled features.
//! The code is organized to support different environments and use cases:
//!
//! - Client code supports both std and `no_std` environments
//! - Server code requires std environment
//! - WASM compatibility is maintained for client code

#[cfg(feature = "client")]
pub mod client {
    //! Client-side generated protobuf code
    //!
    //! Provides gRPC client definitions for remote prover services.
    //! Available in both std and `no_std` variants.

    #[cfg(feature = "std")]
    pub mod std {
        //! Standard library client implementation
        //!
        //! Full-featured client with transport support for native platforms.
        include!("client/std/mod.rs");
    }

    #[cfg(not(feature = "std"))]
    pub mod nostd {
        //! No-std client implementation
        //!
        //! Core/alloc-based client implementation without transport layer.
        include!("client/nostd/mod.rs");
    }

    // Re-export everything from the appropriate variant based on std feature
    #[cfg(feature = "std")]
    pub use std::*;

    #[cfg(not(feature = "std"))]
    pub use nostd::*;
}

#[cfg(feature = "server")]
#[allow(clippy::similar_names)]
#[allow(clippy::doc_markdown)]
#[allow(clippy::default_trait_access)]
pub mod server {
    //! Server-side generated protobuf code
    //!
    //! Provides gRPC server definitions and implementations.
    //! Requires std environment.
    include!("server/mod.rs");
}

// Convenience re-exports based on current environment and features
// Proto re-exports for compatibility - always use server protobuf types for now
// For client-only no-std builds, use nostd client protobuf types
#[cfg(all(feature = "client", not(feature = "server"), not(feature = "std")))]
pub use client::nostd as proto;
// For client-only builds, use client protobuf types
#[cfg(all(feature = "client", not(feature = "server"), feature = "std"))]
pub use client::std as proto;
#[cfg(feature = "server")]
pub use server as proto;
#[cfg(feature = "server")]
pub use server::*;
