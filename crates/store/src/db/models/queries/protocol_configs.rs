use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl, SqliteConnection};
use miden_protocol::Word;
use miden_protocol::protocol_config::ProtocolConfig;
use miden_protocol::utils::serde::{ByteReader, Deserializable, Serializable, SliceReader};

use crate::db::schema::protocol_configs;
use crate::errors::DatabaseError;

/// Ensures that a block's active configuration is stored in the current transaction.
pub(crate) fn ensure_protocol_config(
    conn: &mut SqliteConnection,
    commitment: Word,
    supplied: Option<&ProtocolConfig>,
) -> Result<(), DatabaseError> {
    if let Some(config) = supplied {
        let calculated = config.to_commitment();
        if calculated != commitment {
            return Err(DatabaseError::ProtocolConfigCommitmentMismatch {
                expected: commitment,
                calculated,
            });
        }
    }
    if select_protocol_config(conn, commitment)?.is_none() {
        let config = supplied.ok_or(DatabaseError::ProtocolConfigNotFound(commitment))?;
        insert_protocol_config(conn, config)?;
    }
    Ok(())
}

/// Inserts a protocol configuration by its commitment.
pub(crate) fn insert_protocol_config(
    conn: &mut SqliteConnection,
    protocol_config: &ProtocolConfig,
) -> Result<usize, DatabaseError> {
    diesel::insert_into(protocol_configs::table)
        .values((
            protocol_configs::commitment.eq(protocol_config.to_commitment().to_bytes()),
            protocol_configs::protocol_config.eq(protocol_config.to_bytes()),
        ))
        .execute(conn)
        .map_err(Into::into)
}

/// Selects a protocol configuration and verifies its commitment.
pub(crate) fn select_protocol_config(
    conn: &mut SqliteConnection,
    commitment: Word,
) -> Result<Option<ProtocolConfig>, DatabaseError> {
    let bytes = protocol_configs::table
        .filter(protocol_configs::commitment.eq(commitment.to_bytes()))
        .select(protocol_configs::protocol_config)
        .get_result::<Vec<u8>>(conn)
        .optional()?;

    let Some(bytes) = bytes else {
        return Ok(None);
    };

    let mut reader = SliceReader::new(&bytes);
    let protocol_config = ProtocolConfig::read_from(&mut reader)?;
    if reader.has_more_bytes() {
        return Err(DatabaseError::DataCorrupted(format!(
            "protocol config {commitment} has trailing bytes"
        )));
    }
    let calculated = protocol_config.to_commitment();
    if calculated != commitment {
        return Err(DatabaseError::ProtocolConfigCommitmentMismatch {
            expected: commitment,
            calculated,
        });
    }

    Ok(Some(protocol_config))
}

#[cfg(test)]
mod tests {
    use diesel::{ExpressionMethods, RunQueryDsl, SqliteConnection};
    use miden_node_utils::fee::test_protocol_config;
    use miden_protocol::Word;
    use miden_protocol::protocol_config::ProtocolConfig;
    use miden_protocol::utils::serde::Serializable;

    use super::{insert_protocol_config, select_protocol_config};
    use crate::db::schema::protocol_configs;
    use crate::errors::DatabaseError;

    fn connection() -> SqliteConnection {
        crate::db::migrations::test_connection()
    }

    #[test]
    fn inserts_and_selects_protocol_config() {
        let mut conn = connection();
        let config = test_protocol_config();
        let commitment = config.to_commitment();

        insert_protocol_config(&mut conn, &config).unwrap();

        assert_eq!(select_protocol_config(&mut conn, commitment).unwrap(), Some(config));
    }

    #[test]
    fn block_config_requires_a_known_matching_configuration() {
        let mut conn = connection();
        let config = test_protocol_config();
        let commitment = config.to_commitment();
        assert!(super::ensure_protocol_config(&mut conn, commitment, None).is_err());
        assert!(super::ensure_protocol_config(&mut conn, Word::empty(), Some(&config)).is_err());
        assert_eq!(select_protocol_config(&mut conn, commitment).unwrap(), None);
        super::ensure_protocol_config(&mut conn, commitment, Some(&config)).unwrap();
        super::ensure_protocol_config(&mut conn, commitment, Some(&config)).unwrap();
        super::ensure_protocol_config(&mut conn, commitment, None).unwrap();
        assert_eq!(select_protocol_config(&mut conn, commitment).unwrap(), Some(config));
    }

