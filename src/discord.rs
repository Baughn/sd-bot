use anyhow::{bail, Context as anyhowCtx, Result};
use log::{debug, error, info, trace};

use serenity::{
    async_trait,
    builder::CreateActionRow,
    model::prelude::{
        application_command::{ApplicationCommandInteraction, CommandDataOptionValue},
        command::{Command, CommandOptionType},
        component::ButtonStyle,
        message_component::MessageComponentInteraction,
        *,
    },
    prelude::*,
};

use tokio_stream::StreamExt;

use crate::{
    generator::{self, GenerationEvent, UserRequest},
    utils, BotContext,
};

pub struct DiscordTask {
    context: BotContext,
    token: String,
}

struct Handler {
    context: BotContext,
    action_buttons: CreateActionRow,
    yes_no_buttons: CreateActionRow,
}

impl DiscordTask {
    pub fn new(context: BotContext) -> Result<Self> {
        let token =
            std::env::var("DISCORD_BOT_TOKEN").context("Expected a token in the environment")?;
        Ok(Self { context, token })
    }

    pub async fn run(&mut self) -> Result<()> {
        let intents = GatewayIntents::non_privileged();

        let mut client = Client::builder(&self.token, intents)
            .event_handler(Handler::new(self.context.clone()))
            .await
            .context("Error creating client")?;

        client.start().await.context("Discord client error")?;

        bail!("Discord client unexpectedly stopped");
    }
}

impl Handler {
    async fn handle_command(
        &self,
        ctx: &Context,
        command: &ApplicationCommandInteraction,
    ) -> Result<()> {
        let cprefix = self
            .context
            .config
            .with_config(|c| c.command_prefix.clone())
            .await;
        let cmd = command.data.name.trim_start_matches(&cprefix);
        let mention_user = command.user.mention();
        let request = match cmd {
            "dream" => {
                let prompt = command
                    .data
                    .options
                    .get(0)
                    .context("Expected prompt")?
                    .resolved
                    .as_ref()
                    .context("Expected prompt")?;
                if let CommandDataOptionValue::String(prompt) = prompt {
                    generator::UserRequest {
                        user: command.user.to_string(),
                        raw: prompt.to_string(),
                        dream: Some(prompt.to_string()),
                        source: generator::Source::Discord,
                    }
                } else {
                    bail!("Expected parameter to be a string");
                }
            }
            "prompt" => {
                let mut prompt = None;
                let mut style = None;
                let mut ar = None;
                let mut model = None;
                for option in &command.data.options {
                    match option.name.as_str() {
                        "prompt" => {
                            prompt = option
                                .resolved
                                .as_ref();
                        },
                        "style" => {
                            style = option
                                .resolved
                                .as_ref();
                        },
                        "ar" => {
                            ar = option
                                .resolved
                                .as_ref();
                        },
                        "model" => {
                            model = option
                                .resolved
                                .as_ref();
                        },
                        unknown => {
                            bail!("Unknown option: {}", unknown);
                        }
                    };
                }

                // Type-check and unwrap the options.
                let prompt = if let Some(CommandDataOptionValue::String(prompt)) = prompt {
                    prompt
                } else {
                    bail!("Expected prompt to be a string");
                };
                let style = if let Some(CommandDataOptionValue::String(style)) = style {
                    Some(style)
                } else {
                    None
                };
                let ar = if let Some(CommandDataOptionValue::String(ar)) = ar {
                    Some(ar)
                } else {
                    None
                };
                let model = if let Some(CommandDataOptionValue::String(model)) = model {
                    Some(model)
                } else {
                    None
                };

                // Stick these on the end of the 'command line' so that the parser
                // can pick them up.
                let mut raw = prompt.to_string();
                if let Some(style) = style {
                    raw.push_str(&format!(" --style {}", style));
                }
                if let Some(ar) = ar {
                    raw.push_str(&format!(" --ar {}", ar));
                }
                if let Some(model) = model {
                    raw.push_str(&format!(" --model {}", model));
                }

                // Now we can generate.
                generator::UserRequest {
                    user: command.user.to_string(),
                    raw,
                    dream: None,
                    source: generator::Source::Discord,
                }
            },
            x => bail!("Unknown command: {}", x),
        };

        let statusbox = command.edit_original_interaction_response(&ctx.http, |f| {
            f.content("Dreaming...")
        }).await.context("Creating initial statusbox")?;

        self.do_generate(ctx, statusbox, request, mention_user).await
    }

