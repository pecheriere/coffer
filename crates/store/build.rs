//! `sqlx::migrate!` embeds the migration files at compile time but does not
//! tell cargo to watch the directory, so without this an incremental build
//! can embed a stale migration set.

fn main() {
    println!("cargo::rerun-if-changed=migrations");
}
