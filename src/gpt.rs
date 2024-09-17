use anyhow::{bail, Context, Result};
use log::{info, trace};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::fmt::{self, Display};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::{config::BotConfigModule, utils};

// This is what we ask GPT-4 to generate.
#[derive(Debug, Deserialize, Clone)]
pub struct GeneratedPrompt {
    prompt: String,
    aspect_ratio: String,
    pub comment: String,
}

impl Display for GeneratedPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} --ar {} -m flux", self.prompt, self.aspect_ratio)
    }
}

#[derive(Clone)]
pub struct PromptGeneratorModule {
    #[allow(dead_code)]
    config: BotConfigModule,
    userdata: Arc<Mutex<HashMap<String, UserData>>>,
}

struct UserData {
    last_prompt: String,
    last_response: String,
}

/* Basic Claude request:
curl https://api.anthropic.com/v1/messages \
     --header "x-api-key: $ANTHROPIC_API_KEY" \
     --header "anthropic-version: 2023-06-01" \
     --header "anthropic-beta: prompt-caching-2024-07-31" \
     --header "content-type: application/json" \
     --data \
'{
    "model": "claude-3-5-sonnet-20240620",
    "max_tokens": 4096,
    system=[
        {"type": "text", "text": $SYSTEM_PROMPT, "cache_control": {"type": "ephemeral"}},
    ],
    "messages": [
        {"role": "user", "content": "Hello World"}
    ]
}'

200 response:
{
  "content": [
    {
      "text": "Hi! My name is Claude.",
      "type": "text"
    }
  ],
  "id": "msg_013Zva2CMHLNnXjNJJKqJ2EF",
  "model": "claude-3-5-sonnet-20240620",
  "role": "assistant",
  "stop_reason": "end_turn",
  "stop_sequence": null,
  "type": "message",
  "usage": {
    "input_tokens": 2095,
    "output_tokens": 503
  }
}

4xx response:
{
  "type": "error",
  "error": {
    "type": "invalid_request_error",
    "message": "<string>"
  }
}
*/

// Generic wrapper for Claude calls.
// This also puts a 120-second timeout on the request.
async fn claude<T: DeserializeOwned + Clone>(
    system: &str,
    user: &str,
    schema: Option<&serde_json::Value>,
) -> Result<ClaudeResult<T>> {
    let strategy = tokio_retry::strategy::FixedInterval::from_millis(5000).take(2);
    let do_with_timeout = || async {
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            do_claude::<T>(system, user, schema),
        )
        .await
        .context("while waiting for Claude")?
    };

    tokio_retry::Retry::spawn(strategy, do_with_timeout).await
}

pub async fn claude_simple(system: &str, user: &str) -> Result<String> {
    let result = claude::<()>(system, user, None).await?;
    match result {
        ClaudeResult::NoSchema(msg) => Ok(msg),
        ClaudeResult::Schema(_) => bail!("Unexpected schema response from Claude"),
    }
}

pub async fn claude_schema<T: DeserializeOwned + Clone>(
    system: &str,
    user: &str,
    schema: &serde_json::Value,
) -> Result<T> {
    let result = claude::<T>(system, user, Some(schema)).await?;
    match result {
        ClaudeResult::NoSchema(msg) => bail!("Unexpected non-schema response from Claude: {}", msg),
        ClaudeResult::Schema(x) => Ok(x),
    }
}

#[derive(Debug)]
enum ClaudeResult<T> {
    NoSchema(String),
    Schema(T),
}

