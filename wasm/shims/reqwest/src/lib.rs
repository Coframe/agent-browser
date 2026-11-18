//! The subset of the reqwest API that agent-browser uses, for wasm32-wasip1.
//!
//! Requests go to the JS host's `fetch` through `agent-browser-host`, and
//! responses arrive fully buffered. `fetch` decides redirects, proxies, DNS
//! and TLS, so the builder options for those are accepted and ignored.

use std::fmt;
use std::time::Duration;

use agent_browser_host::{FetchError, FetchRequest};
use base64::Engine;
use bytes::Bytes;
use futures_util::Stream;

pub use http::{header, Method, StatusCode, Version};
pub use url::Url;

pub mod dns;
pub mod redirect;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Builder,
    Request,
    Timeout,
    Status,
    Decode,
}

pub struct Error {
    kind: Kind,
    message: String,
    url: Option<Url>,
    status: Option<StatusCode>,
}

impl Error {
    fn new(kind: Kind, message: impl fmt::Display) -> Self {
        Error {
            kind,
            message: message.to_string(),
            url: None,
            status: None,
        }
    }

    fn builder(error: impl fmt::Display) -> Self {
        Self::new(Kind::Builder, error)
    }

    pub fn is_builder(&self) -> bool {
        self.kind == Kind::Builder
    }

    pub fn is_request(&self) -> bool {
        self.kind == Kind::Request
    }

    pub fn is_timeout(&self) -> bool {
        self.kind == Kind::Timeout
    }

    /// `fetch` reports every network failure the same way, so any request
    /// failure that is not a timeout counts as a connect failure.
    pub fn is_connect(&self) -> bool {
        self.kind == Kind::Request
    }

    pub fn is_status(&self) -> bool {
        self.kind == Kind::Status
    }

    pub fn is_decode(&self) -> bool {
        self.kind == Kind::Decode
    }

    pub fn is_body(&self) -> bool {
        false
    }

    pub fn is_redirect(&self) -> bool {
        false
    }

    pub fn status(&self) -> Option<StatusCode> {
        self.status
    }

    pub fn url(&self) -> Option<&Url> {
        self.url.as_ref()
    }

    pub fn without_url(mut self) -> Self {
        self.url = None;
        self
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("reqwest::Error")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .field("url", &self.url.as_ref().map(Url::as_str))
            .finish()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            Kind::Builder => "builder error",
            Kind::Request => "error sending request",
            Kind::Timeout => "operation timed out",
            Kind::Status => "HTTP status error",
            Kind::Decode => "error decoding response body",
        };
        f.write_str(what)?;
        if let Some(url) = &self.url {
            write!(f, " for url ({url})")?;
        }
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

pub trait IntoUrl {
    fn into_url(self) -> Result<Url>;
}

impl IntoUrl for Url {
    fn into_url(self) -> Result<Url> {
        Ok(self)
    }
}

impl IntoUrl for &Url {
    fn into_url(self) -> Result<Url> {
        Ok(self.clone())
    }
}

impl IntoUrl for &str {
    fn into_url(self) -> Result<Url> {
        Url::parse(self).map_err(Error::builder)
    }
}

impl IntoUrl for String {
    fn into_url(self) -> Result<Url> {
        self.as_str().into_url()
    }
}

impl IntoUrl for &String {
    fn into_url(self) -> Result<Url> {
        self.as_str().into_url()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Body(Vec<u8>);

impl Body {
    pub fn as_bytes(&self) -> Option<&[u8]> {
        Some(&self.0)
    }
}

impl From<Vec<u8>> for Body {
    fn from(body: Vec<u8>) -> Self {
        Body(body)
    }
}

impl From<String> for Body {
    fn from(body: String) -> Self {
        Body(body.into_bytes())
    }
}

impl From<&'static str> for Body {
    fn from(body: &'static str) -> Self {
        Body(body.as_bytes().to_vec())
    }
}

impl From<&'static [u8]> for Body {
    fn from(body: &'static [u8]) -> Self {
        Body(body.to_vec())
    }
}

impl From<Bytes> for Body {
    fn from(body: Bytes) -> Self {
        Body(body.to_vec())
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    method: Method,
    url: Url,
    headers: header::HeaderMap,
    body: Option<Body>,
    timeout: Option<Duration>,
}

impl Request {
    pub fn new(method: Method, url: Url) -> Self {
        Request {
            method,
            url,
            headers: header::HeaderMap::new(),
            body: None,
            timeout: None,
        }
    }

