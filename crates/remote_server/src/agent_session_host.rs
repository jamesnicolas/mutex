use agent::{
    AgentToolAuthorizationRequestEvent, AgentToolAuthorizationResolvedEvent, NativeAgent,
    NativeAgentConnection, SerializableThreadEvent, Templates, ThreadStore, ToolCallAuthorization,
};
use agent_client_protocol::schema::v1 as acp;
use anyhow::{Context as _, Result, anyhow};
use client::{Client, UserStore};
use collections::{HashMap, HashSet};
use credentials_provider::CredentialsProvider;
use fs::Fs;
use futures::StreamExt as _;
use gpui::{AppContext as _, AsyncApp, Context, Entity, Task};
use http_client::HttpClient;
use language::LanguageRegistry;
use language_model::LanguageModelRegistry;
use language_models::provider::openai_subscribed::OpenAiSubscribedProvider;
use node_runtime::NodeRuntime;
use project::{LocalProjectFlags, Project, ThreadMetadata, ThreadRegistry};
use rpc::{AnyProtoClient, TypedEnvelope, proto};
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use thiserror::Error;
use util::ResultExt as _;

const OPENAI_CODEX_CREDENTIALS_KEY: &str = "https://chatgpt.com/backend-api/codex";
const AGENT_EVENT_BUFFER_LIMIT: usize = 1024;

#[derive(Clone, Default)]
pub struct AgentTurnActivity {
    active_turns: Arc<AtomicUsize>,
}

impl AgentTurnActivity {
    pub fn active_turn_count(&self) -> usize {
        self.active_turns.load(Ordering::SeqCst)
    }

    fn start_turn(&self) -> AgentTurnGuard {
        self.active_turns.fetch_add(1, Ordering::SeqCst);
        AgentTurnGuard {
            active_turns: self.active_turns.clone(),
        }
    }
}

struct AgentTurnGuard {
    active_turns: Arc<AtomicUsize>,
}

impl Drop for AgentTurnGuard {
    fn drop(&mut self) {
        self.active_turns.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ForwardedAgentCredentials {
    access_token: String,
    refresh_token: String,
    expires_at_ms: u64,
    account_id: Option<String>,
    email: Option<String>,
}

impl ForwardedAgentCredentials {
    fn from_proto(message: proto::UpdateAgentCredentials) -> Self {
        Self {
            access_token: message.access_token,
            refresh_token: message.refresh_token,
            expires_at_ms: message.expires_at_ms.unwrap_or_default(),
            account_id: message.account_id,
            email: message.email,
        }
    }

    fn to_updated_proto(&self) -> proto::AgentCredentialsUpdated {
        proto::AgentCredentialsUpdated {
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            expires_at_ms: Some(self.expires_at_ms),
            account_id: self.account_id.clone(),
            email: self.email.clone(),
        }
    }
}

#[derive(Clone)]
struct StoredCredentials {
    username: String,
    credentials: ForwardedAgentCredentials,
    bytes: Vec<u8>,
}

struct InMemoryAgentCredentialsProvider {
    session: AnyProtoClient,
    credentials: Mutex<Option<StoredCredentials>>,
}

impl InMemoryAgentCredentialsProvider {
    fn new(session: AnyProtoClient) -> Self {
        Self {
            session,
            credentials: Mutex::new(None),
        }
    }

    fn update_from_client(
        &self,
        credentials: ForwardedAgentCredentials,
    ) -> Result<Option<ForwardedAgentCredentials>> {
        let bytes = serde_json::to_vec(&credentials).context("serialize agent credentials")?;
        let mut lock = self
            .credentials
            .lock()
            .map_err(|_| anyhow!("agent credentials lock poisoned"))?;

        if let Some(stored) = lock.as_ref()
            && stored.credentials.refresh_token != credentials.refresh_token
            && stored.credentials.expires_at_ms >= credentials.expires_at_ms
        {
            return Ok(Some(stored.credentials.clone()));
        }

        *lock = Some(StoredCredentials {
            username: "Bearer".to_string(),
            credentials,
            bytes,
        });
        Ok(None)
    }

    #[cfg(test)]
    pub fn has_credentials(&self) -> bool {
        self.credentials
            .lock()
            .map(|credentials| credentials.is_some())
            .unwrap_or(false)
    }
}

impl CredentialsProvider for InMemoryAgentCredentialsProvider {
    fn read_credentials<'a>(
        &'a self,
        url: &'a str,
        _cx: &'a AsyncApp,
    ) -> Pin<Box<dyn Future<Output = Result<Option<(String, Vec<u8>)>>> + 'a>> {
        Box::pin(async move {
            if url != OPENAI_CODEX_CREDENTIALS_KEY {
                return Ok(None);
            }

            let credentials = self
                .credentials
                .lock()
                .map_err(|_| anyhow!("agent credentials lock poisoned"))?
                .clone()
                .map(|credentials| (credentials.username, credentials.bytes));
            Ok(credentials)
        })
    }