    #[test]
    fn config_insert_rolls_back_with_the_block_transaction() {
        use diesel::Connection;
        let mut conn = connection();
        let config = test_protocol_config();
        let commitment = config.to_commitment();
        let result: Result<(), DatabaseError> = conn.transaction(|conn| {
            super::ensure_protocol_config(conn, commitment, Some(&config))?;
            assert_eq!(select_protocol_config(conn, commitment)?, Some(config.clone()));
            Err(DatabaseError::DataCorrupted("block rejected".into()))
        });
        assert!(result.is_err());
        assert_eq!(select_protocol_config(&mut conn, commitment).unwrap(), None);
    }

    #[test]
    fn returns_none_for_unknown_commitment() {
        let mut conn = connection();

        assert_eq!(select_protocol_config(&mut conn, Word::empty()).unwrap(), None);
    }

    #[test]
    fn selects_multiple_protocol_configs_by_commitment() {
        use miden_protocol::asset::AssetId;
        use miden_protocol::testing::account_id::{
            ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET,
            ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET_1,
        };

        let mut conn = connection();
        let first = ProtocolConfig::current(AssetId::new_fungible(
            ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET.try_into().unwrap(),
        ))
        .unwrap();
        let second = ProtocolConfig::current(AssetId::new_fungible(
            ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET_1.try_into().unwrap(),
        ))
        .unwrap();

        insert_protocol_config(&mut conn, &first).unwrap();
        insert_protocol_config(&mut conn, &second).unwrap();

        assert_eq!(select_protocol_config(&mut conn, first.to_commitment()).unwrap(), Some(first));
        assert_eq!(
            select_protocol_config(&mut conn, second.to_commitment()).unwrap(),
            Some(second)
        );
    }

    #[test]
    fn rejects_a_row_stored_under_the_wrong_commitment() {
        let mut conn = connection();
        let config = test_protocol_config();
        let expected = Word::empty();
        let calculated = config.to_commitment();
        diesel::insert_into(protocol_configs::table)
            .values((
                protocol_configs::commitment.eq(expected.to_bytes()),
                protocol_configs::protocol_config.eq(config.to_bytes()),
            ))
            .execute(&mut conn)
            .unwrap();

        assert!(matches!(
            select_protocol_config(&mut conn, expected),
            Err(DatabaseError::ProtocolConfigCommitmentMismatch {
                expected: actual_expected,
                calculated: actual_calculated,
            }) if actual_expected == expected && actual_calculated == calculated
        ));
    }

    #[test]
    fn rejects_invalid_serialized_protocol_config() {
        let mut conn = connection();
        let commitment = Word::empty();
        diesel::insert_into(protocol_configs::table)
            .values((
                protocol_configs::commitment.eq(commitment.to_bytes()),
                protocol_configs::protocol_config.eq(vec![0xff]),
            ))
            .execute(&mut conn)
            .unwrap();

        assert!(matches!(
            select_protocol_config(&mut conn, commitment),
            Err(DatabaseError::DeserializationError(_))
        ));
    }

    #[test]
    fn rejects_trailing_serialized_bytes() {
        let mut conn = connection();
        let config = test_protocol_config();
        let commitment = config.to_commitment();
        let mut bytes = config.to_bytes();
        bytes.push(0xff);
        diesel::insert_into(protocol_configs::table)
            .values((
                protocol_configs::commitment.eq(commitment.to_bytes()),
                protocol_configs::protocol_config.eq(bytes),
            ))
            .execute(&mut conn)
            .unwrap();

        assert!(matches!(
            select_protocol_config(&mut conn, commitment),
            Err(DatabaseError::DataCorrupted(_))
        ));
    }
}
