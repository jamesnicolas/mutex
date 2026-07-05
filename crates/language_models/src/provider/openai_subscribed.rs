use anyhow::{Context as _, Result, anyhow};
use credentials_provider::CredentialsProvider;
use futures::{FutureExt, StreamExt, future::BoxFuture, future::Shared};
use gpui::{AnyView, App, AsyncApp, Context, Entity, SharedString, Task, Window};
use http_client::{
    CustomHeaders, HttpClient,
    http::{HeaderName, HeaderValue},
};
use language_model::{
    AuthenticateError, FastModeConfirmation, IconOrSvg, LanguageModel,
    LanguageModelCompletionError, LanguageModelCompletionEvent, LanguageModelEffortLevel,
    LanguageModelId, LanguageModelName, LanguageModelProvider, LanguageModelProviderId,
    LanguageModelProviderName, LanguageModelProviderState, LanguageModelRequest,
    LanguageModelToolChoice, RateLimiter,
};
use oauth_callback_server::{CodexCredentials, CodexTokenRefreshError};
use open_ai::{ReasoningEffort, responses::stream_response};
use std::sync::Arc;
use ui::{ConfiguredApiCard, prelude::*};
use util::ResultExt as _;

use crate::provider::open_ai::{OpenAiResponseEventMapper, into_open_ai_response};

#[cfg(test)]
use oauth_callback_server::now_ms;

const PROVIDER_ID: LanguageModelProviderId = LanguageModelProviderId::new("openai-subscribed");
const PROVIDER_NAME: LanguageModelProviderName = LanguageModelProviderName::new("Codex");

const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const CREDENTIALS_KEY: &str = oauth_callback_server::CODEX_CREDENTIALS_KEY;
const TOKEN_REFRESH_BUFFER_MS: u64 = 5 * 60 * 1000;

pub struct State {
    credentials: Option<CodexCredentials>,
    sign_in_task: Option<Task<Result<()>>>,
    refresh_task: Option<Shared<Task<Result<CodexCredentials, Arc<anyhow::Error>>>>>,
    load_task: Option<Shared<Task<Result<(), Arc<anyhow::Error>>>>>,
    credentials_provider: Arc<dyn CredentialsProvider>,
    auth_generation: u64,
    last_auth_error: Option<SharedString>,
}

impl State {
    fn is_authenticated(&self) -> bool {
        self.credentials.is_some()
    }

    fn email(&self) -> Option<&str> {
        self.credentials.as_ref().and_then(|c| c.email.as_deref())
    }

    fn is_signing_in(&self) -> bool {
        self.sign_in_task.is_some()
    }
}

pub struct OpenAiSubscribedProvider {
    http_client: Arc<dyn HttpClient>,
    state: Entity<State>,
}

impl OpenAiSubscribedProvider {
    pub fn new(
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut App,
    ) -> Self {
        let state = cx.new(|_cx| State {
            credentials: None,
            sign_in_task: None,
            refresh_task: None,
            load_task: None,
            credentials_provider,
            auth_generation: 0,
            last_auth_error: None,
        });

        let provider = Self { http_client, state };

        provider.load_credentials(cx);

        provider
    }

    fn load_credentials(&self, cx: &mut App) {
        let state = self.state.downgrade();
        let load_task = cx
            .spawn(async move |cx| {
                let credentials_provider =
                    state.read_with(&*cx, |s, _| s.credentials_provider.clone())?;
                let result = credentials_provider
                    .read_credentials(CREDENTIALS_KEY, &*cx)
                    .await;
                state.update(cx, |s, cx| {
                    if let Ok(Some((_, bytes))) = result {
                        match serde_json::from_slice::<CodexCredentials>(&bytes) {
                            Ok(creds) => s.credentials = Some(creds),
                            Err(err) => {
                                log::warn!(
                                    "Failed to deserialize ChatGPT subscription credentials: {err}"
                                );
                            }
                        }
                    }
                    s.load_task = None;
                    cx.notify();
                })?;
                Ok::<(), Arc<anyhow::Error>>(())
            })
            .shared();

        self.state.update(cx, |s, _| {
            s.load_task = Some(load_task);
        });
    }

    pub fn reload_credentials(&self, cx: &mut App) {
        self.load_credentials(cx);
    }

    fn sign_out(&self, cx: &mut App) -> Task<Result<()>> {
        do_sign_out(&self.state.downgrade(), cx)
    }

    fn create_language_model(&self, model: ChatGptModel) -> Arc<dyn LanguageModel> {
        Arc::new(OpenAiSubscribedLanguageModel {
            id: LanguageModelId::from(model.id().to_string()),
            model,
            state: self.state.clone(),
            http_client: self.http_client.clone(),
            request_limiter: RateLimiter::new(4),
        })
    }
}

impl LanguageModelProviderState for OpenAiSubscribedProvider {
    type ObservableEntity = State;

    fn observable_entity(&self) -> Option<Entity<Self::ObservableEntity>> {
        Some(self.state.clone())
    }
}

