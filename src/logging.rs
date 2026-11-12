use std::fmt;

use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::db::Db;
use crate::util;

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub ts: i64,
    pub level: String,
    pub target: String,
    pub message: String,
}

struct DbLogLayer {
    sender: mpsc::Sender<LogEntry>,
}

impl<S: Subscriber> Layer<S> for DbLogLayer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let metadata = event.metadata();
        let level = *metadata.level();
        if level > Level::INFO {
            return;
        }
        let target = metadata.target();
        if level == Level::INFO && !target.starts_with("workamon") {
            return;
        }

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let message = visitor
            .message
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| {
                if visitor.fields.is_empty() {
                    metadata.name().to_string()
                } else {
                    visitor.fields.join(" ")
                }
            });

        let entry = LogEntry {
            ts: util::now(),
            level: level.as_str().to_string(),
            target: target.to_string(),
            message: util::truncate_chars(&message, 4_000),
        };
        if self.sender.try_send(entry).is_err() {
            eprintln!("workamon: buffer log database penuh, satu entri dibuang");
        }
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
    fields: Vec<String>,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }
}

pub fn init(default_filter: &str) -> mpsc::Receiver<LogEntry> {
    let (sender, receiver) = mpsc::channel(2_048);
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_filter.to_string()));
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_level(true);
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(DbLogLayer { sender })
        .init();
    receiver
}

pub async fn run_writer(db: Db, mut receiver: mpsc::Receiver<LogEntry>) {
    while let Some(entry) = receiver.recv().await {
        if let Err(error) = db.insert_log(entry.ts, &entry.level, &entry.target, &entry.message) {
            eprintln!("workamon: gagal menulis log ke database: {error:#}");
        }
    }
}
