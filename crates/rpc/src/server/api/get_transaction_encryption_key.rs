use std::time::Instant;

use miden_node_proto::generated as proto;
use miden_node_utils::tracing::miden_instrument;
use tracing::debug;

use super::{CachedEncryptionKey, ENCRYPTION_KEY_CACHE_TTL, Request, RpcMode, RpcService};
use crate::{COMPONENT, LOG_TARGET};

#[tonic::async_trait]
impl proto::server::rpc_api::GetTransactionEncryptionKey for RpcService {
    type Input = ();
    type Output = proto::transaction::TransactionEncryptionKey;

    fn decode(request: ()) -> tonic::Result<Self::Input> {
        Ok(request)
    }

    fn encode(output: Self::Output) -> tonic::Result<proto::transaction::TransactionEncryptionKey> {
        Ok(output)
    }

    #[miden_instrument(
        target = COMPONENT,
        name = "get_transaction_encryption_key",
        skip_all,
        err,
    )]
    async fn handle(
        &self,
        _input: Self::Input,
        metadata: &tonic::metadata::MetadataMap,
        _extensions: &tonic::codegen::http::Extensions,
    ) -> tonic::Result<Self::Output> {
        let original_accept_header = metadata.get(http::header::ACCEPT.as_str()).cloned();

        debug!(target: LOG_TARGET, "Getting transaction encryption key");

        let mut cache = self.encryption_key_cache.lock().await;
        if let Some(cached) = cache.as_ref()
            && cached.fetched_at.elapsed() < ENCRYPTION_KEY_CACHE_TTL
        {
            return Ok(cached.key.clone());
        }

        let mut forwarded_request = Request::new(());
        if let Some(accept) = original_accept_header {
            forwarded_request.metadata_mut().insert(http::header::ACCEPT.as_str(), accept);
        }

        // Nodes connected to a validator ask for it directly, otherwise the request is forwarded.
        let key = match &self.mode {
            RpcMode::Sequencer { validator, .. }
            | RpcMode::FullNode { validator: Some(validator), .. } => validator
                .as_ref()
                .clone()
                .get_transaction_encryption_key(forwarded_request)
                .await
                .map(tonic::Response::into_inner),
            RpcMode::FullNode { source_rpc, validator: None, .. } => source_rpc
                .as_ref()
                .clone()
                .get_transaction_encryption_key(forwarded_request)
                .await
                .map(tonic::Response::into_inner),
        }?;

        *cache = Some(CachedEncryptionKey {
            key: key.clone(),
            fetched_at: Instant::now(),
        });

        Ok(key)
    }
}
