//! Renders the note-transport card (URL, gRPC serving status and server stats).

use maud::{Markup, html};

use super::super::helpers::{copy_button, format_timestamp, metric_row};
use crate::status::NoteTransportStatusDetails;

pub(in crate::view) fn render_note_transport(
    details: &NoteTransportStatusDetails,
    healthy: bool,
) -> Markup {
    let metrics_class = if healthy {
        "test-metrics healthy"
    } else {
        "test-metrics unhealthy"
    };
    html! {
        div class="service-details" {
            div class="nested-status" {
                strong { "Note Transport:" }
                div class=(metrics_class) {
                    div class="metric-row" {
                        span class="metric-label" { "URL:" }
                        span class="metric-value" {
                            (details.url) (copy_button(&details.url, "URL"))
                        }
                    }
                    (metric_row("Serving Status:", &details.serving_status))
                    (metric_row("Version:", &stat_or_dash(details.version.clone(), healthy)))
                    (metric_row(
                        "Total Notes:",
                        &stat_or_dash(details.total_notes.map(|v| v.to_string()), healthy),
                    ))
                    (metric_row(
                        "Total Tags:",
                        &stat_or_dash(details.total_tags.map(|v| v.to_string()), healthy),
                    ))
                    (metric_row(
                        "Last Note Activity:",
                        &stat_or_dash(details.last_activity.map(format_timestamp), healthy),
                    ))
                }
            }
        }
    }
}

/// Renders the stat when present and the service is healthy, `-` otherwise. Mirrors the convention
/// used across cards: stale numbers from an unhealthy probe are not shown.
fn stat_or_dash(value: Option<String>, healthy: bool) -> String {
    match value {
        Some(v) if healthy => v,
        _ => "-".to_string(),
    }
}
