use crate::{ThreadId, ThreadMetadata};
use anyhow::{Context as _, Result};
use client::ProjectId;
use collections::HashMap;
use gpui::{AsyncApp, Context, Entity, EventEmitter};
use rpc::{AnyProtoClient, TypedEnvelope, proto};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use util::{ResultExt as _, path_list::SerializedPathList};

pub struct ThreadRegistry {
    persistence_path: Option<PathBuf>,
    threads: HashMap<ThreadId, ThreadMetadata>,
    downstream: Option<(AnyProtoClient, ProjectId)>,
}

#[derive(Clone, Debug)]
pub enum ThreadRegistryEvent {
    ThreadUpdated(ThreadMetadata),
    ThreadRemoved(ThreadId),
}

impl EventEmitter<ThreadRegistryEvent> for ThreadRegistry {}

#[derive(Clone, Serialize, Deserialize)]
struct PersistedThreadRegistry {
    threads: Vec<PersistedThreadMetadata>,
}

#[derive(Clone, Serialize, Deserialize)]
struct PersistedThreadMetadata {
    thread_id: String,
    session_id: Option<String>,
    agent_id: String,
    title: Option<String>,
    title_override: Option<String>,
    parallel_attempt_group: Option<String>,
    landed: Option<String>,
    updated_at_millis: i64,
    created_at_millis: Option<i64>,
    interacted_at_millis: Option<i64>,
    folder_paths: SerializedPathList,
    main_worktree_paths: SerializedPathList,
    remote_connection_json: Option<String>,
    archived: bool,
}

impl ThreadRegistry {
    pub fn init(client: &AnyProtoClient) {
        client.add_entity_request_handler(Self::handle_list_agent_threads);
        client.add_entity_request_handler(Self::handle_upsert_agent_thread);
        client.add_entity_request_handler(Self::handle_remove_agent_thread);
        client.add_entity_message_handler(Self::handle_agent_thread_updated);
        client.add_entity_message_handler(Self::handle_agent_thread_removed);
    }

    pub fn local(persistence_path: PathBuf, _cx: &mut Context<Self>) -> Self {
        let threads = Self::load(&persistence_path)
            .log_err()
            .unwrap_or_default()
            .into_iter()
            .map(|thread| (thread.thread_id, thread))
            .collect();

        Self {
            persistence_path: Some(persistence_path),
            threads,
            downstream: None,
        }
    }

    pub fn remote(_cx: &mut Context<Self>) -> Self {
        Self {
            persistence_path: None,
            threads: HashMap::default(),
            downstream: None,
        }
    }

    fn reload(&mut self) -> Result<()> {
        let Some(persistence_path) = self.persistence_path.as_ref() else {
            return Ok(());
        };
        self.threads = Self::load(persistence_path)?
            .into_iter()
            .map(|thread| (thread.thread_id, thread))
            .collect();
        Ok(())
    }

    pub fn shared(&mut self, project_id: u64, client: AnyProtoClient, _cx: &mut Context<Self>) {
        for metadata in self.threads.values() {
            self.send_update(&client, project_id, metadata);
        }
        self.downstream = Some((client, ProjectId(project_id)));
    }

    pub fn list(&self) -> Vec<ThreadMetadata> {
        let mut threads = self.threads.values().cloned().collect::<Vec<_>>();
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
        threads
    }

    pub fn upsert(&mut self, metadata: ThreadMetadata, cx: &mut Context<Self>) -> Result<()> {
        self.reload()?;
        self.threads.insert(metadata.thread_id, metadata.clone());
        self.persist()?;
        if let Some((client, project_id)) = &self.downstream {
            self.send_update(client, project_id.0, &metadata);
        }
        cx.emit(ThreadRegistryEvent::ThreadUpdated(metadata));
        cx.notify();
        Ok(())
    }

    pub fn remove(&mut self, thread_id: ThreadId, cx: &mut Context<Self>) -> Result<()> {
        self.reload()?;
        self.threads.remove(&thread_id);
        self.persist()?;
        if let Some((client, project_id)) = &self.downstream {
            client
                .send(proto::AgentThreadRemoved {
                    project_id: project_id.0,
                    thread_id: thread_id.to_key_string(),
                })
                .log_err();
        }
        cx.emit(ThreadRegistryEvent::ThreadRemoved(thread_id));
        cx.notify();
        Ok(())
    }

    fn send_update(&self, client: &AnyProtoClient, project_id: u64, metadata: &ThreadMetadata) {
        let Some(thread) = metadata.to_proto().log_err() else {
            return;
        };
        client
            .send(proto::AgentThreadUpdated {
                project_id,
                thread: Some(thread),
            })
            .log_err();
    }

