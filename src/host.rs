//! Host resource sampling for the administration console.
//!
//! Everything here reads `/proc`, which is where the deployment already gets its
//! filesystem and permission guarantees from. A field that cannot be read is reported
//! as absent rather than as zero, so an unavailable measurement is never mistaken for
//! an idle one.
//!
//! CPU occupancy is a rate, not a reading: it needs two samples. The first sample
//! records a baseline and reports nothing, and every later sample compares against the
//! previous one. A sample older than a minute is discarded instead of producing a
//! number that describes an idle gap.
//!
//! Both the process rate and the machine rate are percentages of *the whole machine*,
//! so the two are directly comparable and the process can never exceed the machine it
//! runs on. The machine rate is already that scale, because `/proc/stat`'s aggregate row
//! sums every core; the process rate sums every thread, so it is divided by the core
//! count to match.
use std::{
    sync::Mutex,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

/// A CPU sample older than this describes a window long enough that the average would
/// say more about the gap than about the work.
const MAX_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Default, Clone)]
pub struct Process {
    /// Resident set size: the memory this process actually occupies.
    pub rss_bytes: Option<u64>,
    pub virtual_bytes: Option<u64>,
    pub threads: Option<u64>,
    pub open_files: Option<u64>,
    pub uptime_ms: Option<u64>,
    pub cpu_time_ms: Option<u64>,
}

#[derive(Default, Clone)]
pub struct Memory {
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    /// Pages held by processes, backed by no file.
    pub anon_bytes: Option<u64>,
    /// Cached file contents. The kernel drops these under pressure without writing
    /// them anywhere, so this is memory saved, not memory spent.
    pub file_cache_bytes: Option<u64>,
    pub slab_bytes: Option<u64>,
    /// The part of the slab the kernel can hand back, which makes it cache too.
    pub slab_reclaimable_bytes: Option<u64>,
    pub swap_total_bytes: Option<u64>,
    pub swap_free_bytes: Option<u64>,
}

impl Memory {
    /// Memory genuinely in use: what programs hold, plus the slab the kernel cannot
    /// reclaim.
    ///
    /// This is deliberately *not* `MemTotal - MemAvailable`. That subtraction counts
    /// the file cache as spent even though the kernel will drop it the moment
    /// anything needs the room, so on a machine that has read a lot of files it reads
    /// as several gigabytes used when nothing is.
    pub fn used_bytes(&self) -> Option<u64> {
        self.anon_bytes?
            .checked_add(self.slab_bytes?.checked_sub(self.slab_reclaimable_bytes?)?)
    }

    pub fn swap_used_bytes(&self) -> Option<u64> {
        self.swap_total_bytes?.checked_sub(self.swap_free_bytes?)
    }
}

/// Cumulative CPU time, split into the part spent working and the part spent idle.
#[derive(Clone, Copy)]
pub struct CpuTimes {
    pub busy: u64,
    pub idle: u64,
}

/// Aggregated CPU times across every core, from the first line of `/proc/stat`.
pub fn system_cpu_times() -> Option<CpuTimes> {
    let stat = read_trimmed("/proc/stat")?;
    let line = stat.lines().next()?;
    // The aggregate row is the one named exactly "cpu"; the per-core rows are "cpu0".
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let mut busy = 0u64;
    let mut idle = 0u64;
    for (index, field) in fields.enumerate() {
        let value: u64 = field.parse().ok()?;
        match index {
            // Idle and iowait are both "not working"; the rest is the machine busy.
            3 | 4 => idle = idle.checked_add(value)?,
            _ => busy = busy.checked_add(value)?,
        }
    }
    Some(CpuTimes { busy, idle })
}

/// How many logical CPUs this machine has.
pub fn system_cpu_cores() -> Option<u64> {
    let stat = read_trimmed("/proc/stat")?;
    let cores = stat
        .lines()
        .filter(|line| {
            line.split_whitespace()
                .next()
                .is_some_and(|name| name.starts_with("cpu") && name.len() > 3)
        })
        .count();
    u64::try_from(cores).ok().filter(|cores| *cores > 0)
}

/// One-, five- and fifteen-minute load averages.
pub fn load_average() -> Option<(f64, f64, f64)> {
    let line = read_trimmed("/proc/loadavg")?;
    let mut values = line.split_whitespace();
    let first = values.next()?.parse().ok()?;
    let second = values.next()?.parse().ok()?;
    let third = values.next()?.parse().ok()?;
    Some((first, second, third))
}

