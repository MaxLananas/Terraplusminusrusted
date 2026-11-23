//! Retries, per-host concurrency and rate-limit handling around an [`HttpTransport`].

use crate::http::{HttpResponse, HttpTransport, TransportError, TransportLimits};
use std::collections::HashMap;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};
use terra_elevation::{ElevationError, FetchOutcome, TileFetcher};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts per URL, including the first.
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    /// Longest `Retry-After` honoured; longer requests fail the attempt instead.
    pub max_retry_after: Duration,
    /// Requests in flight per host.
    pub max_per_host: usize,
    /// Hard deadline for one fetch: queueing for the host, every attempt and every backoff.
    /// Each attempt's transport deadline is cut to what is left.
    pub fetch_deadline: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(8),
            max_retry_after: Duration::from_secs(30),
            max_per_host: 8,
            fetch_deadline: Duration::from_secs(90),
        }
    }
}

#[derive(Default)]
struct HostState {
    in_flight: usize,
    /// No new request to the host before this instant (rate limiting).
    cooldown_until: Option<Instant>,
}

/// TerraMinusMinus' `TerraConfig.http.maxConcurrentRequests`, in its own `"<n>: <url>"` form.
/// Upstream's `maxConcurrentRequests`, plus the IGN Géoplateforme (40 requests per second per IP
/// allowed; 6 in flight stays under it at the usual latencies) for `elevation.ign_lidar_hd`.
pub const DEFAULT_HOST_LIMITS: [&str; 6] = [
    "8: https://cloud.daporkchop.net/",
    "8: https://s3.amazonaws.com/",
    "1: http://gis-treecover.wri.org/",
    "1: https://overpass.kumi.systems/",
    "1: https://lz4.overpass-api.de/",
    "6: https://data.geopf.fr/",
];

/// One `maxConcurrentRequests` entry, `"<n>: <url>"` (`^(\d+): (.+)$`), as (host, n). Only the
/// URL's authority counts: an `https://` host fetched over plain HTTP keeps its limit.
pub fn parse_host_limit(entry: &str) -> Option<(String, usize)> {
    let (count, url) = entry.split_once(": ")?;
    if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let count: usize = count.parse().ok().filter(|count| *count > 0)?;
    let (_, rest) = url.split_once("://")?;
    let host = rest.split('/').next().filter(|host| !host.is_empty())?;
    Some((host.to_ascii_lowercase(), count))
}

pub struct HttpTileFetcher<T> {
    transport: T,
    limits: TransportLimits,
    policy: RetryPolicy,
    /// Per-host overrides of `policy.max_per_host`, by lower-case authority.
    host_limits: HashMap<String, usize>,
    hosts: Mutex<HashMap<String, HostState>>,
    host_released: Condvar,
}

enum AttemptOutcome {
    Done(Result<FetchOutcome, String>),
    Retry { reason: String, wait: Option<Duration> },
}

fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest)
}

fn retry_after(response: &HttpResponse) -> Option<Duration> {
    response.header("retry-after")?.trim().parse::<u64>().ok().map(Duration::from_secs)
}

impl<T: HttpTransport> HttpTileFetcher<T> {
    pub fn new(transport: T, limits: TransportLimits, policy: RetryPolicy) -> Self {
        Self { transport, limits, policy, host_limits: HashMap::new(), hosts: Mutex::new(HashMap::new()), host_released: Condvar::new() }
    }

    /// Requests in flight allowed for each listed host, instead of `max_per_host`.
    pub fn with_host_limits(mut self, limits: impl IntoIterator<Item = (String, usize)>) -> Self {
        self.host_limits = limits.into_iter().map(|(host, limit)| (host.to_ascii_lowercase(), limit.max(1))).collect();
        self
    }

    fn host_limit(&self, host: &str) -> usize {
        self.host_limits.get(&host.to_ascii_lowercase()).copied().unwrap_or(self.policy.max_per_host).max(1)
    }

