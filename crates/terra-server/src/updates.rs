//! Update notifications, as upstream's `Updater` (PluginUpdater): the latest GitHub release is
//! looked up once at startup (and again after a day), and operators with `t+-.notify.update`
//! are told on join when it is newer than the running version.
//!
//! The mod has no TLS stack, so the HTTPS request goes through the system's `curl` or `wget`
//! when present, or through any `url` the configuration gives (a plain-HTTP mirror, or a local
//! file with `file://`). Every failure is silent apart from one log line: an update check never
//! stops the server or delays a tick, as it runs on its own thread.

use std::cmp::Ordering;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use terra_codec::parse_relaxed_json;

/// Upstream checks `BTE-Germany/TerraPlusMinus`; the port's releases live in its own repository.
pub const DEFAULT_REPOSITORY: &str = "MaxLananas/Terraplusminusrusted";
pub const RECHECK_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
const REQUEST_TIMEOUT_SECONDS: u32 = 10;
const MAX_RESPONSE_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSettings {
    pub enabled: bool,
    /// `owner/name` on GitHub.
    pub repository: String,
    /// Replaces `https://api.github.com/repos/<repository>/releases/latest`: an `http://` mirror
    /// fetched without TLS, or `file://` for a release document kept by other means.
    pub url: Option<String>,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { enabled: true, repository: DEFAULT_REPOSITORY.to_owned(), url: None }
    }
}

impl UpdateSettings {
    pub fn release_url(&self) -> String {
        self.url.clone().unwrap_or_else(|| format!("https://api.github.com/repos/{}/releases/latest", self.repository))
    }
}

/// `owner/name` with GitHub's allowed characters, so it is safe inside a URL and a process
/// argument.
pub fn valid_repository(repository: &str) -> bool {
    let mut parts = repository.split('/');
    let valid = |part: Option<&str>| part.is_some_and(|part| !part.is_empty() && part.len() <= 100 && part.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)));
    valid(parts.next()) && valid(parts.next()) && parts.next().is_none()
}

/// The `tag_name` of a GitHub release document.
pub fn latest_tag(document: &str) -> Option<String> {
    let value = parse_relaxed_json(document).ok()?;
    let tag = value.get("tag_name")?.as_str()?.trim();
    (!tag.is_empty()).then(|| tag.to_owned())
}

/// Compares release names the way version numbers read: an optional `v`, dot-separated numbers
/// compared numerically, and a pre-release suffix (`-beta`) lower than none. `None` when either
/// does not start with a number.
pub fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    fn split(version: &str) -> Option<(Vec<u64>, Option<&str>)> {
        let version = version.trim().trim_start_matches(['v', 'V']);
        let (core, suffix) = match version.find(['-', '+']) {
            Some(index) => (&version[..index], Some(&version[index + 1..])),
            None => (version, None),
        };
        let numbers: Vec<u64> = core.split('.').map(|part| part.parse().ok()).collect::<Option<_>>()?;
        let suffix = suffix.filter(|suffix| !suffix.is_empty() && version.as_bytes()[version.len() - suffix.len() - 1] == b'-');
        (!numbers.is_empty()).then_some((numbers, suffix))
    }
    let (left_numbers, left_suffix) = split(left)?;
    let (right_numbers, right_suffix) = split(right)?;
    let length = left_numbers.len().max(right_numbers.len());
    let padded = |numbers: &[u64], index: usize| numbers.get(index).copied().unwrap_or(0);
    for index in 0..length {
        match padded(&left_numbers, index).cmp(&padded(&right_numbers, index)) {
            Ordering::Equal => {}
            other => return Some(other),
        }
    }
    Some(match (left_suffix, right_suffix) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => left.cmp(right),
    })
}

/// Upstream's notification, `%plugin% %current_version% -> %latest_version%`, as text spans
/// with their colours (`<#ffe27a>`, `<#e0c01b>`).
pub fn notification_spans(plugin: &str, current: &str, latest: &str) -> Vec<(String, &'static str)> {
    const LIGHT: &str = "#ffe27a";
    const DARK: &str = "#e0c01b";
    vec![
        ("A new ".to_owned(), LIGHT),
        (plugin.to_owned(), DARK),
        (" update is now available! ".to_owned(), LIGHT),
        (current.to_owned(), DARK),
        (" -> ".to_owned(), LIGHT),
        (latest.to_owned(), DARK),
    ]
}