    pub fn method(&self) -> &Method {
        &self.method
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn headers(&self) -> &header::HeaderMap {
        &self.headers
    }

    pub fn headers_mut(&mut self) -> &mut header::HeaderMap {
        &mut self.headers
    }

    pub fn body(&self) -> Option<&Body> {
        self.body.as_ref()
    }

    pub fn timeout(&self) -> Option<&Duration> {
        self.timeout.as_ref()
    }
}

async fn execute(request: Request) -> Result<Response> {
    let url = request.url.clone();
    let headers = request
        .headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let sent = agent_browser_host::fetch(FetchRequest {
        method: request.method.as_str().to_string(),
        url: url.to_string(),
        headers,
        body: request.body.map(|body| body.0),
        timeout_ms: request
            .timeout
            .map(|timeout| timeout.as_secs_f64() * 1000.0),
    })
    .await;
    let response = sent.map_err(|FetchError { message, timeout }| {
        let kind = if timeout {
            Kind::Timeout
        } else {
            Kind::Request
        };
        let mut error = Error::new(kind, message);
        error.url = Some(url.clone());
        error
    })?;

    let mut headers = header::HeaderMap::new();
    for (name, value) in &response.headers {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            header::HeaderValue::from_str(value),
        ) {
            headers.append(name, value);
        }
    }
    Ok(Response {
        status: StatusCode::from_u16(response.status).map_err(|e| Error::new(Kind::Request, e))?,
        headers,
        url: Url::parse(&response.url).unwrap_or(url),
        body: Bytes::from(response.body),
    })
}

pub async fn get<T: IntoUrl>(url: T) -> Result<Response> {
    Client::new().get(url).send().await
}

#[derive(Debug)]
#[must_use = "RequestBuilder does nothing until you send it"]
pub struct RequestBuilder(Result<Request>);

impl RequestBuilder {
    fn map(self, f: impl FnOnce(&mut Request) -> Result<()>) -> Self {
        RequestBuilder(self.0.and_then(|mut request| {
            f(&mut request)?;
            Ok(request)
        }))
    }

    pub fn header<K, V>(self, key: K, value: V) -> Self
    where
        header::HeaderName: TryFrom<K>,
        <header::HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        header::HeaderValue: TryFrom<V>,
        <header::HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        self.map(|request| {
            let name = header::HeaderName::try_from(key).map_err(|e| Error::builder(e.into()))?;
            let value =
                header::HeaderValue::try_from(value).map_err(|e| Error::builder(e.into()))?;
            request.headers.append(name, value);
            Ok(())
        })
    }

    pub fn headers(self, headers: header::HeaderMap) -> Self {
        self.map(|request| {
            for (name, value) in &headers {
                request.headers.insert(name, value.clone());
            }
            Ok(())
        })
    }