impl LanguageModelProvider for OpenAiSubscribedProvider {
    fn id(&self) -> LanguageModelProviderId {
        PROVIDER_ID
    }

    fn name(&self) -> LanguageModelProviderName {
        PROVIDER_NAME
    }

    fn icon(&self) -> IconOrSvg {
        IconOrSvg::Icon(IconName::AiOpenAi)
    }

    fn default_model(&self, _cx: &App) -> Option<Arc<dyn LanguageModel>> {
        Some(self.create_language_model(ChatGptModel::Gpt55))
    }

    fn default_fast_model(&self, _cx: &App) -> Option<Arc<dyn LanguageModel>> {
        // No GPT-5.5 Mini exists yet; per the OpenAI Codex docs, gpt-5.4-mini
        // is the recommended fast/cheap default alongside gpt-5.5.
        Some(self.create_language_model(ChatGptModel::Gpt54Mini))
    }

    fn provided_models(&self, _cx: &App) -> Vec<Arc<dyn LanguageModel>> {
        ChatGptModel::all()
            .into_iter()
            .map(|m| self.create_language_model(m))
            .collect()
    }

    fn is_authenticated(&self, cx: &App) -> bool {
        self.state.read(cx).is_authenticated()
    }

    fn authenticate(&self, cx: &mut App) -> Task<Result<(), AuthenticateError>> {
        if self.is_authenticated(cx) {
            return Task::ready(Ok(()));
        }
        let load_task = self.state.read(cx).load_task.clone();
        if let Some(load_task) = load_task {
            let weak_state = self.state.downgrade();
            cx.spawn(async move |cx| {
                let _ = load_task.await;
                let is_auth = weak_state
                    .read_with(&*cx, |s, _| s.is_authenticated())
                    .unwrap_or(false);
                if is_auth {
                    Ok(())
                } else {
                    Err(anyhow!(
                        "Sign in with your ChatGPT Plus or Pro subscription to use this provider."
                    )
                    .into())
                }
            })
        } else {
            Task::ready(Err(anyhow!(
                "Sign in with your ChatGPT Plus or Pro subscription to use this provider."
            )
            .into()))
        }
    }

    fn configuration_view(
        &self,
        _target_agent: language_model::ConfigurationViewTargetAgent,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        let state = self.state.clone();
        let http_client = self.http_client.clone();
        cx.new(|_cx| ConfigurationView { state, http_client })
            .into()
    }

    fn reset_credentials(&self, cx: &mut App) -> Task<Result<()>> {
        self.sign_out(cx)
    }

    fn authentication_error_message(&self) -> SharedString {
        "Your Codex subscription session is invalid or has expired. \
        Sign in again via the Agent Panel settings to continue."
            .into()
    }

    fn missing_credentials_error_message(&self) -> SharedString {
        "You are not signed in to your ChatGPT account for Codex. \
        Sign in via the Agent Panel settings to continue."
            .into()
    }

    fn fast_mode_confirmation(&self, _cx: &App) -> Option<FastModeConfirmation> {
        Some(FastModeConfirmation {
            title: "Enable Fast Mode for OpenAI?".into(),
            message: "Fast mode sends requests using OpenAI's Priority processing tier, which \
                targets significantly lower latency than the standard tier and is billed at a \
                premium per-token rate."
                .into(),
        })
    }
}

//
// The Codex subscription provider routes requests to chatgpt.com/backend-api/codex,
// which only supports a subset of OpenAI models. This list is maintained separately
// from the standard OpenAI API model list (open_ai::Model).
//
// TODO: The Codex CLI fetches this list dynamically from
// `GET <codex_base_url>/models?client_version=...` (see
// codex-rs/codex-api/src/endpoint/models.rs in openai/codex) and falls back to
// a bundled models.json. Beyond going stale, the static approach also can't
// model per-account access (e.g. free accounts cannot use gpt-5.4 even though
// paid accounts can), so the backend still rejects some requests. The bundled
// list at
// codex-rs/models-manager/models.json (openai/codex) is the closest
// approximation; the entries below mirror that file's picker-visible models.
#[derive(Clone, Debug, PartialEq)]
enum ChatGptModel {
    Gpt55,
    Gpt54,
    Gpt54Mini,
}

impl ChatGptModel {
    fn all() -> Vec<Self> {
        vec![Self::Gpt55, Self::Gpt54, Self::Gpt54Mini]
    }

    fn id(&self) -> &str {
        match self {
            Self::Gpt55 => "gpt-5.5",
            Self::Gpt54 => "gpt-5.4",
            Self::Gpt54Mini => "gpt-5.4-mini",
        }
    }

    fn display_name(&self) -> &str {
        match self {
            Self::Gpt55 => "GPT-5.5",
            Self::Gpt54 => "GPT-5.4",
            Self::Gpt54Mini => "GPT-5.4 Mini",
        }
    }

