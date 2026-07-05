use crate::{
    AGENT_TOOL_AUTHORIZATION_REQUEST_EVENT, AGENT_TOOL_AUTHORIZATION_RESOLVED_EVENT,
    AgentToolAuthorizationRequestEvent, AgentToolAuthorizationResolvedEvent, NativeAgentConnection,
    SerializableThreadEvent, ThreadEvent, ZED_AGENT_ID,
};
use acp_thread::{AcpThread, AgentConnection, UserMessageId};
use action_log::ActionLog;
use agent_client_protocol::schema::v1 as acp;
use anyhow::{Context as _, Result, anyhow};
use chrono::Utc;
use collections::{HashMap, HashSet};
use futures::{StreamExt as _, channel::mpsc};
use gpui::{App, AppContext as _, Entity, SharedString, Task, WeakEntity};
use project::{self as project_crate, Project, ThreadId, ThreadMetadata, WorktreePaths};
use rpc::{AnyProtoClient, proto};
use std::{any::Any, cell::RefCell, rc::Rc};
use util::{ResultExt as _, path_list::PathList};
use watch::Receiver;

const CHATGPT_SUBSCRIPTION_AUTH_METHOD_ID: &str = "chatgpt-subscription";

#[derive(Clone)]
pub struct RemoteAgentConnection {
    proto_client: AnyProtoClient,
    auth_methods: Vec<acp::AuthMethod>,
    auth_state: Rc<RefCell<RemoteAgentAuthState>>,
    sessions: Rc<RefCell<HashMap<acp::SessionId, RemoteSession>>>,
}

#[derive(Default)]
struct RemoteAgentAuthState {
    authenticated: bool,
    email: Option<String>,
}

struct RemoteSession {
    acp_thread: WeakEntity<AcpThread>,
    next_sequence: u64,
    prompt_events_tx: Option<mpsc::UnboundedSender<Result<ThreadEvent>>>,
    suppress_next_user_message: bool,
    resolved_approvals: HashSet<String>,
    _live_task: Task<()>,
}

impl RemoteAgentConnection {
    pub fn new(proto_client: AnyProtoClient) -> Self {
        Self {
            proto_client,
            auth_methods: vec![acp::AuthMethod::Agent(acp::AuthMethodAgent::new(
                CHATGPT_SUBSCRIPTION_AUTH_METHOD_ID,
                "Sign in with ChatGPT",
            ))],
            auth_state: Rc::new(RefCell::new(RemoteAgentAuthState::default())),
            sessions: Rc::new(RefCell::new(HashMap::default())),
        }
    }

    pub fn refresh_auth_status(&self, cx: &App) {
        let proto_client = self.proto_client.clone();
        let auth_state = self.auth_state.clone();
        cx.foreground_executor()
            .spawn(async move {
                match proto_client.request(proto::GetAgentAuthStatus {}).await {
                    Ok(status) => {
                        Self::set_auth_status(&auth_state, status);
                    }
                    Err(error) => {
                        auth_state.borrow_mut().authenticated = false;
                        log::warn!("failed to query remote agent auth status: {error:?}");
                    }
                }
            })
            .detach();
    }

    fn set_auth_status(
        auth_state: &Rc<RefCell<RemoteAgentAuthState>>,
        status: proto::AgentAuthStatus,
    ) {
        *auth_state.borrow_mut() = RemoteAgentAuthState {
            authenticated: status.authenticated,
            email: status.email,
        };
    }

    async fn ensure_authenticated(
        proto_client: &AnyProtoClient,
        auth_state: &Rc<RefCell<RemoteAgentAuthState>>,
    ) -> Result<()> {
        let status = proto_client
            .request(proto::GetAgentAuthStatus {})
            .await
            .context("query remote agent auth status")?;
        let authenticated = status.authenticated;
        Self::set_auth_status(auth_state, status);
        if !authenticated {
            return Err(acp_thread::AuthRequired::new()
                .with_description("Sign in with ChatGPT to continue.".to_string())
                .into());
        }
        Ok(())
    }

