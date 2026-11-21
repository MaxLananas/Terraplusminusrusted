//! Chooses how each tile URL is fetched: a configured local mirror, plain HTTP, or an explicit
//! refusal. `https://` is only fetched over plain HTTP for hosts the operator listed.

use crate::http::HttpTransport;
use crate::policy::HttpTileFetcher;
use std::path::PathBuf;
use terra_elevation::{ElevationError, FetchOutcome, MirrorFetcher, TileFetcher};

pub struct RoutingFetcher<T> {
    mirror_prefixes: Vec<String>,
    mirrors: MirrorFetcher,
    http: HttpTileFetcher<T>,
    plain_http_hosts: Vec<String>,
}

impl<T: HttpTransport> RoutingFetcher<T> {
    pub fn new(mirrors: &[(String, PathBuf)], http: HttpTileFetcher<T>, plain_http_hosts: Vec<String>) -> Self {
        let mut fetcher = MirrorFetcher::new();
        for (prefix, directory) in mirrors {
            fetcher = fetcher.with_mapping(prefix.clone(), directory.clone());
        }
        Self { mirror_prefixes: mirrors.iter().map(|(prefix, _)| prefix.clone()).collect(), mirrors: fetcher, http, plain_http_hosts }
    }

    /// The URL actually requested over the network, or why none is.
    pub fn network_url(&self, url: &str) -> Result<String, String> {
        if url.starts_with("http://") {
            return Ok(url.to_owned());
        }
        let Some(rest) = url.strip_prefix("https://") else { return Err(format!("unsupported URL scheme in {url}")) };
        let host = rest.split('/').next().unwrap_or("").to_ascii_lowercase();
        if self.plain_http_hosts.contains(&host) {
            Ok(format!("http://{rest}"))
        } else {
            Err(format!("{url} needs TLS, which this build lacks; list `{host}` in network.plain_http_hosts or mirror it"))
        }
    }
}

impl<T: HttpTransport> TileFetcher for RoutingFetcher<T> {
    fn fetch(&self, url: &str) -> Result<FetchOutcome, ElevationError> {
        if self.mirror_prefixes.iter().any(|prefix| url.starts_with(prefix.as_str())) {
            return self.mirrors.fetch(url);
        }
        let network_url = self.network_url(url).map_err(|reason| ElevationError::Fetch { url: url.to_owned(), reason })?;
        self.http.fetch(&network_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlainHttpTransport, RetryPolicy, TransportLimits};

    #[test]
    fn routes_mirrors_rewrites_listed_hosts_and_refuses_others() {
        let http = HttpTileFetcher::new(PlainHttpTransport::default(), TransportLimits::default(), RetryPolicy::default());
        let router = RoutingFetcher::new(&[("https://mirror.example/".into(), std::env::temp_dir())], http, vec!["s3.amazonaws.com".into()]);
        assert_eq!(router.network_url("https://s3.amazonaws.com/a/1.png").unwrap(), "http://s3.amazonaws.com/a/1.png");
        assert!(router.network_url("https://other.example/x").is_err());
        assert!(router.network_url("ftp://x/y").is_err());
        assert_eq!(router.fetch("https://mirror.example/definitely-absent.png").unwrap(), FetchOutcome::Missing);
        assert!(router.fetch("https://other.example/x").is_err());
    }
}