// Generic wrapper for Claude calls.
async fn do_claude<T: DeserializeOwned + Clone>(
    system: &str,
    user: &str,
    schema: Option<&serde_json::Value>,
) -> Result<ClaudeResult<T>> {
    // Set up the Anthropic client. There's no library for this, so we'll use reqwest.
    let key = dotenv::var("ANTHROPIC_API_KEY")
        .context("ANTHROPIC_API_KEY not set. Please stick to real help subjects.")?;
    let client = reqwest::Client::new();

    // Ask Claude to complete the prompt.
    let url = "https://api.anthropic.com/v1/messages";
    let req = if let Some(schema) = schema {
        json!({
            "model": "claude-3-5-sonnet-20240620",
            "max_tokens": 4096,
            "tools": [schema],
            "tool_choice": {"type": "tool", "name": schema["name"]},
            "system": [
                {"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [
                {"role": "user", "content": user},
            ]
        })
    } else {
        json!({
            "model": "claude-3-5-sonnet-20240620",
            "max_tokens": 4096,
            "system": [
                {"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [
                {"role": "user", "content": user},
            ]
        })
    };
    let resp = client
        .post(url)
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "prompt-caching-2024-07-31")
        .header("content-type", "application/json")
        .json(&req)
        .send()
        .await
        .context("while sending request to Claude")?;

    // Parse the response.
    #[derive(Deserialize)]
    struct APISuccess<T> {
        content: Vec<APIContent<T>>,
        usage: APIUsage,
    }
    #[derive(Deserialize)]
    struct APIContent<T> {
        #[serde(rename = "type")]
        content_type: String,
        text: Option<String>,
        input: Option<T>,
    }
    #[derive(Deserialize, Debug)]
    struct APIUsage {
        input_tokens: u32,
        output_tokens: u32,
        cache_creation_input_tokens: u32,
        cache_read_input_tokens: u32,
    }
    #[derive(Deserialize)]
    struct APIError {
        error: APIErrorDetails,
    }
    #[derive(Deserialize)]
    struct APIErrorDetails {
        message: String,
    }

    if resp.status().is_success() {
        let body: APISuccess<T> = resp.json().await?;
        info!(
            "Claude usage (in/out/cwr/crd): {}, {}, {}, {}",
            body.usage.input_tokens,
            body.usage.output_tokens,
            body.usage.cache_creation_input_tokens,
            body.usage.cache_read_input_tokens
        );
        if schema.is_some() {
            if let Some(input) = &body.content[0].input {
                Ok(ClaudeResult::Schema(input.clone()))
            } else {
                bail!("Claude response was empty");
            }
        } else {
            let text = &body.content[0].text.as_ref();
            Ok(ClaudeResult::NoSchema(
                text.context("Claude response was empty")?.clone(),
            ))
        }
    } else {
        let body: APIError = resp.json().await?;
        bail!("Claude error: {}", body.error.message);
    }
}

impl PromptGeneratorModule {
    pub fn new(config: BotConfigModule) -> Self {
        Self {
            config,
            userdata: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn generate(&self, user: &str, dream: &str) -> Result<GeneratedPrompt> {
        self.inner_generate(user, dream, "prompt-completion.tmpl")
            .await
    }

    pub async fn comment(&self, user: &str, dream: &str) -> Result<String> {
        let result = self
            .inner_generate(user, dream, "prompt-comment.tmpl")
            .await?;
        Ok(result.comment)
    }

    /// Generates a fully baked prompt for the user.
    async fn inner_generate(
        &self,
        user: &str,
        dream: &str,
        template: &str,
    ) -> Result<GeneratedPrompt> {
        let system_message =
            std::fs::read_to_string(template).context(format!("While reading {:?}", template))?;
        trace!("Prompt template hash: {}", utils::hash(&system_message));

        // Fetch the user history message, if any.
        let last_prompt;
        let last_response;
        let user_message;
        {
            let user_data = self.userdata.lock().await;
            if let Some(data) = user_data.get(user) {
                last_prompt = data.last_prompt.as_ref();
                last_response = data.last_response.as_ref();
            } else {
                last_prompt = "None";
                last_response = "None";
            }
            user_message = format!("Username: {}\nNSFW disallowed\n\nPrevious prompt:\n{}\n\nPrevious response:\n{}\n\nCurrent prompt:\n{}",
                user, last_prompt, last_response, dream);
        }

        // Define the 'tool' schema.
        let schema = json!({
            "name": "record_prompt_completion",
            "description": "Record the prompt completion and comment using well-formed JSON.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "comment": {
                        "type": "string",
                        "description": "The comment on the users's request."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The prompt that was invented."
                    },
                    "aspect_ratio": {
                        "type": "string",
                        "description": "The aspect ratio of the image."
                    }
                },
                "required": ["prompt", "comment", "aspect_ratio"]
            },
        });

        // Ask Claude to complete the prompt.
        let parsed = claude_schema::<GeneratedPrompt>(&system_message, &user_message, &schema)
            .await
            .context("while asking Claude to complete the prompt")?;
        trace!("Claude response: {:?}", parsed);

        // Guess it's fine. Update the user data.
        {
            let mut user_data = self.userdata.lock().await;
            user_data.insert(
                user.to_string(),
                UserData {
                    last_prompt: dream.to_string(),
                    // We'll pretend it only output the prompt and comment, and store that as JSON.
                    last_response: format!(
                        "{{ \"prompt\": \"{}\", \"comment\": \"{}\" }}",
                        parsed.prompt, parsed.comment
                    ),
                },
            );
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_claude() {
        let result = claude_simple(
            "You are a test runner.",
            "Output 42. Just that single number.",
        )
        .await
        .unwrap();
        assert_eq!(result, "42");
    }

    #[tokio::test]
    async fn test_with_schema() {
        #[derive(Deserialize, Clone)]
        struct TestSchema {
            number: String,
        }

        let schema = json!({
            "name": "record_prompt_completion",
            "description": "Record the number using well-formed json.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "number": {
                        "type": "string",
                        "description": "The number."
                    }
                },
                "required": ["number"]
            },
        });
        let result = claude_schema::<TestSchema>(
            "You are a test runner.",
            "Output 42. Just that single number.",
            &schema,
        )
        .await
        .unwrap();
        assert_eq!(result.number, "42");
    }
}
