// sqlx::migrate!() embeds the .sql files at compile time; without this,
// adding a migration doesn't rebuild the crate and the old set runs.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
    println!("cargo:rerun-if-changed=migrations_postgres");
}