    async fn complete_authentication_from_callback(
        proto_client: AnyProtoClient,
        auth_state: Rc<RefCell<RemoteAgentAuthState>>,
        begin: proto::BeginAgentSignInResponse,
        callback: oauth_callback_server::OAuthCallbackParams,
    ) -> Result<()> {
        if callback.state != begin.state {
            auth_state.borrow_mut().authenticated = false;
            return Err(anyhow!("OAuth state mismatch"));
        }

        let complete = proto_client
            .request(proto::CompleteAgentSignIn {
                code: callback.code,
                state: callback.state,
            })
            .await
            .context("complete remote agent sign-in")?;
        *auth_state.borrow_mut() = RemoteAgentAuthState {
            authenticated: true,
            email: complete.email,
        };
        Ok(())
    }

    #[cfg(test)]
    fn authenticate_with_callback_for_test(
        &self,
        method: acp::AuthMethodId,
        callback: oauth_callback_server::OAuthCallbackParams,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let proto_client = self.proto_client.clone();
        let auth_state = self.auth_state.clone();
        cx.spawn(async move |cx| {
            if method != acp::AuthMethodId::new(CHATGPT_SUBSCRIPTION_AUTH_METHOD_ID) {
                return Err(anyhow!("unsupported remote agent auth method {method}"));
            }

            let begin = proto_client
                .request(proto::BeginAgentSignIn {})
                .await
                .context("begin remote agent sign-in")?;
            cx.update(|cx| cx.open_url(&begin.authorize_url));
            Self::complete_authentication_from_callback(proto_client, auth_state, begin, callback)
                .await
        })
    }

    fn make_thread(
        self: &Rc<Self>,
        session_id: acp::SessionId,
        project: Entity<Project>,
        work_dirs: PathList,
        title: Option<SharedString>,
        cx: &mut App,
    ) -> Entity<AcpThread> {
        let action_log = cx.new(|_| ActionLog::new(project.clone()));
        let connection = self.clone() as Rc<dyn AgentConnection>;
        cx.new(|cx| {
            AcpThread::new(
                None,
                title,
                Some(work_dirs),
                connection,
                project,
                action_log,
                session_id,
                Receiver::constant(acp::PromptCapabilities::new()),
                cx,
            )
        })
    }

    fn spawn_live_task(
        self: &Rc<Self>,
        session_id: acp::SessionId,
        live_rx: async_channel::Receiver<proto::AgentSessionEvent>,
        cx: &mut App,
    ) -> Task<()> {
        let this = self.clone();
        cx.spawn(async move |cx| {
            while let Ok(event) = live_rx.recv().await {
                if let Err(error) = this.clone().handle_live_event(&session_id, event, cx).await {
                    log::error!("remote agent session live event failed: {error:?}");
                }
            }
        })
    }

    async fn handle_live_event(
        self: Rc<Self>,
        session_id: &acp::SessionId,
        event: proto::AgentSessionEvent,
        cx: &mut gpui::AsyncApp,
    ) -> Result<()> {
        if event.session_id != session_id.0.as_ref() {
            return Ok(());
        }

        let Some(serialized) = event.event else {
            return Err(anyhow!("agent session event missing event payload"));
        };
        let serialized = serializable_event_from_proto(serialized)?;

        let application = self.prepare_sequence_event(session_id, event.sequence, &serialized)?;
        match application {
            SequenceEvent::Duplicate => {}
            SequenceEvent::Gap => {
                self.resubscribe(session_id.clone(), cx).await?;
            }
            SequenceEvent::Apply { acp_thread, tx } => {
                self.apply_serialized_event(session_id, serialized, acp_thread, tx, cx)?;
            }
        }
        Ok(())
    }

    fn prepare_sequence_event(
        &self,
        session_id: &acp::SessionId,
        sequence: u64,
        event: &SerializableThreadEvent,
    ) -> Result<SequenceEvent> {
        let mut sessions = self.sessions.borrow_mut();
        let session = sessions
            .get_mut(session_id)
            .context("remote agent session not found")?;

        match classify_sequence(&mut session.next_sequence, sequence) {
            SequenceDecision::Duplicate => return Ok(SequenceEvent::Duplicate),
            SequenceDecision::Gap => return Ok(SequenceEvent::Gap),
            SequenceDecision::Apply => {}
        }

        if session.suppress_next_user_message && event.variant == "user_message" {
            session.suppress_next_user_message = false;
            return Ok(SequenceEvent::Duplicate);
        }

        Ok(SequenceEvent::Apply {
            acp_thread: session.acp_thread.clone(),
            tx: session.prompt_events_tx.clone(),
        })
    }

