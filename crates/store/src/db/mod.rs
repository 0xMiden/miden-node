// The module contains transitions for database representations
// to in memory representations, where it is _often_ require to
// convert from the database _signed_ representation to the
// in-memory _unsigned_ representations, and hence we'd sprinkle
// the clippy exception around in quite a few places.
// TODO moving away from the `TryInto` implementations
// TODO we should be able to isolate the signed to unsigned and
// TODO vice versa such that this can be removed again.
#![allow(clippy::cast_sign_loss)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use anyhow::Context;
use diesel::{
    BoolExpressionMethods, Connection, ExpressionMethods, JoinOnDsl, NullableExpressionMethods,
    OptionalExtension, QueryDsl, RunQueryDsl, SelectableHelper, SqliteConnection,
    connection::SimpleConnection, query_dsl::methods::SelectDsl,
};
use miden_lib::utils::Serializable;
use miden_node_proto::{
    domain::account::{AccountInfo, AccountSummary},
    generated::note as proto,
};
use miden_objects::{
    account::{AccountDelta, AccountId},
    block::{BlockHeader, BlockNoteIndex, BlockNumber, ProvenBlock},
    crypto::{hash::rpo::RpoDigest, merkle::MerklePath, utils::Deserializable},
    note::{NoteDetails, NoteId, NoteInclusionProof, NoteMetadata, Nullifier},
    transaction::TransactionId,
};
use tokio::sync::oneshot;
use tracing::{info, info_span, instrument};

use crate::{
    COMPONENT,
    db::{
        migrations::apply_migrations,
        models::{
            AccountRaw, AccountSummaryRaw, NoteRecordRaw, NoteSyncRecordRawRow,
            TransactionSummaryRaw, serialize_vec, vec_raw_try_into,
        },
    },
    errors::{DatabaseError, DatabaseSetupError, NoteSyncError, StateSyncError},
    genesis::GenesisBlock,
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
    diesel: deadpool_diesel::sqlite::Pool,
}

#[derive(Debug, PartialEq)]
pub struct NullifierInfo {
    pub nullifier: Nullifier,
    pub block_num: BlockNumber,
}

#[derive(Debug, PartialEq)]
pub struct TransactionSummary {
    pub account_id: AccountId,
    pub block_num: BlockNumber,
    pub transaction_id: TransactionId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NoteRecord {
    pub block_num: BlockNumber,
    pub note_index: BlockNoteIndex,
    pub note_id: RpoDigest,
    pub metadata: NoteMetadata,
    pub details: Option<NoteDetails>,
    pub merkle_path: MerklePath,
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

#[derive(Debug, Clone, PartialEq)]
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
        let mut conn: SqliteConnection =
            diesel::sqlite::SqliteConnection::establish(database_filepath.to_str().unwrap()) // TODO FIXME avoid unwrap
                .context("failed to open a database connection")?;

        // Run migrations.
        apply_migrations(&mut conn).context("failed to apply database migrations")?;

        // Insert genesis block data.
        let genesis = genesis.inner();
        conn.transaction(move |conn| {
            models::queries::apply_block(
                conn,
                genesis.header(),
                &[],
                &[],
                genesis.updated_accounts(),
                genesis.transactions(),
            )
        })
        .context("failed to insert genesis block")?;
        Ok(())
    }

    /// Avoid repeated boilerplate, frame the query in a transaction
    pub(crate) async fn framed<R, E, Q, M>(&self, msg: M, query: Q) -> std::result::Result<R, E>
    where
        Q: Send + FnOnce(&mut SqliteConnection) -> std::result::Result<R, E> + 'static,
        R: Send + 'static,
        M: Send + ToString,
        E: From<DatabaseError>,
        E: From<diesel::result::Error>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let conn = self.diesel.get().await.map_err(DatabaseError::from)?;