#[derive(Default, Clone)]
pub struct Disk {
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
}

fn page_size() -> u64 {
    // SAFETY: sysconf with a valid name cannot fail in a way that matters here; the
    // fallback is the near-universal 4 KiB and only affects a display figure.
    let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if value > 0 { value as u64 } else { 4096 }
}

/// Reads this process's own footprint. Every field is independent: a kernel that hides
/// one of them still yields the others.
pub fn process() -> Process {
    let page = page_size();
    let mut result = Process::default();

    if let Some(fields) = read_trimmed("/proc/self/statm") {
        let values: Vec<u64> = fields
            .split_whitespace()
            .filter_map(|value| value.parse().ok())
            .collect();
        if let Some(virtual_bytes) = values.first() {
            result.virtual_bytes = virtual_bytes.checked_mul(page);
        }
        if let Some(rss_pages) = values.get(1) {
            result.rss_bytes = rss_pages.checked_mul(page);
        }
    }

    if let Some(status) = read_trimmed("/proc/self/status") {
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("VmRSS:") {
                result.rss_bytes = value
                    .split_whitespace()
                    .next()
                    .and_then(|kib| kib.parse::<u64>().ok())
                    .map(|kib| kib * 1024);
            } else if let Some(value) = line.strip_prefix("VmSize:") {
                result.virtual_bytes = value
                    .split_whitespace()
                    .next()
                    .and_then(|kib| kib.parse::<u64>().ok())
                    .map(|kib| kib * 1024);
            } else if let Some(value) = line.strip_prefix("Threads:") {
                result.threads = value.trim().parse().ok();
            }
        }
    }

    // Counting directory entries is cheap next to the rest and tells an operator
    // whether the process is leaking descriptors.
    result.open_files = std::fs::read_dir("/proc/self/fd")
        .ok()
        .map(|entries| entries.count() as u64);

    if let Some(stat) = read_trimmed("/proc/self/stat")
        && let Some(cpu) = cpu_time_from_stat(&stat)
    {
        result.cpu_time_ms = Some(cpu);
    }

    if let Some(uptime) = read_trimmed("/proc/uptime")
        && let Some(seconds) = uptime
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<f64>().ok())
    {
        // Total uptime, not this process's own age; subtracting boot-independent
        // process start time would need another file and buy nothing here.
        result.uptime_ms = Some((seconds * 1000.0) as u64);
    }

    result
}

/// User plus system time, in milliseconds, from the comm-bearing `/proc/self/stat`.
fn cpu_time_from_stat(stat: &str) -> Option<u64> {
    // The second field is the command name in parentheses and may itself contain
    // spaces, so parsing starts after the last ')'.
    let tail = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = tail.split_whitespace().collect();
    // After the state field, utime is index 11 and stime index 12.
    let ticks = |index: usize| -> Option<u64> {
        let value: i64 = fields.get(index)?.parse().ok()?;
        u64::try_from(value).ok()
    };
    let hertz = 100.0;
    // SAFETY: sysconf with a valid name; the fallback is the near-universal 100 Hz.
    let configured = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let hertz = if configured > 0 {
        configured as f64
    } else {
        hertz
    };
    Some(((ticks(11)? + ticks(12)?) as f64 / hertz * 1000.0).round() as u64)
}

pub fn memory() -> Memory {
    let mut result = Memory::default();
    let Some(info) = read_trimmed("/proc/meminfo") else {
        return result;
    };
    for line in info.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let parsed = kib(value);
        match key {
            "MemTotal" => result.total_bytes = parsed,
            "MemAvailable" => result.available_bytes = parsed,
            "AnonPages" => result.anon_bytes = parsed,
            "Cached" => result.file_cache_bytes = parsed,
            "Slab" => result.slab_bytes = parsed,
            "SReclaimable" => result.slab_reclaimable_bytes = parsed,
            "SwapTotal" => result.swap_total_bytes = parsed,
            "SwapFree" => result.swap_free_bytes = parsed,
            _ => {}
        }
    }
    result
}

fn kib(value: &str) -> Option<u64> {
    value
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value * 1024)
}

