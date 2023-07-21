use std::{time::Duration};

use anyhow::{Result, bail, Context, Ok};
use irc::client::prelude::*;
use log::{info, debug, trace, warn};
use tokio_stream::StreamExt;

use crate::{config::{IrcConfig}, generator::{UserRequest}, BotContext, utils};


pub struct IrcTask {
    context: BotContext,
    irc_config: IrcConfig,
}

impl IrcTask {
    pub fn new(irc_config: IrcConfig, context: BotContext) -> Self {
        Self {
            context,
            irc_config,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        self.run_inner().await
    }

    async fn run_inner(&mut self) -> Result<()> {
        let config: Config = Config {
            server: Some(self.irc_config.server.clone()),
            port: Some(self.irc_config.port),
            nickname: Some(self.irc_config.nick.clone()),
            channels: self.irc_config.channels.clone(),
            ..Config::default()
        };
        let command_prefix = format!("!{}", self.context.config.with_config(|c| c.command_prefix.clone()).await);
        let mut client = Client::from_config(config).await.context("failed to connect to IRC")?;
        client.identify().context("failed to identify to IRC")?;
        info!("Connected to {}. Command prefix: {}", self.irc_config.server, command_prefix);

        let mut stream = client.stream()?;
        while let Some(message) = stream.next().await.transpose()? {
            if let Command::PRIVMSG(ref target, ref msg) = message.command {
                if let Some((_, msg)) = msg.split_once(&command_prefix) {
                    debug!("Received command: {}", msg);
                    let nick = message.source_nickname().context("No nickname")?;
                    let in_channel = target.starts_with('#');
                    // target is ourselves if it's a private message. Fix.
                    let target = if in_channel {
                        target.to_owned()
                    } else {
                        nick.to_owned()
                    };
                    if let Some((cmd, params)) = msg.trim().split_once(' ') {
                        trace!("Command: {}, params: {}", cmd, params);
                        let context = self.context.clone();
                        let sender = client.sender();
                        let target = target.to_owned();
                        let nick = nick.to_owned();
                        let cmd = cmd.to_owned();
                        let params = params.trim().to_owned();
                        tokio::task::spawn(
                            Self::handle_command(context, sender, target, nick, cmd, params)
                        );
                    } else {
                        warn!("Received command without parameters: {}", msg);
                    }
                }
            }
        }
        bail!("IRC client exited");
    }

    async fn handle_command(context: BotContext, sender: Sender, target: String, nick: String, cmd: String, params: String) -> Result<()> {
        let request = match cmd.as_ref() {
            "dream" => UserRequest {
                user: nick.clone(),
                dream: Some(params.clone()),
                raw: params,
                source: crate::generator::Source::Irc,
            },
            "prompt" => UserRequest {
                user: nick.clone(),
                dream: None,
                raw: params,
                source: crate::generator::Source::Irc,
            },
            _ => return Ok(()),
        };
        let mut events = Box::pin(context.image_generator.generate(request).await);
        while let Some(event) = events.next().await {
            trace!("Event: {:?}", event);
            match event {
                crate::generator::GenerationEvent::Completed(c) => {
                    // Send the results to the user.
                    let urls = utils::upload_images(&context.config, c.images).await
                        .context("failed to upload images")?
                        .join(" ");
                    send(&sender, &target, &format!("{}: {}", nick, urls)).await?;
                },
                crate::generator::GenerationEvent::Error(e) => {
                    send(&sender, &target, &format!("{}: Error: {}", nick, e)).await?;
                },
                crate::generator::GenerationEvent::GPTCompleted(req) => {
                    send(&sender, &target, &format!("{}: Dreaming about `{}`", nick, req.raw)).await?;
                },
                crate::generator::GenerationEvent::Parsed(_) => {
                    // Ignoring this one.
                },
                crate::generator::GenerationEvent::Queued(n) => {
                    if n >= 3 {
                        send(&sender, &target, &format!("{}: You're in position {} in the queue.", nick, n)).await?;
                    }
                },
                crate::generator::GenerationEvent::Generating(_) => {
                    // Ignoring this one.
                },
            };
        }
        debug!("Command completed");
        Ok(())
    }
}

async fn send(sender: &Sender, target: &str, text: &str) -> Result<()> {
    const LENGTH_LIMIT: usize = 350;
    let lines = utils::segment_lines(text, LENGTH_LIMIT);
    for line in lines {
        trace!("Sending line: {} to {}", line, target);
        sender.send_privmsg(target, line).context("failed to send answer")?;
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
    Ok(())
} 