use std::collections::HashMap;

use tonic::metadata::AsciiMetadataValue;
use tonic::service::Interceptor;

/// Interceptor designed to inject required metadata into all [`super::ApiClient`] requests.
#[derive(Default, Clone)]
pub struct MetadataInterceptor {
    metadata: HashMap<&'static str, AsciiMetadataValue>,
}

impl MetadataInterceptor {
    /// Adds or overwrites HTTP ACCEPT metadata to the interceptor.
    ///
    /// Provided version string must be ASCII.
    pub fn with_accept_metadata(mut self, version: &str) -> Result<Self, anyhow::Error> {
        let accept_value = format!("application/vnd.miden; version={version}");
        self.metadata.insert("accept", AsciiMetadataValue::try_from(accept_value)?);
        Ok(self)
    }
}

impl Interceptor for MetadataInterceptor {
    fn call(&mut self, request: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        let mut request = request;
        for (key, value) in &self.metadata {
            request.metadata_mut().insert(*key, value.clone());
        }
        Ok(request)
    }
}
