use std::{thread, time::Duration};

use miden_node_store::DataDirectory;
use sysinfo::{Disks, Pid, System};
use tokio::task;
use tracing::{error, info};

const COMPONENT: &str = "system-monitor";

/// Runs a system monitor loop that logs the system metrics every `MONITOR_INTERVAL` seconds.
/// It logs the following information:
/// - Memory usage
/// - CPU usage
/// - Disk usage
/// - Process memory, CPU and disk usage
/// - Database size
/// - Database WAL size
/// - Block storage size
#[derive(Clone)]
pub struct SystemMonitor {
    pid: Pid,
    monitor_interval: Duration,
    data_directory: Option<DataDirectory>,
}

impl SystemMonitor {
    /// Creates a new system monitor.
    pub fn new(monitor_interval: Duration) -> Self {
        Self {
            pid: Pid::from(std::process::id() as usize),
            data_directory: None,
            monitor_interval,
        }
    }

    /// Creates a new system monitor with a data directory to collect store metrics.
    pub fn with_store_metrics(self, data_directory: DataDirectory) -> Self {
        Self {
            data_directory: Some(data_directory),
            ..self
        }
    }

    /// Runs a supervisor task that re-runs the system monitor if it panics.
    pub fn run_with_supervisor(self) -> task::JoinHandle<()> {
        task::spawn(async move {
            loop {
                if let Err(err) = self.clone().run().await {
                    error!(target: COMPONENT, ?err, "Panicked while collecting system metrics. Restarting.");
                }
                // Wait an interval before restarting the monitor.
                tokio::time::sleep(self.monitor_interval).await;
            }
        })
    }

    /// Runs the system monitor loop on a separate blocking task.
    pub fn run(self) -> task::JoinHandle<()> {
        task::spawn_blocking(move || {
            let mut sys = System::new_all();
            loop {
                thread::sleep(self.monitor_interval);
                sys.refresh_all();
                if let Err(err) = self.log_system_metrics(&sys) {
                    error!(target: COMPONENT, ?err, "Error collecting system metrics");
                }
            }
        })
    }

    /// Collects the system metrics and posts the structured log.
    fn log_system_metrics(&self, sys: &System) -> anyhow::Result<()> {
        let disks = Disks::new_with_refreshed_list();
        let current_dir = std::env::current_dir()?;
        // NOTE: shows data for the disk that backs the current directory.
        let disk = disks
            .iter()
            .filter(|d| current_dir.starts_with(d.mount_point()))
            .max_by_key(|d| d.mount_point().as_os_str().len())
            .ok_or(anyhow::anyhow!("No disk found"))?;
        let system_disk_limit = disk.total_space();
        let system_disk_state_available = disk.available_space();
        let system_disk_used = system_disk_limit - system_disk_state_available;
        #[allow(clippy::cast_precision_loss)]
        let system_disk_utilization = (system_disk_used as f64 / system_disk_limit as f64) * 100.0;

        let system_cpu_utilization = sys.global_cpu_usage() / 100.0;
        let system_cpu_physical_count = System::physical_core_count();

        let system_memory_usage = sys.used_memory();
        let system_memory_limit = sys.total_memory();
        let system_memory_available = sys.available_memory();
        #[allow(clippy::cast_precision_loss)]
        let system_memory_utilization = system_memory_usage as f64 / system_memory_limit as f64;

        // SAFETY: the process exists since it is the current process.
        let process = sys.process(self.pid).unwrap();
        let process_memory_usage = process.memory();
        let process_cpu_utilization = process.cpu_usage() / 100.0;
        let process_disk_written = process.disk_usage().written_bytes;
        let process_disk_read = process.disk_usage().read_bytes;

        let store_metrics = self.collect_store_metrics()?;

        info!(
            target: COMPONENT,
            // System memory
            system_memory_limit,
            system_memory_available,
            system_memory_usage,
            system_memory_utilization,
            // System CPU
            system_cpu_physical_count,
            %system_cpu_utilization,
            // Disks
            system_disk_limit,
            system_disk_state_available,
            %system_disk_utilization,
            // Process
            process_memory_usage,
            %process_cpu_utilization,
            process_disk_written,
            process_disk_read,
            // Store
            db_file_size=store_metrics.db_file,
            db_wal_size=store_metrics.db_wal,
            block_storage_size=store_metrics.block_storage,
        );

        Ok(())
    }

    /// Collects the store metrics.
    fn collect_store_metrics(&self) -> anyhow::Result<StoreMetrics> {
        let Some(data_dir) = &self.data_directory else {
            return Ok(StoreMetrics::default());
        };

        let db_file = std::fs::metadata(data_dir.database_path())?.len();
        let db_wal =
            std::fs::metadata(format!("{}-wal", data_dir.database_path().display()))?.len();
        let block_storage = std::fs::metadata(data_dir.block_store_dir())?.len();

        Ok(StoreMetrics { db_file, db_wal, block_storage })
    }
}

/// Metrics of the store.
#[derive(Default)]
struct StoreMetrics {
    pub db_file: u64,
    pub db_wal: u64,
    pub block_storage: u64,
}
