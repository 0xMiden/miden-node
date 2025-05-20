use alloc::{collections::btree_map::BTreeMap, string::String};
use tonic::{
    metadata::{AsciiMetadataValue, errors::InvalidMetadataValue},
    service::Interceptor,
};

/// Interceptor designed to inject required metadata into all [`super::RpcClient`] requests.
#[derive(Default, Clone)]
pub struct MetadataInterceptor {
    metadata: BTreeMap<&'static str, AsciiMetadataValue>,
}

impl MetadataInterceptor {
    /// Adds or overwrites metadata to the interceptor.
    pub fn with_metadata(
        mut self,
        key: &'static str,
        value: String,
    ) -> Result<Self, InvalidMetadataValue> {
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
