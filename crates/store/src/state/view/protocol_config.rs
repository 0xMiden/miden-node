use miden_protocol::Word;
use miden_protocol::protocol_config::ProtocolConfig;

use super::StateView;
use crate::errors::DatabaseError;

impl StateView {
    /// Returns the protocol configuration with the specified commitment.
    pub async fn get_protocol_config(
        &self,
        commitment: Word,
    ) -> Result<Option<ProtocolConfig>, DatabaseError> {
        self.db.select_protocol_config_by_commitment(commitment).await
    }
}
