//! Endpoint validation.
//!
//! The SDK accepts only an origin root: no path prefix, query, fragment or
//! userinfo. HTTPS is required except for a literal loopback address, which is
//! what local development and the test suite use.

use crate::Error;
use std::{fmt, net::IpAddr, str::FromStr};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    base: Url,
}

impl Endpoint {
    pub fn parse(raw: &str) -> Result<Self, Error> {
        let url = Url::parse(raw.trim())
            .map_err(|error| Error::Endpoint(format!("not a valid URL: {error}")))?;
        Self::from_url(url)
    }

    pub fn from_url(url: Url) -> Result<Self, Error> {
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Error::Endpoint("must not carry userinfo".into()));
        }
        if url.query().is_some() {
            return Err(Error::Endpoint("must not carry a query".into()));
        }
        if url.fragment().is_some() {
            return Err(Error::Endpoint("must not carry a fragment".into()));
        }
        if url.path() != "/" {
            return Err(Error::Endpoint(
                "must be an origin root without a path prefix".into(),
            ));
        }
        if url.port() == Some(0) {
            return Err(Error::Endpoint("port must not be zero".into()));
        }
        let host = url
            .host_str()
            .ok_or_else(|| Error::Endpoint("must contain a host".into()))?
            .to_string();
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Error::Endpoint(format!(
                "scheme {:?} is not supported",
                url.scheme()
            )));
        }
        if url.scheme() == "http" && !is_loopback_literal(&host) {
            return Err(Error::Endpoint(
                "must use HTTPS (cleartext HTTP is allowed only for a literal loopback address)"
                    .into(),
            ));
        }
        Ok(Self { base: url })
    }

    pub fn as_str(&self) -> &str {
        self.base.as_str()
    }

    pub fn url(&self) -> &Url {
        &self.base
    }

    /// `(scheme, host, port)`. A resource URL is used only when all three agree
    /// with the configured endpoint.
    pub fn origin(&self) -> (String, String, Option<u16>) {
        (
            self.base.scheme().to_string(),
            self.base.host_str().unwrap_or_default().to_string(),
            self.base.port(),
        )
    }

    pub(crate) fn join(&self, segments: &[&str]) -> Result<Url, Error> {
        let mut url = self.base.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| Error::Endpoint("cannot be extended with path segments".into()))?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    pub(crate) fn check_origin(&self, url: &Url) -> Result<(), Error> {
        if url.scheme() != self.base.scheme()
            || url.host_str() != self.base.host_str()
            || url.port_or_known_default() != self.base.port_or_known_default()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::Protocol(
                "resource URL points at a different origin; refusing to send credentials".into(),
            ));
        }
        Ok(())
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Endpoint {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

fn is_loopback_literal(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    let Ok(address) = host.parse::<IpAddr>() else {
        return false;
    };
    address.is_loopback()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_origin_roots_and_loopback_cleartext() {
        assert!(Endpoint::parse("https://sync.example.com").is_ok());
        assert!(Endpoint::parse("https://sync.example.com:8443/").is_ok());
        assert!(Endpoint::parse("http://127.0.0.1:8790").is_ok());
        assert!(Endpoint::parse("http://[::1]:8790").is_ok());
    }

    #[test]
    fn rejects_prefixes_credentials_and_public_cleartext() {
        for raw in [
            "https://sync.example.com/api",
            "https://sync.example.com/?a=1",
            "https://sync.example.com/#f",
            "https://user:secret@sync.example.com",
            "http://sync.example.com",
            "ftp://sync.example.com",
            "http://localhost:8790",
            "https://sync.example.com:0",
        ] {
            assert!(Endpoint::parse(raw).is_err(), "{raw}");
        }
    }
}
