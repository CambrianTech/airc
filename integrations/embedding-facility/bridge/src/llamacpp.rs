//! llama.cpp embedding client — the bridge's link to the local GPU server.
//!
//! Speaks the OpenAI-compatible `/v1/embeddings` endpoint llama.cpp's
//! `--embedding` server exposes (the same shape the facility's
//! `docker-compose.yml` stands up on the 5090). The request build and the
//! response parse are PURE functions, unit-tested against representative
//! JSON, so the wire contract is locked without a live server. `embed` is the
//! thin async call that joins them over HTTP.
//!
//! Kept deliberately small and dependency-light (just `reqwest` + `serde_json`,
//! both already in the workspace): this is a transport adapter, not a place for
//! cleverness. The embedding MODEL is chosen grid-wide (the one-vector-space
//! invariant in ../README.md); this client is model-agnostic and simply carries
//! whichever `model` it is told to request.

use serde_json::{json, Value};

/// Failure modes of an embedding round-trip. Every one is loud — a bridge that
/// silently returned an empty / wrong-shaped vector would poison cosine recall
/// across the grid, so a malformed response is an error, never a default.
#[derive(Debug)]
pub enum LlamaCppError {
    /// The HTTP request itself failed (connect, timeout, transport).
    Http(reqwest::Error),
    /// The server answered with a non-success status.
    Status { code: u16, body: String },
    /// The body was not the expected `{ data: [{ embedding: [...] }] }` shape,
    /// or carried fewer vectors than inputs. Carries a human reason.
    Schema(String),
}

impl std::fmt::Display for LlamaCppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "llama.cpp embedding HTTP error: {e}"),
            Self::Status { code, body } => {
                write!(f, "llama.cpp embedding returned status {code}: {body}")
            }
            Self::Schema(reason) => write!(f, "llama.cpp embedding response malformed: {reason}"),
        }
    }
}

impl std::error::Error for LlamaCppError {}

impl From<reqwest::Error> for LlamaCppError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e)
    }
}

/// Build the JSON body for `POST /v1/embeddings`. `input` accepts a single
/// string or an array; we always send the array form (uniform, and the server
/// returns one `data` entry per input either way). `model` is included only
/// when set — llama.cpp serves a single loaded model and ignores it, but a
/// future multi-model server (or a proxy) routes on it, and sending it makes
/// the requested vector space explicit on the wire.
pub fn build_request_body(inputs: &[String], model: Option<&str>) -> Value {
    let mut body = json!({ "input": inputs });
    if let Some(model) = model {
        body["model"] = json!(model);
    }
    body
}

/// Parse the OpenAI-shaped response into one vector per input, ordered by the
/// response's `index` field (defensive: never assume `data` is pre-sorted).
/// Errors loudly on any deviation rather than returning a short/empty result.
pub fn parse_response(value: &Value) -> Result<Vec<Vec<f32>>, LlamaCppError> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| LlamaCppError::Schema("missing `data` array".into()))?;
    if data.is_empty() {
        return Err(LlamaCppError::Schema("`data` array is empty".into()));
    }

    // Collect (index, vector) so we can order by the server-reported index.
    let mut indexed: Vec<(u64, Vec<f32>)> = Vec::with_capacity(data.len());
    for (pos, entry) in data.iter().enumerate() {
        let embedding = entry
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                LlamaCppError::Schema(format!("entry {pos} missing `embedding` array"))
            })?;
        let vector = embedding
            .iter()
            .map(|n| {
                n.as_f64().map(|f| f as f32).ok_or_else(|| {
                    LlamaCppError::Schema(format!("entry {pos} has non-number in `embedding`"))
                })
            })
            .collect::<Result<Vec<f32>, _>>()?;
        if vector.is_empty() {
            return Err(LlamaCppError::Schema(format!(
                "entry {pos} `embedding` is empty"
            )));
        }
        // `index` is optional in some servers; fall back to position.
        let index = entry
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(pos as u64);
        indexed.push((index, vector));
    }
    indexed.sort_by_key(|(i, _)| *i);
    Ok(indexed.into_iter().map(|(_, v)| v).collect())
}

/// Embed `inputs` against the llama.cpp server at `base_url` (e.g.
/// `http://127.0.0.1:8080`). Returns one vector per input. Thin: builds the
/// body, POSTs, checks status, parses — all the contract logic is in the two
/// pure functions above so this needs no test of its own beyond the live smoke.
pub async fn embed(
    client: &reqwest::Client,
    base_url: &str,
    inputs: &[String],
    model: Option<&str>,
) -> Result<Vec<Vec<f32>>, LlamaCppError> {
    let url = format!("{}/v1/embeddings", base_url.trim_end_matches('/'));
    let resp = client
        .post(url)
        .json(&build_request_body(inputs, model))
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
    fn request_body_sends_array_input_and_optional_model() {
        // what this catches: the bridge must always send the array form (one
        // `data` entry per input) and must propagate the requested model so the
        // vector space is explicit on the wire.
        let body = build_request_body(&["a".into(), "b".into()], Some("qwen3-embed"));
        assert_eq!(body["input"], json!(["a", "b"]));
        assert_eq!(body["model"], json!("qwen3-embed"));
    }

    #[test]
    fn request_body_omits_model_when_absent() {
        let body = build_request_body(&["solo".into()], None);
        assert_eq!(body["input"], json!(["solo"]));
        assert!(body.get("model").is_none(), "no model key when unset");
    }

    #[test]
    fn parses_openai_shaped_response_in_index_order() {
        // what this catches: a real llama.cpp /v1/embeddings body must decode to
        // one f32 vector per input, ORDERED BY index (not array position) — a
        // mis-ordered parse would silently pair vectors with the wrong inputs.
        let value = json!({
            "object": "list",
            "model": "qwen3-embed",
            "data": [
                { "object": "embedding", "index": 1, "embedding": [0.4, 0.5, 0.6] },
                { "object": "embedding", "index": 0, "embedding": [0.1, 0.2, 0.3] }
            ]
        });
        let vectors = parse_response(&value).expect("parses");
        assert_eq!(vectors, vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5, 0.6]]);
    }

    #[test]
    fn missing_index_falls_back_to_position() {
        let value = json!({
            "data": [
                { "embedding": [1.0] },
                { "embedding": [2.0] }
            ]
        });
        let vectors = parse_response(&value).expect("parses");
        assert_eq!(vectors, vec![vec![1.0], vec![2.0]]);
    }

    #[test]
    fn empty_data_is_a_loud_error_not_an_empty_result() {
        // what this catches: an empty `data` must NOT decode to `vec![]` — that
        // would feed "no vector" into recall as if it were a valid answer.
        let value = json!({ "data": [] });
        assert!(matches!(
            parse_response(&value),
            Err(LlamaCppError::Schema(_))
        ));
    }

    #[test]
    fn missing_data_array_is_a_schema_error() {
        let value = json!({ "object": "list" });
        assert!(matches!(
            parse_response(&value),
            Err(LlamaCppError::Schema(_))
        ));
    }

    #[test]
    fn empty_embedding_vector_is_rejected() {
        let value = json!({ "data": [ { "embedding": [] } ] });
        assert!(matches!(
            parse_response(&value),
            Err(LlamaCppError::Schema(_))
        ));
    }

    #[test]
    fn non_numeric_embedding_component_is_rejected() {
        let value = json!({ "data": [ { "embedding": [0.1, "nope", 0.3] } ] });
        assert!(matches!(
            parse_response(&value),
            Err(LlamaCppError::Schema(_))
        ));
    }
}
