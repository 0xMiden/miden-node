#[cfg(feature = "vergen")]
pub use vergen::vergen;

/// Contains build metadata which can be formatted into a pretty --version output using its Display
/// implementation.
///
/// The build metadata can be embedded at compile time using the `vergen` function available from
/// the `vergen` feature. See that functions description for a list of the environment variables
/// emitted which map nicely to [`LongVersion`].
///
/// Unfortunately these values must be transferred manually by the end user since the env variables
/// are only available once the caller's build script has run - which is after this crate is
/// compiled.
pub struct LongVersion {
    pub version: &'static str,
    pub sha: &'static str,
    pub branch: &'static str,
    pub dirty: &'static str,
    pub features: &'static str,
    pub rust_version: &'static str,
    pub host: &'static str,
    pub target: &'static str,
    pub opt_level: &'static str,
    pub debug: &'static str,
}

impl std::fmt::Display for LongVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let &Self {
            version,
            mut sha,
            mut branch,
            dirty,
            features,
            rust_version,
            host,
            target,
            opt_level,
            debug,
        } = self;

        let dirty = match dirty {
            "true" => "-dirty",
            _ => "",
        };

        // This is the default value set by `vergen` when these values are missing.
        // The git values can be missing for a published crate, and while we do attempt
        // to set default values in the build.rs, its still possible for these to be skipped
        // e.g. when cargo publish --allow-dirty is used.
        if branch == "VERGEN_IDEMPOTENT_OUTPUT" {
            branch = "";
        }
        if sha == "VERGEN_IDEMPOTENT_OUTPUT" {
            sha = "";
        }

        f.write_fmt(format_args!(
            "{version}

SHA:          {sha}{dirty}
branch:       {branch}
features:     {features}
rust version: {rust_version}
target arch:  {target}
host arch:    {host}
opt-level:    {opt_level}
debug:        {debug}
"
        ))
    }
}

#[cfg(feature = "vergen")]
mod vergen {
    use std::path::PathBuf;

    use anyhow::{Context, Result};

    /// Emits environment variables for build metadata intended for extended version information.
    ///
    /// The following environment variables are emitted:
    ///
    ///   - `VERGEN_GIT_BRANCH`
    ///   - `VERGEN_GIT_SHA`
    ///   - `VERGEN_GIT_DIRTY`
    ///   - `VERGEN_RUSTC_SEMVER`
    ///   - `VERGEN_RUSTC_HOST_TRIPLE`
    ///   - `VERGEN_CARGO_TARGET_TRIPLE`
    ///   - `VERGEN_CARGO_FEATURES`
    ///   - `VERGEN_CARGO_OPT_LEVEL`
    ///   - `VERGEN_CARGO_DEBUG`
    pub fn vergen() -> Result<()> {
        if let Some(sha) = published_git_sha().context("Checking for published vcs info")? {
            // git data is not available if in a published state, so we set them manually.
            println!("cargo::rustc-env=VERGEN_GIT_SHA={sha}");
            println!("cargo::rustc-env=VERGEN_GIT_BRANCH=NA (published)");
            println!("cargo::rustc-env=VERGEN_GIT_DIRTY=");

            vergen_gitcl::Emitter::new()
        } else {
            // In a non-published state so we can expect git instructions to work.
            let mut emitter = vergen_gitcl::Emitter::new();
            emitter
                .add_instructions(&git_instructions()?)
                .context("Adding git instructions")?;

            emitter
        }
        .add_instructions(&cargo_instructions()?)
        .context("Adding cargo instructions")?
        .add_instructions(&rustc_instructions()?)
        .context("Adding rustc instructions")?
        .emit()
    }

    /// Normal git info is lost on `cargo publish`, which instead adds a file containing the SHA1
    /// hash.
    ///
    /// This function returns the short SHA value. If present, this indicates this we're in a
    /// published state.
    fn published_git_sha() -> Result<Option<String>> {
        let cargo_vcs_info = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".cargo_vcs_info.json");
        if cargo_vcs_info.exists() {
            // The file is small so reading to string is acceptable.
            let contents = fs_err::read_to_string(cargo_vcs_info).context("Reading vcs info")?;

            // File format:
            // {
            //   "git": {
            //     "sha1": "9d48046e9654d93a86212e77d6c92f14c95de44b"
            //   },
            //   "path_in_vcs": "bin/node"
            // }
            let offset = contents.find(r#""sha1""#).context("Searching for sha1 property")?
                + r#""sha1""#.len();

            let sha1 = contents[offset + 1..]
            .chars()
            // Find and skip opening quote.
            .skip_while(|&c| c != '"')
            .skip(1)
            // Take until closing quote.
            .take_while(|&c| c != '"')
            // Short SHA format is 7 digits.
            .take(7)
            .collect();

            Ok(Some(sha1))
        } else {
            Ok(None)
        }
    }

    fn git_instructions() -> Result<vergen_gitcl::Gitcl> {
        const INCLUDE_UNTRACKED: bool = true;
        const SHORT_SHA: bool = true;

        vergen_gitcl::GitclBuilder::default()
            .branch(true)
            .dirty(INCLUDE_UNTRACKED)
            .sha(SHORT_SHA)
            .build()
            .context("Building git instructions")
    }

    fn cargo_instructions() -> Result<vergen::Cargo> {
        vergen_gitcl::CargoBuilder::default()
            .debug(true)
            .features(true)
            .target_triple(true)
            .opt_level(true)
            .build()
            .context("Building git instructions")
    }

    fn rustc_instructions() -> Result<vergen::Rustc> {
        vergen_gitcl::RustcBuilder::default()
            .semver(true)
            .host_triple(true)
            .build()
            .context("Building rustc instructions")
    }
}
