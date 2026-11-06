//! Repositories: their lifecycle, the redirects left by renames, and who may do what with them.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;

use super::{Hook, State, Tracker, now};
use crate::api::Caller;
use crate::parse;
use chrono::{DateTime, Utc};

/// A repository, owned by exactly one user.
pub struct Repo {
    pub id: u64,
    pub name: String,
    pub description: Option<String>,
    pub private: bool,
    pub default_branch: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub pushed_at: Option<DateTime<Utc>>,
    /// Bare git repository, named after the id so renames leave it in place.
    pub git_dir: PathBuf,
    /// The issues, labels and comments of the repository.
    pub tracker: Tracker,
    /// Webhooks configured for this repository.
    pub hooks: Vec<Hook>,
    /// Append-only commit status history, keyed by the resolved commit SHA.
    pub statuses: std::collections::BTreeMap<String, Vec<crate::types::CommitStatus>>,
    /// Actions runs known to this repository, in creation order.
    pub action_tasks: Vec<crate::types::ActionTask>,
    /// Workflow runs and their job state, keyed by the global run identifier.
    pub action_runs: std::collections::BTreeMap<i64, super::ActionRun>,
    /// Names of write-only repository Actions secrets.
    pub action_secrets: BTreeSet<String>,
    /// Dispatches explicitly scripted by the test fixture.
    pub workflow_dispatches: VecDeque<super::WorkflowDispatch>,
    /// Repository default used when scheduling a pull-request merge.
    pub delete_branch_after_merge: bool,
}

impl Repo {
    /// Points `default_branch` and the bare repository's `HEAD` at `branch`, which need not
    /// exist yet; the two always agree.
    ///
    /// # Errors
    ///
    /// Fails when `HEAD` cannot be rewritten, leaving `default_branch` unchanged.
    pub fn set_default_branch(&mut self, branch: &str) -> std::io::Result<()> {
        ninot_common::git::set_head(&self.git_dir, branch)?;
        self.default_branch = branch.to_owned();
        Ok(())
    }

    /// Whether the repository has no commits yet.
    pub fn is_empty(&self) -> bool {
        ninot_common::git::is_empty(&self.git_dir).unwrap_or(true)
    }
}

/// What a caller may do with a repository, as Forgejo's `permissions` object reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permission {
    pub admin: bool,
    pub push: bool,
    pub pull: bool,
}

impl Permission {
    /// What an owner or an administrator may do.
    pub const ADMIN: Self = Self {
        admin: true,
        push: true,
        pull: true,
    };
    /// What anyone may do with a public repository they do not own.
    pub const READ: Self = Self {
        admin: false,
        push: false,
        pull: true,
    };
}

impl State {
    /// Records a repository, reserving its name. Returns the message to answer a request that
    /// cannot have it with; the name being taken is a 409.
    #[allow(clippy::too_many_arguments)]
    pub fn create_repo(
        &mut self,
        owner: &str,
        name: &str,
        private: bool,
        description: Option<String>,
        default_branch: &str,
    ) -> Result<(), String> {
        if self.user(owner).is_none() {
            return Err(format!("user {owner} does not exist"));
        }
        if self.repo_key_taken(owner, name) {
            return Err(format!(
                "repository already exists [uname: {owner}, name: {name}]"
            ));
        }
        let id = self.next_id();
        let git_dir = self.git_dir(id);
        let created = now();
        let user = self.user_mut(owner).expect("the owner exists");
        user.repos.insert(
            name.to_lowercase(),
            Repo {
                id,
                name: name.to_owned(),
                description,
                private,
                default_branch: default_branch.to_owned(),
                created_at: created,
                updated_at: created,
                pushed_at: None,
                git_dir,
                tracker: Tracker::default(),
                hooks: Vec::new(),
                statuses: std::collections::BTreeMap::new(),
                action_tasks: Vec::new(),
                action_runs: std::collections::BTreeMap::new(),
                action_secrets: BTreeSet::new(),
                workflow_dispatches: VecDeque::new(),
                delete_branch_after_merge: false,
            },
        );
        // A repository may take the name a rename left behind.
        self.redirects.remove(&self.repo_key(owner, name));
        Ok(())
    }

    /// Removes a repository and any redirect pointing at it. Its `git_dir` stays on disk for
    /// the caller to remove with the lock released.
    pub fn delete_repo(&mut self, owner: &str, name: &str) -> Option<Repo> {
        let key = self.repo_key(owner, name);
        let repo = self.user_mut(owner)?.repos.remove(&name.to_lowercase())?;
        self.redirects.retain(|_, target| *target != key);
        Some(repo)
    }

