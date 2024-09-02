use std::io::Write;

use anyhow::{Context, Result};
use log::trace;
use openai_api_rs::v1::chat_completion::{self, ChatCompletionRequest, Content};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::fmt::{self, Display};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::{config::BotConfigModule, utils};

// This is what we ask GPT-4 to generate.
#[derive(Debug, Deserialize)]
pub struct GPTPrompt {
    prompt: String,
    aspect_ratio: String,
    pub comment: String,
}

impl Display for GPTPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} --ar {} -m flux", self.prompt, self.aspect_ratio)
    }
}

#[derive(Clone)]
pub struct GPTPromptGeneratorModule {
    #[allow(dead_code)]
    config: BotConfigModule,
    userdata: Arc<Mutex<HashMap<String, UserData>>>,
}

struct UserData {
    last_prompt: String,
    last_response: String,
}

impl GPTPromptGeneratorModule {
    pub fn new(config: BotConfigModule) -> Self {
        Self {
            config,
            userdata: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    // TODO: Fix this duplication.
    pub async fn gpt3_5(&self, system_prompt: &str, user_prompt: &str) -> Result<String> {
        // Set up the OpenAI client.
        let key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY not set. Please stick to real help subjects.")?;
        let client = openai_api_rs::v1::api::OpenAIClient::new(key);
        // Ask GPT-3.5-turbo to complete the prompt.
        let model = "gpt-4o".to_string();
        let req = ChatCompletionRequest {
            model,
            messages: vec![
                chat_completion::ChatCompletionMessage {
                    role: chat_completion::MessageRole::system,
                    content: Content::Text(system_prompt.to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                chat_completion::ChatCompletionMessage {
                    role: chat_completion::MessageRole::user,
                    content: Content::Text(user_prompt.to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            temperature: Some(1.0),
            top_p: None,
            n: None,
            stream: Some(false),
            stop: None,
            max_tokens: Some(4096),
            presence_penalty: None,
            frequency_penalty: None,
            logit_bias: None,
            user: None,
            seed: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
            response_format: None,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            client.chat_completion(req),
        );
        let result = result.await.context("While completing prompt")??;
        log::info!("Generated (3.5) {} to {:?}", user_prompt, result);
        let result = result
            .choices
            .first()
            .context("No choices in GPT response")?
            .message
            .content
            .clone()
            .context("No content in GPT response")?;
        Ok(result)
    }

    /// Wraps the generation function in a retry handler.
    /// You know, just in case.
    pub async fn generate(&self, user: &str, dream: &str) -> Result<GPTPrompt> {
        let strategy = tokio_retry::strategy::FixedInterval::from_millis(1000).take(2);
        tokio_retry::Retry::spawn(strategy, || self.do_generate(user, dream))
            .await
            .context("While generating prompt")
    }

    async fn do_generate(&self, user: &str, dream: &str) -> Result<GPTPrompt> {
        self.inner_generate(user, dream, "prompt-completion.tmpl")
            .await
    }

    pub async fn comment(&self, user: &str, dream: &str) -> Result<String> {
        let strategy = tokio_retry::strategy::FixedInterval::from_millis(1000).take(2);
        tokio_retry::Retry::spawn(strategy, || self.do_comment(user, dream))
            .await
            .context("While generating comment")
    }

    async fn do_comment(&self, user: &str, dream: &str) -> Result<String> {
        let result = self
            .inner_generate(user, dream, "prompt-comment.tmpl")
            .await?;
        Ok(result.comment)
    }

    /// Generates a fully baked prompt for the user, using GPT-4.
    async fn inner_generate(&self, user: &str, dream: &str, template: &str) -> Result<GPTPrompt> {
        let prompt_template =
            std::fs::read_to_string(template).context(format!("While reading {:?}", template))?;
        trace!("Prompt template hash: {}", utils::hash(&prompt_template));
        // Set up the OpenAI client.
        let key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY not set. Please use /prompt.")?;
        let client = openai_api_rs::v1::api::OpenAIClient::new(key);
        // Create the user history message.
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
        // Ask GPT-4 to complete the prompt.
        let model = "gpt-4o".to_string();
        let req = ChatCompletionRequest {
            model: model.clone(),
            messages: vec![
                chat_completion::ChatCompletionMessage {
                    role: chat_completion::MessageRole::system,
                    content: Content::Text(prompt_template),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                chat_completion::ChatCompletionMessage {
                    role: chat_completion::MessageRole::user,
                    content: Content::Text(user_message),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            seed: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
            temperature: Some(0.5),
            top_p: None,
            n: None,
            stream: Some(false),
            stop: None,
            max_tokens: Some(2048),
            presence_penalty: None,
            frequency_penalty: None,
            logit_bias: None,
            user: Some(user.to_string()),
            response_format: Some(json!({"type": "json_object"})),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            client.chat_completion(req),
        )
        .await
        .context("While completing prompt")??;
        log::info!("Autocompleted {} to {:?}", dream, result);
        let result = result
            .choices
            .first()
            .context("No choices in GPT response")?
            .message
            .content
            .clone()
            .context("No content in GPT response")?;
        // Also save these to a file, for later fine-tuning of a local model.
        let mut logfile = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open("prompt-completions.log")
            .context("While opening prompt-completions.txt")?;
        writeln!(
            logfile,
            "User: {:?}\nDream: {:?}\nModel: {:?}\nGPT: {:?}\n\n",
            user, dream, model, result
        )
        .context("While writing to prompt-completions.txt")?;
        // Parse the result into a GPTPrompt.
        // If it isn't valid, we'll bail with the whole completion in the error.
        let parsed = serde_json::from_str::<GPTPrompt>(&result)
            .with_context(|| format!("While parsing GPTPrompt from {:?}", result))?;
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