    fn max_token_count(&self) -> u64 {
        // All Codex-supported models use a 272K context window in the Codex
        // backend, even when the raw model exposes a larger context window via the
        // public API (e.g. gpt-5.4 has max_context_window 1M, but Codex uses
        // context_window 272K). Source: openai/codex models-manager/models.json.
        272_000
    }

    fn max_output_tokens(&self) -> Option<u64> {
        // Codex model metadata does not expose a max output token cap for these
        // models. Source: openai/codex models-manager/models.json.
        None
    }

    fn supports_images(&self) -> bool {
        true
    }

    fn default_reasoning_effort(&self) -> Option<ReasoningEffort> {
        // Codex bundled models all default to Medium reasoning effort.
        Some(ReasoningEffort::Medium)
    }

    fn supported_reasoning_efforts(&self) -> &'static [ReasoningEffort] {
        // The Codex backend's supported_reasoning_levels for every model in this list is low/medium/high/xhigh
        &[
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
        ]
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    fn supports_prompt_cache_key(&self) -> bool {
        true
    }

    fn supports_priority(&self) -> bool {
        match self {
            Self::Gpt55 | Self::Gpt54 => true,
            Self::Gpt54Mini => false,
        }
    }
}

struct OpenAiSubscribedLanguageModel {
    id: LanguageModelId,
    model: ChatGptModel,
    state: Entity<State>,
    http_client: Arc<dyn HttpClient>,
    request_limiter: RateLimiter,
}

impl LanguageModel for OpenAiSubscribedLanguageModel {
    fn id(&self) -> LanguageModelId {
        self.id.clone()
    }

    fn name(&self) -> LanguageModelName {
        LanguageModelName::from(self.model.display_name().to_string())
    }

    fn provider_id(&self) -> LanguageModelProviderId {
        PROVIDER_ID
    }

