fn main() {
    // trigger a rebuild if any migrations change
    println!("cargo:rerun-if-changed=./src/db/migrations");
    // if we do one re-write, the defaults are gone
    // <https://doc.rust-lang.org/cargo/reference/build-scripts.html#rerun-if-changed>
    // TODO in our case migrations are part of the source tree, investigate if this file is needed
    // TODO at all
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
}
