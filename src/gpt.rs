use std::io::Write;

use anyhow::{Result, bail, Context};
use openai_api_rs::v1::chat_completion::{ChatCompletionRequest, self};
use serde::Deserialize;

use crate::BackendCommand;

const PROMPT_TEMPLATE: &str = include_str!("../prompt-completion.tmpl");

// This is what we ask GPT-4 to generate.
#[derive(Debug, Deserialize)]
struct GPTPrompt {
    prompt: String,
    style: String,
    aspect_ratio: String,
}

impl ToString for GPTPrompt {
    fn to_string(&self) -> String {
        format!("{} --style {} --ar {}", self.prompt, self.style, self.aspect_ratio)
    }
}

// Generates a fully baked prompt for the user, using GPT-4.
pub async fn prompt_completion(user: &str, dream: &str) -> Result<BackendCommand> {
    // First, make sure it isn't pointlessly long.
    if dream.len() > 200 {
        bail!("There's no point in using /dream with long prompts. Maybe try /prompt instead?");
    }
    // Set up the OpenAI client.
    let key = std::env::var("OPENAI_API_KEY").context("OPENAI_API_KEY not set. Please use /prompt.")?;
    let client = openai_api_rs::v1::api::Client::new(key);
    // Ask GPT-4 to complete the prompt.
    let model = chat_completion::GPT4.to_string();
    let req = ChatCompletionRequest {
        model: model.clone(),
        messages: vec![chat_completion::ChatCompletionMessage {
            role: chat_completion::MessageRole::system,
            content: Some(String::from(PROMPT_TEMPLATE)),
            name: None,
            function_call: None,
        }, chat_completion::ChatCompletionMessage {
            role: chat_completion::MessageRole::user,
            content: Some(dream.to_string()),
            name: None,
            function_call: None,
        }],
        functions: None,
        function_call: None,
        temperature: Some(1.0),
        top_p: None,
        n: None,
        stream: Some(false),
        stop: Some(vec!["\n".to_string()]),
        max_tokens: Some(300),
        presence_penalty: None,
        frequency_penalty: None,
        logit_bias: None,
        user: Some(user.to_string()),
    };
    let result = client.chat_completion(req).await.context("While completing prompt")?;
    log::info!("Autocompleted {} to {:?}", dream, result);
    let result = result.choices.get(0).context("No choices in GPT response")?.message.content.clone().context("No content in GPT response")?;
    // Also save these to a file, for later fine-tuning of a local model.
    let mut logfile = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open("prompt-completions.log")
        .context("While opening prompt-completions.txt")?;
    writeln!(logfile, "User: {:?}\nDream: {:?}\nModel: {:?}\nGPT: {:?}\n\n", user, dream, model, result).context("While writing to prompt-completions.txt")?;
    // Parse the result into a GPTPrompt.
    // If it isn't valid, we'll bail with the whole completion in the error.
    let parsed = serde_json::from_str::<GPTPrompt>(&result)
        .with_context(|| format!("While parsing GPTPrompt from {:?}", result))?;
    // Convert to BackendCommand and return.
    let (width, height) = BackendCommand::parse_aspect_ratio(&parsed.aspect_ratio)?;
    Ok(BackendCommand {
        linguistic_prompt: parsed.prompt,
        supporting_prompt: parsed.style,
        width, height,
        ..Default::default()
    })
}
