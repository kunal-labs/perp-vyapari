//! Telegram transport: owner-only command handling and pushing venue events.

use std::{sync::Arc, time::Duration};

use anyhow::{bail, Context, Result};
use teloxide::{prelude::*, types::ParseMode};
use tokio_stream::StreamExt;
use tracing::{info, warn};
use venue_proto::v1::{event, StreamEventsRequest};

use crate::{app::App, format};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner(pub u64);

/// Read the bot token from a file (never from argv or plain env, never logged).
pub fn read_token(path: &str) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read TELEGRAM_TOKEN_FILE {path}"))?;
    let t = raw.trim().to_string();
    // Format: <bot id digits>:<secret>. Validate shape without echoing it.
    let ok = t.split_once(':').is_some_and(|(id, s)| {
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && s.len() >= 30
    });
    if !ok {
        bail!("TELEGRAM_TOKEN_FILE does not contain a bot token");
    }
    Ok(t)
}

pub fn parse_owner(raw: Option<String>) -> Result<Owner> {
    let raw = raw.context("TELEGRAM_OWNER_ID is required (your numeric Telegram user id)")?;
    let id: u64 = raw
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("TELEGRAM_OWNER_ID must be numeric (not a @username)"))?;
    if id == 0 {
        bail!("TELEGRAM_OWNER_ID must be non-zero");
    }
    Ok(Owner(id))
}

/// Only the owner, only in a private chat. Everything else is silently ignored.
pub fn is_owner_dm(msg: &Message, owner: Owner) -> bool {
    msg.chat.is_private() && msg.from.as_ref().is_some_and(|u| u.id.0 == owner.0)
}

async fn send(bot: &Bot, owner: Owner, text: String) {
    for chunk in split_message(&text, 4000) {
        if let Err(e) = bot
            .send_message(ChatId(owner.0 as i64), chunk)
            .parse_mode(ParseMode::Html)
            .await
        {
            warn!(error = %e, "telegram send failed");
        }
    }
}

/// Split on line boundaries to stay under Telegram's 4096-char message limit.
pub fn split_message(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        if cur.chars().count() + line.chars().count() > max && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push_str(line);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Long-poll Telegram until `shutdown` resolves.
pub async fn run(
    bot: Bot,
    owner: Owner,
    app: App,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let me = bot
        .get_me()
        .await
        .context("telegram getMe failed (bad token?)")?;
    info!(bot = %me.username(), "telegram connected");

    for name in app.venue_names() {
        tokio::spawn(forward_events(bot.clone(), owner, app.clone(), name));
    }

    let app = Arc::new(app);
    let handler = Update::filter_message().endpoint(
        |bot: Bot, msg: Message, owner: Owner, app: Arc<App>| async move {
            if !is_owner_dm(&msg, owner) {
                let from = msg.from.as_ref().map(|u| u.id.0).unwrap_or_default();
                warn!(from, chat = msg.chat.id.0, "ignored message from non-owner");
                return respond(());
            }
            if let Some(text) = msg.text() {
                if let Some(reply) = app.handle(text).await {
                    send(&bot, owner, reply).await;
                }
            }
            respond(())
        },
    );

    let mut dispatcher = Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![owner, app])
        .build();
    let token = dispatcher.shutdown_token();
    tokio::spawn(async move {
        shutdown.await;
        info!("shutdown requested");
        if let Ok(f) = token.shutdown() {
            f.await;
        }
    });
    dispatcher.dispatch().await;
    Ok(())
}

/// Subscribe to one venue's event stream forever, pushing user-facing events to the owner.
/// Reconnects with backoff and tells the owner when a venue goes silent / comes back.
async fn forward_events(bot: Bot, owner: Owner, app: App, name: String) {
    const SILENT_AFTER: Duration = Duration::from_secs(60);
    let mut backoff = Duration::from_secs(1);
    let mut down_since: Option<tokio::time::Instant> = None;
    let mut told_down = false;

    loop {
        let Some(mut client) = app.client(&name) else {
            return;
        };
        match client.stream_events(StreamEventsRequest {}).await {
            Ok(resp) => {
                let mut stream = resp.into_inner();
                backoff = Duration::from_secs(1);
                if told_down {
                    send(
                        &bot,
                        owner,
                        format!(
                            "✅ <b>{}</b> venue service reachable again",
                            format::esc(&name)
                        ),
                    )
                    .await;
                }
                down_since = None;
                told_down = false;
                loop {
                    match tokio::time::timeout(SILENT_AFTER, stream.next()).await {
                        Ok(Some(Ok(ev))) => {
                            if let Some(event::Kind::Heartbeat(h)) = &ev.kind {
                                if !h.venue_stream_connected {
                                    tracing::debug!(venue = %name, "venue reports exchange API not connected");
                                }
                            }
                            if let Some(text) = format::event(&name, &ev) {
                                send(&bot, owner, text).await;
                            }
                        }
                        Ok(Some(Err(s))) => {
                            warn!(venue = %name, error = %s, "event stream error");
                            break;
                        }
                        Ok(None) => {
                            warn!(venue = %name, "event stream ended");
                            break;
                        }
                        Err(_) => {
                            warn!(venue = %name, "no events or heartbeats for {SILENT_AFTER:?}");
                            break;
                        }
                    }
                }
            }
            Err(s) => warn!(venue = %name, error = %s, "cannot subscribe to events"),
        }
        let since = *down_since.get_or_insert_with(tokio::time::Instant::now);
        if !told_down && since.elapsed() >= SILENT_AFTER {
            send(
                &bot,
                owner,
                format!(
                    "⚠️ <b>{}</b> venue service unreachable for over a minute; alerts paused",
                    format::esc(&name)
                ),
            )
            .await;
            told_down = true;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn owner_must_be_numeric() {
        assert_eq!(parse_owner(Some(" 12345 ".into())).unwrap(), Owner(12345));
        assert!(parse_owner(Some("kunallimaye".into())).is_err());
        assert!(parse_owner(Some("0".into())).is_err());
        assert!(parse_owner(None).is_err());
    }

    #[test]
    fn token_file_shape_checked_without_echo() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "123456:{}", "A".repeat(35)).unwrap();
        assert!(read_token(f.path().to_str().unwrap()).is_ok());

        let mut bad = tempfile::NamedTempFile::new().unwrap();
        writeln!(bad, "SECRETVALUE").unwrap();
        let err = read_token(bad.path().to_str().unwrap()).unwrap_err();
        assert!(!format!("{err:#}").contains("SECRETVALUE"));
    }

    #[test]
    fn owner_filter_requires_private_chat_from_owner() {
        let mk = |from: u64, chat_type: &str| -> Message {
            serde_json::from_value(serde_json::json!({
                "message_id": 1, "date": 0,
                "chat": {"id": from as i64, "type": chat_type, "title": "t", "first_name": "x"},
                "from": {"id": from, "is_bot": false, "first_name": "x"},
                "text": "/pos"
            }))
            .unwrap()
        };
        assert!(is_owner_dm(&mk(42, "private"), Owner(42)));
        assert!(!is_owner_dm(&mk(7, "private"), Owner(42)));
        assert!(!is_owner_dm(&mk(42, "group"), Owner(42)));
    }

    #[test]
    fn splits_long_messages_on_lines() {
        let text = "aaaa\nbbbb\ncccc\n";
        assert_eq!(split_message(text, 10), vec!["aaaa\nbbbb\n", "cccc\n"]);
        assert_eq!(split_message("short", 10), vec!["short"]);
    }
}