pub fn notification_text(plugin: &str, current: &str, latest: &str) -> String {
    notification_spans(plugin, current, latest).into_iter().map(|(text, _)| text).collect()
}

/// Fetches `url`: `file://` from disk, `http://` over the built-in plain transport, `https://`
/// through `curl` then `wget`.
pub fn fetch(url: &str, user_agent: &str) -> Result<String, String> {
    if let Some(path) = url.strip_prefix("file://") {
        let bytes = std::fs::read(Path::new(path)).map_err(|error| format!("{path}: {error}"))?;
        return limit(bytes);
    }
    if url.starts_with("http://") {
        return fetch_plain(url, user_agent);
    }
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("unsupported URL {url}"));
    }
    let timeout = REQUEST_TIMEOUT_SECONDS.to_string();
    let agent = format!("User-Agent: {user_agent}");
    let attempts: [(&str, Vec<&str>); 2] = [
        ("curl", vec!["--silent", "--show-error", "--fail", "--location", "--max-time", &timeout, "--max-filesize", "1048576", "--header", &agent, "--header", "Accept: application/vnd.github+json", "--", url]),
        ("wget", vec!["--quiet", "--timeout", &timeout, "--tries", "1", "--header", &agent, "--output-document", "-", "--", url]),
    ];
    let mut errors = Vec::new();
    for (program, arguments) in attempts {
        match Command::new(program).args(arguments).stdin(Stdio::null()).stderr(Stdio::piped()).output() {
            Ok(output) if output.status.success() => return limit(output.stdout),
            Ok(output) => errors.push(format!("{program}: {} {}", output.status, String::from_utf8_lossy(&output.stderr).trim())),
            Err(error) => errors.push(format!("{program}: {error}")),
        }
    }
    Err(errors.join("; "))
}

fn limit(bytes: Vec<u8>) -> Result<String, String> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!("the release document is larger than {MAX_RESPONSE_BYTES} bytes"));
    }
    String::from_utf8(bytes).map_err(|_| "the release document is not UTF-8".to_owned())
}

fn fetch_plain(url: &str, user_agent: &str) -> Result<String, String> {
    use terra_acquisition::{HttpTransport, PlainHttpTransport, TransportLimits};
    let transport = PlainHttpTransport { user_agent: user_agent.to_owned() };
    let limits = TransportLimits { total_timeout: Duration::from_secs(u64::from(REQUEST_TIMEOUT_SECONDS) * 2), max_body_bytes: MAX_RESPONSE_BYTES as u64, ..TransportLimits::default() };
    let response = transport.get(url, &limits).map_err(|error| error.to_string())?;
    if response.status != 200 {
        return Err(format!("HTTP {}", response.status));
    }
    limit(response.body)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    NotChecked,
    UpToDate { latest: String },
    Available { latest: String },
    Failed(String),
}

/// The result of the last check, shared between the checking thread and the server tick.
#[derive(Debug)]
pub struct UpdateChecker {
    settings: UpdateSettings,
    current: String,
    state: Mutex<(UpdateStatus, Option<Instant>, bool)>,
}

impl UpdateChecker {
    pub fn new(settings: UpdateSettings, current: &str) -> Self {
        Self { settings, current: current.to_owned(), state: Mutex::new((UpdateStatus::NotChecked, None, false)) }
    }

    pub fn current(&self) -> &str {
        &self.current
    }

    pub fn status(&self) -> UpdateStatus {
        self.state.lock().map(|state| state.0.clone()).unwrap_or(UpdateStatus::NotChecked)
    }

    /// The newer release to announce, if any.
    pub fn available(&self) -> Option<String> {
        match self.status() {
            UpdateStatus::Available { latest } => Some(latest),
            _ => None,
        }
    }

    /// Whether a check should start now: enabled, none running, and none in the last day.
    /// Marks one as running when it returns true.
    pub fn claim(&self, now: Instant) -> bool {
        let Ok(mut state) = self.state.lock() else { return false };
        let due = state.1.is_none_or(|at| now.saturating_duration_since(at) >= RECHECK_AFTER);
        if !self.settings.enabled || state.2 || !due {
            return false;
        }
        state.1 = Some(now);
        state.2 = true;
        true
    }

