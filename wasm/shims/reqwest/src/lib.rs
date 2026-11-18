//! reqwest for wasm32 hosts.
//!
//! Requests go through reqwest's own wasm backend, which calls the host's
//! `fetch`. That backend has no redirect policy, proxy, DNS resolver, TLS or
//! connect-timeout options, because `fetch` decides those. This crate accepts
//! the options agent-browser sets so upstream code compiles unchanged, and
//! ignores the ones `fetch` does not expose.

//!
//! `fetch` values belong to the JS thread, which makes reqwest's wasm
//! futures `!Send`. Upstream awaits them inside `Send` futures, so requests
//! and responses are held in [`SendWrapper`]: sound here because a wasm32
//! host runs this code on one thread, and checked at run time.

use std::fmt;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use send_wrapper::SendWrapper;

pub use real_reqwest::{header, Body, IntoUrl, Method, Request, StatusCode, Url, Version};

pub mod dns;
pub mod redirect;

pub type Result<T> = std::result::Result<T, Error>;

pub struct Error(real_reqwest::Error);

impl Error {
    pub fn is_timeout(&self) -> bool {
        self.0.is_timeout()
    }

    /// `fetch` reports every network failure the same way, so any request
    /// failure that is not a timeout counts as a connect failure.
    pub fn is_connect(&self) -> bool {
        self.0.is_request() && !self.0.is_timeout()
    }

    pub fn is_request(&self) -> bool {
        self.0.is_request()
    }

    pub fn is_builder(&self) -> bool {
        self.0.is_builder()
    }

    pub fn is_redirect(&self) -> bool {
        self.0.is_redirect()
    }

    pub fn is_status(&self) -> bool {
        self.0.is_status()
    }

    pub fn is_body(&self) -> bool {
        self.0.is_body()
    }

    pub fn is_decode(&self) -> bool {
        self.0.is_decode()
    }

    pub fn status(&self) -> Option<StatusCode> {
        self.0.status()
    }

    pub fn url(&self) -> Option<&Url> {
        self.0.url()
    }

    pub fn without_url(self) -> Self {
        Error(self.0.without_url())
    }
}

impl From<real_reqwest::Error> for Error {
    fn from(error: real_reqwest::Error) -> Self {
        Error(error)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

pub async fn get<T: IntoUrl>(url: T) -> Result<Response> {
    Client::new().get(url).send().await
}

#[must_use = "RequestBuilder does nothing until you send it"]
pub struct RequestBuilder(SendWrapper<real_reqwest::RequestBuilder>);

impl RequestBuilder {
    fn map(
        self,
        f: impl FnOnce(real_reqwest::RequestBuilder) -> real_reqwest::RequestBuilder,
    ) -> Self {
        RequestBuilder(SendWrapper::new(f(self.0.take())))
    }

    pub fn header<K, V>(self, key: K, value: V) -> Self
    where
        header::HeaderName: TryFrom<K>,
        <header::HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        header::HeaderValue: TryFrom<V>,
        <header::HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        self.map(|request| request.header(key, value))
    }

    pub fn headers(self, headers: header::HeaderMap) -> Self {
        self.map(|request| request.headers(headers))
    }

    pub fn basic_auth<U, P>(self, username: U, password: Option<P>) -> Self
    where
        U: fmt::Display,
        P: fmt::Display,
    {
        self.map(|request| request.basic_auth(username, password))
    }

    pub fn bearer_auth<T: fmt::Display>(self, token: T) -> Self {
        self.map(|request| request.bearer_auth(token))
    }

    pub fn body<T: Into<Body>>(self, body: T) -> Self {
        self.map(|request| request.body(body))
    }

    pub fn timeout(self, timeout: Duration) -> Self {
        self.map(|request| request.timeout(timeout))
    }

    pub fn query<T: serde::Serialize + ?Sized>(self, query: &T) -> Self {
        self.map(|request| request.query(query))
    }

    pub fn form<T: serde::Serialize + ?Sized>(self, form: &T) -> Self {
        self.map(|request| request.form(form))
    }

    pub fn json<T: serde::Serialize + ?Sized>(self, json: &T) -> Self {
        self.map(|request| request.json(json))
    }

    pub fn build(self) -> Result<Request> {
        Ok(self.0.take().build()?)
    }

    pub async fn send(self) -> Result<Response> {
        SendWrapper::new(async move { Ok(Response::new(self.0.take().send().await?)) }).await
    }
}

impl fmt::Debug for RequestBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RequestBuilder")
    }
}

