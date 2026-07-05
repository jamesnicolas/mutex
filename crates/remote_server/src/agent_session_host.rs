use agent::{NativeAgent, NativeAgentConnection, SerializableThreadEvent, Templates, ThreadStore};
use agent_client_protocol::schema::v1 as acp;
use agent_settings::{AgentProfileId, AgentSettings, builtin_profiles};
use anyhow::{Context as _, Result, anyhow};
use client::{Client, UserStore};
use collections::HashMap;
use credentials_provider::CredentialsProvider;
use fs::Fs;
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
    fn push(&mut self, event: SerializableThreadEvent, limit: usize) {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.events
            .push_back(BufferedAgentEvent { sequence, event });
        while self.events.len() > limit {
            self.events.pop_front();
        }
    }

    fn events(&self) -> Vec<BufferedAgentEvent> {
        self.events.iter().cloned().collect()
    }
}

struct HostedAgentSession {
    thread: Entity<agent::Thread>,
    events: Arc<Mutex<SessionEventBuffer>>,
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
    turn_activity: AgentTurnActivity,
    running_turn_count: usize,
}

impl AgentSessionHost {
    pub fn init(session: &AnyProtoClient, host: &Entity<Self>) {
        session.add_request_handler(host.downgrade(), Self::handle_update_agent_credentials);
        session.add_request_handler(host.downgrade(), Self::handle_create_agent_session);
        session.add_request_handler(host.downgrade(), Self::handle_agent_session_prompt);
        session.add_request_handler(host.downgrade(), Self::handle_agent_session_cancel);
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

        let mut agent_settings = AgentSettings::get_global(cx).clone();
        agent_settings.default_profile = AgentProfileId(Arc::from(builtin_profiles::MINIMAL));
        AgentSettings::override_global(agent_settings, cx);

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

            this.update(cx, |this, cx| {
                let events = Arc::new(Mutex::new(SessionEventBuffer::default()));
                this.sessions.insert(
                    session_id.clone(),
                    HostedAgentSession {
                        thread,
                        events,
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
        let turn_guard = self.turn_activity.start_turn();
        self.running_turn_count += 1;

        let task = cx.spawn({
            let session_id = session_id.clone();
            async move |this, cx| {
                let prompt_task = cx.update(|cx| {
                    connection.prompt_headless(
                        session_id.clone(),
                        prompt_markdown,
                        move |event| push_buffered_event(&events, event),
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

    pub fn buffered_events(&self, session_id: &acp::SessionId) -> Vec<BufferedAgentEvent> {
        self.sessions
            .get(session_id)
            .and_then(|session| session.events.lock().ok().map(|events| events.events()))
            .unwrap_or_default()
    }

    pub fn has_lazy_project(&self) -> bool {
        self.project.is_some()
    }

    pub fn running_turn_count(&self) -> usize {
        self.running_turn_count
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
}

fn push_buffered_event(events: &Arc<Mutex<SessionEventBuffer>>, event: SerializableThreadEvent) {
    match events.lock() {
        Ok(mut events) => {
            let sequence = events.next_sequence;
            log::info!(
                "buffering server-side agent event seq={} variant={}",
                sequence,
                event.variant
            );
            events.push(event, AGENT_EVENT_BUFFER_LIMIT);
        }
        Err(error) => {
            log::error!("failed to buffer server-side agent event: {error}");
        }
    }
}
