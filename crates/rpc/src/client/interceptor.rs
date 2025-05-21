use std::collections::HashMap;

use tonic::{metadata::AsciiMetadataValue, service::Interceptor};

/// Interceptor designed to inject required metadata into all [`super::ApiClient`] requests.
#[derive(Default)]
pub struct MetadataInterceptor {
    metadata: HashMap<&'static str, AsciiMetadataValue>,
}

impl MetadataInterceptor {
    /// Adds or overwrites metadata to the interceptor.
    pub fn with_metadata(
        mut self,
        key: &'static str,
        value: String,
    ) -> Result<Self, anyhow::Error> {
        self.metadata.insert(key, AsciiMetadataValue::try_from(value)?);
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