    /// Records the outcome of a claimed check from the fetched document.
    pub fn record(&self, fetched: Result<String, String>) -> UpdateStatus {
        let status = match fetched.and_then(|document| latest_tag(&document).ok_or_else(|| "no tag_name in the release document".to_owned())) {
            Ok(latest) => match compare_versions(&latest, &self.current) {
                Some(Ordering::Greater) => UpdateStatus::Available { latest },
                Some(_) => UpdateStatus::UpToDate { latest },
                None => UpdateStatus::Failed(format!("cannot compare release `{latest}` with version {}", self.current)),
            },
            Err(reason) => UpdateStatus::Failed(reason),
        };
        if let Ok(mut state) = self.state.lock() {
            state.0 = status.clone();
            state.2 = false;
        }
        status
    }

    /// Runs one claimed check on a background thread; `report` hears the outcome there.
    pub fn spawn_check(self: &std::sync::Arc<Self>, user_agent: String, report: impl FnOnce(&UpdateStatus) + Send + 'static) -> bool {
        let checker = std::sync::Arc::clone(self);
        let url = self.settings.release_url();
        std::thread::Builder::new()
            .name("terraplusminus-update-check".into())
            .spawn(move || {
                let status = checker.record(fetch(&url, &user_agent));
                report(&status);
            })
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(compare_versions("v1.10.0", "1.9.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("1.7", "v1.7.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("0.1.0", "0.2.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("1.0.0-beta", "1.0.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("1.0.0+build5", "1.0.0"), Some(Ordering::Equal), "build metadata is ignored");
        assert_eq!(compare_versions("1.0.0-rc.2", "1.0.0-rc.1"), Some(Ordering::Greater));
        assert_eq!(compare_versions("latest", "1.0"), None);
        assert_eq!(compare_versions("1..0", "1.0"), None);
    }

    #[test]
    fn release_documents_and_repositories() {
        assert_eq!(latest_tag(r#"{"tag_name": "v1.7.1", "name": "x"}"#).as_deref(), Some("v1.7.1"));
        assert_eq!(latest_tag(r#"{"message": "Not Found"}"#), None);
        assert_eq!(latest_tag("<html>"), None);
        assert!(valid_repository("BTE-Germany/TerraPlusMinus"));
        assert!(valid_repository(DEFAULT_REPOSITORY));
        for bad in ["", "a", "a/b/c", "a/b c", "a/$(x)", "/b"] {
            assert!(!valid_repository(bad), "{bad}");
        }
        let settings = UpdateSettings::default();
        assert_eq!(settings.release_url(), "https://api.github.com/repos/MaxLananas/Terraplusminusrusted/releases/latest");
    }

    #[test]
    fn notification_matches_upstream() {
        assert_eq!(notification_text("Terraplusminus", "0.1.0", "v0.2.0"), "A new Terraplusminus update is now available! 0.1.0 -> v0.2.0");
    }

    #[test]
    fn checks_are_claimed_once_a_day_and_recorded() {
        let checker = UpdateChecker::new(UpdateSettings::default(), "0.1.0");
        let start = Instant::now();
        assert!(checker.claim(start));
        assert!(!checker.claim(start), "already running");
        assert_eq!(checker.record(Ok(r#"{"tag_name": "v0.2.0"}"#.into())), UpdateStatus::Available { latest: "v0.2.0".into() });
        assert_eq!(checker.available().as_deref(), Some("v0.2.0"));
        assert!(!checker.claim(start + Duration::from_secs(60)), "checked recently");
        assert!(checker.claim(start + RECHECK_AFTER));
        assert_eq!(checker.record(Ok(r#"{"tag_name": "0.1.0"}"#.into())), UpdateStatus::UpToDate { latest: "0.1.0".into() });
        assert_eq!(checker.available(), None);
        assert!(checker.claim(start + RECHECK_AFTER * 2));
        assert!(matches!(checker.record(Err("offline".into())), UpdateStatus::Failed(_)));
        let disabled = UpdateChecker::new(UpdateSettings { enabled: false, ..UpdateSettings::default() }, "0.1.0");
        assert!(!disabled.claim(start));
    }

    #[test]
    fn local_documents_are_read() {
        let directory = std::env::temp_dir().join(format!("tpm-updates-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("latest.json");
        std::fs::write(&path, r#"{"tag_name": "v9.0.0"}"#).unwrap();
        let document = fetch(&format!("file://{}", path.display()), "test").unwrap();
        assert_eq!(latest_tag(&document).as_deref(), Some("v9.0.0"));
        assert!(fetch("ftp://example.org/x", "test").is_err());
        assert!(fetch("https://example.org/a b", "test").is_err());
        let _ = std::fs::remove_dir_all(&directory);
    }
}
