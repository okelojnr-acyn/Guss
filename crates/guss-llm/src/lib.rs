use async_trait::async_trait;
use serde_json::{Value, json};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("request to LLM provider failed: {0}")]
    Request(String),
    #[error("LLM provider returned no completion content")]
    EmptyCompletion,
    #[error("LLM response was not valid JSON: {0}")]
    InvalidJson(String),
}

/// A request to get a JSON object back from the model that should conform
/// to `json_schema`. Providers that support real structured-output/
/// constrained decoding (Ollama 0.3+, Groq, OpenAI) use `json_schema` to
/// constrain generation directly - this eliminates most malformed-output
/// failures before they happen, rather than relying on retry-after-the-fact.
#[derive(Debug, Clone)]
pub struct JsonCompletionRequest {
    pub system_prompt: String,
    pub user_prompt: String,
    /// A human-readable name for the schema (required by OpenAI/Groq's
    /// `json_schema` response_format).
    pub schema_name: String,
    pub json_schema: Value,
    pub temperature: f32,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete_json(&self, req: JsonCompletionRequest) -> Result<Value, LlmError>;
}

/// One implementation against the OpenAI-compatible `/chat/completions`
/// shape, configurable by base_url/api_key/model. Works unmodified against
/// Ollama (http://localhost:11434/v1, no key), Groq
/// (https://api.groq.com/openai/v1), and OpenAI (https://api.openai.com/v1).
pub struct OpenAiCompatibleProvider {
    base_url: String,
    api_key: Option<String>,
    model: String,
    client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }
}

impl OpenAiCompatibleProvider {
    async fn send(
        &self,
        url: &str,
        req: &JsonCompletionRequest,
        schema: &Value,
        strict: bool,
    ) -> Result<reqwest::Response, LlmError> {
        let body = json!({
            "model": self.model,
            "temperature": req.temperature,
            "messages": [
                {"role": "system", "content": req.system_prompt},
                {"role": "user", "content": req.user_prompt},
            ],
            // Structured-output constraint where the provider honors it
            // (Ollama 0.3+, Groq, OpenAI). Providers that ignore unknown
            // fields fall back to plain completion - the schema is also
            // described in `system_prompt` text as a second line of
            // defense, and the caller retries on parse failure.
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": req.schema_name,
                    "schema": schema,
                    "strict": strict,
                }
            },
        });
        let mut request = self.client.post(url).json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        request
            .send()
            .await
            .map_err(|e| LlmError::Request(e.to_string()))
    }
}

/// Rewrites a schemars schema into what OpenAI-style strict mode accepts
/// (Groq's gpt-oss refused the raw one: `format: "uint32"` on integers).
/// `format` is kept only on strings, `oneOf` becomes `anyOf`; every object lists all its
/// properties as required and forbids extras. Optional fields stay
/// optional because schemars already allows `null` for them.
pub fn strict_schema(schema: &Value) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(map) => {
                let is_string = match map.get("type") {
                    Some(Value::String(t)) => t == "string",
                    Some(Value::Array(ts)) => ts.iter().any(|t| t == "string"),
                    _ => false,
                };
                if !is_string {
                    map.remove("format");
                }
                // Strict mode takes anyOf, not oneOf; for schemars' tagged
                // enums (one variant per tag) they mean the same.
                if let Some(variants) = map.remove("oneOf") {
                    map.insert("anyOf".into(), variants);
                }
                if let Some(Value::Object(props)) = map.get("properties") {
                    let keys: Vec<Value> = props.keys().cloned().map(Value::String).collect();
                    map.insert("required".into(), Value::Array(keys));
                    map.insert("additionalProperties".into(), Value::Bool(false));
                }
                for child in map.values_mut() {
                    walk(child);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut out = schema.clone();
    walk(&mut out);
    out
}

#[async_trait]
impl LlmProvider for OpenAiCompatibleProvider {
    async fn complete_json(&self, req: JsonCompletionRequest) -> Result<Value, LlmError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let schema = strict_schema(&req.json_schema);

        let mut response = self.send(&url, &req, &schema, true).await?;
        if response.status() == reqwest::StatusCode::BAD_REQUEST {
            let text = response.text().await.unwrap_or_default();
            // Providers differ in which JSON Schema keywords strict mode
            // accepts; rather than failing every message, retry once with
            // the schema as guidance only (the caller still validates and
            // retries on a parse failure).
            if !text.contains("response_format") && !text.contains("schema") {
                return Err(LlmError::Request(format!("HTTP 400 Bad Request: {text}")));
            }
            tracing::warn!(error = %text, "provider rejected the strict schema, retrying non-strict");
            response = self.send(&url, &req, &schema, false).await?;
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(LlmError::Request(format!("HTTP {status}: {text}")));
        }

        let body: Value = response
            .json()
            .await
            .map_err(|e| LlmError::Request(e.to_string()))?;

        let content = body
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or(LlmError::EmptyCompletion)?;

        serde_json::from_str(content).map_err(|e| LlmError::InvalidJson(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_schema_drops_non_string_format_and_closes_objects() {
        let raw = json!({
            "type": "object",
            "properties": {
                "quantity": {"type": "integer", "format": "uint32", "minimum": 0},
                "note": {"type": ["string", "null"], "format": "date"},
                "items": {"type": "array", "items": {"type": "object", "properties": {"n": {"type": "number", "format": "double"}}}}
            },
            "required": ["quantity"]
        });
        let s = strict_schema(&raw);
        assert!(s["properties"]["quantity"].get("format").is_none());
        assert_eq!(s["properties"]["note"]["format"], "date");
        assert!(
            s["properties"]["items"]["items"]["properties"]["n"]
                .get("format")
                .is_none()
        );
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["required"].as_array().unwrap().len(), 3);
        assert_eq!(
            s["properties"]["items"]["items"]["additionalProperties"],
            false
        );
        let union = strict_schema(&json!({"oneOf": [{"type": "object", "properties": {}}]}));
        assert!(union.get("oneOf").is_none() && union["anyOf"].is_array());
    }
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_request() -> JsonCompletionRequest {
        JsonCompletionRequest {
            system_prompt: "extract events".into(),
            user_prompt: "sold 3 bags of rice at 500 each".into(),
            schema_name: "extracted_events".into(),
            json_schema: json!({"type": "object"}),
            temperature: 0.0,
        }
    }

    #[tokio::test]
    async fn parses_valid_json_completion() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "{\"events\": []}"}}]
            })))
            .mount(&server)
            .await;

        let provider = OpenAiCompatibleProvider::new(server.uri(), None, "test-model");
        let result = provider.complete_json(sample_request()).await.unwrap();
        assert_eq!(result, json!({"events": []}));
    }

    #[tokio::test]
    async fn surfaces_invalid_json_as_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "not json at all"}}]
            })))
            .mount(&server)
            .await;

        let provider = OpenAiCompatibleProvider::new(server.uri(), None, "test-model");
        let err = provider.complete_json(sample_request()).await.unwrap_err();
        assert!(matches!(err, LlmError::InvalidJson(_)));
    }

    #[tokio::test]
    async fn surfaces_http_error_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let provider = OpenAiCompatibleProvider::new(server.uri(), None, "test-model");
        let err = provider.complete_json(sample_request()).await.unwrap_err();
        assert!(matches!(err, LlmError::Request(_)));
    }
}
