use std::time::Duration;

use miden_node_store::DataDirectory;
use sysinfo::{Disk, Disks, Pid, System};
use tracing::{error, info};

const COMPONENT: &str = "system-monitor";
const MONITOR_INTERVAL: Duration = Duration::from_secs(10);

/// Runs a system monitor loop that logs the system metrics every `MONITOR_INTERVAL` seconds.
/// It is intended to be run in a separate thread.
/// It logs the following information:
/// - Memory usage
/// - CPU usage
/// - Disk usage
/// - Process memory, CPU and disk usage
/// - Database size
/// - Database WAL size
/// - Block storage size
pub struct SystemMonitor {
    sys: System,
    pid: Pid,
    monitor_interval: Duration,
    data_directory: Option<DataDirectory>,
}

impl SystemMonitor {
    /// Creates a new system monitor.
    pub fn new(data_directory: Option<DataDirectory>, monitor_interval: Duration) -> Self {
        Self {
            sys: System::new_all(),
            pid: Pid::from(std::process::id() as usize),
            data_directory,
            monitor_interval,
        }
    }

    /// Runs the system monitor loop.
    pub fn run(&mut self) {
        loop {
            std::thread::sleep(self.monitor_interval);
            if let Err(err) = self.collect_system_metrics() {
                error!(target: COMPONENT, ?err, "Error collecting system metrics");
            }
        }
    }

    /// Collects the system metrics.
    fn collect_system_metrics(&mut self) -> anyhow::Result<()> {
        self.sys.refresh_all();

        let disks = Disks::new_with_refreshed_list();
        let system_disk_limit: u64 = disks.iter().map(Disk::total_space).sum();
        let system_disk_state_available: u64 = disks.iter().map(Disk::available_space).sum();
        let system_disk_used = system_disk_limit - system_disk_state_available;
        #[allow(clippy::cast_precision_loss)]
        let system_disk_utilization = (system_disk_used as f64 / system_disk_limit as f64) * 100.0;

        let system_cpu_utilization = self.sys.global_cpu_usage() / 100.0;
        let system_cpu_physical_count = System::physical_core_count();

        let system_memory_usage = self.sys.used_memory();
        let system_memory_limit = self.sys.total_memory();
        let system_memory_available = self.sys.available_memory();
        #[allow(clippy::cast_precision_loss)]
        let system_memory_utilization = system_memory_usage as f64 / system_memory_limit as f64;

        // SAFETY: the process exists since it is the current process.
        let process = self.sys.process(self.pid).unwrap();
        let process_memory_usage = process.memory();
        let process_cpu_utilization = process.cpu_usage() / 100.0;
        let process_disk_written = process.disk_usage().written_bytes;
        let process_disk_read = process.disk_usage().read_bytes;

        let (db_file_size, db_wal_size, block_storage_size) = self.collect_store_metrics()?;

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
            // Node store data
            db_file_size,
            db_wal_size,
            block_storage_size,
        );

        Ok(())
    }

    /// Collects the store metrics.
    fn collect_store_metrics(&self) -> anyhow::Result<(u64, u64, u64)> {
        if let Some(data_dir) = &self.data_directory {
            let db_file_size = std::fs::metadata(data_dir.database_path())?.len();
            let db_wal_size =
                std::fs::metadata(format!("{}-wal", data_dir.database_path().display()))?.len();
            let block_storage_size = std::fs::metadata(data_dir.block_store_dir())?.len();

            Ok((db_file_size, db_wal_size, block_storage_size))
        } else {
            Ok((0, 0, 0))
        }
    }
}
