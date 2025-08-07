use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Context;
use diesel::{Connection, RunQueryDsl, SqliteConnection};
use miden_lib::utils::Serializable;
use miden_node_proto::domain::account::{AccountInfo, AccountSummary};
use miden_node_proto::generated as proto;
use miden_objects::Word;
use miden_objects::account::{AccountDelta, AccountId};
use miden_objects::block::{BlockHeader, BlockNoteIndex, BlockNumber, ProvenBlock};
use miden_objects::crypto::merkle::SparseMerklePath;
use miden_objects::note::{NoteDetails, NoteId, NoteInclusionProof, NoteMetadata, Nullifier};
use miden_objects::transaction::TransactionId;
use tokio::sync::oneshot;
use tracing::{info, info_span, instrument};

use crate::COMPONENT;
use crate::db::manager::{ConnectionManager, configure_connection_on_creation};
use crate::db::migrations::apply_migrations;
use crate::db::models::{Page, queries};
use crate::errors::{DatabaseError, DatabaseSetupError, NoteSyncError, StateSyncError};
use crate::genesis::GenesisBlock;

pub(crate) mod manager;

mod migrations;

#[cfg(test)]
mod tests;

pub(crate) mod models;

/// [diesel](https://diesel.rs) generated schema
pub(crate) mod schema;

pub type Result<T, E = DatabaseError> = std::result::Result<T, E>;

pub struct Db {
    pool: deadpool_diesel::Pool<ConnectionManager, deadpool::managed::Object<ConnectionManager>>,
}

#[derive(Debug, PartialEq)]
pub struct NullifierInfo {
    pub nullifier: Nullifier,
    pub block_num: BlockNumber,
}

impl PartialEq<(Nullifier, BlockNumber)> for NullifierInfo {
    fn eq(&self, (nullifier, block_num): &(Nullifier, BlockNumber)) -> bool {
        &self.nullifier == nullifier && &self.block_num == block_num
    }
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
    pub note_id: Word,
    pub metadata: NoteMetadata,
    pub details: Option<NoteDetails>,
    pub inclusion_path: SparseMerklePath,
}

impl From<NoteRecord> for proto::note::CommittedNote {
    fn from(note: NoteRecord) -> Self {
        let inclusion_proof = Some(proto::note::NoteInclusionInBlockProof {
            note_id: Some(note.note_id.into()),
            block_num: note.block_num.as_u32(),
            note_index_in_block: note.note_index.leaf_index_value().into(),
            inclusion_path: Some(Into::into(note.inclusion_path)),
        });
        let note = Some(proto::note::Note {
            metadata: Some(note.metadata.into()),
            details: note.details.map(|details| details.to_bytes()),
        });
        Self { inclusion_proof, note }
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
    pub note_id: Word,
    pub metadata: NoteMetadata,
    pub inclusion_path: SparseMerklePath,
}

impl From<NoteSyncRecord> for proto::note::NoteSyncRecord {
    fn from(note: NoteSyncRecord) -> Self {
        Self {
            note_index_in_block: note.note_index.leaf_index_value().into(),
            note_id: Some(note.note_id.into()),
            metadata: Some(note.metadata.into()),
            inclusion_path: Some(Into::into(note.inclusion_path)),
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
            inclusion_path: note.inclusion_path,
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
        let mut conn: SqliteConnection = diesel::sqlite::SqliteConnection::establish(
            database_filepath.to_str().context("database filepath is invalid")?,
        )
        .context("failed to open a database connection")?;

        configure_connection_on_creation(&mut conn)?;

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

    /// Create and commit a transaction with the queries added in the provided closure
    pub(crate) async fn transact<R, E, Q, M>(&self, msg: M, query: Q) -> std::result::Result<R, E>
    where
        Q: Send
            + for<'a, 't> FnOnce(&'a mut SqliteConnection) -> std::result::Result<R, E>
            + 'static,
        R: Send + 'static,
        M: Send + ToString,
        E: From<diesel::result::Error>,
        E: From<DatabaseError>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| DatabaseError::ConnectionPoolObtainError(Box::new(e)))?;

        conn.interact(|conn| <_ as diesel::Connection>::transaction::<R, E, Q>(conn, query))
            .await
            .map_err(|err| E::from(DatabaseError::interact(&msg.to_string(), &err)))?
    }

    /// Run the query _without_ a transaction
    pub(crate) async fn query<R, E, Q, M>(&self, msg: M, query: Q) -> std::result::Result<R, E>
    where
        Q: Send + FnOnce(&mut SqliteConnection) -> std::result::Result<R, E> + 'static,
        R: Send + 'static,
        M: Send + ToString,
        E: From<DatabaseError>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| DatabaseError::ConnectionPoolObtainError(Box::new(e)))?;

        conn.interact(move |conn| {
            let r = query(conn)?;
            Ok(r)
        })
        .await
        .map_err(|err| E::from(DatabaseError::interact(&msg.to_string(), &err)))?
    }