/// Totals and free space of the filesystem holding a path.
pub fn disk(path: &std::path::Path) -> Disk {
    // SAFETY: statvfs only reads the mounted filesystem's counters; the null check
    // below is the only precondition it has.
    let raw = std::ffi::CString::new(path.as_os_str().as_encoded_bytes());
    let Ok(raw) = raw else {
        return Disk::default();
    };
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(raw.as_ptr(), &mut stats) } != 0 {
        return Disk::default();
    }
    let block = stats.f_frsize as u64;
    Disk {
        total_bytes: Some(stats.f_blocks as u64 * block),
        free_bytes: Some(stats.f_bavail as u64 * block),
    }
}

/// One reading of this process, kept so the console can draw a recent history.
#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub ts_ms: i64,
    /// Occupancy since the previous sample, as a percentage of one core. Absent on the
    /// first sample, which has no interval to divide by.
    pub cpu_percent: Option<f64>,
    pub cpu_time_ms: u64,
    pub rss_bytes: Option<u64>,
    pub threads: Option<u64>,
    pub open_files: Option<u64>,
    pub host_memory_total_bytes: Option<u64>,
    pub host_memory_used_bytes: Option<u64>,
    /// Held by processes, as opposed to the file cache. Watching these separately is
    /// what tells a slow leak apart from a large cache that will be dropped anyway.
    pub host_memory_anon_bytes: Option<u64>,
    pub host_memory_file_cache_bytes: Option<u64>,
    /// Occupancy of the whole machine, as a percentage of every core combined.
    pub system_cpu_percent: Option<f64>,
    pub system_cpu_cores: Option<u64>,
    #[serde(rename = "load1m")]
    pub load_1m: Option<f64>,
    #[serde(rename = "load5m")]
    pub load_5m: Option<f64>,
    #[serde(rename = "load15m")]
    pub load_15m: Option<f64>,
    pub requests: u64,
    pub client_errors: u64,
    pub server_errors: u64,
}

/// How many fine samples are retained. At the sampling interval this is a quarter of
/// an hour at full detail.
const HISTORY: usize = 180;
/// How many one-minute rollups and one-hour rollups are retained: a day and a week.
const MINUTES: usize = 1440;
const HOURS: usize = 168;
const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;

/// The windows the console can ask for, mirroring how a hosting dashboard offers
/// "last 15 minutes" through "last 7 days".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Window {
    QuarterHour,
    Hour,
    TwelveHours,
    Day,
    ThreeDays,
    Week,
}

impl Window {
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "15m" => Window::QuarterHour,
            "1h" => Window::Hour,
            "12h" => Window::TwelveHours,
            "1d" => Window::Day,
            "3d" => Window::ThreeDays,
            "7d" => Window::Week,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Window::QuarterHour => "15m",
            Window::Hour => "1h",
            Window::TwelveHours => "12h",
            Window::Day => "1d",
            Window::ThreeDays => "3d",
            Window::Week => "7d",
        }
    }

    fn span_ms(self) -> i64 {
        match self {
            Window::QuarterHour => 15 * MINUTE_MS,
            Window::Hour => 60 * MINUTE_MS,
            Window::TwelveHours => 12 * 60 * MINUTE_MS,
            Window::Day => 24 * 60 * MINUTE_MS,
            Window::ThreeDays => 3 * 24 * 60 * MINUTE_MS,
            Window::Week => 7 * 24 * 60 * MINUTE_MS,
        }
    }

    /// Fine samples only make sense for the shortest window; anything longer reads the
    /// rollups, because one point per five seconds over a week is a chart nobody reads.
    fn resolution_ms(self) -> i64 {
        match self {
            Window::QuarterHour => 5_000,
            Window::Hour | Window::TwelveHours | Window::Day => MINUTE_MS,
            Window::ThreeDays | Window::Week => HOUR_MS,
        }
    }
}

/// A rollup of the fine samples inside one minute or one hour.
///
/// Rates are averaged because an average is what "how busy" means; memory is kept at
/// its peak because a leak is a peak that matters, not the mean between leaks.
#[derive(Default, Clone)]
struct Bucket {
    start_ms: i64,
    end_ms: i64,
    count: u64,
    cpu_total: f64,
    cpu_count: u64,
    system_cpu_total: f64,
    system_cpu_count: u64,
    rss_max: u64,
    memory_total: u64,
    memory_count: u64,
    anon_total: u64,
    anon_count: u64,
    cache_total: u64,
    cache_count: u64,
    requests: u64,
}

