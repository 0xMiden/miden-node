pub mod config;
pub mod cors;
pub mod crypto;
pub mod formatting;
pub mod grpc;
pub mod logging;
pub mod tracing;
pub mod version;

pub trait ErrorReport: std::error::Error {
    /// Returns a string representation of the error and its source chain.
    fn as_report(&self) -> String {
        use std::fmt::Write;
        let mut report = self.to_string();

        // SAFETY: write! is suggested by clippy, and is trivially safe usage.
        std::iter::successors(self.source(), |child| child.source())
            .for_each(|source| write!(report, "\nCaused by: {source}").unwrap());

        report
    }

    /// Creates a new root in the error chain and returns a string representation of the error and
    /// its source chain.
    fn as_report_context(&self, context: &'static str) -> String {
        format!("{context}: \nCaused by: {}", self.as_report())
    }
}

impl<T: std::error::Error> ErrorReport for T {}

#[cfg(test)]
mod tests {
    use crate::ErrorReport;

    #[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
    pub enum TestSourceError {
        #[error("Source error")]
        Source,
    }

    #[derive(thiserror::Error, Debug)]
    pub enum TestError {
        #[error("Parent error")]
        Parent(#[from] TestSourceError),
    }

    #[test]
    fn as_report() {
        let error = TestError::Parent(TestSourceError::Source);
        assert_eq!("Parent error\nCaused by: Source error", error.as_report());
    }

    #[test]
    fn as_report_context() {
        let error = TestError::Parent(TestSourceError::Source);
        assert_eq!(
            "Final error: \nCaused by: Parent error\nCaused by: Source error",
            error.as_report_context("Final error")
        );
    }
}