    /// Open a connection to the DB and apply any pending migrations.
    #[instrument(target = COMPONENT, skip_all)]
    pub async fn load(database_filepath: PathBuf) -> Result<Self, DatabaseSetupError> {
        let manager = ConnectionManager::new(database_filepath.to_str().unwrap());
        let pool = deadpool_diesel::Pool::builder(manager).max_size(16).build()?;

        info!(
            target: COMPONENT,
            sqlite= %database_filepath.display(),
            "Connected to the database"
        );

        let me = Db { pool };
        me.query("migrations", apply_migrations).await?;
        Ok(me)
    }

    /// Loads all the nullifiers from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_all_nullifiers(&self) -> Result<Vec<NullifierInfo>> {
        self.transact("all nullifiers", move |conn| {
            let nullifiers = queries::select_all_nullifiers(conn)?;
            Ok(nullifiers)
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

        self.transact("nullifieres by prefix", move |conn| {
            let nullifier_prefixes =
                Vec::from_iter(nullifier_prefixes.into_iter().map(|prefix| prefix as u16));
            queries::select_nullifiers_by_prefix(
                conn,
                prefix_len as u8,
                &nullifier_prefixes[..],
                block_num,
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
        maybe_block_number: Option<BlockNumber>,
    ) -> Result<Option<BlockHeader>> {
        self.transact("block headers by block number", move |conn| {
            let val = queries::select_block_header_by_block_num(conn, maybe_block_number)?;
            Ok(val)
        })
        .await
    }

    /// Loads multiple block headers from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_block_headers(
        &self,
        blocks: impl Iterator<Item = BlockNumber> + Send + 'static,
    ) -> Result<Vec<BlockHeader>> {
        self.transact("block headers from given block numbers", move |conn| {
            let raw = queries::select_block_headers(conn, blocks)?;
            Ok(raw)
        })
        .await
    }

    /// Loads all the block headers from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_all_block_headers(&self) -> Result<Vec<BlockHeader>> {
        self.transact("all block headers", |conn| {
            let raw = queries::select_all_block_headers(conn)?;
            Ok(raw)
        })
        .await
    }

    /// Loads all the account commitments from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_all_account_commitments(&self) -> Result<Vec<(AccountId, Word)>> {
        self.transact("read all account commitments", move |conn| {
            queries::select_all_account_commitments(conn)
        })
        .await
    }

    /// Loads public account details from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_account(&self, id: AccountId) -> Result<AccountInfo> {
        self.transact("Get account details", move |conn| queries::select_account(conn, id))
            .await
    }

    /// Loads public account details from the DB based on the account ID's prefix.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_network_account_by_prefix(
        &self,
        id_prefix: u32,
    ) -> Result<Option<AccountInfo>> {
        self.transact("Get account by id prefix", move |conn| {
            queries::select_account_by_id_prefix(conn, id_prefix)
        })
        .await
    }

    /// Loads public accounts details from the DB.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_accounts_by_ids(
        &self,
        account_ids: Vec<AccountId>,
    ) -> Result<Vec<AccountInfo>> {
        self.transact("Select account by id set", |conn| {
            queries::select_accounts_by_id(conn, account_ids)
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
        self.transact::<StateSyncUpdate, StateSyncError, _, _>("state sync", move |conn| {
            queries::get_state_sync(conn, block_number, account_ids, note_tags)
        })
        .await
    }

    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn get_note_sync(
        &self,
        block_num: BlockNumber,
        note_tags: Vec<u32>,
    ) -> Result<NoteSyncUpdate, NoteSyncError> {
        self.transact("notes sync task", move |conn| {
            queries::get_note_sync(conn, block_num, note_tags.as_slice())
        })
        .await
    }

    /// Loads all the Note's matching a certain NoteId from the database.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_notes_by_id(&self, note_ids: Vec<NoteId>) -> Result<Vec<NoteRecord>> {
        self.transact("note by id", move |conn| {
            queries::select_notes_by_id(conn, note_ids.as_slice())
        })
        .await
    }

    /// Loads inclusion proofs for notes matching the given IDs.
    #[instrument(level = "debug", target = COMPONENT, skip_all, ret(level = "debug"), err)]
    pub async fn select_note_inclusion_proofs(
        &self,
        note_ids: BTreeSet<NoteId>,
    ) -> Result<BTreeMap<NoteId, NoteInclusionProof>> {
        self.transact("block note inclusion proofs", move |conn| {
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
        self.transact("apply block", move |conn| -> Result<()> {
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

            // XXX FIXME TODO free floating mutex MUST NOT exist
            // it doesn't bind it properly to the data locked!
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
        self.transact("select account state data", move |conn| {
            models::queries::select_account_delta(conn, account_id, from_block, to_block)
        })
        .await
    }

    /// Runs database optimization.
    #[instrument(level = "debug", target = COMPONENT, skip_all, err)]
    pub async fn optimize(&self) -> Result<(), DatabaseError> {
        self.transact("db optimization", |conn| {
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
        self.transact("unconsumed network notes", move |conn| {
            models::queries::unconsumed_network_notes(conn, page)
        })
        .await
    }
}
