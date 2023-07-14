

use anyhow::{Context as anyhowCtx, bail};
use log::{info, error};

use serenity::{prelude::*, async_trait, model::{prelude::{*, command::{Command, CommandOptionType}, application_command::{ApplicationCommandInteraction, CommandDataOptionValue}}}};
use tokio::sync::mpsc;
use unicode_segmentation::UnicodeSegmentation;

use crate::{QueuedCommand, BackendCommand, CommandResult, upload_images, gpt};


struct Handler {
    dispatcher: mpsc::Sender<QueuedCommand>,
}


fn trim_string(s: &str, limit: usize) -> String {
    UnicodeSegmentation::graphemes(s, true).take(limit).collect::<Vec<_>>().join("")
}


impl Handler {
    // Uses GPT-4 to generate a prompt from a loose description, then passes it to handle_prompt.
    async fn handle_dream(&self, ctx: &Context, command: &ApplicationCommandInteraction, dream: &str) -> anyhow::Result<()> {
        // Deferring the response here is important, because the completion can take a while.
        // If we don't defer, Discord will time us out.
        command.defer(&ctx.http).await?;

        let parsed = gpt::prompt_completion(
            &command.user.to_string(),
            dream,
        ).await.context("While generating prompt")?;

        self.dispatch(ctx, command, parsed, true).await
    }

    async fn handle_prompt(&self, ctx: &Context, command: &ApplicationCommandInteraction, prompt: &str) -> anyhow::Result<()> {
        let parsed = BackendCommand::from_prompt(prompt).context(format!("While parsing `{}`", prompt))?;
        self.dispatch(ctx, command, parsed, false).await
    }

    async fn dispatch(&self, ctx: &Context, command: &ApplicationCommandInteraction, parsed: BackendCommand, was_deferred: bool) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.dispatcher.send(QueuedCommand { command: parsed.clone(), sender: tx }).await?;
        // Create an interaction response to let the user know we're working on it.
        // Deferred won't work here; it takes too long.
        let status = format!("Dreaming about `{}`, style `{}`", trim_string(&parsed.linguistic_prompt, 900), trim_string(&parsed.supporting_prompt, 500));
        if was_deferred {
            command.edit_original_interaction_response(&ctx.http, |response| {
                response.content(status)
            }).await.context("While sending status response")?;
        } else {
            command.create_interaction_response(&ctx.http, |response| {
                response.kind(InteractionResponseType::ChannelMessageWithSource)
                        .interaction_response_data(|message| {
                            message.content(status)
                        })
            }).await.context("While sending status response")?;
        }
        // Now we can wait for the backend to finish.
        let images = match rx.await.context("While waiting for backend")? {
            CommandResult::Success(result) => result,
            CommandResult::Failure(e) => bail!("Backend error: {:?}", e),
        };
        // And send the result, as a separate message.
        let urls = upload_images(images).await?.split_whitespace().collect::<Vec<_>>().join("\n");
        let text = vec![
            format!("Dreams of `{}` | For {}", trim_string(&parsed.linguistic_prompt, 900), command.user.mention()),
            format!("Style: `{}`", trim_string(&parsed.supporting_prompt, 900)),
            format!("Seed {} | {}x{} | {} steps | Aesthetic {} | Guidance {}", parsed.seed, parsed.width, parsed.height, parsed.steps, parsed.aesthetic_scale, parsed.guidance_scale),
            urls,
        ].join("\n");
        
        command.create_followup_message(&ctx.http, |message| {
            message.content(text)
        }).await.context("While sending result message")?;
        
        command.delete_original_interaction_response(&ctx.http).await.context("While deleting status response")?;

        Ok(())
    }

    async fn handle_command(&self, ctx: &Context, command: &ApplicationCommandInteraction) -> anyhow::Result<()> {
        let cmd = command.data.name.as_str();
        match cmd {
            "dream" | "prompt" => {
                let prompt = command.data.options.get(0).context("Expected prompt")?.resolved.as_ref().context("Expected prompt")?;
                if let CommandDataOptionValue::String(prompt) = prompt {
                    match cmd {
                        "dream" => self.handle_dream(ctx, command, prompt).await,
                        "prompt" => self.handle_prompt(ctx, command, prompt).await,
                        _ => unreachable!(),
                    }.context(format!("While handling `{}`", prompt))
                } else {
                    bail!("Expected parameter to be a string");
                }
            },
            _ => bail!("Unknown command"),
        }
    }
}


#[async_trait]
impl EventHandler for Handler {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {

        if let Interaction::ApplicationCommand(command) = interaction {
            info!("Received command: {:?}", command);

            match self.handle_command(&ctx, &command).await {
                Ok(_) => (),
                Err(e) => {
                    error!("Error handling command: {:?}", e);
                    let e = trim_string(&format!("{:?}", e), 1800);
                    // We might be in one of four states.
                    // - We haven't responded yet.
                    // - We've deferred the response.
                    // - We've responded with a status message.
                    // - We've responded with a result message.
                    //
                    // In general we just ignore errors in this error handler, because there's nothing we can do.
                    if let Ok(_) = command.create_interaction_response(&ctx.http, |message| {
                        message.kind(InteractionResponseType::ChannelMessageWithSource)
                                .interaction_response_data(|message| {
                                    message.content(format!("Error: {}", e))
                                })
                    }).await {
                        // We hadn't responded yet.
                    } else if let Err(err_err) = command.create_followup_message(&ctx.http, |message| {
                        message.content(format!("Error: {}", e))
                    }).await {
                        // We couldn't send a followup.
                        error!("Error sending error message: {:?}", err_err);
                    }
                }
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("Connected as {}", ready.user.name);

        let commands = Command::set_global_application_commands(&ctx.http, |c| {
            c.create_application_command(|c| {
                c.name("dream")
                 .description("Dream an excellent dream")
                 .create_option(|o| {
                    o.name("prompt")
                     .description("The dream to dream")
                     .kind(CommandOptionType::String)
                     .required(true)
                 })
            })
             .create_application_command(|c| {
                c.name("prompt")
                 .description("Generate using raw prompt")
                 .create_option(|o| {
                    o.name("prompt")
                     .description("The prompt to dream about")
                     .kind(CommandOptionType::String)
                     .required(true)
                 })
            })
        }).await;

        if let Err(e) = commands {
            panic!("Error registering commands: {:?}", e);
        }
    }
}



pub async fn client(dispatcher: mpsc::Sender<QueuedCommand>) -> anyhow::Result<()> {
    let token = std::env::var("DISCORD_BOT_TOKEN")
        .context("Expected a token in the environment")?;
    
    let mut client = Client::builder(&token, GatewayIntents::empty())
        .event_handler(Handler { dispatcher })
        .await
        .context("Error creating client")?;

    client.start()
        .await
        .context("Discord client error")?;

    bail!("Discord client unexpectedly stopped");
}
