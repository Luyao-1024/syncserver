//! Operator-facing read path over the log database and its compressed archive.

use super::{LOG_DATABASE, LogFilter, LogFormat, normalize_level, now_ms, query, render};
use crate::{Config, Error, Result};
use std::io::{self, Write};

const USAGE: &str = "usage: syncserver logs [--level LEVEL[,...]] [--since DURATION|EPOCH_MS] [--until DURATION|EPOCH_MS] [--contains TEXT] [--request-id ID] [--limit N] [--archive] [--json]";
const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 100_000;

/// Reads `<data_dir>/logs.sqlite` without taking the data directory lock, so it also
/// works while the service is running.
pub fn run(config: Config, args: &[String]) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let mut filter = LogFilter {
        limit: DEFAULT_LIMIT,
        ..LogFilter::default()
    };
    let mut format = LogFormat::Text;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        let mut read_value = || -> Result<String> {
            let value = args
                .get(index)
                .cloned()
                .filter(|value| !value.is_empty())
                .ok_or(Error::Config("logs option is missing a non-empty value"))?;
            index += 1;
            Ok(value)
        };
        match flag {
            "--level" => {
                let value = read_value()?;
                filter.levels = value
                    .split(',')
                    .map(normalize_level)
                    .filter(|level| !level.is_empty())
                    .collect();
            }
            "--since" => filter.since_ms = Some(reference(&read_value()?)?),
            "--until" => filter.until_ms = Some(reference(&read_value()?)?),
            "--contains" => filter.contains = Some(read_value()?),
            "--request-id" => filter.request_id = Some(read_value()?),
            "--limit" => {
                let value = read_value()?;
                filter.limit = value
                    .parse::<usize>()
                    .ok()
                    .filter(|limit| *limit > 0 && *limit <= MAX_LIMIT)
                    .ok_or(Error::Config("logs limit must be between 1 and 100000"))?;
            }
            "--archive" | "--all" => filter.include_archive = true,
            "--json" => format = LogFormat::Json,
            _ => return Err(Error::Config("unknown logs option")),
        }
    }
    let path = config.data_directory().join(LOG_DATABASE);
    if !path.exists() {
        return Err(Error::Config(
            "log database is missing; run the service with a persist level above off first",
        ));
    }
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for row in query(&path, &filter)? {
        writeln!(output, "{}", render(&row.record, format)).map_err(|_| Error::LogStorage)?;
    }
    output.flush().map_err(|_| Error::LogStorage)?;
    Ok(())
}

/// Durations such as 36h are relative to now; plain integers are epoch milliseconds.
fn reference(value: &str) -> Result<i64> {
    let text = value.trim();
    let (digits, multiplier) = match text.chars().last() {
        Some('s') => (&text[..text.len() - 1], 1_000),
        Some('m') => (&text[..text.len() - 1], 60_000),
        Some('h') => (&text[..text.len() - 1], 3_600_000),
        Some('d') => (&text[..text.len() - 1], 86_400_000),
        _ => (text, 1),
    };
    let amount: i64 = digits
        .trim()
        .parse()
        .ok()
        .filter(|value| *value >= 0)
        .ok_or(Error::Config(
            "log time must be a non-negative duration or epoch milliseconds",
        ))?;
    let scaled = amount
        .checked_mul(multiplier)
        .ok_or(Error::Config("log time is out of range"))?;
    if multiplier == 1 {
        Ok(scaled)
    } else {
        Ok(now_ms().saturating_sub(scaled))
    }
}