    pub fn basic_auth<U, P>(self, username: U, password: Option<P>) -> Self
    where
        U: fmt::Display,
        P: fmt::Display,
    {
        let credentials = match password {
            Some(password) => format!("{username}:{password}"),
            None => format!("{username}:"),
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        self.header(header::AUTHORIZATION, format!("Basic {encoded}"))
    }

    pub fn bearer_auth<T: fmt::Display>(self, token: T) -> Self {
        self.header(header::AUTHORIZATION, format!("Bearer {token}"))
    }

    pub fn body<T: Into<Body>>(self, body: T) -> Self {
        self.map(|request| {
            request.body = Some(body.into());
            Ok(())
        })
    }

    pub fn timeout(self, timeout: Duration) -> Self {
        self.map(|request| {
            request.timeout = Some(timeout);
            Ok(())
        })
    }

    pub fn query<T: serde::Serialize + ?Sized>(self, query: &T) -> Self {
        self.map(|request| {
            let encoded = serde_urlencoded::to_string(query).map_err(Error::builder)?;
            if !encoded.is_empty() {
                let joined = match request.url.query() {
                    Some(existing) if !existing.is_empty() => format!("{existing}&{encoded}"),
                    _ => encoded,
                };
                request.url.set_query(Some(&joined));
            }
            Ok(())
        })
    }

    pub fn form<T: serde::Serialize + ?Sized>(self, form: &T) -> Self {
        self.map(|request| {
            let encoded = serde_urlencoded::to_string(form).map_err(Error::builder)?;
            request.headers.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/x-www-form-urlencoded"),
            );
            request.body = Some(Body(encoded.into_bytes()));
            Ok(())
        })
    }

    pub fn json<T: serde::Serialize + ?Sized>(self, json: &T) -> Self {
        self.map(|request| {
            let encoded = serde_json::to_vec(json).map_err(Error::builder)?;
            if !request.headers.contains_key(header::CONTENT_TYPE) {
                request.headers.insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/json"),
                );
            }
            request.body = Some(Body(encoded));
            Ok(())
        })
    }

    pub fn build(self) -> Result<Request> {
        self.0
    }

    pub async fn send(self) -> Result<Response> {
        execute(self.0?).await
    }
}

pub struct Response {
    status: StatusCode,
    headers: header::HeaderMap,
    url: Url,
    body: Bytes,
}

impl Response {
    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn headers(&self) -> &header::HeaderMap {
        &self.headers
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn content_length(&self) -> Option<u64> {
        self.headers
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
    }

    pub async fn bytes(self) -> Result<Bytes> {
        Ok(self.body)
    }

    pub async fn text(self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.body).into_owned())
    }

    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|e| {
            let mut error = Error::new(Kind::Decode, e);
            error.url = Some(self.url.clone());
            error
        })
    }

    /// Returns what is left of the buffered body, then `None`.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        if self.body.is_empty() {
            return Ok(None);
        }
        Ok(Some(std::mem::take(&mut self.body)))
    }

    pub fn bytes_stream(self) -> impl Stream<Item = Result<Bytes>> + Send {
        futures_util::stream::iter((!self.body.is_empty()).then_some(Ok(self.body)))
    }

    pub fn error_for_status(self) -> Result<Self> {
        match self.error_for_status_ref() {
            Ok(_) => Ok(self),
            Err(error) => Err(error),
        }
    }

    pub fn error_for_status_ref(&self) -> Result<&Self> {
        if self.status.is_client_error() || self.status.is_server_error() {
            let mut error = Error::new(Kind::Status, self.status);
            error.url = Some(self.url.clone());
            error.status = Some(self.status);
            return Err(error);
        }
        Ok(self)
    }
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("url", &self.url.as_str())
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish()
    }
}

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
        Url::parse(url).map(|_| Proxy(())).map_err(Error::builder)
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
    headers: header::HeaderMap,
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
        RequestBuilder(url.into_url().map(|url| {
            let mut request = Request::new(method, url);
            request.headers = self.headers.clone();
            request.timeout = self.timeout;
            request
        }))
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
        execute(request).await
    }
}

#[derive(Debug, Default)]
pub struct ClientBuilder {
    headers: header::HeaderMap,
    timeout: Option<Duration>,
    error: Option<Error>,
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn build(self) -> Result<Client> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(Client {
                headers: self.headers,
                timeout: self.timeout,
            }),
        }
    }

    pub fn user_agent<V>(mut self, value: V) -> Self
    where
        V: TryInto<header::HeaderValue>,
        V::Error: Into<http::Error>,
    {
        match value.try_into() {
            Ok(value) => {
                self.headers.insert(header::USER_AGENT, value);
            }
            Err(error) => self.error = Some(Error::builder(error.into())),
        }
        self
    }

    pub fn default_headers(mut self, headers: header::HeaderMap) -> Self {
        self.headers.extend(headers);
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