    /// Takes a slot for `host`, or returns false when the deadline passes first.
    fn acquire(&self, host: &str, deadline: Instant) -> bool {
        let mut hosts = self.hosts.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let state = hosts.entry(host.to_owned()).or_default();
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            match state.cooldown_until {
                Some(until) if until > now => {
                    hosts = self.host_released.wait_timeout(hosts, until.min(deadline) - now).unwrap_or_else(PoisonError::into_inner).0;
                }
                _ if state.in_flight >= self.host_limit(host) => {
                    hosts = self.host_released.wait_timeout(hosts, deadline - now).unwrap_or_else(PoisonError::into_inner).0;
                }
                _ => {
                    state.in_flight += 1;
                    return true;
                }
            }
        }
    }

    fn release(&self, host: &str, cooldown: Option<Duration>) {
        let mut hosts = self.hosts.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(state) = hosts.get_mut(host) {
            state.in_flight = state.in_flight.saturating_sub(1);
            if let Some(cooldown) = cooldown {
                let until = Instant::now() + cooldown;
                state.cooldown_until = Some(state.cooldown_until.map_or(until, |current| current.max(until)));
            }
        }
        self.host_released.notify_all();
    }

    fn attempt(&self, url: &str, deadline: Instant) -> AttemptOutcome {
        let left = deadline.saturating_duration_since(Instant::now());
        let limits = TransportLimits { total_timeout: self.limits.total_timeout.min(left), ..self.limits };
        match self.transport.get(url, &limits) {
            Ok(response) => match response.status {
                200 => AttemptOutcome::Done(Ok(FetchOutcome::Found(response.body))),
                404 => AttemptOutcome::Done(Ok(FetchOutcome::Missing)),
                429 | 503 => {
                    let wait = retry_after(&response);
                    match wait {
                        Some(wait) if wait > self.policy.max_retry_after => {
                            AttemptOutcome::Done(Err(format!("HTTP {} with Retry-After {}s beyond limit", response.status, wait.as_secs())))
                        }
                        _ => AttemptOutcome::Retry { reason: format!("HTTP {}", response.status), wait: Some(wait.unwrap_or(self.policy.initial_backoff)) },
                    }
                }
                500..=599 | 408 => AttemptOutcome::Retry { reason: format!("HTTP {}", response.status), wait: None },
                status => AttemptOutcome::Done(Err(format!("HTTP {status}"))),
            },
            Err(error) if error.is_transient() => AttemptOutcome::Retry { reason: error.to_string(), wait: None },
            Err(error @ (TransportError::UnsupportedUrl(_) | TransportError::Protocol(_) | TransportError::BodyTooLarge { .. })) => {
                AttemptOutcome::Done(Err(error.to_string()))
            }
            Err(error) => AttemptOutcome::Done(Err(error.to_string())),
        }
    }
}