    async fn do_generate(&self, ctx: &Context, mut statusbox: Message, request: UserRequest, mention_user: Mention) -> Result<()> {
        let mut stream = Box::pin(
            self.context.image_generator.generate(request.clone()).await
        );
        // When generating, we first create an interaction response in which we
        // display event data such as queue #s.
        // Once the generation is complete, we send a followup message with the
        // results.
        let mut status_text = if let Some(dream) = request.dream {
            format!("Dreaming based on `{}`", dream)
        } else {
            format!("Dreaming about `{}`", request.raw)
        };

        statusbox
            .edit(&ctx.http, |message| message.content(&status_text))
            .await
            .context("Creating initial response")?;

        while let Some(event) = stream.next().await {
            trace!("Event: {:?}", event);
            match event {
                GenerationEvent::GPTCompleted(c) => {
                    status_text = format!(
                        "Dreaming about `{}`\nBased on `{}`",
                        c.raw,
                        c.dream.unwrap()
                    );
                    statusbox.edit(&ctx.http, |message| message.content(&status_text)).await?;
                }
                GenerationEvent::Parsed(_) => (
                    // TODO: Implement this.
                ),
                GenerationEvent::Queued(n) => {
                    if n > 0 {
                        status_text.push_str(&format!("\nQueued at position {n}"));
                        statusbox.edit(&ctx.http, |message| message.content(&status_text)).await?;
                    }
                }
                GenerationEvent::Generating(percent) => {
                    // Erase the Queued line, or a previous Generating line.
                    status_text = status_text
                        .lines()
                        .filter(|l| !(l.starts_with("Queued") || l.starts_with("Generating")))
                        .collect::<Vec<_>>()
                        .join("\n");
                    // Add the new Generating line.
                    status_text.push_str(&format!("\nGenerating ({}%)", percent));
                    statusbox.edit(&ctx.http, |message| message.content(&status_text)).await?;
                }
                GenerationEvent::Error(e) => {
                    // There was an error.
                    let err = format!("\n\n{} Error: {:#}", mention_user, e);
                    let err = utils::segment_one(&err, 1800);
                    status_text.push_str(&err);
                    statusbox.edit(&ctx.http, |message| message.content(&status_text)).await?;
                }
                GenerationEvent::Completed(c) => {
                    // Before anything else, let's update the status message.
                    status_text = format!(
                        "Dreamed about `{}`\nGenerated {} images; now uploading",
                        c.base.base.raw,
                        c.images.len()
                    );
                    statusbox.edit(&ctx.http, |message| message.content(&status_text)).await?;

                    // Add images to the database & upload them.
                    let gallery_geometry = utils::gallery_geometry(c.images.len());
                    let urls = self.context.db.add_image_batch(&c).await?;
                    
                    // Send the results to the user.
                    let mut text = vec![
                        format!(
                            "Dreams of `{}` | For {}",
                            c.base.base.raw,
                            mention_user
                        ),
                        format!(
                            "Seed {} | {}x{} | {} steps | Aesthetic {} | Guidance {}",
                            c.base.seed,
                            c.base.width,
                            c.base.height,
                            c.base.steps,
                            c.base.aesthetic_scale,
                            c.base.guidance_scale
                        ),
                    ];
                    if let Some(dream) = c.base.base.dream {
                        text.push(format!("Based on `{}`", dream));
                    }
                    text.push(urls[0].clone());
                    // Create the final message, with:
                    // - One row with a delete, restyle, and retry button.
                    // - NxM rows of upscale buttons (up to 4x4).
                    statusbox.channel_id.send_message(&ctx.http, |message| {
                        message.content(text.join("\n")).components(|c| {
                            let mut c = c.add_action_row(self.action_buttons.clone());
                            // Given a 2x3 gallery geometry, add 3 rows of 2 buttons each.
                            for y in 0..gallery_geometry.1 {
                                let mut row = CreateActionRow::default();
                                for x in 0..gallery_geometry.0 {
                                    let index = y * gallery_geometry.0 + x + 1;
                                    row = row
                                        .create_button(|b| {
                                            b.style(ButtonStyle::Primary)
                                                .label(format!("U{}", index))
                                                .custom_id(format!("upscale.{}", index))
                                        })
                                        .clone();
                                }
                                c = c.add_action_row(row);
                            }
                            c
                        })
                    })
                    .await
                    .context("Posting pictures")?;
                    // When all is said and done, delete the statusbox.
                    statusbox.delete(&ctx.http).await.context("Deleting status message")?;
                }
            }
        }

        Ok(())
    }

