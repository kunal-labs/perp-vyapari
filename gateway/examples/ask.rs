//! Dev helper: run a bot command against live venue services without Telegram.
//!
//!     VENUES=hl=http://127.0.0.1:50051 cargo run -p gateway --example ask -- /pos

use gateway::{app::App, parse_venues};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let venues = parse_venues(&std::env::var("VENUES")?)?;
    let text = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let app = App::new(&venues)?;
    println!(
        "{}",
        app.handle(&text)
            .await
            .unwrap_or_else(|| "(not a command)".into())
    );
    Ok(())
}
