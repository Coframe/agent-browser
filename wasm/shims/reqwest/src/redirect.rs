//! Redirect policy types. `fetch` follows redirects itself, so a policy is
//! accepted and never consulted.

use std::fmt;

use crate::{StatusCode, Url};

pub struct Policy(());

impl Policy {
    pub fn none() -> Self {
        Self(())
    }

    pub fn limited(_max: usize) -> Self {
        Self(())
    }

    pub fn custom<T>(_policy: T) -> Self
    where
        T: Fn(Attempt) -> Action + Send + Sync + 'static,
    {
        Self(())
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self::limited(10)
    }
}

impl fmt::Debug for Policy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Policy")
    }
}

#[derive(Debug)]
pub struct Attempt<'a> {
    status: StatusCode,
    next: &'a Url,
    previous: &'a [Url],
}

impl<'a> Attempt<'a> {
    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn url(&self) -> &Url {
        self.next
    }

    pub fn previous(&self) -> &[Url] {
        self.previous
    }

    pub fn follow(self) -> Action {
        Action(())
    }

    pub fn stop(self) -> Action {
        Action(())
    }

    pub fn error<E: Into<Box<dyn std::error::Error + Send + Sync>>>(self, _error: E) -> Action {
        Action(())
    }
}

#[derive(Debug)]
pub struct Action(());