    async fn subscribe_and_apply(
        self: Rc<Self>,
        session_id: acp::SessionId,
        acp_thread: WeakEntity<AcpThread>,
        from_sequence: u64,
        reset_to_snapshot: bool,
        cx: &mut gpui::AsyncApp,
    ) -> Result<()> {
        let response = self
            .proto_client
            .request(proto::SubscribeAgentSession {
                session_id: session_id.0.to_string(),
                from_sequence,
            })
            .await?;
        let reset_to_snapshot = reset_to_snapshot || response.gap;
        self.apply_subscribe_response(session_id, acp_thread, response, reset_to_snapshot, cx)
            .await
    }

    async fn resubscribe(
        self: Rc<Self>,
        session_id: acp::SessionId,
        cx: &mut gpui::AsyncApp,
    ) -> Result<()> {
        let (from_sequence, acp_thread) = {
            let sessions = self.sessions.borrow();
            let session = sessions
                .get(&session_id)
                .context("remote agent session not found")?;
            (session.next_sequence, session.acp_thread.clone())
        };
        self.subscribe_and_apply(session_id, acp_thread, from_sequence, false, cx)
            .await
    }

    async fn apply_subscribe_response(
        &self,
        session_id: acp::SessionId,
        acp_thread: WeakEntity<AcpThread>,
        response: proto::SubscribeAgentSessionResponse,
        reset_to_snapshot: bool,
        cx: &mut gpui::AsyncApp,
    ) -> Result<()> {
        if reset_to_snapshot {
            acp_thread.update(cx, |thread, cx| {
                thread.reset_entries_for_replay(cx);
            })?;
            for event in response.snapshot {
                let event = serializable_event_from_proto(event)?.to_thread_event()?;
                NativeAgentConnection::apply_remote_thread_event(event, &acp_thread, cx)?;
            }
            if let Some(session) = self.sessions.borrow_mut().get_mut(&session_id) {
                session.next_sequence = response.snapshot_sequence;
            }
        }

        for event in response.events {
            let sequence = event.sequence;
            let serialized = event
                .event
                .context("buffered agent session event missing payload")
                .and_then(serializable_event_from_proto)?;
            let application = self.prepare_sequence_event(&session_id, sequence, &serialized)?;
            if let SequenceEvent::Apply { acp_thread, tx } = application {
                self.apply_serialized_event(&session_id, serialized, acp_thread, tx, cx)?;
            }
        }
        Ok(())
    }

    fn apply_serialized_event(
        &self,
        session_id: &acp::SessionId,
        serialized: SerializableThreadEvent,
        acp_thread: WeakEntity<AcpThread>,
        tx: Option<mpsc::UnboundedSender<Result<ThreadEvent>>>,
        cx: &mut gpui::AsyncApp,
    ) -> Result<Option<acp::PromptResponse>> {
        if self.apply_remote_authorization_event(session_id, &serialized, &acp_thread, cx)? {
            return Ok(None);
        }

        let event = serialized.to_thread_event()?;
        if let Some(tx) = tx {
            tx.unbounded_send(Ok(event))
                .map_err(|_| anyhow!("remote prompt event receiver was dropped"))?;
            Ok(None)
        } else {
            NativeAgentConnection::apply_remote_thread_event(event, &acp_thread, cx)
        }
    }

