use miden_remote_prover::COMPONENT;
use pingora::{prelude::sleep, server::ShutdownWatch, services::background::BackgroundService};
use tonic::async_trait;
use tracing::{debug_span, error};

use super::LoadBalancerState;

/// Implement the BackgroundService trait for the LoadBalancer
///
/// A [BackgroundService] can be run as part of a Pingora application to add supporting logic that
/// exists outside of the request/response lifecycle.
///
/// We use this implementation to periodically check the health of the workers and update the list
/// of available workers.
#[async_trait]
impl BackgroundService for LoadBalancerState {
    /// Starts the health check background service.
    ///
    /// This function is called when the Pingora server tries to start all the services. The
    /// background service can return at anytime or wait for the `shutdown` signal.
    ///
    /// The health check background service will periodically check the health of the workers
    /// using the gRPC status endpoint. If a worker is not healthy, it will be removed from
    /// the list of available workers.
    ///
    /// # Errors
    /// - If the worker has an invalid URI.
    async fn start(&self, shutdown: ShutdownWatch) {
        Box::pin(async move {
            loop {
                // Check if the shutdown signal has been received
                {
                    if *shutdown.borrow() {
                        break;
                    }
                }

                // Create a new spawn to perform the health check
                let span = debug_span!(target: COMPONENT, "proxy.health_check");
                let _guard = span.enter();
                {
                    let mut workers = self.workers.write().await;

                    for worker in workers.iter_mut() {
                        let status_result = worker.check_status(self.supported_proof_type).await;

                        if let Err(ref reason) = status_result {
                            error!(
                                err = %reason,
                                worker.address = worker.address(),
                                "Worker failed health check"
                            );
                        }

                        worker.update_status(status_result);
                    }
                }

                // Update the status cache with current worker status
                self.update_status_cache().await;

                // Sleep for the defined interval before the next health check
                sleep(self.health_check_interval).await;
            }
        })
        .await;
    }
}
