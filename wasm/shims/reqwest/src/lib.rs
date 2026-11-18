//! reqwest for wasm32 hosts.
//!
//! Requests go through reqwest's own wasm backend, which calls the host's
//! `fetch`. That backend has no redirect policy, proxy, DNS resolver, TLS or
//! connect-timeout options, because `fetch` decides those. This crate accepts
//! the options agent-browser sets so upstream code compiles unchanged, and
//! ignores the ones `fetch` does not expose.

use std::time::Duration;

pub use real_reqwest::*;

pub mod dns;
pub mod redirect;

/// Proxies are the host's concern on this platform. Constructors validate
/// the URL and the value is otherwise unused.
#[derive(Clone, Debug)]
pub struct Proxy(());

impl Proxy {
    pub fn http<U: AsRef<str>>(url: U) -> Result<Proxy> {
        Self::parse(url.as_ref())
    }

    pub fn https<U: AsRef<str>>(url: U) -> Result<Proxy> {
        Self::parse(url.as_ref())
    }

    pub fn all<U: AsRef<str>>(url: U) -> Result<Proxy> {
        Self::parse(url.as_ref())
    }

    fn parse(url: &str) -> Result<Proxy> {
        // `get` reports an unparsable URL as a builder error, which is the
        // error kind reqwest's own Proxy constructors return.
        real_reqwest::Client::new().get(url).build().map(|_| Proxy(()))
    }

    pub fn no_proxy(self, _no_proxy: Option<NoProxy>) -> Proxy {
        self
    }

    pub fn basic_auth(self, _username: &str, _password: &str) -> Proxy {
        self
    }
}

#[derive(Clone, Debug)]
pub struct NoProxy(());

impl NoProxy {
    pub fn from_env() -> Option<NoProxy> {
        None
    }

    pub fn from_string(no_proxy: &str) -> Option<NoProxy> {
        (!no_proxy.trim().is_empty()).then_some(NoProxy(()))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Client {
    inner: real_reqwest::Client,
    timeout: Option<Duration>,
}

impl Client {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }

    pub fn request<U: IntoUrl>(&self, method: Method, url: U) -> RequestBuilder {
        let request = self.inner.request(method, url);
        match self.timeout {
            Some(timeout) => request.timeout(timeout),
            None => request,
        }
    }

    pub fn get<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    pub fn post<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::POST, url)
    }

    pub fn put<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::PUT, url)
    }

    pub fn patch<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::PATCH, url)
    }

    pub fn delete<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::DELETE, url)
    }

    pub fn head<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::HEAD, url)
    }

    pub async fn execute(&self, request: Request) -> Result<Response> {
        self.inner.execute(request).await
    }
}

#[derive(Debug, Default)]
pub struct ClientBuilder {
    inner: real_reqwest::ClientBuilder,
    timeout: Option<Duration>,
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn build(self) -> Result<Client> {
        Ok(Client {
            inner: self.inner.build()?,
            timeout: self.timeout,
        })
    }

    pub fn user_agent<V>(mut self, value: V) -> Self
    where
        V: TryInto<header::HeaderValue>,
        V::Error: Into<http::Error>,
    {
        self.inner = self.inner.user_agent(value);
        self
    }

    pub fn default_headers(mut self, headers: header::HeaderMap) -> Self {
        self.inner = self.inner.default_headers(headers);
        self
    }

    /// Applied to each request the client sends.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    // `fetch` does not expose the options below.

    pub fn connect_timeout(self, _timeout: Duration) -> Self {
        self
    }

    pub fn redirect(self, _policy: redirect::Policy) -> Self {
        self
    }

    pub fn https_only(self, _enabled: bool) -> Self {
        self
    }

    pub fn no_proxy(self) -> Self {
        self
    }

    pub fn proxy(self, _proxy: Proxy) -> Self {
        self
    }

    pub fn dns_resolver<R: dns::Resolve + 'static>(self, _resolver: std::sync::Arc<R>) -> Self {
        self
    }

    pub fn use_preconfigured_tls(self, _tls: rustls::ClientConfig) -> Self {
        self
    }

    pub fn danger_accept_invalid_certs(self, _accept: bool) -> Self {
        self
    }
}
