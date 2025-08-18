use clap::Parser;
use pingora::apps::HttpServerOptions;
use pingora::prelude::{Opt, background_service};
use pingora::server::Server;
use pingora::server::configuration::ServerConf;
use pingora::services::listening::Service;
use pingora_proxy::http_proxy_service;
use tracing::{info, warn};

use super::ProxyConfig;
use crate::COMPONENT;
use crate::server::commands::PROXY_HOST;
use crate::server::error::RemoteProverError;
use crate::server::proxy::update_workers::LoadBalancerUpdateService;
use crate::server::proxy::{LoadBalancer, LoadBalancerState};
use crate::server::utils::check_port_availability;

/// Starts the proxy.
///
/// Example: `miden-remote-prover start-proxy --workers 0.0.0.0:8080,127.0.0.1:9090`
#[derive(Debug, Parser)]
pub struct StartProxy {
    /// List of workers as host:port strings.
    ///
    /// Example: `127.0.0.1:8080,192.168.1.1:9090`
    #[arg(long, env = "MRP_PROXY_WORKERS_LIST", value_delimiter = ',')]
    workers: Vec<String>,
    /// Proxy configurations.
    #[command(flatten)]
    proxy_config: ProxyConfig,
}

impl StartProxy {
    /// Starts the proxy using the configuration defined in the command.
    ///
    /// This method will start a proxy with each worker passed as command argument as a backend,
    /// using the configurations passed as options for the commands or the equivalent environmental
    /// variables.
    ///
    /// # Errors
    /// Returns an error in the following cases:
    /// - The backend cannot be created.
    /// - The Pingora configuration fails.
    /// - The server cannot be started.
    #[tracing::instrument(target = COMPONENT, name = "proxy.execute")]
    pub async fn execute(&self) -> anyhow::Result<()> {
        // Check if all required ports are available
        check_port_availability(self.proxy_config.port, "Proxy")?;
        check_port_availability(self.proxy_config.control_port, "Control")?;

        // First, check if the metrics port is specified (metrics enabled)
        if let Some(metrics_port) = self.proxy_config.metrics_config.metrics_port {
            check_port_availability(metrics_port, "Metrics")?;
        }

        let mut conf = ServerConf::new().ok_or(RemoteProverError::PingoraConfigFailed(
            "Failed to create server conf".to_string(),
        ))?;
        conf.grace_period_seconds = Some(self.proxy_config.grace_period.as_secs());
        conf.graceful_shutdown_timeout_seconds =
            Some(self.proxy_config.graceful_shutdown_timeout.as_secs());

        let mut server = Server::new_with_opt_and_conf(Some(Opt::default()), conf);

        server.bootstrap();

        if self.workers.is_empty() {
            warn!(target: COMPONENT, "Starting proxy without any workers");
        } else {
            info!(target: COMPONENT,
                worker_count = %self.workers.len(),
                workers = ?self.workers,
                "Proxy starting with workers"
            );
        }

        let worker_lb = LoadBalancerState::new(self.workers.clone(), &self.proxy_config).await?;

        let health_check_service = background_service("health_check", worker_lb);

        let worker_lb = health_check_service.task();

        let updater_service = LoadBalancerUpdateService::new(worker_lb.clone());

        let mut update_workers_service =
            Service::new("update_workers".to_string(), updater_service);
        update_workers_service
            .add_tcp(format!("{}:{}", PROXY_HOST, self.proxy_config.control_port).as_str());

        // Set up the load balancer
        let mut lb = http_proxy_service(&server.configuration, LoadBalancer(worker_lb.clone()));

        lb.add_tcp(format!("{}:{}", PROXY_HOST, self.proxy_config.port).as_str());
        info!(target: COMPONENT,
            endpoint = %format!("{}:{}", PROXY_HOST, self.proxy_config.port),
            "Proxy service listening"
        );
        let logic = lb
            .app_logic_mut()
            .ok_or(RemoteProverError::PingoraConfigFailed("app logic not found".to_string()))?;
        let mut http_server_options = HttpServerOptions::default();

        // Enable HTTP/2 for plaintext
        http_server_options.h2c = true;
        logic.server_options = Some(http_server_options);

        // Enable Prometheus metrics if metrics_port is specified
        if let Some(metrics_port) = self.proxy_config.metrics_config.metrics_port {
            let metrics_addr = format!("{PROXY_HOST}:{metrics_port}");
            info!(target: COMPONENT,
                endpoint = %metrics_addr,
                "Metrics service initialized"
            );
            let mut prometheus_service =
                pingora::services::listening::Service::prometheus_http_service();
            prometheus_service.add_tcp(&metrics_addr);
            server.add_service(prometheus_service);
        } else {
            info!(target: COMPONENT, "Metrics service disabled");
        }

        server.add_service(health_check_service);
        server.add_service(update_workers_service);
        server.add_service(lb);
        tokio::task::spawn_blocking(|| server.run_forever()).await?;

        Ok(())
    }
}