    /// Renames a repository, leaving a redirect from the old `owner/name` behind, as Forgejo
    /// keeps serving the old URL. Redirects that led to the old name follow it.
    pub fn rename_repo(&mut self, owner: &str, name: &str, new_name: &str) -> Result<(), String> {
        let old = self.repo_key(owner, name);
        let new = self.repo_key(owner, new_name);
        if old == new {
            return Ok(());
        }
        let taken = self.repo_key_taken(owner, new_name);
        {
            let user = self
                .user_mut(owner)
                .filter(|user| user.repo(name).is_some())
                .ok_or_else(|| format!("repository {old} does not exist"))?;
            if taken {
                return Err(format!(
                    "repository already exists [uname: {}, name: {new_name}]",
                    user.login
                ));
            }
            let mut repo = user.repos.remove(&name.to_lowercase()).unwrap();
            repo.name = new_name.to_owned();
            repo.updated_at = now();
            user.repos.insert(new_name.to_lowercase(), repo);
        }
        for target in self.redirects.values_mut() {
            if *target == old {
                *target = new.clone();
            }
        }
        self.redirects.insert(old, new);
        Ok(())
    }

    /// Where a renamed repository lives now, as its `owner/name`.
    pub fn redirect(&self, owner: &str, name: &str) -> Option<(&str, &str)> {
        parse::owner_and_name(self.redirects.get(&self.repo_key(owner, name))?)
    }

    /// What `caller` may do with `owner/name`; `None` when the repository is invisible to them.
    pub fn permission(
        &self,
        caller: Option<&Caller>,
        owner: &str,
        name: &str,
    ) -> Option<Permission> {
        let user = self.user(owner)?;
        let repo = user.repo(name)?;
        let caller = caller.and_then(|caller| self.user(&caller.login));
        let owns = caller.is_some_and(|caller| caller.owns_everything() || caller.is(&user.login));
        if owns {
            return Some(Permission::ADMIN);
        }
        (!repo.private).then_some(Permission::READ)
    }

    /// Whether the user already has a repository named `name`.
    fn repo_key_taken(&self, owner: &str, name: &str) -> bool {
        self.user(owner)
            .is_some_and(|user| user.repo(name).is_some())
    }

    /// The directory of the bare git repository with id `id`.
    fn git_dir(&self, id: u64) -> PathBuf {
        self.git_root
            .as_ref()
            .expect("the fake always serves git repositories")
            .join(format!("{id}.git"))
    }

    fn repo_key(&self, owner: &str, name: &str) -> String {
        format!("{}/{}", owner.to_lowercase(), name.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        let mut state = State {
            git_root: Some(std::env::temp_dir().join("ninot-forgejo-state-test")),
            ..Default::default()
        };
        state.add_user("alice");
        state.add_user("bob");
        state
    }

    #[test]
    fn a_renamed_repository_keeps_its_old_name_as_a_redirect() {
        let mut state = state();
        state
            .create_repo("alice", "tool", false, None, "main")
            .unwrap();
        state.rename_repo("alice", "tool", "gear").unwrap();
        assert_eq!(state.redirect("Alice", "TOOL"), Some(("alice", "gear")));
        state.rename_repo("alice", "gear", "gadget").unwrap();
        assert_eq!(state.redirect("alice", "tool"), Some(("alice", "gadget")));
        assert_eq!(state.redirect("alice", "gear"), Some(("alice", "gadget")));
        state
            .create_repo("alice", "taken", false, None, "main")
            .unwrap();
        assert!(state.rename_repo("alice", "gadget", "taken").is_err());
        state
            .create_repo("alice", "tool", false, None, "main")
            .unwrap();
        assert_eq!(state.redirect("alice", "tool"), None);
    }

    #[test]
    fn deleting_a_repository_removes_its_redirects() {
        let mut state = state();
        state
            .create_repo("alice", "tool", false, None, "main")
            .unwrap();
        state.rename_repo("alice", "tool", "gear").unwrap();
        assert!(state.delete_repo("alice", "gear").is_some());
        assert_eq!(state.redirect("alice", "tool"), None);
        assert!(state.delete_repo("alice", "gear").is_none());
    }

    #[test]
    fn private_repositories_are_invisible_to_outsiders() {
        let mut state = state();
        state
            .create_repo("alice", "secret", true, None, "main")
            .unwrap();
        let bob = Caller {
            login: "bob".to_owned(),
            scopes: Vec::new(),
        };
        assert_eq!(state.permission(Some(&bob), "alice", "secret"), None);
        assert_eq!(state.permission(None, "alice", "secret"), None);
        let alice = Caller {
            login: "Alice".to_owned(),
            scopes: Vec::new(),
        };
        assert_eq!(
            state.permission(Some(&alice), "alice", "secret"),
            Some(Permission::ADMIN)
        );
    }

    #[test]
    fn a_public_repository_is_readable_but_not_pushable() {
        let mut state = state();
        state
            .create_repo("alice", "open", false, None, "main")
            .unwrap();
        let bob = Caller {
            login: "bob".to_owned(),
            scopes: Vec::new(),
        };
        assert_eq!(
            state.permission(Some(&bob), "alice", "open"),
            Some(Permission::READ)
        );
        assert_eq!(
            state.permission(None, "alice", "open"),
            Some(Permission::READ)
        );
        assert_eq!(state.permission(None, "nobody", "open"), None);
    }
}
