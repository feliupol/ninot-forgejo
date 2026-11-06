//! Users and the repositories they own.

use std::collections::BTreeMap;

use super::Repo;
use chrono::{DateTime, Utc};

/// A user of the fake. Forgejo's operations the fake serves have no organizations, so every
/// owner of a repository is a user.
pub struct User {
    pub id: u64,
    pub login: String,
    /// Name to show beside the login; empty, as the fake never sets one.
    pub full_name: String,
    pub email: String,
    /// Whether the user administers every repository, as the built-in `admin` does.
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
    pub active: bool,
    pub restricted: bool,
    /// Repositories by lowercase name.
    pub repos: BTreeMap<String, Repo>,
}

impl User {
    /// Whether this user administers every repository.
    pub fn owns_everything(&self) -> bool {
        self.is_admin
    }

    /// Whether this user is `login`, whatever its case.
    pub fn is(&self, login: &str) -> bool {
        self.login.eq_ignore_ascii_case(login)
    }

    pub fn repo(&self, name: &str) -> Option<&Repo> {
        self.repos.get(&name.to_lowercase())
    }

    pub fn repo_mut(&mut self, name: &str) -> Option<&mut Repo> {
        self.repos.get_mut(&name.to_lowercase())
    }
}
