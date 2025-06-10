use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use anyhow::Context;
use diesel::{
    deserialize::{FromSql, FromSqlRow}, query_dsl::methods::SelectDsl, sql_types::BigInt, sqlite::Sqlite, BoolExpressionMethods, ExpressionMethods, NullableExpressionMethods, OptionalExtension, QueryDsl, Queryable, QueryableByName, RunQueryDsl, Selectable, SelectableHelper, SqliteConnection
};
use miden_lib::utils::Serializable;
use miden_node_proto::{
    domain::account::{AccountInfo, AccountSummary},
    generated::note as proto,
};
use miden_objects::{
    Word,
    account::{AccountDelta, AccountId},
    block::{BlockHeader, BlockNoteIndex, BlockNumber, ProvenBlock},
    crypto::{hash::rpo::RpoDigest, merkle::MerklePath, utils::Deserializable},
    note::{
        NoteAssets, NoteDetails, NoteId, NoteInclusionProof, NoteInputs, NoteMetadata,
        NoteRecipient, NoteScript, Nullifier,
    },
    transaction::TransactionId,
};
use sql::utils::{column_value_as_u64, read_block_number};
use tokio::sync::oneshot;
use tracing::{info, info_span, instrument};

use crate::{
    db::{
        migrations::apply_migrations,
        models::{AccountIdRaw, AccountInfoRawRow, BlockNumberRaw, NullifierRaw},
        pool_manager::{Pool, SqlitePoolManager},
    }, errors::{DatabaseError, DatabaseSetupError, NoteSyncError, StateSyncError}, genesis::GenesisBlock, COMPONENT
};

mod migrations;
#[macro_use]
mod sql;
pub use sql::Page;

mod connection;
mod pool_manager;
#[cfg(test)]
mod query_plan;
mod settings;
#[cfg(test)]
mod tests;
mod transaction;

pub(crate) mod models;
/// [diesel](https://diesel.rs) generated schema
///
/// ```sh
/// cargo binstall diesel_cli
/// sqlite3 -init ./src/db/migrations/001-init.sql ephemeral_setup.db ""
/// diesel setup --datebase-url=./ephemeral_setup.db
/// diesel print-schema > src/db/schema.rs
/// ```
pub(crate) mod schema;

pub type Result<T, E = DatabaseError> = std::result::Result<T, E>;

pub struct Db {
    pool: Pool,
    diesel: deadpool_diesel::sqlite::Pool,
}

#[derive(Debug, PartialEq)]
#[derive(diesel::Queryable, diesel::QueryableByName, diesel::Selectable)]
pub struct NullifierInfo {
    // #[diesel(deserialize_as = NullifierRaw)]
    pub nullifier: NullifierRaw,
    // #[diesel(deserialize_as = BlockNumberRaw)]
    pub block_num: BlockNumberRaw,
}

#[derive(Debug, PartialEq)]
#[derive(diesel::Queryable, diesel::QueryableByName, diesel::Selectable)]
pub struct TransactionSummary {
    // #[diesel(deserialize_as = AccountIdRaw)]
    pub account_id: AccountIdRaw,
    // #[diesel(deserialize_as = BlockNumber)]
    pub block_num: BlockNumberRaw,
    // #[diesel(deserialize_as = TransactionIdRaw)]
    pub transaction_id: TransactionIdRaw,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NoteRecord {
    // #[diesel(deserialize_as = BlockNumberRaw)]
    pub block_num: BlockNumber,
    pub note_index: BlockNoteIndex,
    pub note_id: RpoDigest,
    pub metadata: NoteMetadata,
    pub details: Option<NoteDetails>,
    // #[diesel(deserialize_as = MerklePathRaw)]
    pub merkle_path: MerklePath,
}

impl NoteRecord {
    /// Columns from the `notes` table ordered to match [`Self::from_row`].
    const SELECT_COLUMNS: &'static str = "
            block_num,
            batch_index,
            note_index,
            note_id,
            note_type,
            sender,
            tag,
            aux,
            execution_hint,
            merkle_path,
            assets,
            inputs,
            script,
            serial_num
    ";

