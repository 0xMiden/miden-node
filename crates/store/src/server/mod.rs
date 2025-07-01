use std::{
    ops::Not,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use miden_node_proto::generated::store;
use miden_node_proto_build::store_api_descriptor;
use miden_node_utils::tracing::grpc::store_trace_fn;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tower_http::trace::TraceLayer;
use tracing::{info, instrument};

use crate::{
    COMPONENT, DATABASE_MAINTENANCE_INTERVAL, GenesisState, blocks::BlockStore, db::Db,
    server::db_maintenance::DbMaintenance, state::State,
};

mod api;
mod block_producer;
mod db_maintenance;
mod ntx_builder;
mod rpc_api;

/// The store server.
pub struct Store {
    pub listener: TcpListener,
    pub data_directory: PathBuf,
}

impl Store {
    /// Bootstraps the Store, creating the database state and inserting the genesis block data.
    #[instrument(
        target = COMPONENT,
        name = "store.bootstrap",
        skip_all,
        err,
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

    /// Serves the store APIs (rpc, ntx-builder, block-producer) and DB maintenance background task.
    ///
    /// Note: this blocks until the server dies.
    pub async fn serve(self) -> anyhow::Result<()> {
        info!(target: COMPONENT, endpoint=?self.listener, ?self.data_directory, "Loading database");

        let data_directory =
            DataDirectory::load(self.data_directory.clone()).with_context(|| {
                format!("failed to load data directory at {}", self.data_directory.display())
            })?;

        let block_store =
            Arc::new(BlockStore::load(data_directory.block_store_dir()).with_context(|| {
                format!("failed to load block store at {}", self.data_directory.display())
            })?);

        let database_filepath = data_directory.database_path();
        let db = Db::load(database_filepath.clone()).await.with_context(|| {
            format!("failed to load database at {}", database_filepath.display())
        })?;

        let state = Arc::new(State::load(db, block_store).await.context("failed to load state")?);

        let db_maintenance_service =
            DbMaintenance::new(Arc::clone(&state), DATABASE_MAINTENANCE_INTERVAL);

        let rpc_service =
            store::rpc_server::RpcServer::new(api::StoreApi { state: Arc::clone(&state) });
        let ntx_builder_service = store::ntx_builder_server::NtxBuilderServer::new(api::StoreApi {
            state: Arc::clone(&state),
        });
        let block_producer_service =
            store::block_producer_server::BlockProducerServer::new(api::StoreApi {
                state: Arc::clone(&state),
            });
        let reflection_service = tonic_reflection::server::Builder::configure()
            .register_file_descriptor_set(store_api_descriptor())
            .build_v1()
            .context("failed to build reflection service")?;

        // This is currently required for postman to work properly because
        // it doesn't support the new version yet.
        //
        // See: <https://github.com/postmanlabs/postman-app-support/issues/13120>.
        let reflection_service_alpha = tonic_reflection::server::Builder::configure()
            .register_file_descriptor_set(store_api_descriptor())
            .build_v1alpha()
            .context("failed to build reflection service")?;

        info!(target: COMPONENT, "Database loaded");

        tokio::spawn(db_maintenance_service.run());
        // Build the gRPC server with the API services and trace layer.
        tonic::transport::Server::builder()
            .layer(TraceLayer::new_for_grpc().make_span_with(store_trace_fn))
            .add_service(rpc_service)
            .add_service(ntx_builder_service)
            .add_service(block_producer_service)
            .add_service(reflection_service)
            .add_service(reflection_service_alpha)
            .serve_with_incoming(TcpListenerStream::new(self.listener))
            .await
            .context("failed to serve store API")
    }
}

/// Represents the store's data-directory and its content paths.
///
/// Used to keep our filepath assumptions in one location.
#[derive(Clone)]
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