    fn write_credentials<'a>(
        &'a self,
        url: &'a str,
        username: &'a str,
        password: &'a [u8],
        _cx: &'a AsyncApp,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            if url != OPENAI_CODEX_CREDENTIALS_KEY {
                return Ok(());
            }

            let credentials = serde_json::from_slice::<ForwardedAgentCredentials>(password)
                .context("deserialize refreshed agent credentials")?;
            {
                let mut lock = self
                    .credentials
                    .lock()
                    .map_err(|_| anyhow!("agent credentials lock poisoned"))?;
                *lock = Some(StoredCredentials {
                    username: username.to_string(),
                    credentials: credentials.clone(),
                    bytes: password.to_vec(),
                });
            }

            self.session
                .send(credentials.to_updated_proto())
                .context("send refreshed agent credentials to client")?;
            Ok(())
        })
    }

    fn delete_credentials<'a>(
        &'a self,
        url: &'a str,
        _cx: &'a AsyncApp,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            log::warn!(
                "ignoring server-side request to delete forwarded agent credentials for {url}"
            );
            Ok(())
        })
    }
}

#[derive(Clone, Debug)]
pub struct BufferedAgentEvent {
    pub sequence: u64,
    pub event: SerializableThreadEvent,
}

#[derive(Default)]
struct SessionEventBuffer {
    next_sequence: u64,
    events: VecDeque<BufferedAgentEvent>,
}

impl SessionEventBuffer {
    fn push(&mut self, event: SerializableThreadEvent, limit: usize) -> BufferedAgentEvent {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let event = BufferedAgentEvent { sequence, event };
        self.events.push_back(event.clone());
        while self.events.len() > limit {
            self.events.pop_front();
        }
        event
    }

    fn events(&self) -> Vec<BufferedAgentEvent> {
        self.events.iter().cloned().collect()
    }

    fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    fn first_sequence(&self) -> u64 {
        self.events
            .front()
            .map(|event| event.sequence)
            .unwrap_or(self.next_sequence)
    }

    fn has_gap_before(&self, from_sequence: u64) -> bool {
        from_sequence < self.first_sequence() && from_sequence < self.next_sequence
    }

    fn events_from(&self, from_sequence: u64) -> Vec<BufferedAgentEvent> {
        self.events
            .iter()
            .filter(|event| event.sequence >= from_sequence)
            .cloned()
            .collect()
    }
}

struct HostedToolAuthorization {
    request: AgentToolAuthorizationRequestEvent,
    response: Option<futures::channel::oneshot::Sender<acp_thread::SelectedPermissionOutcome>>,
    resolved_outcome: Option<acp_thread::SelectedPermissionOutcome>,
}

#[derive(Debug, Error, PartialEq, Eq)]
enum ToolAuthorizationResolveError {
    #[error("agent tool authorization not found")]
    NotFound,
    #[error("agent tool authorization already resolved")]
    AlreadyResolved,
    #[error("agent tool authorization receiver was dropped")]
    ReceiverDropped,
}

#[derive(Default)]
struct ToolAuthorizationStore {
    next_approval_id: u64,
    approvals: HashMap<String, HostedToolAuthorization>,
}