    /// Parses a row from the `notes` table. The sql selection must use [`Self::SELECT_COLUMNS`] to
    /// ensure ordering is correct.
    fn from_row(row: &rusqlite::Row<'_>) -> Result<Self> {
        let block_num = read_block_number(row, 0)?;
        let batch_idx = row.get(1)?;
        let note_idx_in_batch = row.get(2)?;
        // SAFETY: We can assume the batch and note indices stored in the DB are valid so this
        // should never panic.
        let note_index = BlockNoteIndex::new(batch_idx, note_idx_in_batch)
            .expect("batch and note index from DB should be valid");
        let note_id = row.get_ref(3)?.as_blob()?;
        let note_id = RpoDigest::read_from_bytes(note_id)?;
        let note_type = row.get::<_, u8>(4)?.try_into()?;
        let sender = AccountId::read_from_bytes(row.get_ref(5)?.as_blob()?)?;
        let tag: u32 = row.get(6)?;
        let aux: u64 = row.get(7)?;
        let aux = aux.try_into().map_err(DatabaseError::InvalidFelt)?;
        let execution_hint = column_value_as_u64(row, 8)?;
        let merkle_path_data = row.get_ref(9)?.as_blob()?;
        let merkle_path = MerklePath::read_from_bytes(merkle_path_data)?;

        let assets = row.get_ref(10)?.as_blob_or_null()?;
        let inputs = row.get_ref(11)?.as_blob_or_null()?;
        let script = row.get_ref(12)?.as_blob_or_null()?;
        let serial_num = row.get_ref(13)?.as_blob_or_null()?;

        debug_assert!(
            (assets.is_some() && inputs.is_some() && script.is_some() && serial_num.is_some())
                || (assets.is_none()
                    && inputs.is_none()
                    && script.is_none()
                    && serial_num.is_none()),
            "assets, inputs, script, serial_num must be either all present or all absent"
        );
        let details = if let (Some(assets), Some(inputs), Some(script), Some(serial_num)) =
            (assets, inputs, script, serial_num)
        {
            Some(NoteDetails::new(
                NoteAssets::read_from_bytes(assets)?,
                NoteRecipient::new(
                    Word::read_from_bytes(serial_num)?,
                    NoteScript::from_bytes(script)?,
                    NoteInputs::read_from_bytes(inputs)?,
                ),
            ))
        } else {
            None
        };

        let metadata =
            NoteMetadata::new(sender, note_type, tag.into(), execution_hint.try_into()?, aux)?;

        Ok(NoteRecord {
            block_num,
            note_index,
            note_id,
            metadata,
            details,
            merkle_path,
        })
    }
}

impl From<NoteRecord> for proto::Note {
    fn from(note: NoteRecord) -> Self {
        Self {
            block_num: note.block_num.as_u32(),
            note_index: note.note_index.leaf_index_value().into(),
            note_id: Some(note.note_id.into()),
            metadata: Some(note.metadata.into()),
            merkle_path: Some(Into::into(&note.merkle_path)),
            details: note.details.as_ref().map(Serializable::to_bytes),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct StateSyncUpdate {
    pub notes: Vec<NoteSyncRecord>,
    pub block_header: BlockHeader,
    pub account_updates: Vec<AccountSummary>,
    pub transactions: Vec<TransactionSummary>,
}

#[derive(Debug, PartialEq)]
pub struct NoteSyncUpdate {
    pub notes: Vec<NoteSyncRecord>,
    pub block_header: BlockHeader,
}

#[derive(Debug, Clone, PartialEq, Queryable, Selectable)]
pub struct NoteSyncRecord {
    pub block_num: BlockNumber,
    pub note_index: BlockNoteIndex,
    pub note_id: RpoDigest,
    pub metadata: NoteMetadata,
    pub merkle_path: MerklePath,
}

impl From<NoteSyncRecord> for proto::NoteSyncRecord {
    fn from(note: NoteSyncRecord) -> Self {
        Self {
            note_index: note.note_index.leaf_index_value().into(),
            note_id: Some(note.note_id.into()),
            metadata: Some(note.metadata.into()),
            merkle_path: Some(Into::into(&note.merkle_path)),
        }
    }
}

impl From<NoteRecord> for NoteSyncRecord {
    fn from(note: NoteRecord) -> Self {
        Self {
            block_num: note.block_num,
            note_index: note.note_index,
            note_id: note.note_id,
            metadata: note.metadata,
            merkle_path: note.merkle_path,
        }
    }
}

impl Db {
    /// Creates a new database and inserts the genesis block.
    #[instrument(
        target = COMPONENT,
        name = "store.database.bootstrap",
        skip_all,
        fields(path=%database_filepath.display())
        err,
    )]
    pub fn bootstrap(database_filepath: PathBuf, genesis: &GenesisBlock) -> anyhow::Result<()> {
        // Create database.
        //
        // This will create the file if it does not exist, but will also happily open it if already
        // exists. In the latter case we will error out when attempting to insert the genesis
        // block so this isn't such a problem.
        let mut conn = connection::Connection::open(database_filepath)
            .context("failed to open a database connection")?;

        // Run migrations.
        apply_migrations(&mut conn).context("failed to apply database migrations")?;

        // Insert genesis block data.
        let db_tx = conn.transaction().context("failed to create database transaction")?;
        let genesis = genesis.inner();
        sql::apply_block(
            &db_tx,
            genesis.header(),
            &[],
            &[],
            genesis.updated_accounts(),
            genesis.transactions(),
        )
        .context("failed to insert genesis block")?;
        db_tx.commit().context("failed to commit database transaction")
    }

    /// Avoid repeated boilerplate
    pub(crate) async fn query<Q, R, M>(&self, msg: M, query: Q) -> Result<R>
    where
        Q: Send + FnOnce(&mut SqliteConnection) -> Result<R> + 'static,
        R: Send + 'static,
        M: Send + ToString,
    {
        self.diesel
            .get()
            .await?
            .interact(query)
            .await
            .map_err(|err| DatabaseError::interact(&msg.to_string(), &err))?
    }

    /// Open a connection to the DB and apply any pending migrations.
    #[instrument(target = COMPONENT, skip_all)]
    pub async fn load(database_filepath: PathBuf) -> Result<Self, DatabaseSetupError> {
        let sqlite_pool_manager = SqlitePoolManager::new(database_filepath.clone());
        let pool = Pool::builder(sqlite_pool_manager).build()?;

        info!(
            target: COMPONENT,
            sqlite= %database_filepath.display(),
            "Connected to the database"
        );

        let conn = pool.get().await.map_err(DatabaseError::MissingDbConnection)?;

        conn.interact(apply_migrations)
            .await
            .map_err(|err| DatabaseError::interact("Migration", &err))??;

        // TODO rationalize magic numbers, and make them constant
        let manager = deadpool_diesel::sqlite::Manager::new(
            database_filepath.to_str().unwrap().to_owned(),
            deadpool_diesel::sqlite::Runtime::Tokio1,
        );
        let diesel = deadpool_diesel::sqlite::Pool::builder(manager).max_size(16).build()?;

        Ok(Db { pool, diesel })
    }

    /// Loads all the nullifiers from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_all_nullifiers(&self) -> Result<Vec<NullifierInfo>> {
        self.query("all nullifiers", move |conn| {
            let nullifiers_raw = schema::nullifiers::table.load::<models::NullifierRow>(conn)?;
            Result::<Vec<_>, _>::from_iter(
                nullifiers_raw.into_iter().map(std::convert::TryInto::try_into),
            )
        })
        .await
    }