impl Bucket {
    fn push(&mut self, sample: &Sample) {
        self.end_ms = sample.ts_ms;
        self.requests = self.requests.max(sample.requests);
        if let Some(value) = sample.cpu_percent {
            self.cpu_total += value;
            self.cpu_count += 1;
        }
        if let Some(value) = sample.system_cpu_percent {
            self.system_cpu_total += value;
            self.system_cpu_count += 1;
        }
        if let Some(value) = sample.rss_bytes {
            self.rss_max = self.rss_max.max(value);
        }
        if let Some(value) = sample.host_memory_used_bytes {
            self.memory_total += value;
            self.memory_count += 1;
        }
        if let Some(value) = sample.host_memory_anon_bytes {
            self.anon_total += value;
            self.anon_count += 1;
        }
        if let Some(value) = sample.host_memory_file_cache_bytes {
            self.cache_total += value;
            self.cache_count += 1;
        }
        self.count += 1;
    }

    fn sample(&self, threads: Option<u64>, open_files: Option<u64>, cores: Option<u64>) -> Sample {
        let average = |total: f64, count: u64| {
            (count > 0).then(|| ((total / count as f64) * 10.0).round() / 10.0)
        };
        Sample {
            ts_ms: self.end_ms,
            cpu_percent: average(self.cpu_total, self.cpu_count),
            cpu_time_ms: 0,
            rss_bytes: (self.rss_max > 0).then_some(self.rss_max),
            threads,
            open_files,
            host_memory_total_bytes: None,
            host_memory_used_bytes: (self.memory_count > 0)
                .then(|| self.memory_total / self.memory_count),
            host_memory_anon_bytes: (self.anon_count > 0)
                .then(|| self.anon_total / self.anon_count),
            host_memory_file_cache_bytes: (self.cache_count > 0)
                .then(|| self.cache_total / self.cache_count),
            system_cpu_percent: average(self.system_cpu_total, self.system_cpu_count),
            system_cpu_cores: cores,
            load_1m: None,
            load_5m: None,
            load_15m: None,
            requests: self.requests,
            client_errors: 0,
            server_errors: 0,
        }
    }
}

/// A fixed-size ring of recent readings.
///
/// Memory and CPU are only meaningful together with time: a single number cannot say
/// whether the server was busy or has been busy all week. The ring is bounded, so a
/// console left open for days costs the same as one opened for a minute.
#[derive(Default)]
pub struct History {
    recent: Mutex<std::collections::VecDeque<Sample>>,
    minutes: Mutex<std::collections::VecDeque<Bucket>>,
    hours: Mutex<std::collections::VecDeque<Bucket>>,
    /// The minute and hour currently being filled.
    open: Mutex<(Bucket, Bucket)>,
    /// Previous process CPU time, for this process's own rate.
    previous: Mutex<Option<(Instant, u64)>>,
    /// Previous machine-wide CPU times, for the machine's rate.
    previous_system: Mutex<Option<(Instant, CpuTimes)>>,
}

impl std::fmt::Debug for History {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let fine = self.recent.lock().map(|items| items.len()).unwrap_or(0);
        let minutes = self.minutes.lock().map(|items| items.len()).unwrap_or(0);
        let hours = self.hours.lock().map(|items| items.len()).unwrap_or(0);
        formatter
            .debug_struct("History")
            .field("recent", &fine)
            .field("minutes", &minutes)
            .field("hours", &hours)
            .finish()
    }
}

/// This process's occupancy over an interval, as a percentage of the whole machine.
///
/// `used` and `elapsed` are both milliseconds of CPU time and wall time, so their ratio
/// is a number of cores. Dividing by `cores` puts that on the same scale as
/// [`History::system_rate`], where 100% is every core combined. Without it the process
/// is measured against a single core while the machine is measured against all of them,
/// and the process line rides `cores` times too high — often past the machine's own.
fn process_cpu_percent(used: f64, elapsed: f64, cores: u64) -> f64 {
    debug_assert!(elapsed > 0.0, "an empty interval has no rate");
    let cores = cores.max(1) as f64;
    ((used / elapsed / cores) * 1000.0).round() / 10.0
}

