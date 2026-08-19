//! Help section support for the node CLI.
//!
//! Clap supports grouping options under a shared `help_heading`, but it does not support adding
//! explanatory text under those generated headings. We keep the heading constants here so the clap
//! attributes and help post-processing stay in sync, then inject descriptions into clap's rendered
//! help output for the sections that need extra context.

pub(crate) const RPC_CONFIGURATION_HELP_HEADING: &str = "RPC configuration";
pub(crate) const BLOCK_PRODUCTION_HELP_HEADING: &str = "Block production";
pub(crate) const STORE_CONFIGURATION_HELP_HEADING: &str = "Store configuration";

const STORE_CONFIGURATION_HELP_DESCRIPTION: &str = concat!(
    "      Defaults are reasonable for most use cases. Only change these settings if you understand\n",
    "      the storage and performance tradeoffs.\n\n",
);

const HELP_SECTION_DESCRIPTIONS: &[(&str, &str)] =
    &[(STORE_CONFIGURATION_HELP_HEADING, STORE_CONFIGURATION_HELP_DESCRIPTION)];

/// Inserts explanatory text below clap-generated help section headings.
pub(crate) fn inject_section_descriptions(mut help: String) -> String {
    for (heading, description) in HELP_SECTION_DESCRIPTIONS {
        let marker = format!("{heading}:\n");
        let replacement = format!("{marker}{description}");
        help = help.replacen(&marker, &replacement, 1);
    }

    help
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::STORE_CONFIGURATION_HELP_HEADING;
    use crate::Cli;

    fn subcommand_help_headings(command: &str) -> Vec<String> {
        let mut cli = Cli::command();
        let subcommand = cli
            .find_subcommand_mut(command)
            .unwrap_or_else(|| panic!("missing {command} subcommand"));

        subcommand
            .get_arguments()
            .filter_map(|argument| argument.get_help_heading())
            .map(ToOwned::to_owned)
            .collect()
    }

    fn assert_injectable_section_headings(command: &str) {
        let headings = subcommand_help_headings(command);

        assert!(
            headings.iter().any(|candidate| candidate == STORE_CONFIGURATION_HELP_HEADING),
            "{command} is missing the {STORE_CONFIGURATION_HELP_HEADING} heading targeted by help injection"
        );
    }

    #[test]
    fn sequencer_mode_contains_injectable_section_headings() {
        assert_injectable_section_headings("sequencer");
    }

    #[test]
    fn full_node_mode_contains_injectable_section_headings() {
        assert_injectable_section_headings("full");
    }
}