    /// Loads the nullifiers that match the prefixes from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_nullifiers_by_prefix(
        &self,
        prefix_len: u32,
        nullifier_prefixes: Vec<u32>,
        block_num: BlockNumber,
    ) -> Result<Vec<NullifierInfo>> {
        assert_eq!(prefix_len, 16, "Only 16-bit prefixes are supported");

        self.query("nullifieres by prefix", move |conn| {
            let prefixes =
                nullifier_prefixes.into_iter().map(|prefix| i32::try_from(prefix).unwrap()); // TODO XXX ensure these type conversions are sane
            let nullifiers_raw =
                SelectDsl::select(schema::nullifiers::table, models::NullifierRow::as_select())
                    .filter(schema::nullifiers::nullifier_prefix.eq_any(prefixes))
                    .filter(schema::nullifiers::block_num.ge(i64::from(block_num.as_u32())))
                    .order(schema::nullifiers::block_num.asc())
                    .load::<models::NullifierRow>(conn)?;
            Result::<Vec<_>, _>::from_iter(
                nullifiers_raw.into_iter().map(std::convert::TryInto::try_into),
            )
        })
        .await
    }

    /// Search for a [BlockHeader] from the database by its `block_num`.
    ///
    /// When `block_number` is [None], the latest block header is returned.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_block_header_by_block_num(
        &self,
        block_number: Option<BlockNumber>,
    ) -> Result<Option<BlockHeader>> {
        self.query("block headers by block number", move |conn| {
            let sel = SelectDsl::select(
                schema::block_headers::table,
                models::BlockHeadersRow::as_select(),
            );
            let row = if let Some(block_number) = block_number {
                sel.find(i64::from(block_number.as_u32()))
                    .first::<models::BlockHeadersRow>(conn)
                    .optional()
                // invariant: only one block exists with the given block header, so the length is
                // always zero or one
            } else {
                sel.order(schema::block_headers::block_header.desc()).first(conn).optional()
            }?;
            row.map(std::convert::TryInto::try_into).transpose()
        })
        .await
    }

    /// Loads multiple block headers from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_block_headers(
        &self,
        blocks: impl Iterator<Item = BlockNumber> + Send + 'static,
    ) -> Result<Vec<BlockHeader>> {
        self.query("block headers from given block numbers", |conn| {
            let blocks = Vec::from_iter(blocks.map(|block_num| i64::from(block_num.as_u32())));
            let raw = 
                schema::block_headers::table.filter(
                schema::block_headers::block_num.eq_any(&blocks[..])
            )
            .load::<models::BlockHeadersRow>(conn)?;
            Result::<Vec<_>>::from_iter(raw.into_iter().map(std::convert::TryInto::try_into))
        })
        .await
    }

    /// Loads all the block headers from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_all_block_headers(&self) -> Result<Vec<BlockHeader>> {
        self.query("all block headers", |conn| {
            let raw = QueryDsl::select(
                schema::block_headers::table,
                models::BlockHeadersRow::as_select(),
            )
            .load::<models::BlockHeadersRow>(conn)?;
            Result::<Vec<_>>::from_iter(raw.into_iter().map(std::convert::TryInto::try_into))
        })
        .await
    }

    /// Loads all the account commitments from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_all_account_commitments(&self) -> Result<Vec<(AccountId, RpoDigest)>> {
        self.pool
            .get()
            .await?
            .interact(|conn| {
                let transaction = conn.transaction()?;
                sql::select_all_account_commitments(&transaction)
            })
            .await
            .map_err(|err| {
                DatabaseError::InteractError(format!(
                    "Select account commitments task failed: {err}"
                ))
            })?
    }

    /// Loads public account details from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_account(&self, id: AccountId) -> Result<AccountInfo> {
        self.query("Get account details", move |conn| {
            let val =
                QueryDsl::select(schema::accounts::table, models::AccountInfoRawRow::as_select())
                    .find(id.to_bytes())
                    .first::<models::AccountInfoRawRow>(conn)?;
            val.try_into()
        })
        .await
    }

    /// Loads public account details from the DB based on the account ID's prefix.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_network_account_by_prefix(
        &self,
        id_prefix: u32,
    ) -> Result<Option<AccountInfo>> {
        self.query("Get account by id prefix", move |conn| {
            let maybe_info = QueryDsl::filter(
                QueryDsl::select(schema::accounts::table, AccountInfoRawRow::as_select()),
                schema::accounts::network_account_id_prefix.eq(Some(i64::from(id_prefix))),
            )
            .first(conn)
            .optional()?;
            maybe_info.map(std::convert::TryInto::try_into).transpose()
        })
        .await
    }

    /// Loads public accounts details from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_accounts_by_ids(
        &self,
        account_ids: Vec<AccountId>,
    ) -> Result<Vec<AccountInfo>> {
        self.query("Select account by id set", move |conn| {
            let account_ids = account_ids.iter().map(|account_id| account_id.to_bytes().clone());

            let accounts_raw = QueryDsl::filter(
                QueryDsl::select(schema::accounts::table, models::AccountInfoRawRow::as_select()),
                schema::accounts::account_id.eq_any(account_ids),
            )
            .load(conn)?;
            Result::<Vec<_>, _>::from_iter(
                accounts_raw.into_iter().map(std::convert::TryInto::try_into),
            )
        })
        .await
    }

    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn get_state_sync(
        &self,
        block_number: BlockNumber,
        account_ids: Vec<AccountId>,
        note_tags: Vec<u32>,
    ) -> Result<StateSyncUpdate, StateSyncError> {
        
        let state = self.query("state sync", move |conn| {
            
            let desired_senders = Vec::from_iter(account_ids.iter().map(|id| id.to_bytes()));
            let desired_note_tags = Vec::from_iter(note_tags.iter().map(|tag| *tag as i32 ));

            // select notes since block by tag and sender
            let notes = schema::notes::table
                // .select(S)
                // find the next block which contains at least one note with a matching tag or sender
                .filter(schema::notes::block_num.eq(
                    QueryDsl::select(schema::notes::table, &schema::notes::block_num)
                        .filter(
                            schema::notes::tag.eq_any(&desired_note_tags[..])
                            .or(schema::notes::sender.eq_any(&desired_senders[..]))
                        )
                        .order_by(schema::notes::block_num.asc())
                        .limit(1)
                        .single_value()
                ))
                // filter the block's notes and return only the ones matching the requested tags or senders
                .filter(
                    schema::notes::tag.eq_any(&desired_note_tags)
                    .or(schema::notes::sender.eq_any(&desired_senders))
                )
                .load::<NoteSyncRecord>(conn)?;

            // select block header by block num
            let maybe_note_block_num = notes.first().map(|note| note.block_num);
            let block_header = {
                let sel = SelectDsl::select(
                    schema::block_headers::table,
                    models::BlockHeadersRow::as_select(),
                );
                let row = if let Some(block_number) = maybe_note_block_num {
                    sel.find(i64::from(block_number.as_u32()))
                        .first::<models::BlockHeadersRow>(conn)
                        .optional()
                    // invariant: only one block exists with the given block header, so the length is
                    // always zero or one
                } else {
                    sel.order(schema::block_headers::block_header.desc()).first(conn).optional()
                }?;
                row.map(std::convert::TryInto::try_into).transpose()
            }?.ok_or_else(|| StateSyncError::EmptyBlockHeadersTable)?;
            
            // select accounts by block range
            let account_ids = account_ids.iter().map(|id| id.to_bytes());
            let block_start: i64 = block_number.as_u32() as _;
            let block_end: i64 = block_header.block_num() as _; 
            let account_updates = SelectDsl::select(
                schema::accounts::table
                , (schema::accounts::account_id, schema::accounts::account_commitment, schema::accounts::block_num))
                .filter(schema::accounts::block_num.gt(block_start))
                .filter(schema::accounts::block_num.le(block_end))
                .filter(schema::accounts::account_id.eq_any(&account_ids))
                .order(schema::accounts::block_num.asc())
                .load::<AccountSummary>(conn)?;
            // select transactions by accounts and block range
            let transactions = 
                SelectDsl::select(schema::accounts::table, (schema::transactions::account_id, schema::transactions::block_num, schema::transactions::transaction_id))
                .filter(schema::transactions::block_num.gt(block_start))
                .filter(schema::transactions::block_num.le(block_end))
                .filter(schema::transactions::account_id.eq_any(&account_ids))
                .order(schema::transactions::transaction_id.asc())
                .load::<TransactionSummary>(conn)?;
            
            Ok(StateSyncUpdate {
                notes,
                block_header,
                account_updates,
                transactions,
            })
        })
        .await?;
        Ok(state)
    }

    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn get_note_sync(
        &self,
        block_num: BlockNumber,
        note_tags: Vec<u32>,
    ) -> Result<NoteSyncUpdate, NoteSyncError> {
        self.pool
            .get()
            .await
            .map_err(DatabaseError::MissingDbConnection)?
            .interact(move |conn| {
                let transaction = conn.transaction().map_err(DatabaseError::SqliteError)?;
                sql::get_note_sync(&transaction, block_num, &note_tags)
            })
            .await
            .map_err(|err| {
                DatabaseError::InteractError(format!("Get notes sync task failed: {err}"))
            })?
    }

    /// Loads all the Note's matching a certain NoteId from the database.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_notes_by_id(&self, note_ids: Vec<NoteId>) -> Result<Vec<NoteRecord>> {
        self.pool
            .get()
            .await?
            .interact(move |conn| {
                let transaction = conn.transaction()?;
                sql::select_notes_by_id(&transaction, &note_ids)
            })
            .await
            .map_err(|err| {
                DatabaseError::InteractError(format!("Select note by id task failed: {err}"))
            })?
    }

    /// Loads inclusion proofs for notes matching the given IDs.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_note_inclusion_proofs(
        &self,
        note_ids: BTreeSet<NoteId>,
    ) -> Result<BTreeMap<NoteId, NoteInclusionProof>> {
        self.pool
            .get()
            .await?
            .interact(move |conn| {
                let transaction = conn.transaction()?;
                sql::select_note_inclusion_proofs(&transaction, note_ids)
            })
            .await
            .map_err(|err| {
                DatabaseError::InteractError(format!(
                    "Select block note inclusion proofs task failed: {err}"
                ))
            })?
    }

    /// Loads all note IDs matching a certain NoteId from the database.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_note_ids(&self, note_ids: Vec<NoteId>) -> Result<BTreeSet<NoteId>> {
        self.select_notes_by_id(note_ids)
            .await
            .map(|notes| notes.into_iter().map(|note| note.note_id.into()).collect())
    }

    /// Inserts the data of a new block into the DB.
    ///
    /// `allow_acquire` and `acquire_done` are used to synchronize writes to the DB with writes to
    /// the in-memory trees. Further details available on [super::state::State::apply_block].
    // TODO: This span is logged in a root span, we should connect it to the parent one.
    #[instrument(target = COMPONENT, skip_all, err)]
    pub async fn apply_block(
        &self,
        allow_acquire: oneshot::Sender<()>,
        acquire_done: oneshot::Receiver<()>,
        block: ProvenBlock,
        notes: Vec<(NoteRecord, Option<Nullifier>)>,
    ) -> Result<()> {
        self.pool
            .get()
            .await?
            .interact(move |conn| -> Result<()> {
                // TODO: This span is logged in a root span, we should connect it to the parent one.
                let _span = info_span!(target: COMPONENT, "write_block_to_db").entered();

                let transaction = conn.transaction()?;
                sql::apply_block(
                    &transaction,
                    block.header(),
                    &notes,
                    block.created_nullifiers(),
                    block.updated_accounts(),
                    block.transactions(),
                )?;

                let _ = allow_acquire.send(());
                acquire_done.blocking_recv()?;

                transaction.commit()?;

                Ok(())
            })
            .await
            .map_err(|err| {
                DatabaseError::InteractError(format!("Apply block task failed: {err}"))
            })??;

        Ok(())
    }

    /// Merges all account deltas from the DB for given account ID and block range.
    /// Note, that `from_block` is exclusive and `to_block` is inclusive.
    ///
    /// Returns `Ok(None)` if no deltas were found in the DB for the specified account within
    /// the given block range.
    pub(crate) async fn select_account_state_delta(
        &self,
        account_id: AccountId,
        from_block: BlockNumber,
        to_block: BlockNumber,
    ) -> Result<Option<AccountDelta>> {
        self.pool
            .get()
            .await
            .map_err(DatabaseError::MissingDbConnection)?
            .interact(move |conn| {
                let transaction = conn.transaction()?;
                sql::select_account_delta(&transaction, account_id, from_block, to_block)
            })
            .await
            .map_err(|err| DatabaseError::InteractError(err.to_string()))?
    }

    /// Runs database optimization.
    #[instrument(level = "debug", target = COMPONENT, skip_all, err)]
    pub async fn optimize(&self) -> Result<(), DatabaseError> {
        self.pool
            .get()
            .await?
            .interact(move |conn| -> Result<()> {
                conn.execute("PRAGMA optimize;", ())
                    .map(|_| ())
                    .map_err(DatabaseError::SqliteError)
            })
            .await
            .map_err(|err| {
                DatabaseError::InteractError(format!("Database optimization task failed: {err}"))
            })?
    }

    /// Loads the network notes that have not been consumed yet, using pagination to limit the
    /// number of notes returned.
    pub(crate) async fn select_unconsumed_network_notes(
        &self,
        page: Page,
    ) -> Result<(Vec<NoteRecord>, Page)> {
        self.pool
            .get()
            .await
            .map_err(DatabaseError::MissingDbConnection)?
            .interact(move |conn| sql::unconsumed_network_notes(&conn.transaction()?, page))
            .await
            .map_err(|err| DatabaseError::InteractError(err.to_string()))?
    }
}
