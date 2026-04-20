//! Stream every LCU event to stdout.
//!
//! ```text
//! cargo run --example watch_events
//! ```

use league_link::{authenticate, ws_connect, LcuError};

#[tokio::main]
async fn main() -> Result<(), LcuError> {
    let creds = authenticate(1000, 30).await?;
    let mut stream = ws_connect(&creds, 128).await?;
    println!("connected — waiting for events...");
    while let Some(event) = stream.recv().await {
        println!("[{:?}] {}", event.event_type, event.uri);
    }
    println!("connection closed");
    Ok(())
}
