use std::fmt::Result;

use anyhow::{Context as _, bail};
use log::info;
use serenity::{prelude::*, http::Http, async_trait, model::{prelude::{*, command::{Command, CommandOption, CommandOptionType}}}};
use tokio::sync::mpsc;

use crate::QueuedCommand;


struct Handler {
    dispatcher: mpsc::Sender<QueuedCommand>,
}

#[async_trait]
impl EventHandler for Handler {
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