impl History {
    /// Records one reading and returns it.
    pub fn record(&self, requests: u64, client_errors: u64, server_errors: u64) -> Sample {
        let process = process();
        let memory = memory();
        let now = Instant::now();
        let detected_cores = system_cpu_cores();
        // An unreadable core count is still a machine with at least one core, and the
        // rate needs a divisor either way. The field below keeps reporting the absence.
        let cores = detected_cores.unwrap_or(1).max(1);
        let system_cpu_percent = self.system_rate(now);
        let (load_1m, load_5m, load_15m) = load_average().unwrap_or((0.0, 0.0, 0.0));
        let load_1m = Some(load_1m);
        let load_5m = Some(load_5m);
        let load_15m = Some(load_15m);
        let cpu_percent = process.cpu_time_ms.and_then(|cpu_ms| {
            let mut previous = self.previous.lock().ok()?;
            let measured = match *previous {
                // A stale baseline would average across an idle gap and under-report.
                Some((at, ms)) if now.duration_since(at) <= MAX_WINDOW => Some((at, ms)),
                _ => None,
            };
            *previous = Some((now, cpu_ms));
            let (at, ms) = measured?;
            let elapsed_ms = now.duration_since(at).as_secs_f64() * 1000.0;
            if elapsed_ms <= 0.0 {
                return None;
            }
            let used = cpu_ms.checked_sub(ms)? as f64;
            Some(process_cpu_percent(used, elapsed_ms, cores))
        });

        let sample = Sample {
            ts_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_millis() as i64)
                .unwrap_or(0),
            cpu_percent,
            cpu_time_ms: process.cpu_time_ms.unwrap_or(0),
            rss_bytes: process.rss_bytes,
            threads: process.threads,
            open_files: process.open_files,
            host_memory_total_bytes: memory.total_bytes,
            host_memory_used_bytes: memory.used_bytes(),
            host_memory_anon_bytes: memory.anon_bytes,
            host_memory_file_cache_bytes: memory.file_cache_bytes,
            system_cpu_percent,
            system_cpu_cores: detected_cores,
            load_1m,
            load_5m,
            load_15m,
            requests,
            client_errors,
            server_errors,
        };
        if let Ok(mut samples) = self.recent.lock() {
            if samples.len() >= HISTORY {
                samples.pop_front();
            }
            samples.push_back(sample.clone());
        }
        self.roll_up(&sample);
        sample
    }

    /// Feeds one fine sample into the minute and hour rollups, closing whichever the
    /// sample has moved past. Doing it here rather than on a second timer means the
    /// rollups can never drift from the samples they summarize.
    fn roll_up(&self, sample: &Sample) {
        let Ok(mut open) = self.open.lock() else {
            return;
        };
        let (mut minute, mut hour) = (open.0.clone(), open.1.clone());
        if minute.start_ms == 0 {
            minute.start_ms = sample.ts_ms - sample.ts_ms.rem_euclid(MINUTE_MS);
            hour.start_ms = sample.ts_ms - sample.ts_ms.rem_euclid(HOUR_MS);
        }
        minute.push(sample);
        hour.push(sample);

        let minute_changed = sample.ts_ms - sample.ts_ms.rem_euclid(MINUTE_MS) != minute.start_ms;
        let hour_changed = sample.ts_ms - sample.ts_ms.rem_euclid(HOUR_MS) != hour.start_ms;
        if minute_changed {
            let closed = std::mem::take(&mut minute);
            if let Ok(mut minutes) = self.minutes.lock() {
                if minutes.len() >= MINUTES {
                    minutes.pop_front();
                }
                minutes.push_back(closed);
            }
            minute.start_ms = sample.ts_ms - sample.ts_ms.rem_euclid(MINUTE_MS);
        }
        if hour_changed {
            let closed = std::mem::take(&mut hour);
            if let Ok(mut hours) = self.hours.lock() {
                if hours.len() >= HOURS {
                    hours.pop_front();
                }
                hours.push_back(closed);
            }
            hour.start_ms = sample.ts_ms - sample.ts_ms.rem_euclid(HOUR_MS);
        }
        *open = (minute, hour);
    }

    /// The readings for a window, at a resolution that stays readable at that span.
    pub fn window(&self, window: Window) -> (Window, i64, Vec<Sample>) {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis() as i64)
            .unwrap_or(0)
            - window.span_ms();
        let (threads, open_files, cores) = {
            let process = process();
            (process.threads, process.open_files, system_cpu_cores())
        };
        let guard = |bucket: &Bucket| {
            let mut sample = bucket.sample(threads, open_files, cores);
            sample.ts_ms = bucket.end_ms;
            sample
        };
        let source: Vec<Sample> = match window {
            Window::QuarterHour => self
                .recent
                .lock()
                .map(|samples| {
                    samples
                        .iter()
                        .filter(|sample| sample.ts_ms >= since)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            Window::Hour | Window::TwelveHours | Window::Day => self
                .minutes
                .lock()
                .map(|minutes| {
                    minutes
                        .iter()
                        .filter(|bucket| bucket.end_ms >= since)
                        .map(&guard)
                        .collect()
                })
                .unwrap_or_default(),
            Window::ThreeDays | Window::Week => self
                .hours
                .lock()
                .map(|hours| {
                    hours
                        .iter()
                        .filter(|bucket| bucket.end_ms >= since)
                        .map(&guard)
                        .collect()
                })
                .unwrap_or_default(),
        };
        (window, window.resolution_ms(), source)
    }

    /// The machine's occupancy since the previous sample, as a percentage of all cores
    /// combined. `None` for the first sample, which has no interval.
    fn system_rate(&self, now: Instant) -> Option<f64> {
        let current = system_cpu_times()?;
        let mut previous = self.previous_system.lock().ok()?;
        let measured = match *previous {
            Some((at, times)) if now.duration_since(at) <= MAX_WINDOW => Some((at, times)),
            _ => None,
        };
        *previous = Some((now, current));
        let (_at, before) = measured?;
        let idle = current.idle.checked_sub(before.idle)?;
        let busy = current.busy.checked_sub(before.busy)?;
        let total = busy.checked_add(idle)?;
        if total == 0 {
            return None;
        }
        Some(((busy as f64 / total as f64) * 1000.0).round() / 10.0)
    }

    /// The newest sample, or a zeroed one when nothing has been recorded yet.
    pub fn latest(&self) -> Sample {
        self.recent
            .lock()
            .ok()
            .and_then(|samples| samples.back().cloned())
            .unwrap_or_default()
    }

    /// Every fine sample retained, oldest first. Tests and diagnostics read this; the
    /// console draws the windowed view instead.
    #[cfg(test)]
    pub fn series(&self) -> Vec<Sample> {
        self.recent
            .lock()
            .map(|samples| samples.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_reports_its_own_footprint() {
        let sample = process();
        let rss = sample
            .rss_bytes
            .expect("resident memory is readable on Linux");
        assert!(rss > 0, "resident memory should not be zero");
        assert!(sample.cpu_time_ms.is_some());
        assert!(sample.threads.is_some_and(|threads| threads > 0));
        assert!(sample.open_files.is_some_and(|files| files > 0));
    }

    #[test]
    fn host_memory_and_disk_are_readable_and_consistent() {
        let memory = memory();
        let total = memory.total_bytes.expect("MemTotal is readable");
        let available = memory.available_bytes.expect("MemAvailable is readable");
        assert!(total > 0 && available > 0);
        assert!(available <= total, "available cannot exceed total");

        let disk = disk(std::path::Path::new("/"));
        let size = disk.total_bytes.expect("the root filesystem is mounted");
        let free = disk.free_bytes.expect("free space is readable");
        assert!(size > 0);
        assert!(free <= size, "free space cannot exceed the filesystem size");
    }

    #[test]
    fn the_process_rate_is_measured_against_the_whole_machine() {
        // One core busy for the whole interval on a 16-core machine is 6.3%, not 100%.
        // Measuring it as 100% is what put this process's line above the machine's.
        assert_eq!(process_cpu_percent(1000.0, 1000.0, 16), 6.3);
        assert_eq!(process_cpu_percent(500.0, 1000.0, 4), 12.5);
        // Every core saturated reads as the machine being saturated.
        assert_eq!(process_cpu_percent(4000.0, 1000.0, 4), 100.0);
        // A single-core machine needs no correction.
        assert_eq!(process_cpu_percent(250.0, 1000.0, 1), 25.0);
        // A core count of zero would divide by zero; it is floored instead.
        assert_eq!(process_cpu_percent(100.0, 1000.0, 0), 10.0);
    }

    #[test]
    fn a_single_sample_reports_no_rate_rather_than_zero() {
        let history = History::default();
        history.record(0, 0, 0);
        // One sample has no interval to divide by. Reporting 0% would read as "idle"
        // when it actually means "not measured yet".
        assert!(history.latest().cpu_percent.is_none());
        let second = history.record(0, 0, 0);
        let value = second
            .cpu_percent
            .expect("the second sample has an interval");
        assert!(value >= 0.0, "a rate cannot be negative");
        assert!(series_contains(&history));
    }

    #[test]
    fn the_ring_is_bounded_and_returns_oldest_first() {
        let history = History::default();
        for index in 0..(HISTORY + 25) {
            history.record(index as u64, 0, 0);
        }
        let series = history.series();
        assert_eq!(
            series.len(),
            HISTORY,
            "the ring must not grow without bound"
        );
        assert!(
            series.windows(2).all(|pair| pair[0].ts_ms <= pair[1].ts_ms),
            "a chart needs the samples in time order"
        );
    }

    #[test]
    fn a_stale_baseline_is_discarded_rather_than_averaged() {
        let history = History {
            previous: std::sync::Mutex::new(Some((
                Instant::now() - std::time::Duration::from_secs(3600),
                0,
            ))),
            ..History::default()
        };
        history.record(0, 0, 0);
        assert!(
            history.latest().cpu_percent.is_none(),
            "an hour-old baseline must not produce a rate"
        );
    }

    #[test]
    fn request_counters_travel_with_each_reading() {
        let history = History::default();
        history.record(120, 4, 1);
        let latest = history.latest();
        assert_eq!(latest.requests, 120);
        assert_eq!(latest.client_errors, 4);
        assert_eq!(latest.server_errors, 1);
        assert!(latest.ts_ms > 0, "a reading must carry a timestamp");
    }

    fn series_contains(history: &History) -> bool {
        history.series().len() == 2
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;

    /// Feeds readings so the fine ring and its bound can be checked without waiting.
    fn fill(history: &History, count: u64) {
        for index in 0..count {
            history.record(index * 10, 0, 0);
        }
    }

    #[test]
    fn every_offered_window_resolves_and_names_its_resolution() {
        for (raw, resolution) in [
            ("15m", 5_000),
            ("1h", 60_000),
            ("12h", 60_000),
            ("1d", 60_000),
            ("3d", 3_600_000),
            ("7d", 3_600_000),
        ] {
            let window = Window::parse(raw).unwrap_or_else(|| panic!("{raw} should parse"));
            assert_eq!(window.as_str(), raw);
            assert_eq!(window.resolution_ms(), resolution, "{raw}");
        }
        assert!(Window::parse("2h").is_none());
        assert!(Window::parse("").is_none());
        assert!(Window::parse("../../etc").is_none());
    }

    #[test]
    fn a_short_window_keeps_the_fine_samples_and_a_long_one_switches_resolution() {
        let history = History::default();
        // Only the latest sample can matter here, so the window is read straight after.
        history.record(7, 1, 0);
        let (window, resolution, samples) = history.window(Window::QuarterHour);
        assert_eq!(window, Window::QuarterHour);
        assert_eq!(resolution, 5_000);
        assert_eq!(samples.len(), 1, "a new history has one fine sample");

        // A longer window reads the rollups, which have nothing yet: reporting an empty
        // series is honest, where inventing points would not be.
        let (_, resolution, samples) = history.window(Window::Week);
        assert_eq!(resolution, 3_600_000);
        assert!(samples.is_empty());
    }

    #[test]
    fn the_rollups_stay_bounded() {
        let history = History::default();
        fill(&history, 50);
        let fine = history.series().len();
        assert!(fine > 0);
        // Feeding many samples must not grow the fine ring past its bound.
        for _ in 0..(HISTORY + 50) {
            history.record(1, 0, 0);
        }
        assert_eq!(history.series().len(), HISTORY);
    }

    #[test]
    fn a_bucket_averages_rates_and_keeps_the_memory_peak() {
        let mut bucket = Bucket {
            start_ms: 0,
            ..Bucket::default()
        };
        let sample = |cpu: f64, rss: u64| Sample {
            ts_ms: 1,
            cpu_percent: Some(cpu),
            rss_bytes: Some(rss),
            host_memory_used_bytes: Some(rss * 2),
            requests: 5,
            ..Sample::default()
        };
        bucket.push(&sample(10.0, 100));
        bucket.push(&sample(30.0, 900));
        let rolled = bucket.sample(None, None, None);
        assert_eq!(rolled.cpu_percent, Some(20.0), "a rate is averaged");
        assert_eq!(rolled.rss_bytes, Some(900), "memory keeps its peak");
        assert_eq!(rolled.host_memory_used_bytes, Some(1000));
        assert_eq!(rolled.requests, 5, "requests are cumulative, not summed");
    }

    #[test]
    fn a_rate_is_absent_when_nothing_was_measured() {
        let bucket = Bucket::default();
        let rolled = bucket.sample(None, None, None);
        assert_eq!(rolled.cpu_percent, None);
        assert_eq!(rolled.system_cpu_percent, None);
        assert_eq!(rolled.rss_bytes, None);
    }

    #[test]
    fn the_machine_reads_its_own_cpu_and_load() {
        let cores = system_cpu_cores().expect("/proc/stat lists per-core rows");
        assert!(cores > 0);
        let times = system_cpu_times().expect("the aggregate row is readable");
        assert!(
            times.busy + times.idle > 0,
            "a running machine has spent some time"
        );
        let load = load_average().expect("/proc/loadavg is readable");
        assert!(load.0 >= 0.0 && load.1 >= 0.0 && load.2 >= 0.0);

        let history = History::default();
        history.record(0, 0, 0);
        // The machine-wide rate needs a second sample, just like the process rate.
        assert!(history.latest().system_cpu_percent.is_none());

        // Two readings taken microseconds apart describe no interval at all: the
        // kernel's counters have not moved, so there is no rate to report. The rate
        // appears once at least one clock tick has passed.
        let mut measured = None;
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            if let Some(value) = history.record(0, 0, 0).system_cpu_percent {
                measured = Some(value);
                break;
            }
        }
        let value = measured.expect("a rate appears once the counters advance");
        assert!((0.0..=100.0).contains(&value), "implausible: {value}");
    }

    #[test]
    fn the_process_never_outweighs_the_machine_it_runs_on() {
        let history = History::default();
        // Both counters move in clock ticks, one per core, and they are read at
        // slightly different instants. Over a very short interval one tick of the
        // process is worth a whole 6.25% of a sixteen-core machine, which would make
        // this comparison a coin flip rather than a check. A quarter of a second puts
        // a tick at about 0.2%, well inside the slack below.
        history.record(0, 0, 0);
        std::thread::sleep(std::time::Duration::from_millis(250));
        let sample = history.record(0, 0, 0);
        let process = sample
            .cpu_percent
            .expect("the second sample has an interval");
        let system = sample
            .system_cpu_percent
            .expect("the machine rate shares that interval");
        // The two are read over the same interval, so the process — a part of the
        // machine — cannot exceed it. Rounding at each end accounts for the slack.
        assert!(
            process <= system + 1.0,
            "the process ({process}%) is measured on a different scale than \
             the machine ({system}%), so the two cannot be compared"
        );
    }

    #[test]
    fn used_memory_excludes_the_file_cache() {
        let memory = memory();
        // Whatever the kernel reports, "used" is never more than what the machine has,
        // and the cache never contributes to it.
        if let (Some(total), Some(used)) = (memory.total_bytes, memory.used_bytes()) {
            assert!(used <= total);
        }
        if let (Some(anon), Some(slab), Some(reclaimable)) = (
            memory.anon_bytes,
            memory.slab_bytes,
            memory.slab_reclaimable_bytes,
        ) {
            assert_eq!(memory.used_bytes(), Some(anon + slab - reclaimable));
        }
        let empty = Memory::default();
        assert_eq!(
            empty.used_bytes(),
            None,
            "nothing measured is nothing reported"
        );
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;

    #[test]
    fn the_file_cache_is_not_counted_as_used() {
        // A machine that has read a lot of files but runs nothing has a large cache
        // and nothing in use; the two must not be confused.
        let memory = Memory {
            total_bytes: Some(16 * 2 * GIB),
            available_bytes: Some(7 * GIB),
            anon_bytes: Some(2 * GIB),
            file_cache_bytes: Some(6 * GIB),
            slab_bytes: Some(GIB),
            slab_reclaimable_bytes: Some(GIB / 2),
            swap_total_bytes: Some(8 * GIB),
            swap_free_bytes: Some(6 * GIB),
        };
        let used = memory.used_bytes().expect("all parts were measured");
        assert_eq!(used, 2 * GIB + GIB / 2, "processes plus unreclaimable slab");
        // The naive subtraction would have reported roughly nine gigabytes.
        let naive = memory.total_bytes.unwrap() - memory.available_bytes.unwrap();
        assert!(
            naive - used > GIB,
            "the distinction this method exists for must be visible"
        );
        assert_eq!(memory.swap_used_bytes(), Some(2 * GIB));
    }

    #[test]
    fn an_unmeasured_part_yields_no_figure_rather_than_a_guess() {
        let memory = Memory::default();
        assert_eq!(memory.used_bytes(), None);
        assert_eq!(memory.swap_used_bytes(), None);
    }

    const GIB: u64 = 1024 * 1024 * 1024;
}