    fn apply_remote_authorization_event(
        &self,
        session_id: &acp::SessionId,
        serialized: &SerializableThreadEvent,
        acp_thread: &WeakEntity<AcpThread>,
        cx: &mut gpui::AsyncApp,
    ) -> Result<bool> {
        match serialized.variant.as_str() {
            AGENT_TOOL_AUTHORIZATION_REQUEST_EVENT => {
                let request =
                    serialized.deserialize_payload::<AgentToolAuthorizationRequestEvent>()?;
                if &request.session_id != session_id {
                    return Err(anyhow!("agent tool authorization session id mismatch"));
                }
                if self.is_approval_resolved(session_id, &request.approval_id) {
                    return Ok(true);
                }

                let outcome_task = acp_thread.update(cx, |thread, cx| {
                    thread.request_tool_call_authorization(
                        request.tool_call,
                        request.options,
                        request.kind,
                        cx,
                    )
                })??;

                let proto_client = self.proto_client.clone();
                let sessions = self.sessions.clone();
                let session_id = session_id.clone();
                let approval_id = request.approval_id;
                cx.spawn(async move |_| {
                    let acp_thread::RequestPermissionOutcome::Selected(outcome) =
                        outcome_task.await
                    else {
                        return;
                    };

                    let already_resolved = sessions
                        .borrow()
                        .get(&session_id)
                        .is_some_and(|session| session.resolved_approvals.contains(&approval_id));
                    if already_resolved {
                        return;
                    }

                    let result = async {
                        let selected_outcome_json = serde_json::to_string(&outcome)
                            .context("serialize selected agent tool authorization outcome")?;
                        proto_client
                            .request(proto::RespondAgentToolAuthorization {
                                session_id: session_id.0.to_string(),
                                approval_id,
                                selected_outcome_json,
                            })
                            .await?;
                        anyhow::Ok(())
                    }
                    .await;
                    result.log_err();
                })
                .detach();
                Ok(true)
            }
            AGENT_TOOL_AUTHORIZATION_RESOLVED_EVENT => {
                let resolved =
                    serialized.deserialize_payload::<AgentToolAuthorizationResolvedEvent>()?;
                if &resolved.session_id != session_id {
                    return Err(anyhow!("agent tool authorization session id mismatch"));
                }
                self.mark_approval_resolved(session_id, resolved.approval_id);
                acp_thread.update(cx, |thread, cx| {
                    thread.authorize_tool_call(resolved.tool_call_id, resolved.outcome, cx);
                })?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn is_approval_resolved(&self, session_id: &acp::SessionId, approval_id: &str) -> bool {
        self.sessions
            .borrow()
            .get(session_id)
            .is_some_and(|session| session.resolved_approvals.contains(approval_id))
    }

    fn mark_approval_resolved(&self, session_id: &acp::SessionId, approval_id: String) {
        if let Some(session) = self.sessions.borrow_mut().get_mut(session_id) {
            session.resolved_approvals.insert(approval_id);
        }
    }

    fn prompt_markdown(params: &[acp::ContentBlock]) -> String {
        params
            .iter()
            .map(|block| match block {
                acp::ContentBlock::Text(text) => text.text.to_string(),
                acp::ContentBlock::ResourceLink(resource) => resource.uri.to_string(),
                acp::ContentBlock::Resource(resource) => match &resource.resource {
                    acp::EmbeddedResourceResource::TextResourceContents(resource) => {
                        resource.text.to_string()
                    }
                    _ => format!("{block:?}"),
                },
                _ => format!("{block:?}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SequenceDecision {
    Duplicate,
    Gap,
    Apply,
}

fn classify_sequence(next_sequence: &mut u64, sequence: u64) -> SequenceDecision {
    if sequence < *next_sequence {
        SequenceDecision::Duplicate
    } else if sequence > *next_sequence {
        SequenceDecision::Gap
    } else {
        *next_sequence = next_sequence.saturating_add(1);
        SequenceDecision::Apply
    }
}

enum SequenceEvent {
    Duplicate,
    Gap,
    Apply {
        acp_thread: WeakEntity<AcpThread>,
        tx: Option<mpsc::UnboundedSender<Result<ThreadEvent>>>,
    },
}

impl AgentConnection for RemoteAgentConnection {
    fn agent_id(&self) -> project_crate::AgentId {
        ZED_AGENT_ID.clone()
    }

    fn telemetry_id(&self) -> SharedString {
        "zed_remote".into()
    }

    fn new_session(
        self: Rc<Self>,
        project: Entity<Project>,
        work_dirs: PathList,
        cx: &mut App,
    ) -> Task<Result<Entity<AcpThread>>> {
        let proto_client = self.proto_client.clone();
        let metadata = ThreadMetadata {
            thread_id: ThreadId::new(),
            session_id: None,
            agent_id: ZED_AGENT_ID.clone(),
            title: None,
            title_override: None,
            parallel_attempt_group: None,
            landed: None,
            updated_at: Utc::now(),
            created_at: Some(Utc::now()),
            interacted_at: None,
            worktree_paths: WorktreePaths::from_folder_paths(&work_dirs),
            remote_connection: None,
            archived: false,
            server_hosted: true,
        };
        cx.spawn(async move |cx| {
            Self::ensure_authenticated(&proto_client, &self.auth_state).await?;
            let response = proto_client
                .request(proto::CreateAgentSession {
                    thread_metadata: Some(metadata.to_proto()?),
                })
                .await?;
            let session_id = acp::SessionId::new(response.session_id);
            let live_rx = project_crate::subscribe_agent_session_events(session_id.0.as_ref());
            let acp_thread =
                cx.update(|cx| self.make_thread(session_id.clone(), project, work_dirs, None, cx));
            let live_task = cx.update(|cx| self.spawn_live_task(session_id.clone(), live_rx, cx));
            self.sessions.borrow_mut().insert(
                session_id.clone(),
                RemoteSession {
                    acp_thread: acp_thread.downgrade(),
                    next_sequence: 0,
                    prompt_events_tx: None,
                    suppress_next_user_message: false,
                    resolved_approvals: HashSet::default(),
                    _live_task: live_task,
                },
            );
            self.subscribe_and_apply(session_id, acp_thread.downgrade(), 0, true, cx)
                .await?;
            Ok(acp_thread)
        })
    }

    fn supports_load_session(&self) -> bool {
        true
    }

    fn load_session(
        self: Rc<Self>,
        session_id: acp::SessionId,
        project: Entity<Project>,
        work_dirs: PathList,
        title: Option<SharedString>,
        cx: &mut App,
    ) -> Task<Result<Entity<AcpThread>>> {
        let live_rx = project_crate::subscribe_agent_session_events(session_id.0.as_ref());
        let proto_client = self.proto_client.clone();
        cx.spawn(async move |cx| {
            Self::ensure_authenticated(&proto_client, &self.auth_state).await?;
            let acp_thread =
                cx.update(|cx| self.make_thread(session_id.clone(), project, work_dirs, title, cx));
            let live_task = cx.update(|cx| self.spawn_live_task(session_id.clone(), live_rx, cx));
            self.sessions.borrow_mut().insert(
                session_id.clone(),
                RemoteSession {
                    acp_thread: acp_thread.downgrade(),
                    next_sequence: 0,
                    prompt_events_tx: None,
                    suppress_next_user_message: false,
                    resolved_approvals: HashSet::default(),
                    _live_task: live_task,
                },
            );
            self.subscribe_and_apply(session_id, acp_thread.downgrade(), 0, true, cx)
                .await?;
            Ok(acp_thread)
        })
    }

    fn auth_methods(&self) -> &[acp::AuthMethod] {
        let auth_state = self.auth_state.borrow();
        let _authenticated_email = auth_state.email.as_deref();
        if auth_state.authenticated {
            &[]
        } else {
            &self.auth_methods
        }
    }

    fn authenticate(&self, method: acp::AuthMethodId, cx: &mut App) -> Task<Result<()>> {
        let proto_client = self.proto_client.clone();
        let auth_state = self.auth_state.clone();
        cx.spawn(async move |cx| {
            if method != acp::AuthMethodId::new(CHATGPT_SUBSCRIPTION_AUTH_METHOD_ID) {
                return Err(anyhow!("unsupported remote agent auth method {method}"));
            }

            let begin = proto_client
                .request(proto::BeginAgentSignIn {})
                .await
                .context("begin remote agent sign-in")?;

            let (redirect_uri, callback_rx) =
                oauth_callback_server::start_oauth_callback_server_with_config(
                    oauth_callback_server::OAuthCallbackServerConfig {
                        host: oauth_callback_server::CODEX_CALLBACK_HOST,
                        preferred_port: oauth_callback_server::CODEX_CALLBACK_PORT,
                        fallback_port: None,
                        path: oauth_callback_server::CODEX_CALLBACK_PATH,
                    },
                )
                .context("start remote agent OAuth callback server")?;
            if redirect_uri != oauth_callback_server::CODEX_CALLBACK_REDIRECT_URI {
                return Err(anyhow!(
                    "unexpected remote agent OAuth redirect URI {redirect_uri}"
                ));
            }

            cx.update(|cx| cx.open_url(&begin.authorize_url));

            let callback = callback_rx
                .await
                .map_err(|_| anyhow!("OAuth callback was cancelled"))?
                .context("OAuth callback failed")?;

            Self::complete_authentication_from_callback(proto_client, auth_state, begin, callback)
                .await
        })
    }

    fn supports_logout(&self) -> bool {
        self.auth_state.borrow().authenticated
    }

    fn logout(&self, cx: &mut App) -> Task<Result<()>> {
        let proto_client = self.proto_client.clone();
        let auth_state = self.auth_state.clone();
        cx.spawn(async move |_| {
            proto_client
                .request(proto::AgentSignOut {})
                .await
                .context("sign out remote agent")?;
            *auth_state.borrow_mut() = RemoteAgentAuthState::default();
            Ok(())
        })
    }

    fn prompt(
        &self,
        _user_message_id: UserMessageId,
        params: acp::PromptRequest,
        cx: &mut App,
    ) -> Task<Result<acp::PromptResponse>> {
        let session_id = params.session_id.clone();
        let prompt_markdown = Self::prompt_markdown(&params.prompt);
        let proto_client = self.proto_client.clone();
        let (tx, mut rx) = mpsc::unbounded();
        if let Some(session) = self.sessions.borrow_mut().get_mut(&session_id) {
            session.prompt_events_tx = Some(tx);
            session.suppress_next_user_message = true;
        }
        let acp_thread = self
            .sessions
            .borrow()
            .get(&session_id)
            .map(|session| session.acp_thread.clone());
        let sessions = self.sessions.clone();
        let auth_state = self.auth_state.clone();
        cx.spawn(async move |cx| {
            Self::ensure_authenticated(&proto_client, &auth_state).await?;
            let acp_thread = acp_thread.context("remote agent session not found")?;
            proto_client
                .request(proto::AgentSessionPrompt {
                    session_id: session_id.0.to_string(),
                    prompt_markdown,
                })
                .await?;

            while let Some(event) = rx.next().await {
                let event = event?;
                if let Some(response) =
                    NativeAgentConnection::apply_remote_thread_event(event, &acp_thread, cx)?
                {
                    if let Some(session) = sessions.borrow_mut().get_mut(&session_id) {
                        session.prompt_events_tx = None;
                        session.suppress_next_user_message = false;
                    }
                    return Ok(response);
                }
            }

            if let Some(session) = sessions.borrow_mut().get_mut(&session_id) {
                session.prompt_events_tx = None;
                session.suppress_next_user_message = false;
            }
            Ok(acp::PromptResponse::new(acp::StopReason::EndTurn))
        })
    }

    fn cancel(&self, session_id: &acp::SessionId, cx: &mut App) {
        let proto_client = self.proto_client.clone();
        let session_id = session_id.clone();
        cx.spawn(async move |_| {
            proto_client
                .request(proto::AgentSessionCancel {
                    session_id: session_id.0.to_string(),
                })
                .await
                .log_err();
        })
        .detach();
    }

    fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
}

fn serializable_event_from_proto(
    event: proto::SerializedAgentThreadEvent,
) -> Result<SerializableThreadEvent> {
    Ok(SerializableThreadEvent {
        variant: event.variant,
        payload: event
            .payload_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("deserialize agent thread event payload")?,
        debug: event.debug,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt as _;
    use gpui::TestAppContext;
    use proto::EnvelopedMessage as _;
    use rpc::{ProtoClient, ProtoMessageHandlerSet};
    use std::sync::Arc;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct CompleteSignInRequest {
        code: String,
        state: String,
    }

    #[derive(Default)]
    struct FakeRemoteAgentProtoClient {
        handler_set: parking_lot::Mutex<ProtoMessageHandlerSet>,
        state: parking_lot::Mutex<FakeRemoteAgentProtoState>,
    }

    #[derive(Default)]
    struct FakeRemoteAgentProtoState {
        authenticated: bool,
        request_types: Vec<String>,
        complete_requests: Vec<CompleteSignInRequest>,
    }

    impl FakeRemoteAgentProtoClient {
        fn new(authenticated: bool) -> Arc<Self> {
            Arc::new(Self {
                handler_set: parking_lot::Mutex::new(ProtoMessageHandlerSet::default()),
                state: parking_lot::Mutex::new(FakeRemoteAgentProtoState {
                    authenticated,
                    ..Default::default()
                }),
            })
        }

        fn any(self: &Arc<Self>) -> AnyProtoClient {
            AnyProtoClient::from(self.clone())
        }

        fn set_authenticated(&self, authenticated: bool) {
            self.state.lock().authenticated = authenticated;
        }

        fn request_types(&self) -> Vec<String> {
            self.state.lock().request_types.clone()
        }

        fn complete_requests(&self) -> Vec<CompleteSignInRequest> {
            self.state.lock().complete_requests.clone()
        }
    }

    impl ProtoClient for FakeRemoteAgentProtoClient {
        fn request(
            &self,
            envelope: proto::Envelope,
            request_type: &'static str,
        ) -> futures::future::BoxFuture<'static, Result<proto::Envelope>> {
            let result = (|| {
                let mut state = self.state.lock();
                state.request_types.push(request_type.to_string());

                match request_type {
                    <proto::GetAgentAuthStatus as proto::EnvelopedMessage>::NAME => {
                        Ok(proto::AgentAuthStatus {
                            authenticated: state.authenticated,
                            email: state
                                .authenticated
                                .then(|| "remote@example.com".to_string()),
                        }
                        .into_envelope(0, None, None))
                    }
                    <proto::BeginAgentSignIn as proto::EnvelopedMessage>::NAME => {
                        Ok(proto::BeginAgentSignInResponse {
                            authorize_url: "https://auth.example.test/authorize?state=test-state"
                                .to_string(),
                            state: "test-state".to_string(),
                        }
                        .into_envelope(0, None, None))
                    }
                    <proto::CompleteAgentSignIn as proto::EnvelopedMessage>::NAME => {
                        let request = proto::CompleteAgentSignIn::from_envelope(envelope)
                            .context("decode CompleteAgentSignIn request")?;
                        state.complete_requests.push(CompleteSignInRequest {
                            code: request.code,
                            state: request.state,
                        });
                        state.authenticated = true;
                        Ok(proto::CompleteAgentSignInResponse {
                            email: Some("remote@example.com".to_string()),
                            account_id: Some("account-id".to_string()),
                        }
                        .into_envelope(0, None, None))
                    }
                    <proto::AgentSignOut as proto::EnvelopedMessage>::NAME => {
                        state.authenticated = false;
                        Ok(proto::Ack {}.into_envelope(0, None, None))
                    }
                    <proto::AgentSessionPrompt as proto::EnvelopedMessage>::NAME => {
                        Ok(proto::Ack {}.into_envelope(0, None, None))
                    }
                    _ => Err(anyhow!("unexpected fake proto request {request_type}")),
                }
            })();

            async move { result }.boxed()
        }

        fn send(&self, _envelope: proto::Envelope, _message_type: &'static str) -> Result<()> {
            Ok(())
        }

        fn send_response(
            &self,
            _envelope: proto::Envelope,
            _message_type: &'static str,
        ) -> Result<()> {
            Ok(())
        }

        fn message_handler_set(&self) -> &parking_lot::Mutex<ProtoMessageHandlerSet> {
            &self.handler_set
        }

        fn is_via_collab(&self) -> bool {
            false
        }

        fn has_wsl_interop(&self) -> bool {
            false
        }
    }

    #[gpui::test]
    async fn auth_methods_follow_remote_auth_status(cx: &mut TestAppContext) {
        if let Err(error) = auth_methods_follow_remote_auth_status_impl(cx).await {
            panic!("{error:?}");
        }
    }

    async fn auth_methods_follow_remote_auth_status_impl(cx: &mut TestAppContext) -> Result<()> {
        let proto_client = FakeRemoteAgentProtoClient::new(false);
        let connection = RemoteAgentConnection::new(proto_client.any());

        cx.update(|cx| connection.refresh_auth_status(cx));
        cx.run_until_parked();
        assert_eq!(connection.auth_methods().len(), 1);
        assert_eq!(
            connection.auth_methods()[0].id(),
            &acp::AuthMethodId::new(CHATGPT_SUBSCRIPTION_AUTH_METHOD_ID)
        );

        proto_client.set_authenticated(true);
        cx.update(|cx| connection.refresh_auth_status(cx));
        cx.run_until_parked();
        assert!(connection.auth_methods().is_empty());

        Ok(())
    }

    #[gpui::test]
    async fn authenticate_drives_begin_callback_complete(cx: &mut TestAppContext) {
        if let Err(error) = authenticate_drives_begin_callback_complete_impl(cx).await {
            panic!("{error:?}");
        }
    }

    async fn authenticate_drives_begin_callback_complete_impl(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        let proto_client = FakeRemoteAgentProtoClient::new(false);
        let connection = RemoteAgentConnection::new(proto_client.any());

        let authenticate = cx.update(|cx| {
            connection.authenticate_with_callback_for_test(
                acp::AuthMethodId::new(CHATGPT_SUBSCRIPTION_AUTH_METHOD_ID),
                oauth_callback_server::OAuthCallbackParams {
                    code: "callback-code".to_string(),
                    state: "test-state".to_string(),
                },
                cx,
            )
        });
        authenticate.await?;

        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://auth.example.test/authorize?state=test-state")
        );
        assert!(connection.auth_methods().is_empty());
        assert!(connection.supports_logout());
        assert_eq!(
            proto_client.complete_requests(),
            vec![CompleteSignInRequest {
                code: "callback-code".to_string(),
                state: "test-state".to_string(),
            }]
        );
        assert_eq!(
            proto_client.request_types(),
            vec![
                <proto::BeginAgentSignIn as proto::EnvelopedMessage>::NAME.to_string(),
                <proto::CompleteAgentSignIn as proto::EnvelopedMessage>::NAME.to_string(),
            ]
        );

        Ok(())
    }

    #[gpui::test]
    async fn prompt_while_unauthenticated_returns_auth_required(cx: &mut TestAppContext) {
        if let Err(error) = prompt_while_unauthenticated_returns_auth_required_impl(cx).await {
            panic!("{error:?}");
        }
    }

    async fn prompt_while_unauthenticated_returns_auth_required_impl(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        let proto_client = FakeRemoteAgentProtoClient::new(false);
        let connection = RemoteAgentConnection::new(proto_client.any());
        let task = cx.update(|cx| {
            connection.prompt(
                UserMessageId::new(),
                acp::PromptRequest::new(
                    acp::SessionId::new("session"),
                    vec![acp::ContentBlock::Text(acp::TextContent::new("hello"))],
                ),
                cx,
            )
        });

        let error = match task.await {
            Ok(_) => return Err(anyhow!("prompt unexpectedly succeeded")),
            Err(error) => error,
        };
        assert!(error.downcast_ref::<acp_thread::AuthRequired>().is_some());
        assert_eq!(
            proto_client.request_types(),
            vec![<proto::GetAgentAuthStatus as proto::EnvelopedMessage>::NAME.to_string()]
        );

        Ok(())
    }

    #[test]
    fn classify_sequence_applies_in_order_and_advances() {
        let mut next_sequence = 7;

        assert_eq!(
            classify_sequence(&mut next_sequence, 7),
            SequenceDecision::Apply
        );
        assert_eq!(next_sequence, 8);
    }

    #[test]
    fn classify_sequence_ignores_duplicates_without_advancing() {
        let mut next_sequence = 7;

        assert_eq!(
            classify_sequence(&mut next_sequence, 6),
            SequenceDecision::Duplicate
        );
        assert_eq!(next_sequence, 7);
    }

    #[test]
    fn classify_sequence_reports_gap_without_advancing() {
        let mut next_sequence = 7;

        assert_eq!(
            classify_sequence(&mut next_sequence, 9),
            SequenceDecision::Gap
        );
        assert_eq!(next_sequence, 7);
    }
}