impl<T: HttpTransport> TileFetcher for HttpTileFetcher<T> {
    fn fetch(&self, url: &str) -> Result<FetchOutcome, ElevationError> {
        let host = host_of(url);
        let deadline = Instant::now() + self.policy.fetch_deadline;
        let mut backoff = self.policy.initial_backoff;
        let mut failures = Vec::new();
        for attempt in 1..=self.policy.max_attempts.max(1) {
            if !self.acquire(host, deadline) {
                failures.push(format!("fetch deadline of {} ms reached", self.policy.fetch_deadline.as_millis()));
                break;
            }
            let outcome = self.attempt(url, deadline);
            let cooldown = match &outcome {
                AttemptOutcome::Retry { wait: Some(wait), .. } => Some(*wait),
                _ => None,
            };
            self.release(host, cooldown);
            match outcome {
                AttemptOutcome::Done(result) => {
                    return result.map_err(|reason| {
                        failures.push(reason);
                        ElevationError::Fetch { url: url.to_owned(), reason: failures.join("; ") }
                    });
                }
                AttemptOutcome::Retry { reason, wait } => {
                    failures.push(format!("attempt {attempt}: {reason}"));
                    if attempt < self.policy.max_attempts && wait.is_none() {
                        let left = deadline.saturating_duration_since(Instant::now());
                        if left <= backoff {
                            failures.push(format!("fetch deadline of {} ms reached", self.policy.fetch_deadline.as_millis()));
                            break;
                        }
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(self.policy.max_backoff);
                    }
                }
            }
        }
        Err(ElevationError::Fetch { url: url.to_owned(), reason: failures.join("; ") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scripted {
        responses: Mutex<Vec<Result<HttpResponse, TransportError>>>,
        calls: AtomicUsize,
    }

    impl HttpTransport for Scripted {
        fn get(&self, _: &str, _: &TransportLimits) -> Result<HttpResponse, TransportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.responses.lock().unwrap().remove(0)
        }
    }

    fn response(status: u16, headers: &[(&str, &str)]) -> Result<HttpResponse, TransportError> {
        Ok(HttpResponse { status, headers: headers.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(), body: b"tile".to_vec() })
    }

    fn fetcher(responses: Vec<Result<HttpResponse, TransportError>>) -> HttpTileFetcher<Scripted> {
        let policy = RetryPolicy { initial_backoff: Duration::from_millis(1), max_backoff: Duration::from_millis(2), max_retry_after: Duration::from_secs(1), ..RetryPolicy::default() };
        HttpTileFetcher::new(Scripted { responses: Mutex::new(responses), calls: AtomicUsize::new(0) }, TransportLimits::default(), policy)
    }

    #[test]
    fn retries_transient_failures_then_succeeds() {
        let fetcher = fetcher(vec![Err(TransportError::Timeout), response(502, &[]), response(429, &[("retry-after", "0")]), response(200, &[])]);
        assert_eq!(fetcher.fetch("http://h/t").unwrap(), FetchOutcome::Found(b"tile".to_vec()));
        assert_eq!(fetcher.transport.calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn missing_and_client_errors_are_final() {
        assert_eq!(fetcher(vec![response(404, &[])]).fetch("http://h/t").unwrap(), FetchOutcome::Missing);
        let forbidden = fetcher(vec![response(403, &[]), response(200, &[])]);
        assert!(forbidden.fetch("http://h/t").is_err());
        assert_eq!(forbidden.transport.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn gives_up_after_max_attempts_and_on_long_retry_after() {
        let exhausted = fetcher((0..4).map(|_| response(500, &[])).collect());
        let error = exhausted.fetch("http://h/t").unwrap_err().to_string();
        assert!(error.contains("attempt 4"), "{error}");
        let throttled = fetcher(vec![response(429, &[("retry-after", "3600")])]);
        assert!(throttled.fetch("http://h/t").is_err());
    }

    #[test]
    fn rate_limit_cooldown_applies_to_the_host() {
        let fetcher = fetcher(vec![response(429, &[("retry-after", "1")]), response(200, &[])]);
        let started = Instant::now();
        fetcher.fetch("http://h/t").unwrap();
        assert!(started.elapsed() >= Duration::from_millis(990));
    }

    #[test]
    fn host_limits_follow_terraminusminus() {
        assert_eq!(parse_host_limit("8: https://cloud.daporkchop.net/"), Some(("cloud.daporkchop.net".into(), 8)));
        assert_eq!(parse_host_limit("1: http://gis-treecover.wri.org/"), Some(("gis-treecover.wri.org".into(), 1)));
        assert_eq!(parse_host_limit("3: https://Example.org:8443/x"), Some(("example.org:8443".into(), 3)));
        for bad in ["8 https://a/", "x: https://a/", "0: https://a/", ": https://a/", "8: a/", "8: https:///", "-1: https://a/"] {
            assert_eq!(parse_host_limit(bad), None, "{bad}");
        }
        assert!(DEFAULT_HOST_LIMITS.iter().all(|entry| parse_host_limit(entry).is_some()));
        let fetcher = HttpTileFetcher::new(crate::PlainHttpTransport::default(), TransportLimits::default(), RetryPolicy { max_per_host: 4, ..RetryPolicy::default() })
            .with_host_limits(DEFAULT_HOST_LIMITS.iter().filter_map(|entry| parse_host_limit(entry)));
        assert_eq!(fetcher.host_limit("overpass.kumi.systems"), 1);
        assert_eq!(fetcher.host_limit("CLOUD.daporkchop.net"), 8);
        assert_eq!(fetcher.host_limit("elsewhere.example"), 4);
    }
}
