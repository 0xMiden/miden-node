use std::{
    ops::Not,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use miden_node_proto::generated::store::api_server;
use miden_node_utils::{errors::ApiError, tracing::grpc::store_trace_fn};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tower_http::trace::TraceLayer;
use tracing::{info, instrument};

use crate::{
    COMPONENT, DATABASE_MAINTENANCE_INTERVAL, GenesisState, blocks::BlockStore, db::Db,
    server::db_maintenance::DbMaintenance, state::State,
};

mod api;
mod db_maintenance;

/// Represents an initialized store component where the RPC connection is open, but not yet actively
/// responding to requests.
///
/// Separating the connection binding from the server spawning allows the caller to connect other
/// components to the store without resorting to sleeps or other mechanisms to spawn dependent
/// components.
pub struct Store {
    api_service: api_server::ApiServer<api::StoreApi>,
    db_maintenance_service: DbMaintenance,
    listener: TcpListener,
}

impl Store {
    /// Bootstraps the Store, creating the database state and inserting the genesis block data.
    #[instrument(
        target = COMPONENT,
        name = "store.bootstrap",
        skip_all,
        err,
        fields(data_directory = %data_directory.display())
    )]
    pub fn bootstrap(genesis: GenesisState, data_directory: &Path) -> anyhow::Result<()> {
        let genesis = genesis
            .into_block()
            .context("failed to convert genesis configuration into the genesis block")?;

        let data_directory =
            DataDirectory::load(data_directory.to_path_buf()).with_context(|| {
                format!("failed to load data directory at {}", data_directory.display())
            })?;
        tracing::info!(target=COMPONENT, path=%data_directory.display(), "Data directory loaded");

        let block_store = data_directory.block_store_dir();
        let block_store =
            BlockStore::bootstrap(block_store.clone(), &genesis).with_context(|| {
                format!("failed to bootstrap block store at {}", block_store.display())
            })?;
        tracing::info!(target=COMPONENT, path=%block_store.display(), "Block store created");

        // Create the genesis block and insert it into the database.
        let database_filepath = data_directory.database_path();
        Db::bootstrap(database_filepath.clone(), &genesis).with_context(|| {
            format!("failed to bootstrap database at {}", database_filepath.display())
        })?;
        tracing::info!(target=COMPONENT, path=%database_filepath.display(), "Database created");

        Ok(())
    }

    /// Performs initialization tasks required before [`serve`](Self::serve) can be called.
    pub async fn init(listener: TcpListener, data_directory: PathBuf) -> Result<Self, ApiError> {
        info!(target: COMPONENT, endpoint=?listener, ?data_directory, "Loading database");

        let data_directory = DataDirectory::load(data_directory)?;

        let block_store = Arc::new(BlockStore::load(data_directory.block_store_dir())?);

        let db = Db::load(data_directory.database_path())
            .await
            .map_err(|err| ApiError::ApiInitialisationFailed(err.to_string()))?;

        let state = Arc::new(
            State::load(db, block_store)
                .await
                .map_err(|err| ApiError::DatabaseConnectionFailed(err.to_string()))?,
        );

        let db_maintenance_service =
            DbMaintenance::new(Arc::clone(&state), DATABASE_MAINTENANCE_INTERVAL);
        let api_service = api_server::ApiServer::new(api::StoreApi { state });

        info!(target: COMPONENT, "Database loaded");

        Ok(Self {
            api_service,
            db_maintenance_service,
            listener,
        })
    }

    /// Serves the store's RPC API and DB maintenance background task.
    ///
    /// Note: this blocks until the server dies.
    pub async fn serve(self) -> Result<(), ApiError> {
        tokio::spawn(self.db_maintenance_service.run());
        // Build the gRPC server with the API service and trace layer.
        tonic::transport::Server::builder()
            .layer(TraceLayer::new_for_grpc().make_span_with(store_trace_fn))
            .add_service(self.api_service)
            .serve_with_incoming(TcpListenerStream::new(self.listener))
            .await
            .map_err(ApiError::ApiServeFailed)
    }
}

/// Represents the store's data-directory and its content paths.
///
/// Used to keep our filepath assumptions in one location.
pub struct DataDirectory(PathBuf);

impl DataDirectory {
    /// Creates a new [`DataDirectory`], ensuring that the directory exists and is accessible
    /// insofar as is possible.
    pub fn load(path: PathBuf) -> std::io::Result<Self> {
        let meta = std::fs::metadata(&path)?;
        if meta.is_dir().not() {
            return Err(std::io::ErrorKind::NotConnected.into());
        }

        Ok(Self(path))
    }

    pub fn block_store_dir(&self) -> PathBuf {
        self.0.join("blocks")
    }

    pub fn database_path(&self) -> PathBuf {
        self.0.join("miden-store.sqlite3")
    }

    pub fn display(&self) -> std::path::Display<'_> {
        self.0.display()
    }
}
