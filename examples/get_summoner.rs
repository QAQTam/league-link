//! Print the current summoner. Run the League Client first.
//!
//! ```text
//! cargo run --example get_summoner
//! ```

use league_link::{authenticate, build_lcu_client, lcu_get, LcuError};
use serde_json::Value;

#[tokio::main]
async fn main() -> Result<(), LcuError> {
    let creds = authenticate(1000, 30).await?;
    println!("found client on port {}", creds.port);

    let client = build_lcu_client()?;
    let me: Value = lcu_get(&client, &creds, "/lol-summoner/v1/current-summoner").await?;
    println!("{}", serde_json::to_string_pretty(&me)?);
    Ok(())
}
