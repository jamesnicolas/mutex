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

#[derive(Clone)]
pub struct RemoteAgentConnection {
    proto_client: AnyProtoClient,
    sessions: Rc<RefCell<HashMap<acp::SessionId, RemoteSession>>>,
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
            sessions: Rc::new(RefCell::new(HashMap::default())),
        }
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
        cx.spawn(async move |cx| {
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
        &[]
    }

    fn authenticate(&self, _method: acp::AuthMethodId, _cx: &mut App) -> Task<Result<()>> {
        Task::ready(Ok(()))
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
        cx.spawn(async move |cx| {
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
