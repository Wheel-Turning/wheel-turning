//! Rebuild when a migration is added or changed, because `sqlx::migrate!` embeds them at
//! compile time and Cargo does not otherwise watch the directory.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
