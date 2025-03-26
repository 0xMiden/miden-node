use core::time::Duration;
use std::net::IpAddr;

use miden_objects::{Digest, batch::BatchId, block::BlockNumber};
use opentelemetry::{Key, Value, trace::Status};

/// Utility functions for converting types into [`opentelemetry::Value`].
pub trait ToValue {
    fn to_value(&self) -> Value;
}

impl ToValue for Duration {
    fn to_value(&self) -> Value {
        self.as_secs_f64().into()
    }
}

impl ToValue for Digest {
    fn to_value(&self) -> Value {
        self.to_hex().into()
    }
}

impl ToValue for BlockNumber {
    fn to_value(&self) -> Value {
        i64::from(self.as_u32()).into()
    }
}

impl ToValue for BatchId {
    fn to_value(&self) -> Value {
        self.to_hex().into()
    }
}

impl ToValue for usize {
    fn to_value(&self) -> Value {
        i64::try_from(*self).unwrap_or(i64::MAX).into()
    }
}

/// Generates `impl ToValue` blocks for types that are `ToString`.
macro_rules! impl_to_string_to_value {
    ($($t:ty),*) => {
        $(
            impl ToValue for $t {
                fn to_value(&self) -> Value {
                    self.to_string().into()
                }
            }
        )*
    };
}
impl_to_string_to_value!(IpAddr, &str);

/// Generates `impl ToValue` blocks for integer types.
macro_rules! impl_int_to_value {
    ($($t:ty),*) => {
        $(
            impl ToValue for $t {
                fn to_value(&self) -> Value {
                    i64::from(*self).into()
                }
            }
        )*
    };
}
impl_int_to_value!(u16, u32);

/// Generates `impl ToValue` blocks for types that are `Into<Value>`.
macro_rules! impl_to_value {
    ($($t:ty),*) => {
        $(
            impl ToValue for $t {
                fn to_value(&self) -> Value {
                    (*self).into()
                }
            }
        )*
    };
}
impl_to_value!(f64, i64);

/// Utility functions based on [`tracing_opentelemetry::OpenTelemetrySpanExt`].
///
/// This is a sealed trait. It and cannot be implemented outside of this module.
pub trait OpenTelemetrySpanExt: private::Sealed {
    fn set_attribute(&self, key: impl Into<Key>, value: impl ToValue);
    fn set_error(&self, err: &dyn std::error::Error);
}

impl<S> OpenTelemetrySpanExt for S
where
    S: tracing_opentelemetry::OpenTelemetrySpanExt,
{
    /// Sets an attribute on `Span`.
    ///
    /// Implementations for `ToValue` should be added to this crate (miden-node-utils).
    fn set_attribute(&self, key: impl Into<Key>, value: impl ToValue) {
        tracing_opentelemetry::OpenTelemetrySpanExt::set_attribute(self, key, value.to_value());
    }

    /// Sets a status on `Span` based on an error.
    fn set_error(&self, err: &dyn std::error::Error) {
        // Include the main error and then append causation report.
        let mut report = err.to_string();

        std::iter::successors(err.source(), |child| child.source())
            .for_each(|source| report.push_str(&format!("\nCaused by: {source}")));

        tracing_opentelemetry::OpenTelemetrySpanExt::set_status(
            self,
            Status::Error { description: report.into() },
        );
    }
}

mod private {
    pub trait Sealed {}
    impl<S> Sealed for S where S: tracing_opentelemetry::OpenTelemetrySpanExt {}
}