    async fn handle_component(
        &self,
        ctx: &Context,
        component: &MessageComponentInteraction,
    ) -> Result<()> {
        let (command, params) = component
            .data
            .custom_id
            .split_once('.')
            .unwrap_or((component.data.custom_id.as_str(), ""));
        match command {
            "delete" => {
                // Just delete it.
                // Maybe later we can use a modal to verify.
                component
                    .message
                    .delete(&ctx.http)
                    .await
                    .context("Deleting potentially NSFW message")?;
            }
            "upscale" => {
                // TODO: Actually do upscaling.
                debug!("Upscaling: {:?}", params);
                // Anyway, this sums up as "Find the url in the message, and replace it with the requested invidual image."
                let embed = component
                    .message
                    .embeds
                    .first()
                    .context("Expected an embed")?;
                let url = embed.url.as_ref().context("Expected an embed with a url")?;
                // This should end in ".0.jpg", and we'll replace the 0.
                if let Some((prefix, suffix)) = url.rsplit_once("0.jpg") {
                    if !suffix.is_empty() {
                        bail!("Expected url to end in 0.jpg");
                    }
                    let new_url = format!("{}{}.jpg", prefix, params);
                    debug!("Replacing {} with {}", url, new_url);
                    // Send a new message with the new url.
                    component
                        .create_followup_message(&ctx.http, |message| {
                            message.content(new_url)
                        })
                        .await
                        .context("Sending new message")?;
                } else {
                    bail!("Expected url to end in 0.jpg");
                }
            },
            "retry" | "restyle" => {
                // First, we need to retrieve the original generation parameters from the database.
                // All we have to work with is the UUID. That should be plenty.
                let url = component
                    .message
                    .embeds
                    .first()
                    .context("Expected an embed")?
                    .url
                    .as_ref()
                    .context("Expected an embed with a url")?;
                debug!("Retrieving parameters for {}", url);
                // The URL contains the UUID in the final component.
                let uuid = url
                    .rsplit_once('/')
                    .context("Expected a URL with a UUID")?
                    .1
                    .split_once('.')
                    .context("Expected a URL with an index")?
                    .0;
                debug!("UUID: {}", uuid);
                // Now we can retrieve the parameters.
                let request = self.context.db.get_parameters_for_batch(uuid).await?;
                debug!("Parameters: {:?}", request);
                // And finally we can generate.
                if let Some(mut request) = request {
                    if command == "restyle" {
                        // Swap the style out for something random.
                        request.supporting_prompt = generator::choose_random_style().to_string();
                        request.base.dream = None;
                    }
                    // Recreate the raw prompt.
                    // TODO: Really we should just pass the *already parsed* request in.
                    request.base.raw = format!("{} --style {} --no {} --ar {}:{} --model {}", request.linguistic_prompt, request.supporting_prompt, request.negative_prompt, request.width, request.height, request.model_name);
                    let statusbox = component
                        .create_followup_message(&ctx.http, |message| {
                            message.content("Dreaming...")
                        })
                        .await
                        .context("Creating initial statusbox")?;
                    self.do_generate(ctx, statusbox, request.base, component.user.mention()).await?;
                } else {
                    bail!("No generation parameters found for this batch.");
                }
            },
            unknown => {
                bail!("Unknown component: {}", unknown);
            }
        };
        Ok(())
    }

