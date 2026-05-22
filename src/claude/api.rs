use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use thiserror::Error;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-haiku-4-5-20251001";

#[derive(Debug, Error)]
pub enum ClaudeError {
    #[error("HTTP {status}: {body}")]
    ApiError { status: u16, body: String },
    #[error("could not parse API response")]
    ParseError,
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

impl ToolCall {
    pub fn string(&self, key: &str) -> Option<&str> {
        self.input[key].as_str()
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.input[key].as_i64()
    }

    pub fn float(&self, key: &str) -> Option<f64> {
        self.input[key].as_f64()
    }
}

pub struct ToolResponse {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

pub struct ClaudeAPI {
    api_key: String,
    client: Client,
}

impl ClaudeAPI {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: Client::new(),
        }
    }

    /// Load API key from ~/.mixr/claude_key and construct a client.
    pub fn from_key_file() -> Result<Self> {
        let path = dirs::home_dir().unwrap_or_default().join(".mixr/claude_key");
        let key = std::fs::read_to_string(&path)
            .map_err(|_| anyhow::anyhow!("No Claude API key at ~/.mixr/claude_key"))?
            .trim().to_string();
        Ok(Self::new(key))
    }

    /// Response with tool use support.
    ///
    /// Uses Anthropic prompt caching: the system prompt and tools block
    /// are marked `cache_control: ephemeral` so subsequent calls within
    /// ~5 min pay ~10% of normal input cost on that prefix (cache read
    /// vs. full send). Big deal for our pattern — the tool schema alone
    /// is ~900 tokens and was previously resent every round, which
    /// dominated the input-token budget during multi-round chains and
    /// drove the 429 storm.
    ///
    /// Caching rules: place a `cache_control` marker at the end of each
    /// cacheable block; everything before it up to the prior marker
    /// (or start) becomes the cached prefix. We mark the system prompt
    /// AND the last tool — tools + system become one cached chunk.
    /// The `messages` array stays uncached because it changes each
    /// round.
    pub async fn ask_with_tools(
        &self,
        system: &str,
        messages: &[Value],
        tools: &[Value],
    ) -> Result<ToolResponse> {
        // System prompt as an array so we can attach cache_control.
        let system_block = serde_json::json!([
            { "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }
        ]);
        // Mark the last tool with cache_control so the whole tool list
        // plus system prompt above it become one cached prefix. If
        // tools is empty we skip — caching requires a non-empty block.
        let cached_tools: Vec<Value> = if tools.is_empty() {
            Vec::new()
        } else {
            let mut v = tools.to_vec();
            let last = v.len() - 1;
            if let Some(obj) = v[last].as_object_mut() {
                obj.insert("cache_control".into(),
                    serde_json::json!({ "type": "ephemeral" }));
            }
            v
        };
        // Conversation messages are NOT cached. We tried marking the
        // last message as a cache boundary but the DJ retro-compacts
        // older tool_result bodies between rounds (see
        // `compact_old_tool_results` in dj.rs) — that mutation makes
        // the prior cached prefix invalid, so every round paid a full
        // cache_write again with cache_read=0. Net effect: cache_write
        // costs accumulated, no offsetting reads, hit the 50K/min
        // input-token rate limit. Caching just system+tools (which
        // ARE byte-stable for a given call mode) keeps the cache hot
        // and skips the wasted writes.
        let body = serde_json::json!({
            "model": MODEL,
            "max_tokens": 512,
            "system": system_block,
            "messages": messages,
            "tools": cached_tools,
        });

        let result = self.post(&body).await?;

        // Diagnostic: log cache hits/misses so we can tell at a glance
        // whether prompt caching is actually engaging. If
        // cache_read_input_tokens stays at 0 across many calls, our
        // cache_control markers are being ignored (schema-wrong or
        // prefix below the cacheable-size minimum).
        if let Some(u) = result.get("usage") {
            let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let cache_read = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let cache_write = u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            tracing::info!(
                "Claude usage: input={input} cache_read={cache_read} cache_write={cache_write}"
            );
        }

        let content = result["content"]
            .as_array()
            .ok_or(ClaudeError::ParseError)?;

        let mut text = None;
        let mut calls = Vec::new();

        for block in content {
            match block["type"].as_str() {
                Some("text") => {
                    text = block["text"].as_str().map(String::from);
                }
                Some("tool_use") => {
                    if let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) {
                        calls.push(ToolCall {
                            id: id.to_string(),
                            name: name.to_string(),
                            input: block["input"].clone(),
                        });
                    }
                }
                _ => {}
            }
        }

        Ok(ToolResponse { text, tool_calls: calls })
    }

    /// Post a raw JSON body to the Anthropic API. Used by ai_beat and other modules.
    pub async fn post(&self, body: &Value) -> Result<Value> {
        let resp = self.client
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(body)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(ClaudeError::ApiError { status, body: body_text }.into());
        }

        let json: Value = resp.json().await?;
        Ok(json)
    }
}