impl ToolAuthorizationStore {
    fn request(
        &mut self,
        session_id: acp::SessionId,
        authorization: ToolCallAuthorization,
    ) -> SerializableThreadEvent {
        let approval_id = self.next_approval_id.to_string();
        self.next_approval_id = self.next_approval_id.saturating_add(1);

        let request = AgentToolAuthorizationRequestEvent {
            session_id,
            approval_id: approval_id.clone(),
            tool_call: authorization.tool_call,
            options: authorization.options,
            kind: authorization.kind,
        };

        self.approvals.insert(
            approval_id,
            HostedToolAuthorization {
                request: request.clone(),
                response: Some(authorization.response),
                resolved_outcome: None,
            },
        );

        SerializableThreadEvent::agent_tool_authorization_request(&request)
    }

    fn pending_events(&self) -> Vec<SerializableThreadEvent> {
        self.approvals
            .values()
            .filter(|authorization| authorization.resolved_outcome.is_none())
            .map(|authorization| {
                SerializableThreadEvent::agent_tool_authorization_request(&authorization.request)
            })
            .collect()
    }

    fn resolve(
        &mut self,
        approval_id: &str,
        outcome: acp_thread::SelectedPermissionOutcome,
    ) -> std::result::Result<SerializableThreadEvent, ToolAuthorizationResolveError> {
        let authorization = self
            .approvals
            .get_mut(approval_id)
            .ok_or(ToolAuthorizationResolveError::NotFound)?;

        if authorization.resolved_outcome.is_some() {
            return Err(ToolAuthorizationResolveError::AlreadyResolved);
        }

        let response = authorization
            .response
            .take()
            .ok_or(ToolAuthorizationResolveError::AlreadyResolved)?;
        if response.send(outcome.clone()).is_err() {
            authorization.resolved_outcome = Some(outcome);
            return Err(ToolAuthorizationResolveError::ReceiverDropped);
        }

        authorization.resolved_outcome = Some(outcome.clone());
        Ok(SerializableThreadEvent::agent_tool_authorization_resolved(
            &AgentToolAuthorizationResolvedEvent {
                session_id: authorization.request.session_id.clone(),
                approval_id: authorization.request.approval_id.clone(),
                tool_call_id: authorization.request.tool_call.tool_call_id.clone(),
                outcome,
            },
        ))
    }
}

struct HostedAgentSession {
    thread: Entity<agent::Thread>,
    events: Arc<Mutex<SessionEventBuffer>>,
    approvals: Arc<Mutex<ToolAuthorizationStore>>,
    running_turn: Option<Task<()>>,
}

pub struct AgentSessionHost {
    session: AnyProtoClient,
    fs: Arc<dyn Fs>,
    node_runtime: NodeRuntime,
    languages: Arc<LanguageRegistry>,
    client: Arc<Client>,
    user_store: Entity<UserStore>,
    thread_registry: Entity<ThreadRegistry>,
    connection: NativeAgentConnection,
    credentials_provider: Arc<InMemoryAgentCredentialsProvider>,
    project: Option<Entity<Project>>,
    sessions: HashMap<acp::SessionId, HostedAgentSession>,
    subscribed_sessions: Arc<Mutex<HashSet<acp::SessionId>>>,
    turn_activity: AgentTurnActivity,
    running_turn_count: usize,
}

impl AgentSessionHost {
    pub fn init(session: &AnyProtoClient, host: &Entity<Self>) {
        session.add_request_handler(host.downgrade(), Self::handle_update_agent_credentials);
        session.add_request_handler(host.downgrade(), Self::handle_create_agent_session);
        session.add_request_handler(host.downgrade(), Self::handle_agent_session_prompt);
        session.add_request_handler(host.downgrade(), Self::handle_agent_session_cancel);
        session.add_request_handler(host.downgrade(), Self::handle_subscribe_agent_session);
        session.add_request_handler(
            host.downgrade(),
            Self::handle_respond_agent_tool_authorization,
        );
    }