        conn.interact(|conn| <_ as diesel::Connection>::transaction::<R, E, Q>(conn, query))
            .await
            .map_err(|err| E::from(DatabaseError::interact(&msg.to_string(), &err)))?
    }

    /// Open a connection to the DB and apply any pending migrations.
    #[instrument(target = COMPONENT, skip_all)]
    pub async fn load(database_filepath: PathBuf) -> Result<Self, DatabaseSetupError> {
        let manager = deadpool_diesel::sqlite::Manager::new(
            database_filepath.to_str().unwrap().to_owned(),
            deadpool_diesel::sqlite::Runtime::Tokio1,
        );
        let diesel = deadpool_diesel::sqlite::Pool::builder(manager).max_size(16).build()?;

        info!(
            target: COMPONENT,
            sqlite= %database_filepath.display(),
            "Connected to the database"
        );

        let me = Db { diesel };
        me.framed("migration", apply_migrations).await?;

        // TODO rationalize magic numbers, and make them
        Ok(me)
    }

    /// Loads all the nullifiers from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_all_nullifiers(&self) -> Result<Vec<NullifierInfo>> {
        self.framed("all nullifiers", move |conn| {
            let nullifiers_raw = schema::nullifiers::table.load::<models::NullifierRawRow>(conn)?;
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

        self.framed("nullifieres by prefix", move |conn| {
            let prefixes =
                nullifier_prefixes.into_iter().map(|prefix| i32::try_from(prefix).unwrap()); // TODO XXX ensure these type conversions are sane
            let nullifiers_raw =
                SelectDsl::select(schema::nullifiers::table, models::NullifierRawRow::as_select())
                    .filter(schema::nullifiers::nullifier_prefix.eq_any(prefixes))
                    .filter(schema::nullifiers::block_num.ge(i64::from(block_num.as_u32())))
                    .order(schema::nullifiers::block_num.asc())
                    .load::<models::NullifierRawRow>(conn)?;
            vec_raw_try_into(nullifiers_raw)
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
        self.framed("block headers by block number", move |conn| {
            let sel = SelectDsl::select(
                schema::block_headers::table,
                models::BlockHeaderRawRow::as_select(),
            );
            let row = if let Some(block_number) = block_number {
                sel.find(i64::from(block_number.as_u32()))
                    .first::<models::BlockHeaderRawRow>(conn)
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
        self.framed("block headers from given block numbers", |conn| {
            let blocks = Vec::from_iter(blocks.map(|block_num| i64::from(block_num.as_u32())));
            let raw = QueryDsl::filter(
                schema::block_headers::table,
                schema::block_headers::block_num.eq_any(&blocks[..]),
            )
            .load::<models::BlockHeaderRawRow>(conn)?;
            vec_raw_try_into(raw)
        })
        .await
    }

    /// Loads all the block headers from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_all_block_headers(&self) -> Result<Vec<BlockHeader>> {
        self.framed("all block headers", |conn| {
            let raw = QueryDsl::select(
                schema::block_headers::table,
                models::BlockHeaderRawRow::as_select(),
            )
            .load::<models::BlockHeaderRawRow>(conn)?;
            vec_raw_try_into(raw)
        })
        .await
    }

    /// Loads all the account commitments from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_all_account_commitments(&self) -> Result<Vec<(AccountId, RpoDigest)>> {
        self.framed("read all account commitments", move |conn| {
            let raw = SelectDsl::select(
                schema::accounts::table,
                (schema::accounts::account_id, schema::accounts::account_commitment),
            )
            .order_by(schema::accounts::block_num.asc())
            .load::<(Vec<u8>, Vec<u8>)>(conn)?;

            std::result::Result::<Vec<_>, _>::from_iter(raw.into_iter().map(
                |(raw_account, raw_commitment)| {
                    let account = AccountId::read_from_bytes(raw_account.as_ref())?;
                    let commitment = RpoDigest::read_from_bytes(raw_commitment.as_ref())?;
                    Ok((account, commitment))
                },
            ))
        })
        .await
    }

    /// Loads public account details from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_account(&self, id: AccountId) -> Result<AccountInfo> {
        self.framed("Get account details", move |conn| {
            let val = QueryDsl::select(schema::accounts::table, models::AccountRaw::as_select())
                .find(id.to_bytes())
                .first::<models::AccountRaw>(conn)?;
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
        self.framed("Get account by id prefix", move |conn| {
            let maybe_info = QueryDsl::filter(
                QueryDsl::select(schema::accounts::table, AccountRaw::as_select()),
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
        self.framed("Select account by id set", move |conn| {
            let account_ids = account_ids.iter().map(|account_id| account_id.to_bytes().clone());

            let accounts_raw = QueryDsl::filter(
                QueryDsl::select(schema::accounts::table, models::AccountRaw::as_select()),
                schema::accounts::account_id.eq_any(account_ids),
            )
            .load(conn)?;
            vec_raw_try_into(accounts_raw)
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
        let state = self.framed::<_,StateSyncError, _,_>("state sync", move |conn| {

            let desired_senders = serialize_vec(account_ids.iter());
            let desired_note_tags = Vec::from_iter(note_tags.iter().map(|tag| i32::from_be_bytes(tag.to_be_bytes()) ));

            // select notes since block by tag and sender
            let desired_block_num: i64 = SelectDsl::select(schema::notes::table, schema::notes::block_num)
            .filter(
                schema::notes::tag.eq_any(&desired_note_tags[..])
                .or(schema::notes::sender.eq_any(&desired_senders[..]))
            )
            .order_by(schema::notes::block_num.asc())
            .limit(1)
            .get_result(conn)
            .optional()
            .map_err(DatabaseError::from)?.unwrap(); // XXX what makes sure it's not None?

            let notes =
                SelectDsl::select(schema::notes::table, NoteSyncRecordRawRow::as_select())
                // find the next block which contains at least one note with a matching tag or sender
                .filter(schema::notes::block_num.eq(
                    desired_block_num
                ))
                // filter the block's notes and return only the ones matching the requested tags or senders
                .filter(
                    schema::notes::tag
                    .eq_any(&desired_note_tags)
                    .or(
                        schema::notes::sender
                        .eq_any(&desired_senders)
                    )
                )
                .load::<NoteSyncRecordRawRow>(conn)
                .map_err(DatabaseError::from)?;

            // select block header by block num
            let maybe_note_block_num = notes.first().map(|note| note.block_num);
            let block_header: BlockHeader = {
                let sel = SelectDsl::select(
                    schema::block_headers::table,
                    models::BlockHeaderRawRow::as_select(),
                );
                let row = if let Some(block_number) = maybe_note_block_num {
                    sel.find(block_number) // TODO
                        .first::<models::BlockHeaderRawRow>(conn)
                        .optional()
                    // invariant: only one block exists with the given block header, so the length is
                    // always zero or one
                } else {
                    sel.order(schema::block_headers::block_header.desc()).first(conn).optional()
                }.map_err(DatabaseError::from)?;
                row.map(std::convert::TryInto::try_into).transpose()
            }?.ok_or_else(|| StateSyncError::EmptyBlockHeadersTable)?;

            // select accounts by block range
            let account_ids = serialize_vec(account_ids.iter());
            let block_start: i64 = block_number.as_u32().into();
            let block_end: i64 = block_header.block_num().as_u32().into();
            let account_updates = SelectDsl::select(
                schema::accounts::table
                , AccountSummaryRaw::as_select())
                // (schema::accounts::account_id, schema::accounts::account_commitment, schema::accounts::block_num))
                .filter(schema::accounts::block_num.gt(block_start))
                .filter(schema::accounts::block_num.le(block_end))
                .filter(schema::accounts::account_id.eq_any(&account_ids))
                .order(schema::accounts::block_num.asc())
                .load::<AccountSummaryRaw>(conn)
                .map_err(DatabaseError::from)?;
            let account_updates = vec_raw_try_into::<AccountSummary,AccountSummaryRaw>(account_updates)?;

            // select transactions by accounts and block range
            let transactions =
                SelectDsl::select(schema::transactions::table,
                    (schema::transactions::account_id, schema::transactions::block_num, schema::transactions::transaction_id)
                )
                .filter(schema::transactions::block_num.gt(block_start))
                .filter(schema::transactions::block_num.le(block_end))
                .filter(schema::transactions::account_id.eq_any(&account_ids))
                .order(schema::transactions::transaction_id.asc())
                .load::<TransactionSummaryRaw>(conn)
                .map_err(DatabaseError::from)?;

            Ok(StateSyncUpdate {
                notes: vec_raw_try_into(notes)?,
                block_header,
                account_updates,
                transactions: vec_raw_try_into(transactions)?,
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
        self.framed("notes sync task", move |conn| {
            let update = models::queries::select_notes_since_block_by_tag_and_sender(
                conn,
                block_num,
                &[],
                &note_tags,
            )?;
            let notes = update.notes;
            let block_header = models::queries::select_block_header_by_block_num(
                conn,
                notes.first().map(|note| note.block_num),
            )?
            .ok_or(NoteSyncError::EmptyBlockHeadersTable)?;
            Ok(NoteSyncUpdate { notes, block_header })
        })
        .await
    }

    /// Loads all the Note's matching a certain NoteId from the database.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_notes_by_id(&self, note_ids: Vec<NoteId>) -> Result<Vec<NoteRecord>> {
        self.framed("note by id", move |conn| {
            let note_ids = serialize_vec(&note_ids);
            let cols = (
                schema::notes::block_num,
                schema::notes::batch_index,
                schema::notes::note_index,
                schema::notes::note_id,
                // // metadata
                schema::notes::note_type,
                schema::notes::sender,
                schema::notes::tag,
                schema::notes::aux,
                schema::notes::execution_hint,
                // details
                schema::notes::assets,
                schema::notes::inputs,
                schema::notes::serial_num,
                // // merkle
                schema::notes::merkle_path,
                schema::note_scripts::script.nullable(),
            );

            let q = schema::notes::table
                .left_join(schema::note_scripts::table.on(
                    schema::notes::script_root.eq(schema::note_scripts::script_root.nullable()),
                ))
                .filter(schema::notes::note_id.eq_any(&note_ids));
            let raw: Vec<_> = SelectDsl::select(
                q, cols, // NoteRecordRaw::as_select()
            )
            .load::<NoteRecordRaw>(conn)?;
            let records = vec_raw_try_into::<NoteRecord, _>(raw)?;
            Ok(records)
        })
        .await
    }

    /// Loads inclusion proofs for notes matching the given IDs.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_note_inclusion_proofs(
        &self,
        note_ids: BTreeSet<NoteId>,
    ) -> Result<BTreeMap<NoteId, NoteInclusionProof>> {
        self.framed("block note inclusion proofs", move |conn| {
            models::queries::select_note_inclusion_proofs(conn, &note_ids)
        })
        .await
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
        self.framed("apply block", move |conn| -> Result<()> {
            // TODO: This span is logged in a root span, we should connect it to the parent one.
            let _span = info_span!(target: COMPONENT, "write_block_to_db").entered();

            models::queries::apply_block(
                conn,
                block.header(),
                &notes,
                block.created_nullifiers(),
                block.updated_accounts(),
                block.transactions(),
            )?;

            // XXX FIXME TODO free floating mutex MUST NOT exist, it doesn't bind it properly to the
            // data locked!
            let _ = allow_acquire.send(());
            acquire_done.blocking_recv()?;

            Ok(())
        })
        .await
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
        self.framed("select account state data", move |conn| {
            models::queries::select_account_delta(conn, account_id, from_block, to_block)
        })
        .await
    }

    /// Runs database optimization.
    #[instrument(level = "debug", target = COMPONENT, skip_all, err)]
    pub async fn optimize(&self) -> Result<(), DatabaseError> {
        self.framed("db optimization", |conn| {
            diesel::sql_query("PRAGMA optimize")
                .execute(conn)
                .map_err(DatabaseError::Diesel)
        })
        .await?;
        Ok(())
    }

    /// Loads the network notes that have not been consumed yet, using pagination to limit the
    /// number of notes returned.
    pub(crate) async fn select_unconsumed_network_notes(
        &self,
        page: Page,
    ) -> Result<(Vec<NoteRecord>, Page)> {
        self.framed("unconsumed network notes", move |conn| {
            models::queries::unconsumed_network_notes(conn, page)
        })
        .await
    }
}
