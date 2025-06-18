pub mod config;
pub mod cors;
pub mod crypto;
pub mod formatting;
pub mod grpc;
pub mod logging;
pub mod tracing;
pub mod version;

pub trait ErrorReport: std::error::Error {
    fn as_report(&self) -> String {
        use std::fmt::Write;
        let mut report = self.to_string();

        // SAFETY: write! is suggested by clippy, and is trivially safe usage.
        std::iter::successors(self.source(), |child| child.source())
            .for_each(|source| write!(report, "\nCaused by: {source}").unwrap());

        report
    }
}

impl<T: std::error::Error> ErrorReport for T {}

/// Extends nested results types, allowing them to be flattened.
///
/// Adapted from: https://stackoverflow.com/a/77543839
pub trait FlattenResult<V, OuterError, InnerError>
where
    InnerError: Into<OuterError>,
{
    fn flatten_result(self) -> Result<V, OuterError>;
}

impl<V, OuterError, InnerError> FlattenResult<V, OuterError, InnerError>
    for Result<Result<V, InnerError>, OuterError>
where
    OuterError: From<InnerError>,
{
    fn flatten_result(self) -> Result<V, OuterError> {
        match self {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(inner)) => Err(inner.into()),
            Err(outer) => Err(outer),
        }
    }
}