    pub fn new(
        session: AnyProtoClient,
        fs: Arc<dyn Fs>,
        http_client: Arc<dyn HttpClient>,
        node_runtime: NodeRuntime,
        languages: Arc<LanguageRegistry>,
        thread_registry: Entity<ThreadRegistry>,
        turn_activity: AgentTurnActivity,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.set_http_client(http_client.clone());
        agent_settings::AgentSettings::register(cx);
        language_model::init(cx);

        let credentials_provider = Arc::new(InMemoryAgentCredentialsProvider::new(session.clone()));
        let openai_provider = Arc::new(OpenAiSubscribedProvider::new(
            http_client,
            credentials_provider.clone(),
            cx,
        ));
        LanguageModelRegistry::global(cx).update(cx, |registry, cx| {
            registry.register_provider(openai_provider, cx);
        });

        let client = Client::production(cx);
        let user_store = cx.new(|cx| UserStore::new(client.clone(), cx));
        let thread_store = cx.new(|cx| ThreadStore::new(cx));
        let native_agent = NativeAgent::new(thread_store, Templates::new(), fs.clone(), cx);
        let connection = NativeAgentConnection(native_agent);

        Self {
            session,
            fs,
            node_runtime,
            languages,
            client,
            user_store,
            thread_registry,
            connection,
            credentials_provider,
            project: None,
            sessions: HashMap::default(),
            subscribed_sessions: Arc::new(Mutex::new(HashSet::default())),
            turn_activity,
            running_turn_count: 0,
        }
    }

    fn ensure_project(&mut self, cx: &mut Context<Self>) -> Entity<Project> {
        if let Some(project) = &self.project {
            return project.clone();
        }

        log::info!("creating lazy server-side agent Project::local");
        let project = Project::local(
            self.client.clone(),
            self.node_runtime.clone(),
            self.user_store.clone(),
            self.languages.clone(),
            self.fs.clone(),
            None,
            LocalProjectFlags {
                init_worktree_trust: false,
                watch_global_configs: true,
            },
            cx,
        );
        self.project = Some(project.clone());
        project
    }

    fn create_session(
        &mut self,
        mut metadata: ThreadMetadata,
        cx: &mut Context<Self>,
    ) -> Task<Result<acp::SessionId>> {
        let project = self.ensure_project(cx);
        let connection = self.connection.clone();
        let thread_registry = self.thread_registry.clone();

        let mut root_paths = metadata
            .folder_paths()
            .ordered_paths()
            .cloned()
            .collect::<Vec<_>>();
        if root_paths.is_empty() {
            root_paths = metadata
                .main_worktree_paths()
                .ordered_paths()
                .cloned()
                .collect();
        }

        cx.spawn(async move |this, cx| {
            for path in root_paths {
                project
                    .update(cx, |project, cx| {
                        project.find_or_create_worktree(path.as_path(), true, cx)
                    })
                    .await
                    .with_context(|| format!("create agent worktree for {}", path.display()))?;
            }

            let session_id = cx.update(|cx| connection.new_headless_session(project.clone(), cx));
            let thread = cx
                .update(|cx| connection.thread(&session_id, cx))
                .context("new native agent session did not register a thread")?;
            metadata.session_id = Some(session_id.clone());
            metadata.server_hosted = true;

            this.update(cx, |this, cx| {
                let events = Arc::new(Mutex::new(SessionEventBuffer::default()));
                this.sessions.insert(
                    session_id.clone(),
                    HostedAgentSession {
                        thread,
                        events,
                        approvals: Arc::new(Mutex::new(ToolAuthorizationStore::default())),
                        running_turn: None,
                    },
                );
                thread_registry.update(cx, |registry, cx| registry.upsert(metadata, cx))?;
                anyhow::Ok(())
            })??;

            log::info!("created server-side agent session {session_id}");
            Ok(session_id)
        })
    }