    fn load(path: &PathBuf) -> Result<Vec<ThreadMetadata>> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("read thread registry"),
        };
        let persisted: PersistedThreadRegistry =
            serde_json::from_slice(&bytes).context("deserialize thread registry")?;
        persisted
            .threads
            .into_iter()
            .map(ThreadMetadata::try_from)
            .collect()
    }

    fn persist(&self) -> Result<()> {
        let Some(persistence_path) = self.persistence_path.as_ref() else {
            return Ok(());
        };
        let persisted = PersistedThreadRegistry {
            threads: self
                .list()
                .into_iter()
                .map(PersistedThreadMetadata::try_from)
                .collect::<Result<Vec<_>>>()?,
        };
        if let Some(parent) = persistence_path.parent() {
            std::fs::create_dir_all(parent).context("create thread registry directory")?;
        }
        let temp_path = persistence_path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&persisted).context("serialize thread registry")?;
        std::fs::write(&temp_path, bytes).context("write thread registry temp file")?;
        std::fs::rename(&temp_path, persistence_path).context("replace thread registry")?;
        Ok(())
    }

    async fn handle_list_agent_threads(
        this: Entity<Self>,
        _envelope: TypedEnvelope<proto::ListAgentThreads>,
        mut cx: AsyncApp,
    ) -> Result<proto::ListAgentThreadsResponse> {
        let threads = this.update(&mut cx, |this, _| {
            this.reload()?;
            this.list()
                .into_iter()
                .map(|thread| thread.to_proto())
                .collect::<Result<Vec<_>>>()
        })?;
        Ok(proto::ListAgentThreadsResponse { threads })
    }

    async fn handle_upsert_agent_thread(
        this: Entity<Self>,
        envelope: TypedEnvelope<proto::UpsertAgentThread>,
        mut cx: AsyncApp,
    ) -> Result<proto::Ack> {
        let thread = envelope.payload.thread.context("missing thread metadata")?;
        let thread = ThreadMetadata::from_proto(thread)?;
        this.update(&mut cx, |this, cx| this.upsert(thread, cx))?;
        Ok(proto::Ack {})
    }

    async fn handle_remove_agent_thread(
        this: Entity<Self>,
        envelope: TypedEnvelope<proto::RemoveAgentThread>,
        mut cx: AsyncApp,
    ) -> Result<proto::Ack> {
        let thread_id = ThreadId::parse(&envelope.payload.thread_id)?;
        this.update(&mut cx, |this, cx| this.remove(thread_id, cx))?;
        Ok(proto::Ack {})
    }

    async fn handle_agent_thread_updated(
        this: Entity<Self>,
        envelope: TypedEnvelope<proto::AgentThreadUpdated>,
        mut cx: AsyncApp,
    ) -> Result<()> {
        let thread = envelope.payload.thread.context("missing thread metadata")?;
        let thread = ThreadMetadata::from_proto(thread)?;
        this.update(&mut cx, |this, cx| {
            this.threads.insert(thread.thread_id, thread.clone());
            cx.emit(ThreadRegistryEvent::ThreadUpdated(thread));
            cx.notify();
        });
        Ok(())
    }

    async fn handle_agent_thread_removed(
        this: Entity<Self>,
        envelope: TypedEnvelope<proto::AgentThreadRemoved>,
        mut cx: AsyncApp,
    ) -> Result<()> {
        let thread_id = ThreadId::parse(&envelope.payload.thread_id)?;
        this.update(&mut cx, |this, cx| {
            this.threads.remove(&thread_id);
            cx.emit(ThreadRegistryEvent::ThreadRemoved(thread_id));
            cx.notify();
        });
        Ok(())
    }
}

impl TryFrom<PersistedThreadMetadata> for ThreadMetadata {
    type Error = anyhow::Error;

    fn try_from(value: PersistedThreadMetadata) -> Result<Self> {
        ThreadMetadata::from_proto(proto::AgentThreadMetadata {
            thread_id: value.thread_id,
            session_id: value.session_id,
            agent_id: value.agent_id,
            title: value.title,
            title_override: value.title_override,
            parallel_attempt_group: value.parallel_attempt_group,
            landed: value.landed,
            updated_at_millis: value.updated_at_millis,
            created_at_millis: value.created_at_millis,
            interacted_at_millis: value.interacted_at_millis,
            folder_paths: Some(proto::AgentThreadPathList {
                paths: value.folder_paths.paths,
                order: value.folder_paths.order,
            }),
            main_worktree_paths: Some(proto::AgentThreadPathList {
                paths: value.main_worktree_paths.paths,
                order: value.main_worktree_paths.order,
            }),
            remote_connection_json: value.remote_connection_json,
            archived: value.archived,
        })
    }
}

impl TryFrom<ThreadMetadata> for PersistedThreadMetadata {
    type Error = anyhow::Error;

    fn try_from(value: ThreadMetadata) -> Result<Self> {
        let proto = value.to_proto()?;
        Ok(Self {
            thread_id: proto.thread_id,
            session_id: proto.session_id,
            agent_id: proto.agent_id,
            title: proto.title,
            title_override: proto.title_override,
            parallel_attempt_group: proto.parallel_attempt_group,
            landed: proto.landed,
            updated_at_millis: proto.updated_at_millis,
            created_at_millis: proto.created_at_millis,
            interacted_at_millis: proto.interacted_at_millis,
            folder_paths: proto
                .folder_paths
                .map(|paths| SerializedPathList {
                    paths: paths.paths,
                    order: paths.order,
                })
                .unwrap_or_else(|| SerializedPathList {
                    paths: String::new(),
                    order: String::new(),
                }),
            main_worktree_paths: proto
                .main_worktree_paths
                .map(|paths| SerializedPathList {
                    paths: paths.paths,
                    order: paths.order,
                })
                .unwrap_or_else(|| SerializedPathList {
                    paths: String::new(),
                    order: String::new(),
                }),
            remote_connection_json: proto.remote_connection_json,
            archived: proto.archived,
        })
    }
}
