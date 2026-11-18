//! rustls-native-certs for wasm32 hosts. There is no system trust store to
//! read, so [`load_native_certs`] reports that and returns no certificates.

use rustls_pki_types::CertificateDer;

#[derive(Debug)]
pub struct Error(&'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Default)]
pub struct CertificateResult {
    pub certs: Vec<CertificateDer<'static>>,
    pub errors: Vec<Error>,
}

pub fn load_native_certs() -> CertificateResult {
    CertificateResult {
        certs: Vec::new(),
        errors: vec![Error("no system trust store on this platform")],
    }
}
