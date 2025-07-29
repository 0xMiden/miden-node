fn main() {
    // trigger a rebuild if any migrations change
    // such that we include the latest version and not use a cached
    // version using `diesel_migrations::embed_migrations!(dir)`
    // It's an implementation detail:
    // <https://docs.rs/diesel_migrations/2.2.0/diesel_migrations/macro.embed_migrations.html>
    println!("cargo:rerun-if-changed=./src/db/migrations");
    // if we do one re-write, the defaults are gone
    // <https://doc.rust-lang.org/cargo/reference/build-scripts.html#rerun-if-changed>
    println!("cargo:rerun-if-changed=Cargo.toml");
}
