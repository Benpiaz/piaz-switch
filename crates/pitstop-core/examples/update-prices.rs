//! Refresh the price table Pitstop ships with, from LiteLLM's current one:
//! `cargo run -p pitstop-core --example update-prices`.

use std::path::Path;

fn main() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let table = pitstop_core::pricing::fetch(now).unwrap_or_else(|e| {
        eprintln!("could not fetch LiteLLM's prices: {e}");
        std::process::exit(1);
    });
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("prices.json");
    std::fs::write(&path, table.to_json()).expect("write src/prices.json");
    println!(
        "{} models written to {}",
        table.models.len(),
        path.display()
    );
}
