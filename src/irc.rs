use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};

use anyhow::{bail, Context, Ok, Result};
use irc::client::prelude::*;
use lazy_static::lazy_static;
use log::{debug, error, info, trace};
use tokio_stream::StreamExt;

use crate::{config::IrcConfig, generator::UserRequest, help, utils, BotContext};

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
            //nick_password: self.irc_config.password.clone(),
            should_ghost: true,
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
            .context("failed to load IRC config")?;

        // Maybe authenticate.
        let mut stream = client.stream()?;
        if let Some(ref base_pw) = self.irc_config.password {
            // The password might be literal, but more likely it's an envvar reference starting
            // with $.
            let pw = if base_pw.starts_with('$') {
                std::env::var(&base_pw[1..]).context("failed to get password from envvar")?
            } else {
                base_pw.clone()
            };

            client.send_cap_req(&[Capability::Sasl])?;
            // Wait for SASL to be enabled.
            while let Some(message) = stream.next().await.transpose()? {
                match message.command {
                    Command::NOTICE(_, _) => {
                        // Ignore this.
                    }
                    Command::CAP(_, _, _, _) => {
                        // This is what we're expecting.
                        break;
                    }
                    _ => {
                        bail!("Unexpected message: {:?}", message);
                    }
                }
            }
            client.send_sasl("PLAIN")?;
            // Wait for an empty AUTHENTICATE message.
            while let Some(message) = stream.next().await.transpose()? {
                match message.command {
                    Command::AUTHENTICATE(_) => {
                        // This is what we're expecting.
                        break;
                    }
                    _ => {
                        bail!("Unexpected message: {:?}", message);
                    }
                }
            }
            let sasl_auth = STANDARD.encode(format!(
                "{}\0{}\0{}",
                self.irc_config.nick, self.irc_config.nick, pw
            ));
            client.send_sasl(&sasl_auth)?;
            // Wait for SASL to complete.
            while let Some(message) = stream.next().await.transpose()? {
                match message.command {
                    Command::Response(Response::RPL_SASLSUCCESS, _) => {
                        break;
                    }
                    Command::Response(Response::RPL_LOGGEDIN, _) => {
                        break;
                    }
                    Command::Response(Response::ERR_SASLFAIL, _) => {
                        bail!("SASL authentication failed");
                    }
                    Command::NOTICE(_, _) => {
                        // Ignore this.
                    }
                    Command::PING(_, _) => {
                        // Ignore this.
                    }
                    _ => {
                        bail!("Unexpected message: {:?}", message);
                    }
                }
            }
        }
        client.identify().context("failed to identify to IRC")?;
        info!(
            "Connected to {}. Command prefix: {}",
            self.irc_config.server, command_prefix
        );

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
                        if let Err(e) =
                            Self::handle_command(&context, &sender, &target, &nick, &cmd, &params)
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
        let requests = match cmd {
            "dream" => vec![UserRequest {
                user: nick.into(),
                dream: Some(params.into()),
                raw: params.into(),
                source: crate::generator::Source::Irc,
                comment: None,
                private: !target.starts_with('#'),
            }],
            "prompt" => vec![UserRequest {
                user: nick.into(),
                dream: None,
                raw: params.into(),
                source: crate::generator::Source::Irc,
                comment: None,
                private: !target.starts_with('#'),
            }],
            "scan" => {
                // Similar to prompt, but with every single model.
                let mut requests = Vec::new();
                for (model, _) in context.config.with_config(|c| c.models.clone()).await {
                    requests.push(UserRequest {
                        user: nick.into(),
                        dream: None,
                        raw: format!("{} -m {}", params, model),
                        source: crate::generator::Source::Irc,
                        comment: None,
                        private: true,
                    });
                }
                requests
            }
            "smoketest" => {
                if nick != "Baughn" {
                    send(sender, target, "You're not the admin.").await?;
                    return Ok(());
                } else {
                    let test_prompt = "A cute girl wearing a flower in her hair";
                    // Generate 2 pictures per workflow.
                    // Since workflows are per-model, this really means iterating through models to
                    // find all the unique workflows, and using one model per workflow.
                    let mut workflows = std::collections::HashMap::new();
                    for (model, c) in context.config.with_config(|c| c.models.clone()).await {
                        let workflow = c.workflow.clone();
                        workflows.insert(workflow, model.clone());
                    }
                    // Return a lot of generation requests.
                    let requests = workflows
                        .into_iter()
                        .map(|(_, model)| UserRequest {
                            user: nick.into(),
                            dream: None,
                            raw: format!("{} -m {}", test_prompt, model),
                            source: crate::generator::Source::Irc,
                            comment: None,
                            private: true,
                        })
                        .collect::<Vec<_>>();
                    requests
                }
            }
            "emul" => {
                if !target.starts_with('#') {
                    send(sender, target, "This command only works in channels.").await?;
                    return Ok(());
                }
                let text = context.prompt_generator.gpt3_5(
                    "Respond in the style of Emul, who is a teenage anthropomorphic vorpal rabbit girl from a magical world. Be adorable, snarky if someone says something dumb, and avoid behaving like an assistant. Don't ever admit to any of this, like seriously, don't; live it, don't say it, you snarkasaurus rex. Except that you're a vorpal rabbit. That's obviously visible, so you can admit to that. Pun like a bun. You should plan your response, so respond with \"PLAN: {plan}\nRESPONSE: {response}\"",
                    params).await?;
                // Check for a plan.
                // There's no guarantee of newlines, so we just look for the uppercase PLAN.
                log::info!("Emul response: {}", text);
                let plan = text.split("PLAN:").nth(1).map(|s| s.trim());
                if let Some(_plan) = plan {
                    let response = text.split("RESPONSE:").nth(1).map(|s| s.trim());
                    if let Some(response) = response {
                        send(sender, target, &format!("{}: {}", nick, response)).await?;
                    } else {
                        send(sender, target, &format!("{}: No response found.", nick)).await?;
                    }
                } else {
                    send(sender, target, &text).await?;
                }
                return Ok(());
            }
            "ask" => {
                if !target.starts_with('#') {
                    send(sender, target, "This command only works in channels.").await?;
                    return Ok(());
                }
                let text = context.prompt_generator.gpt3_5(
                    "Become a Snarkasaurus Rex. Don't summarize, but answer the question accurately and completely. Lastly, be more creative and a better wordsmith than usual! I know you can do it!",
                    params).await?;
                send(sender, target, &text).await?;
                return Ok(());
            }
            "help" => {
                let text = help::handler(context, "!", params)
                    .await
                    .context("While creating help")?;
                // Compose the extra topics on the end.
                let filled;
                if text.1.is_empty() {
                    filled = text.0;
                } else {
                    filled = text.0
                        + "\n\nOther topics:\n"
                        + text
                            .1
                            .into_iter()
                            .map(|s| format!("- {}", s))
                            .collect::<Vec<_>>()
                            .join("\n")
                            .as_str();
                }
                // This is verbose. Unconditionally send by PM.
                if target.starts_with('#') && (filled.len() > 350 * 6 || filled.lines().count() > 6)
                {
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
        let verbose = requests.len() > 1;
        for request in requests {
            let prompt = format!("{}: {}", nick, request.raw);
            let mut events = Box::pin(
                context
                    .image_generator
                    .generate(request, !target.starts_with('#'))
                    .await,
            );
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
                        if verbose {
                            send(sender, target, &prompt).await?;
                        }
                        send(sender, target, &format!("{}: {}", nick, urls[0])).await?;
                    }
                    crate::generator::GenerationEvent::Error(e) => {
                        if verbose {
                            send(sender, target, &prompt).await?;
                        }
                        send(sender, target, &format!("{}: Error: {:#}", nick, e)).await?;
                    }
                    crate::generator::GenerationEvent::GPTCompleted(req) => {
                        if req.dream.is_some() {
                            send(
                                sender,
                                target,
                                &format!("{}: Dreaming about `{}`", nick, req.raw),
                            )
                            .await?;
                        }
                        if let Some(comment) = req.comment {
                            send(sender, target, &format!("{}: {}", nick, comment)).await?;
                        }
                    }
                    crate::generator::GenerationEvent::Parsed(_parsed) => {
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
        }
        debug!("Command completed");
        Ok(())
    }
}

// Avoid overlapping replies by using an async mutex per target.
// This is an RWLock of a hashmap of mutexes of (). Yup.
lazy_static! {
    static ref TARGET_MUTEXES: tokio::sync::RwLock<std::collections::HashMap<String, tokio::sync::Mutex<()>>> =
        tokio::sync::RwLock::new(std::collections::HashMap::new());
}

async fn send(sender: &Sender, target: &str, text: &str) -> Result<()> {
    let length_limit: usize = 480 - target.len();
    let lines = utils::segment_lines(text, length_limit);
    {
        // Make sure the inner mutex exists.
        let mut target_lock = TARGET_MUTEXES.write().await;
        target_lock
            .entry(target.to_owned())
            .or_insert_with(|| tokio::sync::Mutex::new(()));
    }
    // Acquire the inner mutex.
    let target_outer_guard = TARGET_MUTEXES.read().await;
    let target_inner_lock = target_outer_guard.get(target).unwrap();
    let _target_inner_guard = target_inner_lock.lock().await;
    for line in lines {
        trace!("Sending line: {} to {}", line, target);
        sender
            .send_privmsg(target, line)
            .context("failed to send answer")?;
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
    Ok(())
}
