//! Demonstrates the database shutdown race condition.
//!
//! `shutdown_background()` returns immediately without waiting for SQLite connections
//! to close, causing "database is locked" errors when reopening.
//!
//! Run: `cargo test -p miden-node-store race_condition -- --nocapture`

use std::path::{Path, PathBuf};
use std::time::Duration;

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use tokio::runtime;

use crate::db::manager::{ConnectionManager, configure_connection_on_creation};

type TestPool =
    deadpool_diesel::Pool<ConnectionManager, deadpool::managed::Object<ConnectionManager>>;

fn temp_db_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("miden_race_{}_{}.db", name, std::process::id()))
}

fn cleanup(path: &Path) {
    let _ = fs_err::remove_file(path);
    let _ = fs_err::remove_file(path.with_extension("db-wal"));
    let _ = fs_err::remove_file(path.with_extension("db-shm"));
}

fn create_db(path: &Path) {
    let mut conn = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
    configure_connection_on_creation(&mut conn).unwrap();
    diesel::sql_query("CREATE TABLE t (id INTEGER)").execute(&mut conn).unwrap();
}

fn try_open(path: &Path) -> bool {
    let Ok(mut conn) = SqliteConnection::establish(path.to_str().unwrap()) else {
        return false;
    };
    configure_connection_on_creation(&mut conn).is_ok()
}

async fn hold_connections(pool: TestPool, ready: tokio::sync::oneshot::Sender<()>) {
    let _c1 = pool.get().await.unwrap();
    let _c2 = pool.get().await.unwrap();
    let _ = ready.send(());
    loop {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Creates a runtime with a pool holding connections, returns runtime ready for shutdown.
fn setup_runtime_with_pool(path: &Path) -> runtime::Runtime {
    let rt = runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let p = path.to_str().unwrap().to_string();
    let pool: TestPool = rt.block_on(async {
        deadpool_diesel::Pool::builder(ConnectionManager::new(&p))
            .max_size(4)
            .build()
            .unwrap()
    });
    rt.spawn(hold_connections(pool, tx));
    rt.block_on(async { rx.await.unwrap() });
    rt
}

/// Shows that `shutdown_background()` can leave the database locked.
#[test]
fn shutdown_background_can_cause_lock() {
    let mut failures = 0;
    for i in 0..20 {
        let path = temp_db_path(&format!("bg{i}"));
        cleanup(&path);
        create_db(&path);

        let rt = setup_runtime_with_pool(&path);
        rt.shutdown_background(); // doesn't wait

        if !try_open(&path) {
            failures += 1;
        }
        cleanup(&path);
    }
    eprintln!("shutdown_background: {failures}/20 failures (race triggered)");
}

/// Shows that `shutdown_timeout()` reliably releases the database.
#[test]
fn shutdown_timeout_releases_lock() {
    for i in 0..20 {
        let path = temp_db_path(&format!("to{i}"));
        cleanup(&path);
        create_db(&path);

        let rt = setup_runtime_with_pool(&path);
        rt.shutdown_timeout(Duration::from_secs(5)); // waits

        assert!(try_open(&path), "database should be accessible after shutdown_timeout");
        cleanup(&path);
    }
    eprintln!("shutdown_timeout: 0/20 failures");
}
