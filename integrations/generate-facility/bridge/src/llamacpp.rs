//! llama.cpp chat-completion client — the generate facility's link to the GPU.
//!
//! Speaks the OpenAI-compatible `/v1/chat/completions` endpoint llama.cpp's
//! server exposes (the same server shape the embedding facility uses, minus
//! `--embedding`). The request build and the response parse are PURE functions,
//! unit-tested against representative JSON, so the wire contract is locked
//! without a live model. `complete` is the thin async call that joins them.
//!
//! Sibling to `integrations/embedding-facility/bridge/src/llamacpp.rs`: the two
//! facilities share a pattern (airc citizen + capability advert + a thin
//! llama.cpp HTTP client). Once both are proven live, the shared citizen-loop is
//! the natural extraction target — but build the second instance first (the
//! outlier-validation discipline), then compress.

use serde_json::{json, Value};

/// Failure modes of a completion round-trip. Loud: a generate facility that
/// silently returned empty text would make a remote persona turn look like a
/// model that chose to say nothing — indistinguishable from a real Pass. So a
/// malformed/empty response is an error the caller surfaces, never a default.
#[derive(Debug)]
pub enum LlamaCppError {
    /// The HTTP request itself failed (connect, timeout, transport).
    Http(reqwest::Error),
    /// The server answered with a non-success status.
    Status { code: u16, body: String },
    /// The body was not the expected `{ choices: [{ message: { content } }] }`
    /// shape, or carried no choices. Carries a human reason.
    Schema(String),
}

impl std::fmt::Display for LlamaCppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "llama.cpp completion HTTP error: {e}"),
            Self::Status { code, body } => {
                write!(f, "llama.cpp completion returned status {code}: {body}")
            }
            Self::Schema(reason) => write!(f, "llama.cpp completion response malformed: {reason}"),
        }
    }
}

impl std::error::Error for LlamaCppError {}

impl From<reqwest::Error> for LlamaCppError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e)
    }
}

/// Build the JSON body for `POST /v1/chat/completions`. The turn `prompt` rides
/// as a single user message so the server applies the model's chat template
/// (correct for instruct models — a raw `/completion` would skip the template
/// and degrade instruct output). `model` is included when set; llama.cpp serves
/// one loaded model and ignores it, but a multi-model server / proxy routes on
/// it and it makes the requested model explicit on the wire.
pub fn build_request_body(
    prompt: &str,
    model: Option<&str>,
    max_tokens: u32,
    temperature: f32,
) -> Value {
    let mut body = json!({
        "messages": [ { "role": "user", "content": prompt } ],
        "max_tokens": max_tokens,
        "temperature": temperature,
        "stream": false,
    });
    if let Some(model) = model {
        body["model"] = json!(model);
    }
    body
}

/// Parse the OpenAI-shaped chat response into the assistant text. Errors loudly
/// on any deviation rather than returning empty (see `LlamaCppError`).
pub fn parse_response(value: &Value) -> Result<String, LlamaCppError> {
    let choices = value
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| LlamaCppError::Schema("missing `choices` array".into()))?;
    let first = choices
        .first()
        .ok_or_else(|| LlamaCppError::Schema("`choices` array is empty".into()))?;
    let message = first
        .get("message")
        .ok_or_else(|| LlamaCppError::Schema("choice 0 missing `message`".into()))?;
    let content = message.get("content").and_then(Value::as_str).unwrap_or("");
    if content.is_empty() {
        // Known limitation, surfaced clearly rather than as a vague "empty":
        // the cross-grid `TurnEmitted` wire is text-only, so a model that emits
        // structured tool calls (and no text) can't be carried over the grid
        // yet. Tool-call-preserving remote inference needs a structured
        // `TurnEmitted` (ContentParts / tool_calls) — a wire extension, not a
        // bug here. Name it so a future debugger isn't chasing "empty content".
        if message.get("tool_calls").is_some() {
            return Err(LlamaCppError::Schema(
                "choice 0 produced tool_calls with no text — remote tool-call inference needs a \
                 structured TurnEmitted (text-only wire today); see README limitations"
                    .into(),
            ));
        }
        return Err(LlamaCppError::Schema(
            "choice 0 `message.content` is empty".into(),
        ));
    }
    Ok(content.to_string())
}

