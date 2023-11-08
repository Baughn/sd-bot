use std::io::Write;

use anyhow::{bail, Context, Result};
use log::trace;
use openai_api_rs::v1::chat_completion::{self, ChatCompletionRequest};
use serde::Deserialize;

use crate::{config::BotConfigModule, utils};

// This is what we ask GPT-4 to generate.
#[derive(Debug, Deserialize)]
pub struct GPTPrompt {
    prompt: String,
    style: String,
    aspect_ratio: String,
    model: String,
}

impl ToString for GPTPrompt {
    fn to_string(&self) -> String {
        format!(
            "{} --style {} --ar {} -m {}",
            self.prompt, self.style, self.aspect_ratio, self.model
        )
    }
}

fn default_model() -> String {
    "pixart".to_string()
}

#[derive(Clone)]
pub struct GPTPromptGeneratorModule {
    config: BotConfigModule,
}

impl GPTPromptGeneratorModule {
    pub fn new(config: BotConfigModule) -> Self {
        Self { config }
    }

    // TODO: Fix this duplication.
    pub async fn gpt3_5(&self, system_prompt: &str, user_prompt: &str) -> Result<String> {
        // Set up the OpenAI client.
        let key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY not set. Please stick to real help subjects.")?;
        let client = openai_api_rs::v1::api::Client::new(key);
        // Ask GPT-3.5-turbo to complete the prompt.
        let model = chat_completion::GPT3_5_TURBO.to_string();
        let req = ChatCompletionRequest {
            model,
            messages: vec![
                chat_completion::ChatCompletionMessage {
                    role: chat_completion::MessageRole::system,
                    content: system_prompt.to_string(),
                    name: None,
                    function_call: None,
                },
                chat_completion::ChatCompletionMessage {
                    role: chat_completion::MessageRole::user,
                    content: user_prompt.to_string(),
                    name: None,
                    function_call: None,
                },
            ],
            functions: None,
            function_call: None,
            temperature: Some(1.2),
            top_p: None,
            n: None,
            stream: Some(false),
            stop: None,
            max_tokens: Some(800),
            presence_penalty: None,
            frequency_penalty: None,
            logit_bias: None,
            user: None,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            client.chat_completion(req),
        );
        let result = result
            .await
            .context("While completing prompt")??;
        log::info!("Generated (3.5) {} to {:?}", user_prompt, result);
        let result = result
            .choices
            .get(0)
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

    /// Generates a fully baked prompt for the user, using GPT-4.
    async fn do_generate(&self, user: &str, dream: &str) -> Result<GPTPrompt> {
        let prompt_template = std::fs::read_to_string("prompt-completion.tmpl")
            .context("While reading prompt-completion.tmpl")?;
        trace!("Prompt template hash: {}", utils::hash(&prompt_template));
        // Set up the OpenAI client.
        let key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY not set. Please use /prompt.")?;
        let client = openai_api_rs::v1::api::Client::new(key);
        // Ask GPT-4 to complete the prompt.
        let model = "gpt-4-1106-preview".to_string();
        let req = ChatCompletionRequest {
            model: model.clone(),
            messages: vec![
                chat_completion::ChatCompletionMessage {
                    role: chat_completion::MessageRole::system,
                    content: prompt_template,
                    name: None,
                    function_call: None,
                },
                chat_completion::ChatCompletionMessage {
                    role: chat_completion::MessageRole::user,
                    content: dream.to_string(),
                    name: None,
                    function_call: None,
                },
            ],
            functions: None,
            function_call: None,
            temperature: Some(1.0),
            top_p: None,
            n: None,
            stream: Some(false),
            stop: Some(vec!["\n\n".to_string(), "}".to_string()]),
            max_tokens: Some(300),
            presence_penalty: None,
            frequency_penalty: None,
            logit_bias: None,
            user: Some(user.to_string()),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            client.chat_completion(req),
        )
        .await
        .context("While completing prompt")??;
        log::info!("Autocompleted {} to {:?}", dream, result);
        let result = result
            .choices
            .get(0)
            .context("No choices in GPT response")?
            .message
            .content
            .clone()
            .context("No content in GPT response")?;
        // This won't include a trailing }, since that's a stop token.
        // So we'll add it back in.
        let result = format!("{} }}", result);
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
        // There's a pretty good chance GPT-4 will try to markdown-escape this with ```json,
        // so we'll strip that out if it's there.
        let result = result.trim_start_matches("```json\n").trim_end_matches("```");
        // If it isn't valid, we'll bail with the whole completion in the error.
        let parsed = serde_json::from_str::<GPTPrompt>(&result)
            .with_context(|| format!("While parsing GPTPrompt from {:?}", result))?;
        Ok(parsed)
    }
}
