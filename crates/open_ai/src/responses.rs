use anyhow::{Result, anyhow};
use futures::{AsyncBufReadExt, AsyncReadExt, StreamExt, io::BufReader, stream::BoxStream};
use http_client::{
    AsyncBody, CustomHeaders, HttpClient, Method, Request as HttpRequest, RequestBuilderExt,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::{ReasoningEffort, RequestError, Role, ServiceTier, ToolChoice};

#[derive(Serialize, Debug)]
pub struct Request {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<ResponseInputItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<ResponseIncludable>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_management: Option<Vec<ContextManagement>>,
}

/// Server-side context management configuration.
///
/// <https://developers.openai.com/api/docs/guides/compaction>
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextManagement {
    Compaction { compact_threshold: u64 },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseIncludable {
    #[serde(rename = "reasoning.encrypted_content")]
    ReasoningEncryptedContent,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseInputItem {
    Message(ResponseMessageItem),
    FunctionCall(ResponseFunctionCallItem),
    FunctionCallOutput(ResponseFunctionCallOutputItem),
    Reasoning(ResponseReasoningInputItem),
    Compaction(ResponseCompactionItem),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseCompactionItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Arc<str>>,
    pub encrypted_content: Arc<str>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResponseMessageItem {
    pub role: Role,
    pub content: Vec<ResponseInputContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResponseFunctionCallItem {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResponseFunctionCallOutputItem {
    pub call_id: String,
    pub output: ResponseFunctionCallOutputContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseReasoningInputItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub summary: Vec<ResponseReasoningSummaryPart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseReasoningSummaryPart {
    SummaryText { text: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseFunctionCallOutputContent {
    List(Vec<ResponseInputContent>),
    Text(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseInputContent {
    #[serde(rename = "input_text")]
    Text { text: String },
    #[serde(rename = "input_image")]
    Image { image_url: String },
    #[serde(rename = "output_text")]
    OutputText {
        text: String,
        #[serde(default)]
        annotations: Vec<serde_json::Value>,
    },
    #[serde(rename = "refusal")]
    Refusal { refusal: String },
}

#[derive(Serialize, Debug)]
pub struct ReasoningConfig {
    pub effort: ReasoningEffort,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummaryMode>,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningSummaryMode {
    Auto,
    Concise,
    Detailed,
}

#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolDefinition {
    Function {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parameters: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResponseError {
    #[serde(default)]
    pub code: Option<String>,
    pub message: String,
    #[serde(default)]
    pub param: Option<Value>,
}

/// Payload of the top-level `error` SSE event from the Responses API.
///
/// OpenAI's spec documents the error fields as being at the top level of the
/// event, but in practice the API often nests them under an `error` object.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct GenericStreamErrorPayload {
    #[serde(flatten)]
    top_level: PartialResponseError,
    #[serde(default)]
    error: Option<PartialResponseError>,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct PartialResponseError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    param: Option<Value>,
}

#[derive(Deserialize, Debug, Default)]
struct HttpErrorLogBody {
    #[serde(default)]
    error: Option<HttpErrorLogObject>,
}

#[derive(Deserialize, Debug, Default)]
struct HttpErrorLogObject {
    #[serde(default)]
    code: Option<String>,
    #[serde(default, rename = "type")]
    error_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    param: Option<Value>,
}

impl GenericStreamErrorPayload {
    pub fn into_response_error(self) -> ResponseError {
        let nested = self.error.unwrap_or_default();
        ResponseError {
            code: self.top_level.code.or(nested.code),
            message: self
                .top_level
                .message
                .or(nested.message)
                .unwrap_or_default(),
            param: self.top_level.param.or(nested.param),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "response.created")]
    Created { response: ResponseSummary },
    #[serde(rename = "response.in_progress")]
    InProgress { response: ResponseSummary },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        output_index: usize,
        #[serde(default)]
        sequence_number: Option<u64>,
        item: ResponseOutputItem,
    },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        output_index: usize,
        #[serde(default)]
        sequence_number: Option<u64>,
        item: ResponseOutputItem,
    },
    #[serde(rename = "response.content_part.added")]
    ContentPartAdded {
        item_id: String,
        output_index: usize,
        content_index: usize,
        part: Value,
    },
    #[serde(rename = "response.content_part.done")]
    ContentPartDone {
        item_id: String,
        output_index: usize,
        content_index: usize,
        part: Value,
    },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta {
        item_id: String,
        output_index: usize,
        #[serde(default)]
        content_index: Option<usize>,
        delta: String,
    },
    #[serde(rename = "response.output_text.done")]
    OutputTextDone {
        item_id: String,
        output_index: usize,
        #[serde(default)]
        content_index: Option<usize>,
        text: String,
    },
    #[serde(rename = "response.refusal.delta")]
    RefusalDelta {
        item_id: String,
        output_index: usize,
        content_index: usize,
        delta: String,
        #[serde(default)]
        sequence_number: Option<u64>,
    },
    #[serde(rename = "response.refusal.done")]
    RefusalDone {
        item_id: String,
        output_index: usize,
        content_index: usize,
        refusal: String,
        #[serde(default)]
        sequence_number: Option<u64>,
    },
    #[serde(rename = "response.reasoning_summary_part.added")]
    ReasoningSummaryPartAdded {
        item_id: String,
        output_index: usize,
        summary_index: usize,
    },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta {
        item_id: String,
        output_index: usize,
        delta: String,
    },
    #[serde(rename = "response.reasoning_summary_text.done")]
    ReasoningSummaryTextDone {
        item_id: String,
        output_index: usize,
        text: String,
    },
    #[serde(rename = "response.reasoning_summary_part.done")]
    ReasoningSummaryPartDone {
        item_id: String,
        output_index: usize,
        summary_index: usize,
    },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta {
        item_id: String,
        output_index: usize,
        delta: String,
        #[serde(default)]
        sequence_number: Option<u64>,
    },
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgumentsDone {
        item_id: String,
        output_index: usize,
        arguments: String,
        #[serde(default)]
        sequence_number: Option<u64>,
    },
    #[serde(rename = "response.completed")]
    Completed { response: ResponseSummary },
    #[serde(rename = "response.incomplete")]
    Incomplete { response: ResponseSummary },
    #[serde(rename = "response.failed")]
    Failed { response: ResponseSummary },
    #[serde(rename = "response.error")]
    Error { error: ResponseError },
    #[serde(rename = "error")]
    GenericError {
        #[serde(flatten)]
        error: GenericStreamErrorPayload,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct ResponseSummary {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub incomplete_details: Option<ResponseIncompleteDetails>,
    #[serde(default)]
    pub error: Option<ResponseError>,
    #[serde(default)]
    pub usage: Option<ResponseUsage>,
    #[serde(default)]
    pub output: Vec<ResponseOutputItem>,
    #[serde(default)]
    pub service_tier: Option<crate::ServiceTier>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct ResponseIncompleteDetails {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct ResponseUsage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub input_tokens_details: ResponseInputTokensDetails,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens_details: ResponseOutputTokensDetails,
    #[serde(default)]
    pub total_tokens: Option<u64>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct ResponseInputTokensDetails {
    #[serde(default)]
    pub cached_tokens: u64,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct ResponseOutputTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: u64,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseOutputItem {
    Message(ResponseOutputMessage),
    FunctionCall(ResponseFunctionToolCall),
    Reasoning(ResponseReasoningItem),
    Compaction(ResponseCompactionItem),
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResponseReasoningItem {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub summary: Vec<ReasoningSummaryPart>,
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default)]
    pub encrypted_content: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningSummaryPart {
    SummaryText {
        text: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResponseOutputMessage {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub phase: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResponseFunctionToolCall {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub arguments: String,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

pub async fn stream_response(
    client: &dyn HttpClient,
    provider_name: &str,
    api_url: &str,
    api_key: &str,
    request: Request,
    extra_headers: &CustomHeaders,
) -> Result<BoxStream<'static, Result<StreamEvent>>, RequestError> {
    let uri = format!("{api_url}/responses");
    let is_streaming = request.stream;
    log::debug!(
        "OpenAI responses request started: provider={provider_name} endpoint={uri} stream={is_streaming}"
    );
    let request = HttpRequest::builder()
        .method(Method::POST)
        .uri(uri.clone())
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .extra_headers(extra_headers)
        .body(AsyncBody::from(
            serde_json::to_string(&request).map_err(|e| RequestError::Other(e.into()))?,
        ))
        .map_err(|e| RequestError::Other(e.into()))?;

    let mut response = client.send(request).await?;
    if response.status().is_success() {
        if is_streaming {
            let reader = BufReader::new(response.into_body());
            let provider_name = provider_name.to_owned();
            let uri = uri.clone();
            Ok(reader
                .lines()
                .filter_map(move |line| {
                    let provider_name = provider_name.clone();
                    let uri = uri.clone();
                    async move {
                    match line {
                        Ok(line) => {
                            let line = line
                                .strip_prefix("data: ")
                                .or_else(|| line.strip_prefix("data:"))?;
                            if line == "[DONE]" {
                                log::debug!(
                                    "OpenAI responses stream done marker: provider={provider_name} endpoint={uri}"
                                );
                                None
                            } else if line.is_empty() {
                                None
                            } else {
                                match serde_json::from_str::<StreamEvent>(line) {
                                    Ok(event) => {
                                        let event_summary = stream_event_log_summary(&event);
                                        if stream_event_is_high_volume(&event) {
                                            log::trace!(
                                                "OpenAI responses stream event: provider={provider_name} endpoint={uri} {event_summary}"
                                            );
                                        } else {
                                            log::debug!(
                                                "OpenAI responses stream event: provider={provider_name} endpoint={uri} {event_summary}"
                                            );
                                        }
                                        Some(Ok(event))
                                    }
                                    Err(error) => {
                                        log::error!(
                                            "Failed to parse OpenAI responses stream event: `{}`\nResponse: `{}`",
                                            error,
                                            line,
                                        );
                                        Some(Err(anyhow!(error)))
                                    }
                                }
                            }
                        }
                        Err(error) => Some(Err(anyhow!(error))),
                    }
                    }
                })
                .boxed())
        } else {
            let mut body = String::new();
            response
                .body_mut()
                .read_to_string(&mut body)
                .await
                .map_err(|e| RequestError::Other(e.into()))?;

            match serde_json::from_str::<ResponseSummary>(&body) {
                Ok(response_summary) => {
                    log::debug!(
                        "OpenAI responses non-streaming response: provider={provider_name} endpoint={uri} {}",
                        response_summary_log_summary(&response_summary)
                    );
                    let events = vec![
                        StreamEvent::Created {
                            response: response_summary.clone(),
                        },
                        StreamEvent::InProgress {
                            response: response_summary.clone(),
                        },
                    ];

                    let mut all_events = events;
                    for (output_index, item) in response_summary.output.iter().enumerate() {
                        all_events.push(StreamEvent::OutputItemAdded {
                            output_index,
                            sequence_number: None,
                            item: item.clone(),
                        });

                        match item {
                            ResponseOutputItem::Message(message) => {
                                for content_item in &message.content {
                                    if let Some(text) = content_item.get("text") {
                                        if let Some(text_str) = text.as_str() {
                                            if let Some(ref item_id) = message.id {
                                                all_events.push(StreamEvent::OutputTextDelta {
                                                    item_id: item_id.clone(),
                                                    output_index,
                                                    content_index: None,
                                                    delta: text_str.to_string(),
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                            ResponseOutputItem::FunctionCall(function_call) => {
                                if let Some(ref item_id) = function_call.id {
                                    all_events.push(StreamEvent::FunctionCallArgumentsDone {
                                        item_id: item_id.clone(),
                                        output_index,
                                        arguments: function_call.arguments.clone(),
                                        sequence_number: None,
                                    });
                                }
                            }
                            ResponseOutputItem::Reasoning(reasoning) => {
                                if let Some(ref item_id) = reasoning.id {
                                    for part in &reasoning.summary {
                                        if let ReasoningSummaryPart::SummaryText { text } = part {
                                            all_events.push(
                                                StreamEvent::ReasoningSummaryTextDelta {
                                                    item_id: item_id.clone(),
                                                    output_index,
                                                    delta: text.clone(),
                                                },
                                            );
                                        }
                                    }
                                }
                            }
                            // No synthesized deltas; the `OutputItemDone`
                            // event pushed below carries the full item.
                            ResponseOutputItem::Compaction(_) => {}
                            ResponseOutputItem::Unknown => {}
                        }

                        all_events.push(StreamEvent::OutputItemDone {
                            output_index,
                            sequence_number: None,
                            item: item.clone(),
                        });
                    }

                    let status = response_summary.status.clone();
                    all_events.push(match status.as_deref() {
                        Some("incomplete") => StreamEvent::Incomplete {
                            response: response_summary,
                        },
                        Some("failed") => StreamEvent::Failed {
                            response: response_summary,
                        },
                        _ => StreamEvent::Completed {
                            response: response_summary,
                        },
                    });

                    Ok(futures::stream::iter(all_events.into_iter().map(Ok)).boxed())
                }
                Err(error) => {
                    log::error!(
                        "Failed to parse OpenAI non-streaming response: `{}`\nResponse: `{}`",
                        error,
                        body,
                    );
                    Err(RequestError::Other(anyhow!(error)))
                }
            }
        }
    } else {
        let mut body = String::new();
        response
            .body_mut()
            .read_to_string(&mut body)
            .await
            .map_err(|e| RequestError::Other(e.into()))?;
        log::debug!(
            "OpenAI responses request failed: provider={provider_name} endpoint={uri} status={} {}",
            response.status(),
            http_error_body_log_summary(&body)
        );

        Err(RequestError::HttpResponseError {
            provider: provider_name.to_owned(),
            status_code: response.status(),
            body,
            headers: response.headers().clone(),
        })
    }
}

fn stream_event_log_summary(event: &StreamEvent) -> String {
    match event {
        StreamEvent::Created { response } => {
            format!(
                "event=response.created {}",
                response_summary_log_summary(response)
            )
        }
        StreamEvent::InProgress { response } => {
            format!(
                "event=response.in_progress {}",
                response_summary_log_summary(response)
            )
        }
        StreamEvent::OutputItemAdded {
            output_index,
            sequence_number,
            item,
        } => format!(
            "event=response.output_item.added output_index={output_index} sequence_number={} item_type={}",
            optional_number(sequence_number),
            response_output_item_kind(item)
        ),
        StreamEvent::OutputItemDone {
            output_index,
            sequence_number,
            item,
        } => format!(
            "event=response.output_item.done output_index={output_index} sequence_number={} item_type={}",
            optional_number(sequence_number),
            response_output_item_kind(item)
        ),
        StreamEvent::ContentPartAdded {
            output_index,
            content_index,
            ..
        } => format!(
            "event=response.content_part.added output_index={output_index} content_index={content_index}"
        ),
        StreamEvent::ContentPartDone {
            output_index,
            content_index,
            ..
        } => format!(
            "event=response.content_part.done output_index={output_index} content_index={content_index}"
        ),
        StreamEvent::OutputTextDelta {
            output_index,
            content_index,
            delta,
            ..
        } => format!(
            "event=response.output_text.delta output_index={output_index} content_index={} delta_chars={}",
            optional_number(content_index),
            delta.chars().count()
        ),
        StreamEvent::OutputTextDone {
            output_index,
            content_index,
            text,
            ..
        } => format!(
            "event=response.output_text.done output_index={output_index} content_index={} text_chars={}",
            optional_number(content_index),
            text.chars().count()
        ),
        StreamEvent::RefusalDelta {
            output_index,
            content_index,
            sequence_number,
            delta,
            ..
        } => format!(
            "event=response.refusal.delta output_index={output_index} content_index={content_index} sequence_number={} delta_chars={}",
            optional_number(sequence_number),
            delta.chars().count()
        ),
        StreamEvent::RefusalDone {
            output_index,
            content_index,
            sequence_number,
            refusal,
            ..
        } => format!(
            "event=response.refusal.done output_index={output_index} content_index={content_index} sequence_number={} refusal_chars={}",
            optional_number(sequence_number),
            refusal.chars().count()
        ),
        StreamEvent::ReasoningSummaryPartAdded {
            output_index,
            summary_index,
            ..
        } => format!(
            "event=response.reasoning_summary_part.added output_index={output_index} summary_index={summary_index}"
        ),
        StreamEvent::ReasoningSummaryTextDelta {
            output_index,
            delta,
            ..
        } => format!(
            "event=response.reasoning_summary_text.delta output_index={output_index} delta_chars={}",
            delta.chars().count()
        ),
        StreamEvent::ReasoningSummaryTextDone {
            output_index, text, ..
        } => format!(
            "event=response.reasoning_summary_text.done output_index={output_index} text_chars={}",
            text.chars().count()
        ),
        StreamEvent::ReasoningSummaryPartDone {
            output_index,
            summary_index,
            ..
        } => format!(
            "event=response.reasoning_summary_part.done output_index={output_index} summary_index={summary_index}"
        ),
        StreamEvent::FunctionCallArgumentsDelta {
            output_index,
            sequence_number,
            delta,
            ..
        } => format!(
            "event=response.function_call_arguments.delta output_index={output_index} sequence_number={} delta_chars={}",
            optional_number(sequence_number),
            delta.chars().count()
        ),
        StreamEvent::FunctionCallArgumentsDone {
            output_index,
            sequence_number,
            arguments,
            ..
        } => format!(
            "event=response.function_call_arguments.done output_index={output_index} sequence_number={} arguments_chars={}",
            optional_number(sequence_number),
            arguments.chars().count()
        ),
        StreamEvent::Completed { response } => {
            format!(
                "event=response.completed {}",
                response_summary_log_summary(response)
            )
        }
        StreamEvent::Incomplete { response } => {
            format!(
                "event=response.incomplete {}",
                response_summary_log_summary(response)
            )
        }
        StreamEvent::Failed { response } => {
            format!(
                "event=response.failed {}",
                response_summary_log_summary(response)
            )
        }
        StreamEvent::Error { error } => {
            format!("event=response.error {}", response_error_log_summary(error))
        }
        StreamEvent::GenericError { error } => {
            let error = error.clone().into_response_error();
            format!("event=error {}", response_error_log_summary(&error))
        }
        StreamEvent::Unknown => "event=unknown".to_string(),
    }
}

fn stream_event_is_high_volume(event: &StreamEvent) -> bool {
    matches!(
        event,
        StreamEvent::OutputTextDelta { .. }
            | StreamEvent::RefusalDelta { .. }
            | StreamEvent::ReasoningSummaryTextDelta { .. }
            | StreamEvent::FunctionCallArgumentsDelta { .. }
    )
}

fn response_summary_log_summary(response: &ResponseSummary) -> String {
    let status = response.status.as_deref().unwrap_or("none");
    let response_id = response.id.as_deref().unwrap_or("none");
    let incomplete_reason = response
        .incomplete_details
        .as_ref()
        .and_then(|details| details.reason.as_deref())
        .unwrap_or("none");
    let error = response
        .error
        .as_ref()
        .map(response_error_log_summary)
        .unwrap_or_else(|| "none".to_string());
    let usage = response
        .usage
        .as_ref()
        .map(response_usage_log_summary)
        .unwrap_or_else(|| "none".to_string());

    format!(
        "response_id={response_id} status={status} output_count={} incomplete_reason={incomplete_reason} error={error} usage={usage}",
        response.output.len()
    )
}

fn response_error_log_summary(error: &ResponseError) -> String {
    format!(
        "code={} message_chars={} param_present={}",
        error.code.as_deref().unwrap_or("none"),
        error.message.chars().count(),
        error.param.is_some()
    )
}

fn response_usage_log_summary(usage: &ResponseUsage) -> String {
    format!(
        "input_tokens={} output_tokens={} total_tokens={} cached_tokens={} reasoning_tokens={}",
        optional_number(&usage.input_tokens),
        optional_number(&usage.output_tokens),
        optional_number(&usage.total_tokens),
        usage.input_tokens_details.cached_tokens,
        usage.output_tokens_details.reasoning_tokens,
    )
}

fn response_output_item_kind(item: &ResponseOutputItem) -> &'static str {
    match item {
        ResponseOutputItem::Message(_) => "message",
        ResponseOutputItem::FunctionCall(_) => "function_call",
        ResponseOutputItem::Reasoning(_) => "reasoning",
        ResponseOutputItem::Compaction(_) => "compaction",
        ResponseOutputItem::Unknown => "unknown",
    }
}

fn optional_number<T: std::fmt::Display>(value: &Option<T>) -> String {
    value
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "none".to_string())
}

fn http_error_body_log_summary(body: &str) -> String {
    let body_chars = body.chars().count();
    match serde_json::from_str::<HttpErrorLogBody>(body) {
        Ok(body) => match body.error {
            Some(error) => format!(
                "body_chars={body_chars} error_type={} code={} message_chars={} param_present={}",
                error.error_type.as_deref().unwrap_or("none"),
                error.code.as_deref().unwrap_or("none"),
                error
                    .message
                    .as_deref()
                    .map(str::chars)
                    .map(Iterator::count)
                    .unwrap_or_default(),
                error.param.is_some()
            ),
            None => format!("body_chars={body_chars} error=none"),
        },
        Err(error) => format!("body_chars={body_chars} parse_error={error}"),
    }
}
