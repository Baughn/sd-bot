

use anyhow::{Context as anyhowCtx, bail};
use log::{info, error};
use serenity::{prelude::*, async_trait, model::{prelude::{*, command::{Command, CommandOptionType}, application_command::{ApplicationCommandInteraction, CommandDataOptionValue}}}};
use tokio::sync::mpsc;
use unicode_segmentation::UnicodeSegmentation;

use crate::{QueuedCommand, BackendCommand, CommandResult, upload_images};


struct Handler {
    dispatcher: mpsc::Sender<QueuedCommand>,
}


fn trim_string(s: &str, limit: usize) -> String {
    UnicodeSegmentation::graphemes(s, true).take(limit).collect::<Vec<_>>().join("")
}


impl Handler {
    async fn handle_dream(&self, ctx: &Context, command: &ApplicationCommandInteraction) -> anyhow::Result<()> {
        let prompt = command.data.options.get(0).context("Expected prompt")?.resolved.as_ref().context("Expected prompt")?;
        if let CommandDataOptionValue::String(prompt) = prompt {
            let parsed = BackendCommand::from_dream(prompt).context(format!("While parsing `{}`", prompt))?;
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.dispatcher.send(QueuedCommand { command: parsed.clone(), sender: tx }).await?;
            // Create an interaction response to let the user know we're working on it.
            // Deferred won't work here; it takes too long.
            let status = format!("Dreaming about `{}`, style `{}`", trim_string(&parsed.linguistic_prompt, 900), trim_string(&parsed.supporting_prompt, 500));
            command.create_interaction_response(&ctx.http, |response| {
                response.kind(InteractionResponseType::ChannelMessageWithSource)
                        .interaction_response_data(|message| {
                            message.content(status)
                        })
            }).await.context("While sending status response")?;
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

        } else {
            bail!("Expected prompt to be a string");
        }

        Ok(())
    }

    async fn handle_command(&self, ctx: &Context, command: &ApplicationCommandInteraction) -> anyhow::Result<()> {
        match command.data.name.as_str() {
            "dream" => self.handle_dream(ctx, command).await,
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
                    // We may or may not have already responded.
                    // If we have, we'll delete the response and send a followup.
                    // If we haven't, we'll send a response.
                    if let Err(_) = command.create_interaction_response(&ctx.http, |response| {
                        response.kind(InteractionResponseType::ChannelMessageWithSource)
                                .interaction_response_data(|message| {
                                    message.content(format!("Error: {}", e))
                                })
                    }).await {
                        // Assume we already responded.
                        command.create_followup_message(&ctx.http, |message| {
                            message.content(format!("Error: {}", e))
                        }).await.context("While sending error message").ok();
                        command.delete_original_interaction_response(&ctx.http).await.context("While deleting status response").ok();
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