    fn prompt_session(
        &mut self,
        session_id: acp::SessionId,
        prompt_markdown: String,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let session = self
            .sessions
            .get_mut(&session_id)
            .context("agent session not found")?;
        if session.running_turn.is_some() {
            return Err(anyhow!("agent session already has a running turn"));
        }

        let connection = self.connection.clone();
        let events = session.events.clone();
        let approvals = session.approvals.clone();
        let proto_session = self.session.clone();
        let subscribed_sessions = self.subscribed_sessions.clone();
        let turn_guard = self.turn_activity.start_turn();
        self.running_turn_count += 1;

        let task = cx.spawn({
            let session_id = session_id.clone();
            async move |this, cx| {
                let prompt_task = cx.update(|cx| {
                    let event_session_id = session_id.clone();
                    let event_events = events.clone();
                    let event_proto_session = proto_session.clone();
                    let event_subscribed_sessions = subscribed_sessions.clone();
                    let authorization_session_id = session_id.clone();
                    let authorization_events = events.clone();
                    let authorization_proto_session = proto_session.clone();
                    let authorization_subscribed_sessions = subscribed_sessions.clone();
                    connection.prompt_headless(
                        session_id.clone(),
                        prompt_markdown,
                        move |event| {
                            push_buffered_event(
                                &event_session_id,
                                &event_events,
                                &event_proto_session,
                                &event_subscribed_sessions,
                                event,
                            )
                        },
                        move |authorization| {
                            let event = approvals
                                .lock()
                                .map_err(|_| anyhow!("agent approvals lock poisoned"))?
                                .request(authorization_session_id.clone(), authorization);
                            push_buffered_event(
                                &authorization_session_id,
                                &authorization_events,
                                &authorization_proto_session,
                                &authorization_subscribed_sessions,
                                event,
                            );
                            Ok(())
                        },
                        cx,
                    )
                });

                let result = prompt_task.await.map(|_| ());

                if let Err(error) = &result {
                    log::error!("server-side agent turn failed for {session_id}: {error:?}");
                }

                this.update(cx, |this, _cx| {
                    if let Some(session) = this.sessions.get_mut(&session_id) {
                        session.running_turn.take();
                    }
                    this.running_turn_count = this.running_turn_count.saturating_sub(1);
                })
                .log_err();

                drop(turn_guard);
            }
        });

        session.running_turn = Some(task);
        Ok(())
    }

    fn cancel_session(
        &mut self,
        session_id: &acp::SessionId,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.sessions
            .get(session_id)
            .context("agent session not found")?;
        self.connection.cancel_headless(session_id, cx);
        Ok(())
    }

    fn respond_tool_authorization(
        &mut self,
        session_id: acp::SessionId,
        approval_id: String,
        outcome: acp_thread::SelectedPermissionOutcome,
    ) -> Result<()> {
        let (events, approvals) = {
            let session = self
                .sessions
                .get(&session_id)
                .context("agent session not found")?;
            (session.events.clone(), session.approvals.clone())
        };

        let event = approvals
            .lock()
            .map_err(|_| anyhow!("agent approvals lock poisoned"))?
            .resolve(&approval_id, outcome)
            .map_err(anyhow::Error::from)?;
        push_buffered_event(
            &session_id,
            &events,
            &self.session,
            &self.subscribed_sessions,
            event,
        );
        Ok(())
    }