    fn new(context: BotContext) -> Self {
        let action_buttons = CreateActionRow::default()
            .create_button(|b| {
                b.style(ButtonStyle::Danger)
                    .label("Delete")
                    .custom_id("delete")
            })
            .create_button(|b| {
                b.style(ButtonStyle::Primary)
                    .label("Restyle")
                    .custom_id("restyle")
            })
            .create_button(|b| {
                b.style(ButtonStyle::Primary)
                    .label("Retry")
                    .custom_id("retry")
            })
            .clone();

        let yes_no_buttons = CreateActionRow::default()
            .create_button(|b| b.style(ButtonStyle::Danger).label("Yes").custom_id("yes"))
            .create_button(|b| b.style(ButtonStyle::Primary).label("No").custom_id("no"))
            .clone();

        Self {
            action_buttons,
            yes_no_buttons,
            context,
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            // Dream and prompt are the same command, but with different parameters.
            Interaction::ApplicationCommand(command) => {
                info!("Received command: {:?}", command);
                let _ = command.defer(&ctx.http).await;
                if let Err(e) = self.handle_command(&ctx, &command).await {
                    error!("Error handling command: {:?}", e);
                    let e = format!("{:#}", e);
                    let e = utils::segment_lines(&e, 1800)[0];
                    // We might be in one of four states.
                    // - We haven't responded yet.
                    // - We've deferred the response.
                    // - We've responded with a status message.
                    // - We've responded with a result message.
                    //
                    // In general we just ignore and log errors in this error handler.
                    if command
                        .create_interaction_response(&ctx.http, |message| {
                            message
                                .kind(InteractionResponseType::ChannelMessageWithSource)
                                .interaction_response_data(|message| {
                                    message.content(format!("Error: {}", e))
                                })
                        })
                        .await
                        .is_ok()
                    {
                        // We hadn't responded yet.
                    } else if let Err(err_err) = command
                        .create_followup_message(&ctx.http, |message| {
                            message.content(format!("Error: {}", e))
                        })
                        .await
                    {
                        // We couldn't send a followup.
                        error!("Error sending error message: {:?}", err_err);
                    }
                }
            }
            // Action buttons; Delete, Restyle, Retry.
            // Delete... deletes. The other two actually just invoke /prompt again!
            Interaction::MessageComponent(component) => {
                info!("Received component interaction: {:?}", component);
                let _ = component.defer(&ctx.http).await;
                if let Err(e) = self.handle_component(&ctx, &component).await {
                    error!("Error handling component: {:?}", e);
                    let e = format!("{:#}", e);
                    let e = utils::segment_lines(&e, 1800)[0];
                    // In this case we always send followup messages.
                    if let Err(err_err) = component.create_followup_message(&ctx.http, |f|
                        f.content(format!("Error: {:#}", e))
                    ).await {
                        // We couldn't send a followup.
                        error!("Error sending error message: {:?}", err_err);
                    }
                }
            }

            // Anything else, we don't care.
            unknown => {
                debug!("Unknown interaction: {:?}", unknown);
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("Connected as {}", ready.user.name);
        let config = self.context.config.snapshot().await;
        let command_prefix = config.command_prefix;
        let cname = |suffix: &str| format!("{}{}", command_prefix, suffix);

        let commands = Command::set_global_application_commands(&ctx.http, |c| {
            // dream
            // - prompt (text)
            c.create_application_command(|c| {
                c.name(cname("dream"))
                 .description("Generate using a loose description; flags are unsupported")
                 .create_option(|o| {
                    o.name("prompt")
                     .description("The dream to dream")
                     .kind(CommandOptionType::String)
                     .required(true)
                 })
            })
             // prompt
             // - prompt (text)
             // - style (multichoice)
             // - AR (multichoice)
             // - Model (multichoice)
             .create_application_command(|c| {
                c.name(cname("prompt"))
                 .description("Generate using raw prompt")
                 .create_option(|o| {
                    o.name("prompt")
                     .description("The prompt and flags; --style, --ar, --model and such. Check help.")
                     .kind(CommandOptionType::String)
                     .required(true)
                 })
                 .create_option(|o| {
                    let o = o.name("style")
                     .description("Style preset (EXPERIMENTAL)")
                     .kind(CommandOptionType::String)
                     .required(false);
                    for (name, value) in generator::STYLES.iter() {
                        o.add_string_choice(name, value);
                    }
                    o
                 })
                .create_option(|o| {
                    o.name("ar")
                     .description("Aspect ratio")
                     .kind(CommandOptionType::String)
                     .required(false)
                     .add_string_choice("1:1", "1:1")
                     .add_string_choice("4:3", "4:3")
                     .add_string_choice("3:2", "3:2")
                     .add_string_choice("16:9", "16:9")
                     .add_string_choice("9:16", "9:16")
                     .add_string_choice("21:9", "21:9")
                     .add_string_choice("9:21", "9:21")
                })
                .create_option(|o| {
                    let mut o = o.name("model")
                    .description("Model")
                    .kind(CommandOptionType::String)
                    .required(false);
                    let mut aliases = config.aliases.keys().collect::<Vec<_>>();
                    aliases.sort();
                    for alias in aliases {
                        o = o.add_string_choice(alias, alias);
                    }
                    o
                })
            })
        }).await;

        if let Err(e) = commands {
            panic!("Error registering commands: {:?}", e);
        }
    }
}
