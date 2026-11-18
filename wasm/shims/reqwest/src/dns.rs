//! DNS resolver types. `fetch` resolves names itself, so a resolver is
//! accepted and never called.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::str::FromStr;

pub type Addrs = Box<dyn Iterator<Item = SocketAddr> + Send>;

pub type Resolving =
    Pin<Box<dyn Future<Output = Result<Addrs, Box<dyn std::error::Error + Send + Sync>>> + Send>>;

pub trait Resolve: Send + Sync {
    fn resolve(&self, name: Name) -> Resolving;
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Name(String);

impl Name {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Name {
    type Err = std::convert::Infallible;
    fn from_str(host: &str) -> Result<Self, Self::Err> {
        Ok(Name(host.to_string()))
    }
}
