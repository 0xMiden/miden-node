mod pr;
mod release;
mod render;
#[cfg(test)]
mod tests;

use anyhow::Result;
use semver::Version;
use serde::Deserialize;

pub fn verify_pr_body(source: &str) -> Result<()> {
    pr::verify_pr_body(source)
}

pub fn render_release_notes(release_tag: &str) -> Result<String> {
    let changelog = release::release_changelog_entries(release_tag)?;
    Ok(render::release_notes(
        &format!("Release {release_tag}"),
        changelog.protocol_update.as_ref(),
        &changelog.entries,
        &changelog.invalid_entries,
    ))
}

pub fn render_current_changelog() -> Result<String> {
    let changelog = release::current_changelog_entries()?;
    Ok(render::release_notes(
        &changelog.title,
        changelog.protocol_update.as_ref(),
        &changelog.entries,
        &changelog.invalid_entries,
    ))
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Scope {
    Rpc,
    Docs,
    Node,
    NoteTransport,
    NetworkMonitor,
    NtxBuilder,
    Prover,
    Validator,
    Internal,
    General,
}

#[derive(Debug, PartialEq, Eq)]
struct ProtocolUpdate {
    previous: Version,
    current: Version,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Impact {
    Breaking,
    Migration,
    Added,
    Changed,
    Fixed,
    Removed,
    Deprecated,
}

#[derive(Debug)]
struct ReleaseNoteEntry {
    pr_number: u64,
    scope: Scope,
    impact: Impact,
    description: String,
    order: usize,
}

#[derive(Debug)]
struct InvalidChangelogEntry {
    source: InvalidChangelogSource,
    reason: String,
    order: usize,
}

#[derive(Debug)]
enum InvalidChangelogSource {
    PullRequest(u64),
    Commit(String),
}
