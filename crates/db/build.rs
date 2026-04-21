// Tell Cargo to re-run this build script (and therefore force the
// `sqlx::migrate!("./migrations")` macro in `src/lib.rs` to be
// re-expanded) whenever a file under `migrations/` is added, removed,
// or modified. Without this, incremental builds can keep a cached
// expansion that omits newly added SQL files, which at runtime
// surfaces as `migration X was previously applied but is missing in
// the resolved migrations`.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