    pub fn buffered_events(&self, session_id: &acp::SessionId) -> Vec<BufferedAgentEvent> {
        self.sessions
            .get(session_id)
            .and_then(|session| session.events.lock().ok().map(|events| events.events()))
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn push_test_event(&self, session_id: &acp::SessionId, event: SerializableThreadEvent) {
        if let Some(session) = self.sessions.get(session_id) {
            match session.events.lock() {
                Ok(mut events) => {
                    events.push(event, AGENT_EVENT_BUFFER_LIMIT);
                }
                Err(error) => {
                    log::error!("failed to buffer test agent event: {error}");
                }
            }
        }
    }

    pub fn has_lazy_project(&self) -> bool {
        self.project.is_some()
    }

    pub fn running_turn_count(&self) -> usize {
        self.running_turn_count
    }

    #[cfg(test)]
    pub fn pending_authorization_count(&self, session_id: &acp::SessionId) -> usize {
        self.sessions
            .get(session_id)
            .and_then(|session| {
                session
                    .approvals
                    .lock()
                    .ok()
                    .map(|approvals| approvals.pending_events().len())
            })
            .unwrap_or_default()
    }

    pub fn session_thread(&self, session_id: &acp::SessionId) -> Option<Entity<agent::Thread>> {
        self.sessions
            .get(session_id)
            .map(|session| session.thread.clone())
    }

    #[cfg(test)]
    pub fn has_forwarded_credentials(&self) -> bool {
        self.credentials_provider.has_credentials()
    }

    async fn handle_update_agent_credentials(
        this: Entity<Self>,
        envelope: TypedEnvelope<proto::UpdateAgentCredentials>,
        mut cx: AsyncApp,
    ) -> Result<proto::Ack> {
        let credentials = ForwardedAgentCredentials::from_proto(envelope.payload);
        let credentials_to_push = this.update(&mut cx, |this, _cx| {
            this.credentials_provider.update_from_client(credentials)
        })?;

        if let Some(credentials) = credentials_to_push {
            this.read_with(&cx, |this, _cx| {
                this.session.send(credentials.to_updated_proto()).log_err();
            });
        }

        Ok(proto::Ack {})
    }

    async fn handle_create_agent_session(
        this: Entity<Self>,
        envelope: TypedEnvelope<proto::CreateAgentSession>,
        mut cx: AsyncApp,
    ) -> Result<proto::CreateAgentSessionResponse> {
        let thread_metadata = envelope
            .payload
            .thread_metadata
            .context("missing thread metadata")?;
        let thread_metadata = ThreadMetadata::from_proto(thread_metadata)?;
        let session_id = this
            .update(&mut cx, |this, cx| this.create_session(thread_metadata, cx))
            .await?;

        Ok(proto::CreateAgentSessionResponse {
            session_id: session_id.0.to_string(),
        })
    }

    async fn handle_agent_session_prompt(
        this: Entity<Self>,
        envelope: TypedEnvelope<proto::AgentSessionPrompt>,
        mut cx: AsyncApp,
    ) -> Result<proto::Ack> {
        let session_id = acp::SessionId::new(envelope.payload.session_id);
        this.update(&mut cx, |this, cx| {
            this.prompt_session(session_id, envelope.payload.prompt_markdown, cx)
        })?;
        Ok(proto::Ack {})
    }

    async fn handle_agent_session_cancel(
        this: Entity<Self>,
        envelope: TypedEnvelope<proto::AgentSessionCancel>,
        mut cx: AsyncApp,
    ) -> Result<proto::Ack> {
        let session_id = acp::SessionId::new(envelope.payload.session_id);
        this.update(&mut cx, |this, cx| this.cancel_session(&session_id, cx))?;
        Ok(proto::Ack {})
    }

    async fn handle_respond_agent_tool_authorization(
        this: Entity<Self>,
        envelope: TypedEnvelope<proto::RespondAgentToolAuthorization>,
        mut cx: AsyncApp,
    ) -> Result<proto::Ack> {
        let session_id = acp::SessionId::new(envelope.payload.session_id);
        let approval_id = envelope.payload.approval_id;
        let outcome = serde_json::from_str(&envelope.payload.selected_outcome_json)
            .context("deserialize selected agent tool authorization outcome")?;
        this.update(&mut cx, |this, _cx| {
            this.respond_tool_authorization(session_id, approval_id, outcome)
        })?;
        Ok(proto::Ack {})
    }

    async fn handle_subscribe_agent_session(
        this: Entity<Self>,
        envelope: TypedEnvelope<proto::SubscribeAgentSession>,
        mut cx: AsyncApp,
    ) -> Result<proto::SubscribeAgentSessionResponse> {
        let session_id = acp::SessionId::new(envelope.payload.session_id);
        let from_sequence = envelope.payload.from_sequence;
        let (snapshot_sequence, gap, mut replay, approvals) =
            this.update(&mut cx, |this, cx| {
                let session = this
                    .sessions
                    .get(&session_id)
                    .context("agent session not found")?;
                let (snapshot_sequence, gap) = {
                    let events = session
                        .events
                        .lock()
                        .map_err(|_| anyhow!("agent session events lock poisoned"))?;
                    (events.next_sequence(), events.has_gap_before(from_sequence))
                };
                this.subscribed_sessions
                    .lock()
                    .map_err(|_| anyhow!("agent session subscriptions lock poisoned"))?
                    .insert(session_id.clone());
                let replay = session.thread.update(cx, |thread, cx| thread.replay(cx));
                anyhow::Ok((snapshot_sequence, gap, replay, session.approvals.clone()))
            })?;

        let mut snapshot = Vec::new();
        while let Some(event) = replay.next().await {
            snapshot.push(serializable_event_to_proto(
                &SerializableThreadEvent::from_thread_event(&event?),
            )?);
        }
        let pending_approval_events = approvals
            .lock()
            .map_err(|_| anyhow!("agent approvals lock poisoned"))?
            .pending_events();
        for event in pending_approval_events {
            snapshot.push(serializable_event_to_proto(&event)?);
        }

        let events = this.update(&mut cx, |this, _cx| {
            let session = this
                .sessions
                .get(&session_id)
                .context("agent session not found")?;
            let events = session
                .events
                .lock()
                .map_err(|_| anyhow!("agent session events lock poisoned"))?;
            events
                .events_from(from_sequence)
                .iter()
                .map(buffered_event_to_proto)
                .collect::<Result<Vec<_>>>()
        })?;

        Ok(proto::SubscribeAgentSessionResponse {
            snapshot_sequence,
            gap,
            snapshot,
            events,
        })
    }
}

fn push_buffered_event(
    session_id: &acp::SessionId,
    events: &Arc<Mutex<SessionEventBuffer>>,
    proto_session: &AnyProtoClient,
    subscribed_sessions: &Arc<Mutex<HashSet<acp::SessionId>>>,
    event: SerializableThreadEvent,
) {
    match events.lock() {
        Ok(mut events) => {
            let sequence = events.next_sequence;
            log::info!(
                "buffering server-side agent event seq={} variant={}",
                sequence,
                event.variant
            );
            let event = events.push(event, AGENT_EVENT_BUFFER_LIMIT);
            if subscribed_sessions
                .lock()
                .map(|sessions| sessions.contains(session_id))
                .unwrap_or(false)
            {
                match buffered_event_to_proto(&event) {
                    Ok(event) => {
                        proto_session
                            .send(proto::AgentSessionEvent {
                                session_id: session_id.0.to_string(),
                                sequence: event.sequence,
                                event: event.event,
                            })
                            .log_err();
                    }
                    Err(error) => {
                        log::error!("failed to serialize server-side agent event: {error:?}");
                    }
                }
            }
        }
        Err(error) => {
            log::error!("failed to buffer server-side agent event: {error}");
        }
    }
}

fn buffered_event_to_proto(event: &BufferedAgentEvent) -> Result<proto::BufferedAgentSessionEvent> {
    Ok(proto::BufferedAgentSessionEvent {
        sequence: event.sequence,
        event: Some(serializable_event_to_proto(&event.event)?),
    })
}

fn serializable_event_to_proto(
    event: &SerializableThreadEvent,
) -> Result<proto::SerializedAgentThreadEvent> {
    Ok(proto::SerializedAgentThreadEvent {
        variant: event.variant.clone(),
        payload_json: event
            .payload
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize agent thread event payload")?,
        debug: event.debug.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent::AGENT_TOOL_AUTHORIZATION_RESOLVED_EVENT;

    #[test]
    fn tool_authorization_store_first_response_wins() {
        let mut store = ToolAuthorizationStore::default();
        let session_id = acp::SessionId::new("session");
        let (response, receiver) = futures::channel::oneshot::channel();

        let request_event = store.request(
            session_id,
            ToolCallAuthorization {
                tool_call: acp::ToolCallUpdate::new(
                    "tool-1",
                    acp::ToolCallUpdateFields::new().title("Needs approval"),
                ),
                options: acp_thread::PermissionOptions::Flat(vec![acp::PermissionOption::new(
                    acp::PermissionOptionId::new("allow"),
                    "Allow",
                    acp::PermissionOptionKind::AllowOnce,
                )]),
                response,
                context: None,
                kind: acp_thread::AuthorizationKind::PermissionGrant,
            },
        );
        assert_eq!(
            request_event.variant,
            agent::AGENT_TOOL_AUTHORIZATION_REQUEST_EVENT
        );
        assert_eq!(store.pending_events().len(), 1);

        let outcome = acp_thread::SelectedPermissionOutcome::new(
            acp::PermissionOptionId::new("allow"),
            acp::PermissionOptionKind::AllowOnce,
        );
        let resolved_event = store.resolve("0", outcome.clone()).unwrap();
        assert_eq!(
            resolved_event.variant,
            AGENT_TOOL_AUTHORIZATION_RESOLVED_EVENT
        );
        assert_eq!(store.pending_events().len(), 0);
        assert_eq!(smol::block_on(receiver).unwrap(), outcome);

        let second = store.resolve("0", outcome).unwrap_err();
        assert_eq!(second, ToolAuthorizationResolveError::AlreadyResolved);
    }
}
