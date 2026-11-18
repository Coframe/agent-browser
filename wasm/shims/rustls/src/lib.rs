//! The subset of the rustls API that agent-browser uses to describe trust
//! roots, for wasm32 hosts.
//!
//! The JS host terminates TLS on this platform, so nothing here verifies a
//! certificate. The types record what the caller selected so the code that
//! builds a trust configuration compiles and runs.

pub use rustls_pki_types as pki_types;

use pki_types::{CertificateDer, TrustAnchor};

#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug, Default)]
pub struct RootCertStore {
    pub roots: Vec<TrustAnchor<'static>>,
    certificates: Vec<CertificateDer<'static>>,
}

impl RootCertStore {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn add(&mut self, der: CertificateDer<'_>) -> Result<(), Error> {
        if der.is_empty() {
            return Err(Error("empty certificate".to_string()));
        }
        self.certificates.push(der.into_owned());
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        self.roots.len() + self.certificates.len()
    }
}

impl Extend<TrustAnchor<'static>> for RootCertStore {
    fn extend<T: IntoIterator<Item = TrustAnchor<'static>>>(&mut self, iter: T) {
        self.roots.extend(iter)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ClientConfig {
    pub root_store: RootCertStore,
}

impl ClientConfig {
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder(RootCertStore::empty())
    }
}

#[derive(Debug)]
pub struct ConfigBuilder(RootCertStore);

impl ConfigBuilder {
    pub fn with_root_certificates(self, roots: impl Into<RootCertStore>) -> Self {
        Self(roots.into())
    }

    pub fn with_no_client_auth(self) -> ClientConfig {
        ClientConfig { root_store: self.0 }
    }
}
