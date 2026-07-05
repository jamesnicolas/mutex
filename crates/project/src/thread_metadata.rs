use crate::{AgentId, WorktreePaths};
use agent_client_protocol::schema::v1 as acp;
use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use gpui::SharedString;
use remote::{RemoteConnectionOptions, same_remote_connection_identity};
use rpc::proto;
use std::{path::Path, str::FromStr};
use util::{path_list::PathList, path_list::SerializedPathList};

pub const DEFAULT_THREAD_TITLE: &str = "New Agent Thread";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct ThreadId(uuid::Uuid);

impl ThreadId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub fn from_uuid(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }

    pub fn as_uuid(&self) -> &uuid::Uuid {
        &self.0
    }

    pub fn parse(value: &str) -> Result<Self> {
        Ok(Self(uuid::Uuid::parse_str(value)?))
    }

    pub fn to_key_string(&self) -> String {
        self.0.hyphenated().to_string()
    }
}

impl std::fmt::Display for ThreadId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.to_key_string())
    }
}

impl FromStr for ThreadId {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadLandingState {
    Merged,
    PullRequested,
}

impl ThreadLandingState {
    pub fn from_db_value(value: &str) -> Option<Self> {
        match value {
            "merged" => Some(Self::Merged),
            "pull_requested" => Some(Self::PullRequested),
            _ => None,
        }
    }

    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::PullRequested => "pull_requested",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadMetadata {
    pub thread_id: ThreadId,
    pub session_id: Option<acp::SessionId>,
    pub agent_id: AgentId,
    pub title: Option<SharedString>,
    /// User-supplied title that takes precedence over `title`.
    pub title_override: Option<SharedString>,
    pub parallel_attempt_group: Option<String>,
    pub landed: Option<ThreadLandingState>,
    pub updated_at: DateTime<Utc>,
    pub created_at: Option<DateTime<Utc>>,
    /// When a user last interacted to send a message, including queueing.
    pub interacted_at: Option<DateTime<Utc>>,
    pub worktree_paths: WorktreePaths,
    pub remote_connection: Option<RemoteConnectionOptions>,
    pub archived: bool,
}

impl ThreadMetadata {
    pub fn is_draft(&self) -> bool {
        self.session_id.is_none()
    }

    pub fn display_title(&self) -> SharedString {
        self.title()
            .unwrap_or_else(|| SharedString::new_static(DEFAULT_THREAD_TITLE))
    }

    pub fn title(&self) -> Option<SharedString> {
        self.title_override.clone().or_else(|| self.title.clone())
    }

    pub fn folder_paths(&self) -> &PathList {
        self.worktree_paths.folder_path_list()
    }

    pub fn main_worktree_paths(&self) -> &PathList {
        self.worktree_paths.main_worktree_path_list()
    }

    pub fn references_folder_path(&self, path: &Path) -> bool {
        self.folder_paths()
            .paths()
            .iter()
            .any(|folder_path| folder_path.as_path() == path)
    }

    pub fn matches_remote_connection(
        &self,
        remote_connection: Option<&RemoteConnectionOptions>,
    ) -> bool {
        same_remote_connection_identity(self.remote_connection.as_ref(), remote_connection)
    }

    pub fn to_proto(&self) -> Result<proto::AgentThreadMetadata> {
        let remote_connection_json = self
            .remote_connection
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize thread metadata remote connection")?;

        Ok(proto::AgentThreadMetadata {
            thread_id: self.thread_id.to_key_string(),
            session_id: self
                .session_id
                .as_ref()
                .map(|session_id| session_id.0.to_string()),
            agent_id: self.agent_id.to_string(),
            title: self.title.as_ref().map(|title| title.to_string()),
            title_override: self.title_override.as_ref().map(|title| title.to_string()),
            parallel_attempt_group: self.parallel_attempt_group.clone(),
            landed: self.landed.map(|landed| landed.as_db_value().to_string()),
            updated_at_millis: self.updated_at.timestamp_millis(),
            created_at_millis: self
                .created_at
                .map(|created_at| created_at.timestamp_millis()),
            interacted_at_millis: self
                .interacted_at
                .map(|interacted_at| interacted_at.timestamp_millis()),
            folder_paths: Some(path_list_to_proto(self.folder_paths())),
            main_worktree_paths: Some(path_list_to_proto(self.main_worktree_paths())),
            remote_connection_json,
            archived: self.archived,
        })
    }

    pub fn from_proto(thread: proto::AgentThreadMetadata) -> Result<Self> {
        let remote_connection = thread
            .remote_connection_json
            .as_deref()
            .map(serde_json::from_str::<RemoteConnectionOptions>)
            .transpose()
            .context("deserialize thread metadata remote connection")?;

        let folder_paths = path_list_from_proto(thread.folder_paths);
        let main_worktree_paths = path_list_from_proto(thread.main_worktree_paths);
        let worktree_paths = WorktreePaths::from_path_lists(main_worktree_paths, folder_paths)
            .unwrap_or_else(|_| WorktreePaths::default());

        Ok(Self {
            thread_id: ThreadId::parse(&thread.thread_id).context("invalid thread id")?,
            session_id: thread.session_id.map(acp::SessionId::new),
            agent_id: AgentId::new(thread.agent_id),
            title: thread
                .title
                .filter(|title| !title.is_empty())
                .map(Into::into),
            title_override: thread
                .title_override
                .filter(|title| !title.is_empty())
                .map(Into::into),
            parallel_attempt_group: thread
                .parallel_attempt_group
                .filter(|group| !group.is_empty()),
            landed: thread
                .landed
                .as_deref()
                .and_then(ThreadLandingState::from_db_value),
            updated_at: timestamp_millis_to_datetime(thread.updated_at_millis)
                .context("invalid updated_at timestamp")?,
            created_at: thread
                .created_at_millis
                .map(|millis| {
                    timestamp_millis_to_datetime(millis).context("invalid created_at timestamp")
                })
                .transpose()?,
            interacted_at: thread
                .interacted_at_millis
                .map(|millis| {
                    timestamp_millis_to_datetime(millis).context("invalid interacted_at timestamp")
                })
                .transpose()?,
            worktree_paths,
            remote_connection,
            archived: thread.archived,
        })
    }
}

fn timestamp_millis_to_datetime(millis: i64) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(millis)
}

fn path_list_to_proto(path_list: &PathList) -> proto::AgentThreadPathList {
    let serialized = path_list.serialize();
    proto::AgentThreadPathList {
        paths: serialized.paths,
        order: serialized.order,
    }
}

fn path_list_from_proto(path_list: Option<proto::AgentThreadPathList>) -> PathList {
    let Some(path_list) = path_list else {
        return PathList::default();
    };
    PathList::deserialize(&SerializedPathList {
        paths: path_list.paths,
        order: path_list.order,
    })
}
