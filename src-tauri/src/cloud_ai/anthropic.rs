use serde::{Deserialize, Serialize};

const CLAUDE_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Serialize)]
struct ClaudeMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ClaudeRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<ClaudeMessage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeContentBlock {
    text: Option<String>,
    #[serde(rename = "type")]
    block_type: String,
}

#[derive(Debug, Deserialize)]
struct ClaudeResponse {
    content: Option<Vec<ClaudeContentBlock>>,
    error: Option<ClaudeError>,
}

#[derive(Debug, Deserialize)]
struct ClaudeError {
    message: String,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    error_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeErrorResponse {
    error: ClaudeError,
}

/// Process text with a Claude model (200K token context)
/// Best for nuanced extraction and structured data
pub async fn process_with_claude(api_key: &str, prompt: &str, text: &str, model: &str) -> Result<String, String> {
    let client = reqwest::Client::new();

    let full_prompt = prompt.replace("{transcript}", text);

    let model_id = if model.is_empty() { "claude-sonnet-4-20250514" } else { model };

    let request = ClaudeRequest {
        model: model_id.to_string(),
        max_tokens: 4096,
        messages: vec![ClaudeMessage {
            role: "user".to_string(),
            content: full_prompt,
        }],
    };

    let response = client
        .post(CLAUDE_API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    let status = response.status();
    let body = response.text().await.map_err(|e| format!("Failed to read response: {}", e))?;

    if !status.is_success() {
        // Try to parse error
        if let Ok(error_resp) = serde_json::from_str::<ClaudeErrorResponse>(&body) {
            return Err(format!("Anthropic API error: {}", error_resp.error.message));
        }
        return Err(format!("API request failed ({}): {}", status, body));
    }

    let result: ClaudeResponse = serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    if let Some(error) = result.error {
        return Err(format!("Anthropic API error: {}", error.message));
    }

    result
        .content
        .and_then(|blocks| {
            blocks
                .into_iter()
                .filter(|b| b.block_type == "text")
                .find_map(|b| b.text)
        })
        .ok_or_else(|| format!("No response from Anthropic model '{}'", model_id))
}