type ByteStream = Pin<Box<dyn Stream<Item = real_reqwest::Result<Bytes>>>>;

enum ResponseBody {
    Unread(real_reqwest::Response),
    Streaming(ByteStream),
}

pub struct Response {
    status: StatusCode,
    headers: header::HeaderMap,
    url: Url,
    content_length: Option<u64>,
    body: SendWrapper<ResponseBody>,
}

impl Response {
    fn new(response: real_reqwest::Response) -> Self {
        Response {
            status: response.status(),
            headers: response.headers().clone(),
            url: response.url().clone(),
            content_length: response.content_length(),
            body: SendWrapper::new(ResponseBody::Unread(response)),
        }
    }

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
        self.content_length
    }

    fn into_stream(self) -> SendWrapper<ByteStream> {
        SendWrapper::new(match self.body.take() {
            ResponseBody::Unread(response) => Box::pin(response.bytes_stream()) as ByteStream,
            ResponseBody::Streaming(stream) => stream,
        })
    }

    pub async fn bytes(self) -> Result<Bytes> {
        SendWrapper::new(async move {
            let mut stream = self.into_stream().take();
            let mut body = Vec::new();
            while let Some(chunk) = stream.next().await {
                body.extend_from_slice(&chunk?);
            }
            Ok(Bytes::from(body))
        })
        .await
    }

    pub async fn text(self) -> Result<String> {
        let bytes = self.bytes().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T> {
        SendWrapper::new(async move {
            match self.body.take() {
                ResponseBody::Unread(response) => Ok(response.json().await?),
                // A body that `chunk` started reading cannot be handed back
                // to reqwest, and its decode error has no public constructor.
                ResponseBody::Streaming(_) => {
                    panic!("Response::json called after Response::chunk")
                }
            }
        })
        .await
    }

    /// Reads the next piece of the body, or `None` at its end.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        let body = &mut self.body;
        SendWrapper::new(async move {
            if let ResponseBody::Unread(_) = &**body {
                // `bytes_stream` consumes the response, so swap in a
                // placeholder stream while taking it out.
                let unread = std::mem::replace(
                    &mut **body,
                    ResponseBody::Streaming(Box::pin(futures_util::stream::empty())),
                );
                if let ResponseBody::Unread(response) = unread {
                    **body = ResponseBody::Streaming(Box::pin(response.bytes_stream()));
                }
            }
            match &mut **body {
                ResponseBody::Streaming(stream) => Ok(stream.next().await.transpose()?),
                ResponseBody::Unread(_) => Ok(None),
            }
        })
        .await
    }

    pub fn bytes_stream(self) -> impl Stream<Item = Result<Bytes>> + Send {
        SendWrapper::new(self.into_stream().take().map(|chunk| chunk.map_err(Error)))
    }

    pub fn error_for_status(self) -> Result<Self> {
        match self.error_for_status_ref() {
            Ok(_) => Ok(self),
            Err(error) => Err(error),
        }
    }

    pub fn error_for_status_ref(&self) -> Result<&Self> {
        match &*self.body {
            ResponseBody::Unread(response) => match response.error_for_status_ref() {
                Ok(_) => Ok(self),
                Err(error) => Err(Error(error)),
            },
            ResponseBody::Streaming(_) => Ok(self),
        }
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
        // `get` reports an unparsable URL as a builder error, which is the
        // error kind reqwest's own Proxy constructors return.
        Ok(real_reqwest::Client::new()
            .get(url)
            .build()
            .map(|_| Proxy(()))?)
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
        RequestBuilder(SendWrapper::new(match self.timeout {
            Some(timeout) => request.timeout(timeout),
            None => request,
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
        let client = self.inner.clone();
        SendWrapper::new(async move { Ok(Response::new(client.execute(request).await?)) }).await
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
