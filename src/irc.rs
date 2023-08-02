use std::time::Duration;

use anyhow::{bail, Context, Ok, Result};
use irc::client::prelude::*;
use log::{debug, error, info, trace};
use tokio_stream::StreamExt;

use crate::{config::IrcConfig, generator::UserRequest, utils, BotContext, help};

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
        // Make some alternative nicknames, in case the primary one is taken.
        // We'll suffix _s in the usual fashion.
        let alt_nicks = (1..=5)
            .map(|i| format!("{}{}", self.irc_config.nick, "_".repeat(i)))
            .collect::<Vec<_>>();
        // Connect to IRC.
        let config: Config = Config {
            server: Some(self.irc_config.server.clone()),
            port: Some(self.irc_config.port),
            nickname: Some(self.irc_config.nick.clone()),
            alt_nicks,
            channels: self.irc_config.channels.clone(),
            ..Config::default()
        };
        let command_prefix = format!(
            "!{}",
            self.context
                .config
                .with_config(|c| c.command_prefix.clone())
                .await
        );
        let mut client = Client::from_config(config)
            .await
            .context("failed to connect to IRC")?;
        client.identify().context("failed to identify to IRC")?;
        info!(
            "Connected to {}. Command prefix: {}",
            self.irc_config.server, command_prefix
        );

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
                    let (cmd, params) = match msg.trim().split_once(" ") {
                        Some((a, b)) => (a, b),
                        None => (msg.trim(), ""),
                    };
                    trace!("Command: {}, params: {}", cmd, params);
                    let context = self.context.clone();
                    let sender = client.sender();
                    let target = target.to_owned();
                    let nick = nick.to_owned();
                    let cmd = cmd.to_owned();
                    let params = params.trim().to_owned();
                    tokio::task::spawn(async move {
                        if let Err(e) = Self::handle_command(
                            &context, &sender, &target, &nick, &cmd, &params,
                        )
                        .await
                        {
                            error!("Error while handling command: {:#}", e);
                            if let Err(e) =
                                send(&sender, &target, &format!("{}: Error: {:#}", nick, e)).await
                            {
                                error!("Error while sending error: {:#}", e);
                            }
                        }
                    });
                }
            }
        }
        bail!("IRC client exited");
    }

    async fn handle_command(
        context: &BotContext,
        sender: &Sender,
        target: &str,
        nick: &str,
        cmd: &str,
        params: &str,
    ) -> Result<()> {
        let request = match cmd {
            "dream" => UserRequest {
                user: nick.into(),
                dream: Some(params.into()),
                raw: params.into(),
                source: crate::generator::Source::Irc,
            },
            "prompt" => UserRequest {
                user: nick.into(),
                dream: None,
                raw: params.into(),
                source: crate::generator::Source::Irc,
            },
            "help" => {
                let text = help::handler(context, "!", params).await
                    .context("While creating help")?;
                // Compose the extra topics on the end.
                let filled;
                if text.1.is_empty() {
                    filled = text.0;
                } else {
                    filled = text.0 + "\n\nOther topics:\n" + text.1.into_iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n").as_str();
                }
                // This is verbose. Unconditionally send by PM.
                if target.starts_with('#') && (filled.len() > 350*4 || filled.lines().count() > 4) {
                    send(sender, target, "Sending help by PM.").await?;
                    send(sender, nick, &filled).await?;
                } else {
                    send(sender, target, &filled).await?;
                }
                return Ok(());
            }
            _ => return Ok(()),
        };
        // Only picture generation below here. Other commands return early.
        // Before we do anything else, send a new changelog entry! If there is one.
        let userid = format!("irc:{nick}");
        if let Some(entry) = crate::changelog::get_new_changelog_entry(context, &userid).await? {
            send(sender, target, &format!("{}: {}", nick, entry)).await?;
        }
        // It's fine, generate the images.
        let mut events = Box::pin(context.image_generator.generate(request, !target.starts_with('#')).await);
        while let Some(event) = events.next().await {
            trace!("Event: {:?}", event);
            match event {
                crate::generator::GenerationEvent::Completed(c) => {
                    let overview = utils::overview_of_pictures(&c.images)?;
                    let all: Vec<Vec<u8>> = std::iter::once(overview)
                        .chain(c.images.into_iter())
                        .collect();
                    // Send the results to the user.
                    let urls = utils::upload_images(&context.config, &c.uuid, all)
                        .await
                        .context("failed to upload images")?;
                    send(sender, target, &format!("{}: {}", nick, urls[0])).await?;
                }
                crate::generator::GenerationEvent::Error(e) => {
                    send(sender, target, &format!("{}: Error: {:#}", nick, e)).await?;
                }
                crate::generator::GenerationEvent::GPTCompleted(req) => {
                    send(
                        sender,
                        target,
                        &format!("{}: Dreaming about `{}`", nick, req.raw),
                    )
                    .await?;
                }
                crate::generator::GenerationEvent::Parsed(parsed) => {
                    // Do nothing.
                }
                crate::generator::GenerationEvent::Queued(n) => {
                    if n >= 3 {
                        send(
                            sender,
                            target,
                            &format!("{}: You're in position {} in the queue.", nick, n),
                        )
                        .await?;
                    }
                }
                crate::generator::GenerationEvent::Generating(_) => {
                    // Ignoring this one.
                }
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
        sender
            .send_privmsg(target, line)
            .context("failed to send answer")?;
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
    Ok(())
}