    fn provider_name(&self) -> LanguageModelProviderName {
        PROVIDER_NAME
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_images(&self) -> bool {
        self.model.supports_images()
    }

    fn supports_tool_choice(&self, _choice: LanguageModelToolChoice) -> bool {
        true
    }

    fn supports_streaming_tools(&self) -> bool {
        true
    }

    fn supports_thinking(&self) -> bool {
        true
    }

    fn supports_fast_mode(&self) -> bool {
        self.model.supports_priority()
    }

    fn supported_effort_levels(&self) -> Vec<LanguageModelEffortLevel> {
        let default_effort = self.model.default_reasoning_effort();
        self.model
            .supported_reasoning_efforts()
            .iter()
            .copied()
            .filter_map(|effort| {
                let (name, value) = match effort {
                    ReasoningEffort::None => return None,
                    ReasoningEffort::Minimal => ("Minimal", "minimal"),
                    ReasoningEffort::Low => ("Low", "low"),
                    ReasoningEffort::Medium => ("Medium", "medium"),
                    ReasoningEffort::High => ("High", "high"),
                    ReasoningEffort::XHigh => ("Extra High", "xhigh"),
                };

                Some(LanguageModelEffortLevel {
                    name: name.into(),
                    value: value.into(),
                    is_default: Some(effort) == default_effort,
                })
            })
            .collect()
    }

    fn telemetry_id(&self) -> String {
        format!("openai-subscribed/{}", self.model.id())
    }

    fn max_token_count(&self) -> u64 {
        self.model.max_token_count()
    }

    fn max_output_tokens(&self) -> Option<u64> {
        self.model.max_output_tokens()
    }

    fn stream_completion(
        &self,
        mut request: LanguageModelRequest,
        cx: &AsyncApp,
    ) -> BoxFuture<
        'static,
        Result<
            futures::stream::BoxStream<
                'static,
                Result<LanguageModelCompletionEvent, LanguageModelCompletionError>,
            >,
            LanguageModelCompletionError,
        >,
    > {
        if !self.model.supports_priority() {
            request.speed = None;
        }

        // The Codex backend rejects `max_output_tokens` (`Unsupported parameter`),
        // unlike the public OpenAI Responses API. Pass `None` so the field is
        // omitted from the serialized request body entirely.
        let mut responses_request = into_open_ai_response(
            request,
            self.model.id(),
            self.model.supports_parallel_tool_calls(),
            self.model.supports_prompt_cache_key(),
            /*max_output_tokens*/ None,
            self.model.default_reasoning_effort(),
            self.model
                .supported_reasoning_efforts()
                .contains(&ReasoningEffort::None),
        );
        responses_request.store = Some(false);

        // The Codex backend requires system messages to be in the top-level
        // `instructions` field rather than as input items.
        let mut instructions = Vec::new();
        responses_request.input.retain(|item| {
            if let open_ai::responses::ResponseInputItem::Message(msg) = item {
                if msg.role == open_ai::Role::System {
                    for part in &msg.content {
                        if let open_ai::responses::ResponseInputContent::Text { text } = part {
                            instructions.push(text.clone());
                        }
                    }
                    return false;
                }
            }
            true
        });
        responses_request.instructions = Some(instructions.join("\n\n"));

        let state = self.state.downgrade();
        let http_client = self.http_client.clone();
        let request_limiter = self.request_limiter.clone();

        let future = cx.spawn(async move |cx| {
            let creds = get_fresh_credentials(&state, &http_client, cx).await?;

            let mut header_pairs: Vec<(HeaderName, HeaderValue)> = vec![
                (
                    HeaderName::from_static("originator"),
                    HeaderValue::from_static("zed"),
                ),
                (
                    HeaderName::from_static("openai-beta"),
                    HeaderValue::from_static("responses=experimental"),
                ),
            ];
            if let Some(ref id) = creds.account_id {
                if !id.is_empty() {
                    if let Ok(value) = HeaderValue::from_str(id) {
                        header_pairs.push((HeaderName::from_static("chatgpt-account-id"), value));
                    }
                }
            }
            let extra_headers = CustomHeaders::new(header_pairs);

            let access_token = creds.access_token.clone();
            request_limiter
                .stream(async move {
                    stream_response(
                        http_client.as_ref(),
                        PROVIDER_NAME.0.as_str(),
                        CODEX_BASE_URL,
                        &access_token,
                        responses_request,
                        &extra_headers,
                    )
                    .await
                    .map_err(LanguageModelCompletionError::from)
                })
                .await
        });

        async move {
            let mapper = OpenAiResponseEventMapper::new();
            Ok(mapper.map_stream(future.await?.boxed()).boxed())
        }
        .boxed()
    }
}

async fn get_fresh_credentials(
    state: &gpui::WeakEntity<State>,
    http_client: &Arc<dyn HttpClient>,
    cx: &mut AsyncApp,
) -> Result<CodexCredentials, LanguageModelCompletionError> {
    let (creds, existing_task) = state
        .read_with(&*cx, |s, _| (s.credentials.clone(), s.refresh_task.clone()))
        .map_err(LanguageModelCompletionError::Other)?;

    let creds = creds.ok_or(LanguageModelCompletionError::NoApiKey {
        provider: PROVIDER_NAME,
    })?;

    if !creds.expires_within(TOKEN_REFRESH_BUFFER_MS) {
        return Ok(creds);
    }

    // If another caller is already refreshing, await their result.
    if let Some(shared_task) = existing_task {
        return shared_task
            .await
            .map_err(|e| LanguageModelCompletionError::Other(anyhow::anyhow!("{e}")));
    }

    // We are the first caller to notice expiry — spawn the refresh task.
    let http_client_clone = http_client.clone();
    let state_clone = state.clone();
    let refresh_token_value = creds.refresh_token.clone();

    // Capture the generation so we can detect sign-outs that happened during refresh.
    let generation = state
        .read_with(&*cx, |s, _| s.auth_generation)
        .map_err(LanguageModelCompletionError::Other)?;

    let shared_task = cx
        .spawn(async move |cx| {
            let result = oauth_callback_server::refresh_codex_token(
                &http_client_clone,
                &refresh_token_value,
            )
            .await;

            match result {
                Ok(refreshed) => {
                    let persist_result: Result<CodexCredentials, Arc<anyhow::Error>> = async {
                        // Check if auth_generation changed (sign-out during refresh).
                        let current_generation = state_clone
                            .read_with(&*cx, |s, _| s.auth_generation)
                            .map_err(|e| Arc::new(e))?;
                        if current_generation != generation {
                            return Err(Arc::new(anyhow!(
                                "Sign-out occurred during token refresh"
                            )));
                        }

                        let credentials_provider = state_clone
                            .read_with(&*cx, |s, _| s.credentials_provider.clone())
                            .map_err(|e| Arc::new(e))?;

                        let json =
                            serde_json::to_vec(&refreshed).map_err(|e| Arc::new(e.into()))?;

                        credentials_provider
                            .write_credentials(CREDENTIALS_KEY, "Bearer", &json, &*cx)
                            .await
                            .map_err(|e| Arc::new(e))?;

                        state_clone
                            .update(cx, |s, _| {
                                s.credentials = Some(refreshed.clone());
                                s.refresh_task = None;
                            })
                            .map_err(|e| Arc::new(e))?;

                        Ok(refreshed)
                    }
                    .await;

                    // Clear refresh_task on failure too.
                    if persist_result.is_err() {
                        let _ = state_clone.update(cx, |s, _| {
                            s.refresh_task = None;
                        });
                    }

                    persist_result
                }
                Err(CodexTokenRefreshError::Fatal(e)) => {
                    log::error!("ChatGPT subscription token refresh failed fatally: {e:?}");
                    let _ = state_clone.update(cx, |s, cx| {
                        s.refresh_task = None;
                        s.credentials = None;
                        s.last_auth_error =
                            Some("Your session has expired. Please sign in again.".into());
                        cx.notify();
                    });
                    // Also clear the keychain so stale credentials aren't loaded next time.
                    if let Ok(credentials_provider) =
                        state_clone.read_with(&*cx, |s, _| s.credentials_provider.clone())
                    {
                        credentials_provider
                            .delete_credentials(CREDENTIALS_KEY, &*cx)
                            .await
                            .log_err();
                    }
                    Err(Arc::new(e))
                }
                Err(CodexTokenRefreshError::Transient(e)) => {
                    log::warn!("ChatGPT subscription token refresh failed transiently: {e:?}");
                    let _ = state_clone.update(cx, |s, _| {
                        s.refresh_task = None;
                    });
                    Err(Arc::new(e))
                }
            }
        })
        .shared();

    // Store the shared task so concurrent callers can join on it.
    state
        .update(cx, |s, _| {
            s.refresh_task = Some(shared_task.clone());
        })
        .map_err(LanguageModelCompletionError::Other)?;

    shared_task
        .await
        .map_err(|e| LanguageModelCompletionError::Other(anyhow::anyhow!("{e}")))
}

async fn do_oauth_flow(
    http_client: Arc<dyn HttpClient>,
    cx: &AsyncApp,
) -> Result<CodexCredentials> {
    let (redirect_uri, callback_rx) =
        oauth_callback_server::start_oauth_callback_server_with_config(
            oauth_callback_server::codex_callback_server_config(),
        )
        .context("Failed to start OAuth callback server")?;
    let oauth_flow = oauth_callback_server::build_codex_oauth_flow(&redirect_uri)
        .context("build ChatGPT subscription authorize URL")?;

    cx.update(|cx| cx.open_url(&oauth_flow.authorize_url));

    let callback = callback_rx
        .await
        .map_err(|_| anyhow!("OAuth callback was cancelled"))?
        .context("OAuth callback failed")?;

    if callback.state != oauth_flow.state {
        return Err(anyhow!("OAuth state mismatch"));
    }

    oauth_callback_server::exchange_codex_code(
        &http_client,
        &callback.code,
        &oauth_flow.verifier,
        &redirect_uri,
    )
    .await
    .context("Token exchange failed")
}

fn do_sign_in(state: &Entity<State>, http_client: &Arc<dyn HttpClient>, cx: &mut App) {
    if state.read(cx).is_signing_in() {
        return;
    }

    let weak_state = state.downgrade();
    let http_client = http_client.clone();

    let task = cx.spawn(async move |cx| {
        match do_oauth_flow(http_client, &*cx).await {
            Ok(creds) => {
                let persist_result = async {
                    let credentials_provider =
                        weak_state.read_with(&*cx, |s, _| s.credentials_provider.clone())?;
                    let json = serde_json::to_vec(&creds)?;
                    credentials_provider
                        .write_credentials(CREDENTIALS_KEY, "Bearer", &json, &*cx)
                        .await?;
                    anyhow::Ok(())
                }
                .await;

                match persist_result {
                    Ok(()) => {
                        weak_state
                            .update(cx, |s, cx| {
                                s.credentials = Some(creds);
                                s.sign_in_task = None;
                                s.last_auth_error = None;
                                cx.notify();
                            })
                            .log_err();
                    }
                    Err(err) => {
                        log::error!(
                            "ChatGPT subscription sign-in failed to persist credentials: {err:?}"
                        );
                        weak_state
                            .update(cx, |s, cx| {
                                s.sign_in_task = None;
                                s.last_auth_error =
                                    Some("Failed to save credentials. Please try again.".into());
                                cx.notify();
                            })
                            .log_err();
                    }
                }
            }
            Err(err) => {
                log::error!("ChatGPT subscription sign-in failed: {err:?}");
                weak_state
                    .update(cx, |s, cx| {
                        s.sign_in_task = None;
                        s.last_auth_error = Some("Sign-in failed. Please try again.".into());
                        cx.notify();
                    })
                    .log_err();
            }
        }
        anyhow::Ok(())
    });

    state.update(cx, |s, cx| {
        s.last_auth_error = None;
        s.sign_in_task = Some(task);
        cx.notify();
    });
}

fn do_sign_out(state: &gpui::WeakEntity<State>, cx: &mut App) -> Task<Result<()>> {
    let weak_state = state.clone();
    // Clear credentials and cancel in-flight work immediately so the UI
    // reflects the sign-out right away.
    weak_state
        .update(cx, |s, cx| {
            s.auth_generation += 1;
            s.credentials = None;
            s.sign_in_task = None;
            s.refresh_task = None;
            s.last_auth_error = None;
            cx.notify();
        })
        .log_err();

    cx.spawn(async move |cx| {
        let credentials_provider =
            weak_state.read_with(&*cx, |s, _| s.credentials_provider.clone())?;
        credentials_provider
            .delete_credentials(CREDENTIALS_KEY, &*cx)
            .await
            .context("Failed to delete ChatGPT subscription credentials from keychain")?;
        anyhow::Ok(())
    })
}

struct ConfigurationView {
    state: Entity<State>,
    http_client: Arc<dyn HttpClient>,
}

impl Render for ConfigurationView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);

        if state.is_authenticated() {
            let label = state
                .email()
                .map(|e| format!("Signed in as {e}"))
                .unwrap_or_else(|| "Signed in".to_string());

            let weak_state = self.state.downgrade();

            return v_flex()
                .child(
                    ConfiguredApiCard::new(SharedString::from(label))
                        .button_label("Sign Out")
                        .on_click(cx.listener(move |_this, _, _window, cx| {
                            do_sign_out(&weak_state, cx).detach_and_log_err(cx);
                        })),
                )
                .into_any_element();
        }

        let last_auth_error = state.last_auth_error.clone();
        let provider_state = self.state.clone();
        let http_client = self.http_client.clone();

        let is_signing_in = state.is_signing_in();
        let button_label = if is_signing_in {
            "Signing in…"
        } else {
            "Sign in to use Codex"
        };

        v_flex()
            .gap_2()
            .child(Label::new(
                "Sign in with your ChatGPT Plus or Pro subscription to use Codex in the agent.",
            ))
            .child(
                Button::new("sign-in", button_label)
                    .full_width()
                    .style(ButtonStyle::Outlined)
                    .loading(is_signing_in)
                    .disabled(is_signing_in)
                    .when(!is_signing_in, |this| {
                        this.start_icon(
                            Icon::new(IconName::AiOpenAi)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                    })
                    .on_click(move |_, _window, cx| {
                        do_sign_in(&provider_state, &http_client, cx);
                    }),
            )
            .when_some(last_auth_error, |this, error| {
                this.child(
                    h_flex()
                        .gap_1()
                        .justify_center()
                        .child(
                            Icon::new(IconName::XCircle)
                                .color(Color::Error)
                                .size(IconSize::Small),
                        )
                        .child(Label::new(error).color(Color::Muted)),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use http_client::FakeHttpClient;
    use parking_lot::Mutex;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeCredentialsProvider {
        storage: Mutex<Option<(String, Vec<u8>)>>,
    }

    impl FakeCredentialsProvider {
        fn new() -> Self {
            Self {
                storage: Mutex::new(None),
            }
        }
    }

    impl CredentialsProvider for FakeCredentialsProvider {
        fn read_credentials<'a>(
            &'a self,
            _url: &'a str,
            _cx: &'a AsyncApp,
        ) -> Pin<Box<dyn Future<Output = Result<Option<(String, Vec<u8>)>>> + 'a>> {
            Box::pin(async { Ok(self.storage.lock().clone()) })
        }

        fn write_credentials<'a>(
            &'a self,
            _url: &'a str,
            username: &'a str,
            password: &'a [u8],
            _cx: &'a AsyncApp,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
            self.storage
                .lock()
                .replace((username.to_string(), password.to_vec()));
            Box::pin(async { Ok(()) })
        }

        fn delete_credentials<'a>(
            &'a self,
            _url: &'a str,
            _cx: &'a AsyncApp,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
            *self.storage.lock() = None;
            Box::pin(async { Ok(()) })
        }
    }

    fn make_expired_credentials() -> CodexCredentials {
        CodexCredentials {
            access_token: "old_access".to_string(),
            refresh_token: "old_refresh".to_string(),
            expires_at_ms: 0,
            account_id: None,
            email: None,
        }
    }

    fn make_fresh_credentials() -> CodexCredentials {
        CodexCredentials {
            access_token: "fresh_access".to_string(),
            refresh_token: "fresh_refresh".to_string(),
            expires_at_ms: now_ms() + 3_600_000,
            account_id: None,
            email: None,
        }
    }

    fn fake_token_response() -> String {
        serde_json::json!({
            "access_token": "fresh_access",
            "refresh_token": "fresh_refresh",
            "expires_in": 3600
        })
        .to_string()
    }

    #[gpui::test]
    async fn test_concurrent_refresh_deduplicates(cx: &mut TestAppContext) {
        let refresh_count = Arc::new(AtomicUsize::new(0));
        let refresh_count_clone = refresh_count.clone();

        let http_client = FakeHttpClient::create(move |_request| {
            let refresh_count = refresh_count_clone.clone();
            async move {
                refresh_count.fetch_add(1, Ordering::SeqCst);
                let body = fake_token_response();
                Ok(http_client::Response::builder()
                    .status(200)
                    .body(http_client::AsyncBody::from(body))?)
            }
        });

        let state = cx.new(|_cx| State {
            credentials: Some(make_expired_credentials()),
            sign_in_task: None,
            refresh_task: None,
            load_task: None,
            credentials_provider: Arc::new(FakeCredentialsProvider::new()),
            auth_generation: 0,
            last_auth_error: None,
        });

        let weak_state = cx.read(|_cx| state.downgrade());
        let http: Arc<dyn HttpClient> = http_client;

        // Spawn two concurrent refresh attempts.
        let weak1 = weak_state.clone();
        let http1 = http.clone();
        let task1 =
            cx.spawn(async move |mut cx| get_fresh_credentials(&weak1, &http1, &mut cx).await);

        let weak2 = weak_state.clone();
        let http2 = http.clone();
        let task2 =
            cx.spawn(async move |mut cx| get_fresh_credentials(&weak2, &http2, &mut cx).await);

        // Drive both to completion.
        cx.run_until_parked();
        let result1 = task1.await;
        let result2 = task2.await;

        assert!(result1.is_ok(), "first refresh should succeed");
        assert!(result2.is_ok(), "second refresh should succeed");
        assert_eq!(result1.unwrap().access_token, "fresh_access");
        assert_eq!(result2.unwrap().access_token, "fresh_access");
        assert_eq!(
            refresh_count.load(Ordering::SeqCst),
            1,
            "refresh_token should only be called once despite two concurrent callers"
        );
    }

    #[gpui::test]
    async fn test_fresh_credentials_skip_refresh(cx: &mut TestAppContext) {
        let refresh_count = Arc::new(AtomicUsize::new(0));
        let refresh_count_clone = refresh_count.clone();

        let http_client = FakeHttpClient::create(move |_request| {
            let refresh_count = refresh_count_clone.clone();
            async move {
                refresh_count.fetch_add(1, Ordering::SeqCst);
                let body = fake_token_response();
                Ok(http_client::Response::builder()
                    .status(200)
                    .body(http_client::AsyncBody::from(body))?)
            }
        });

        let state = cx.new(|_cx| State {
            credentials: Some(make_fresh_credentials()),
            sign_in_task: None,
            refresh_task: None,
            load_task: None,
            credentials_provider: Arc::new(FakeCredentialsProvider::new()),
            auth_generation: 0,
            last_auth_error: None,
        });

        let weak_state = cx.read(|_cx| state.downgrade());
        let http: Arc<dyn HttpClient> = http_client;

        let weak = weak_state.clone();
        let http_clone = http.clone();
        let result = cx
            .spawn(async move |mut cx| get_fresh_credentials(&weak, &http_clone, &mut cx).await)
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().access_token, "fresh_access");
        assert_eq!(
            refresh_count.load(Ordering::SeqCst),
            0,
            "no refresh should happen when credentials are fresh"
        );
    }

    #[gpui::test]
    async fn test_no_credentials_returns_no_api_key(cx: &mut TestAppContext) {
        let http_client = FakeHttpClient::create(|_| async {
            Ok(http_client::Response::builder()
                .status(200)
                .body(http_client::AsyncBody::default())?)
        });

        let state = cx.new(|_cx| State {
            credentials: None,
            sign_in_task: None,
            refresh_task: None,
            load_task: None,
            credentials_provider: Arc::new(FakeCredentialsProvider::new()),
            auth_generation: 0,
            last_auth_error: None,
        });

        let weak_state = cx.read(|_cx| state.downgrade());
        let http: Arc<dyn HttpClient> = http_client;

        let weak = weak_state.clone();
        let http_clone = http.clone();
        let result = cx
            .spawn(async move |mut cx| get_fresh_credentials(&weak, &http_clone, &mut cx).await)
            .await;

        assert!(matches!(
            result,
            Err(LanguageModelCompletionError::NoApiKey { .. })
        ));
    }

    #[gpui::test]
    async fn test_fatal_refresh_clears_auth_state(cx: &mut TestAppContext) {
        let http_client = FakeHttpClient::create(move |_request| async move {
            Ok(http_client::Response::builder()
                .status(401)
                .body(http_client::AsyncBody::from(r#"{"error":"invalid_grant"}"#))?)
        });

        let creds_provider = Arc::new(FakeCredentialsProvider::new());
        let state = cx.new(|_cx| State {
            credentials: Some(make_expired_credentials()),
            sign_in_task: None,
            refresh_task: None,
            load_task: None,
            credentials_provider: creds_provider.clone(),
            auth_generation: 0,
            last_auth_error: None,
        });

        let weak_state = cx.read(|_cx| state.downgrade());
        let http: Arc<dyn HttpClient> = http_client;

        let weak = weak_state.clone();
        let http_clone = http.clone();
        let result = cx
            .spawn(async move |mut cx| get_fresh_credentials(&weak, &http_clone, &mut cx).await)
            .await;

        cx.run_until_parked();

        assert!(result.is_err(), "fatal refresh should return an error");
        cx.read(|cx| {
            let s = state.read(cx);
            assert!(
                s.credentials.is_none(),
                "credentials should be cleared on fatal refresh failure"
            );
            assert!(
                s.last_auth_error.is_some(),
                "last_auth_error should be set on fatal refresh failure"
            );
        });
    }

    #[gpui::test]
    async fn test_transient_refresh_keeps_credentials(cx: &mut TestAppContext) {
        let http_client = FakeHttpClient::create(move |_request| async move {
            Ok(http_client::Response::builder()
                .status(500)
                .body(http_client::AsyncBody::from("Internal Server Error"))?)
        });

        let state = cx.new(|_cx| State {
            credentials: Some(make_expired_credentials()),
            sign_in_task: None,
            refresh_task: None,
            load_task: None,
            credentials_provider: Arc::new(FakeCredentialsProvider::new()),
            auth_generation: 0,
            last_auth_error: None,
        });

        let weak_state = cx.read(|_cx| state.downgrade());
        let http: Arc<dyn HttpClient> = http_client;

        let weak = weak_state.clone();
        let http_clone = http.clone();
        let result = cx
            .spawn(async move |mut cx| get_fresh_credentials(&weak, &http_clone, &mut cx).await)
            .await;

        cx.run_until_parked();

        assert!(result.is_err(), "transient refresh should return an error");
        cx.read(|cx| {
            let s = state.read(cx);
            assert!(
                s.credentials.is_some(),
                "credentials should be kept on transient refresh failure"
            );
            assert!(
                s.last_auth_error.is_none(),
                "last_auth_error should not be set on transient refresh failure"
            );
        });
    }

    #[gpui::test]
    async fn test_sign_out_during_refresh_discards_result(cx: &mut TestAppContext) {
        let (gate_tx, gate_rx) = futures::channel::oneshot::channel::<()>();
        let gate_rx = Arc::new(Mutex::new(Some(gate_rx)));
        let gate_rx_clone = gate_rx.clone();

        let http_client = FakeHttpClient::create(move |_request| {
            let gate_rx = gate_rx_clone.clone();
            async move {
                // Wait until the gate is opened, simulating a slow network.
                let rx = gate_rx.lock().take();
                if let Some(rx) = rx {
                    let _ = rx.await;
                }
                let body = fake_token_response();
                Ok(http_client::Response::builder()
                    .status(200)
                    .body(http_client::AsyncBody::from(body))?)
            }
        });

        let creds_provider = Arc::new(FakeCredentialsProvider::new());
        let state = cx.new(|_cx| State {
            credentials: Some(make_expired_credentials()),
            sign_in_task: None,
            refresh_task: None,
            load_task: None,
            credentials_provider: creds_provider.clone(),
            auth_generation: 0,
            last_auth_error: None,
        });

        let weak_state = cx.read(|_cx| state.downgrade());
        let http: Arc<dyn HttpClient> = http_client;

        // Start a refresh
        let weak = weak_state.clone();
        let http_clone = http.clone();
        let refresh_task =
            cx.spawn(async move |mut cx| get_fresh_credentials(&weak, &http_clone, &mut cx).await);

        cx.run_until_parked();

        // Sign out while the refresh is in-flight
        cx.update(|cx| {
            do_sign_out(&weak_state, cx).detach();
        });
        cx.run_until_parked();

        // Now let the refresh respond by opening the gate
        let _ = gate_tx.send(());
        cx.run_until_parked();

        let result = refresh_task.await;
        assert!(result.is_err(), "refresh should fail after sign-out");

        cx.read(|cx| {
            let s = state.read(cx);
            assert!(
                s.credentials.is_none(),
                "sign-out should have cleared credentials"
            );
        });
    }

    #[gpui::test]
    async fn test_sign_out_completes_fully(cx: &mut TestAppContext) {
        let creds_provider = Arc::new(FakeCredentialsProvider::new());
        // Pre-populate the credential store
        creds_provider
            .storage
            .lock()
            .replace(("Bearer".to_string(), b"some-creds".to_vec()));

        let state = cx.new(|_cx| State {
            credentials: Some(make_fresh_credentials()),
            sign_in_task: None,
            refresh_task: None,
            load_task: None,
            credentials_provider: creds_provider.clone(),
            auth_generation: 0,
            last_auth_error: None,
        });

        let weak_state = cx.read(|_cx| state.downgrade());
        let sign_out_task = cx.update(|cx| do_sign_out(&weak_state, cx));

        cx.run_until_parked();
        sign_out_task.await.expect("sign-out should succeed");

        assert!(
            creds_provider.storage.lock().is_none(),
            "credential store should be empty after sign-out"
        );
        cx.read(|cx| {
            assert!(
                !state.read(cx).is_authenticated(),
                "state should show not authenticated"
            );
        });
    }

    #[gpui::test]
    async fn test_authenticate_awaits_initial_load(cx: &mut TestAppContext) {
        let creds = make_fresh_credentials();
        let creds_json = serde_json::to_vec(&creds).unwrap();
        let creds_provider = Arc::new(FakeCredentialsProvider::new());
        creds_provider
            .storage
            .lock()
            .replace(("Bearer".to_string(), creds_json));

        let http_client = FakeHttpClient::create(|_| async {
            Ok(http_client::Response::builder()
                .status(200)
                .body(http_client::AsyncBody::default())?)
        });

        let provider =
            cx.update(|cx| OpenAiSubscribedProvider::new(http_client, creds_provider, cx));

        // Before load completes, authenticate should still await the load.
        let auth_task = cx.update(|cx| provider.authenticate(cx));

        // Drive the load to completion.
        cx.run_until_parked();

        let result = auth_task.await;
        assert!(
            result.is_ok(),
            "authenticate should succeed after load completes with valid credentials"
        );
    }
}
