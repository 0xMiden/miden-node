fn main() {
    // trigger a rebuild if any migrations change
    println!("cargo:rerun-if-changed=./src/db/migrations");
    // if we do one re-write, the defaults are gone
    // <https://doc.rust-lang.org/cargo/reference/build-scripts.html#rerun-if-changed>
    println!("cargo:rerun-if-changed=Cargo.toml");
}