/// Generate a completion for `prompt` against the llama.cpp server at
/// `base_url`. Thin: builds the body, POSTs, checks status, parses — the
/// contract logic is in the two pure functions above.
pub async fn complete(
    client: &reqwest::Client,
    base_url: &str,
    prompt: &str,
    model: Option<&str>,
    max_tokens: u32,
    temperature: f32,
) -> Result<String, LlamaCppError> {
    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let resp = client
        .post(url)
        .json(&build_request_body(prompt, model, max_tokens, temperature))
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(LlamaCppError::Status {
            code: status.as_u16(),
            body,
        });
    }
    let value: Value = resp.json().await?;
    parse_response(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_sends_prompt_as_user_message_with_params() {
        // what this catches: the turn prompt must ride as a user message (so the
        // chat template applies) with the gen params + the model echoed.
        let body = build_request_body("hello", Some("qwen3-coder"), 256, 0.7);
        assert_eq!(body["messages"][0]["role"], json!("user"));
        assert_eq!(body["messages"][0]["content"], json!("hello"));
        assert_eq!(body["max_tokens"], json!(256));
        assert_eq!(body["model"], json!("qwen3-coder"));
        assert_eq!(body["stream"], json!(false));
    }

    #[test]
    fn request_body_omits_model_when_absent() {
        let body = build_request_body("hi", None, 128, 0.0);
        assert!(body.get("model").is_none(), "no model key when unset");
    }

    #[test]
    fn parses_openai_chat_response_content() {
        // what this catches: a real /v1/chat/completions body must decode to the
        // assistant's content string.
        let value = json!({
            "choices": [
                { "index": 0, "message": { "role": "assistant", "content": "the answer" }, "finish_reason": "stop" }
            ]
        });
        assert_eq!(parse_response(&value).unwrap(), "the answer");
    }

    #[test]
    fn empty_choices_is_a_loud_error() {
        // what this catches: empty choices must NOT decode to "" — that would be
        // indistinguishable from the model deliberately saying nothing.
        let value = json!({ "choices": [] });
        assert!(matches!(
            parse_response(&value),
            Err(LlamaCppError::Schema(_))
        ));
    }

    #[test]
    fn missing_content_is_a_loud_error() {
        let value = json!({ "choices": [ { "message": { "role": "assistant" } } ] });
        assert!(matches!(
            parse_response(&value),
            Err(LlamaCppError::Schema(_))
        ));
    }

    #[test]
    fn empty_content_string_is_rejected() {
        let value = json!({ "choices": [ { "message": { "content": "" } } ] });
        assert!(matches!(
            parse_response(&value),
            Err(LlamaCppError::Schema(_))
        ));
    }

    #[test]
    fn missing_choices_array_is_a_schema_error() {
        let value = json!({ "object": "chat.completion" });
        assert!(matches!(
            parse_response(&value),
            Err(LlamaCppError::Schema(_))
        ));
    }

    #[test]
    fn tool_calls_with_no_text_is_a_clear_named_error() {
        // what this catches: a model emitting structured tool_calls + no text
        // must fail with the SPECIFIC text-only-wire limitation message, not a
        // vague "empty content" — so the wire-extension follow-up is obvious.
        let value = json!({
            "choices": [ { "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [ { "id": "c1", "type": "function",
                    "function": { "name": "read", "arguments": "{}" } } ]
            } } ]
        });
        match parse_response(&value) {
            Err(LlamaCppError::Schema(msg)) => {
                assert!(
                    msg.contains("tool_calls"),
                    "names the tool-call limitation: {msg}"
                )
            }
            other => panic!("expected a Schema error naming tool_calls, got {other:?}"),
        }
    }
}
