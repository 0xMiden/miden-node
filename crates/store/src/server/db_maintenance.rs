use std::sync::Arc;
use std::time::Duration;

use miden_node_utils::tracing::OpenTelemetrySpanExt;
use tracing::{Instrument, Span};

use crate::state::State;

pub struct DbMaintenance {
    state: Arc<State>,
    optimization_interval: Duration,
}

impl DbMaintenance {
    pub fn new(state: Arc<State>, optimization_interval: Duration) -> Self {
        Self { state, optimization_interval }
    }

    /// Runs infinite maintenance loop.
    pub async fn run(self) {
        loop {
            tokio::time::sleep(self.optimization_interval).await;

            let root_span = tracing::info_span!(
                "optimize_database",
                interval = self.optimization_interval.as_secs_f32()
            );
            self.state
                .optimize_db()
                .instrument(root_span)
                .await
                .unwrap_or_else(|err| Span::current().set_error(&err));
        }
    }
}
