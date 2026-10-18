//! The tracing layer that renders to the console and publishes to the log database.

use super::record::{Collector, Correlation, level_of, now_ms};
use super::{LogRecord, Writer, render};
use crate::config::{LogFormat, LogLevel, LogSettings};
use std::sync::Arc;
use tracing::Subscriber;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Maximum number of enclosing spans inspected for a request id.
const SPAN_WALK_LIMIT: usize = 8;

/// Single layer for both outputs, so redaction and truncation cannot diverge between
/// the console and the persisted log database.
pub struct LogLayer {
    console: Option<LogLevel>,
    persist: Option<LogLevel>,
    format: LogFormat,
    payload_detail: bool,
    writer: Option<Arc<Writer>>,
}

impl LogLayer {
    pub fn new(settings: &LogSettings, writer: Option<Arc<Writer>>) -> Self {
        Self {
            console: (settings.level != LogLevel::Off).then_some(settings.level),
            persist: (settings.persist_level != LogLevel::Off).then_some(settings.persist_level),
            format: settings.format,
            payload_detail: settings.payload_detail,
            writer,
        }
    }

    /// True when either output would show this record, so expensive rendering is skipped.
    fn wanted(&self, level: LogLevel) -> bool {
        self.console
            .is_some_and(|threshold| threshold.includes(level))
            || self
                .persist
                .is_some_and(|threshold| threshold.includes(level))
    }
}

impl<S> Layer<S> for LogLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let mut collector = Collector::new(self.payload_detail);
        attrs.record(&mut collector);
        let Some(request_id) = collector.request_id else {
            return;
        };
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Correlation {
                request_id: Some(request_id),
            });
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let level = level_of(event.metadata().level());
        if !self.wanted(level) {
            return;
        }
        let mut collector = Collector::new(self.payload_detail);
        event.record(&mut collector);
        let request_id = collector
            .request_id
            .clone()
            .or_else(|| correlation(&ctx, event));
        let record = LogRecord {
            ts_ms: now_ms(),
            level: level.as_str().to_ascii_uppercase(),
            target: event.metadata().target().to_owned(),
            message: collector.message,
            fields: collector.fields,
            request_id,
            user_id: collector.user_id,
        };
        if self
            .console
            .is_some_and(|threshold| threshold.includes(level))
        {
            println!("{}", render(&record, self.format));
        }
        if self
            .persist
            .is_some_and(|threshold| threshold.includes(level))
            && let Some(writer) = &self.writer
        {
            writer.publish(record);
        }
    }
}

fn correlation<S>(ctx: &Context<'_, S>, event: &tracing::Event<'_>) -> Option<String>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    let mut span = ctx.event_span(event).or_else(|| ctx.lookup_current())?;
    for _ in 0..SPAN_WALK_LIMIT {
        let found = span
            .extensions()
            .get::<Correlation>()
            .and_then(|value| value.request_id.clone());
        if found.is_some() {
            return found;
        }
        span = span.parent()?;
    }
    None
}
