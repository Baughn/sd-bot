use anyhow::{bail, Context as anyhowCtx, Result};
use log::{debug, error, info, trace};

use serenity::{
    async_trait,
    builder::{CreateActionRow, CreateInputText},
    model::prelude::{
        application_command::{ApplicationCommandInteraction, CommandDataOptionValue},
        command::{Command, CommandOptionType},
        component::{ActionRowComponent, ButtonStyle},
        message_component::MessageComponentInteraction,
        modal::ModalSubmitInteraction,
        *,
    },
    prelude::*,
};

use tokio_stream::StreamExt;

use crate::{
    changelog,
    generator::{self, GenerationEvent, UserRequest},
    help, utils, BotContext,
};

pub struct DiscordTask {
    context: BotContext,
    token: String,
}

struct Handler {
    context: BotContext,
    action_buttons: CreateActionRow,
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

struct DiscordMessageData {
    /// Accessible from the start:
    // The user who requested the image.
    pub user: String,
    pub mention: String,
    // The prompt that was used to generate the image.
    pub prompt: String,
    // True for /dream, false for /prompt.
    pub is_dream: bool,
    /// Accessible if there is a changelog entry:
    pub changelog: Option<String>,
    /// Accessible after LLM enhancement:
    pub enhanced: Option<String>,
    pub comment: Option<String>,
    /// Accessible *while* generating:
    pub queue_pos: Option<u32>,
    pub gen_pct: Option<u32>,
    /// Accessible after the image is generated:
    pub gallery_url: Option<String>,
    /// Accessible if there is an error:
    pub error: Option<String>,
}

// Discord message formatter.
// This helper function formats image-gen messages in a size-aware way.
// It shrinks the message segments in priority order:
// - Mention: Always shown.
// - Changelog entry (if present)
// - Error message (if present)
// - Gallery link for the image server.
// - Original prompt
// - 1st paragraph of enhanced prompt
// - The entire comment.
// - The rest of the enhanced prompt.
fn format_message(data: &DiscordMessageData) -> String {
    let mut message = format!("{}\n", data.mention);
    if let Some(changelog) = &data.changelog {
        message.push_str(&format!("\n**{}**\n", changelog));
    }
    if let Some(error) = &data.error {
        message.push_str(&format!("Error: {error}\n"));
    }
    if let Some(url) = &data.gallery_url {
        message.push_str(&format!("Gallery: {url}\n"));
    }
    message.push_str(&format!("`{}`\n\n", data.prompt));
    // So much for the easy stuff. Let's see how much space is left.
    let mut remaining = 1950 - message.len();
    let mut enhanced =
        utils::break_paragraphs(data.enhanced.as_deref().unwrap_or_default()).into_iter();
    let comment = utils::break_paragraphs(data.comment.as_deref().unwrap_or_default()).into_iter();
    let mut accepted_enhanced: Vec<String> = Vec::new();
    let mut accepted_comment: Vec<String> = Vec::new();
    // Add the first paragraph of the enhanced prompt.
    if let Some(first) = enhanced.next() {
        if first.len() <= remaining {
            remaining -= first.len();
            accepted_enhanced.push(first.trim().into());
        }
    }
    // Add as many comment paragraphs as possible.
    // If there is no enhanced prompt, skip this.
    for paragraph in comment {
        if paragraph.len() <= remaining {
            remaining -= paragraph.len();
            accepted_comment.push(paragraph.trim().into());
        } else {
            break;
        }
    }
    // Add the rest of the enhanced prompt, as much as possible.
    for paragraph in enhanced {
        if paragraph.len() <= remaining {
            remaining -= paragraph.len();
            accepted_enhanced.push(paragraph.trim().into());
        } else {
            break;
        }
    }

    // Wasn't that nice? Now we can add the accepted parts to the message.
    if data.enhanced.is_some() && !accepted_enhanced.is_empty() {
        message.push_str("```\n");
        message.push_str(&accepted_enhanced.join("\n\n"));
        message.push_str("```\n");
    }
    if data.comment.is_some() && !accepted_comment.is_empty() {
        message.push_str(&accepted_comment.join("\n\n"));
    }

    // And the queue position / generation percentage / ETA.
    if let Some(queue_pos) = data.queue_pos {
        message.push_str(&format!("\n\nQueued at position #{queue_pos}"));
    }
    if let Some(gen_pct) = data.gen_pct {
        message.push_str(&format!("\n\nGeneration progress: {gen_pct}%"));
    }

    // Do a final check for the message length... just in case.
    if message.len() > 2000 {
        log::error!("Message too long: {}", message.len());
        message.truncate(2000);
    }

    message
}

/// Handler for Discord events.
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
        // Continue with the command.
        let request = match cmd {
            "dream" => {
                let prompt = command
                    .data
                    .options
                    .first()
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
                        comment: None,
                        private: command.guild_id.is_none(),
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
                            prompt = option.resolved.as_ref();
                        }
                        "style" => {
                            style = option.resolved.as_ref();
                        }
                        "ar" => {
                            ar = option.resolved.as_ref();
                        }
                        "model" => {
                            model = option.resolved.as_ref();
                        }
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
                    comment: None,
                    private: command.guild_id.is_none(),
                }
            }
            x => bail!("Unknown command: {}", x),
        };

        let statusbox = command
            .edit_original_interaction_response(&ctx.http, |f| f.content("Dreaming..."))
            .await
            .context("Creating initial statusbox")?;

        let is_private = command.guild_id.is_none();

        self.do_generate(ctx, statusbox, request, mention_user, is_private)
            .await
    }

    async fn do_generate(
        &self,
        ctx: &Context,
        mut statusbox: Message,
        request: UserRequest,
        mention_user: Mention,
        is_private: bool,
    ) -> Result<()> {
        let mut stream = Box::pin(
            self.context
                .image_generator
                .generate(request.clone(), is_private)
                .await,
        );

        // We'll be repeatedly updating the statusbox with the latest progress.
        let mut status_data = DiscordMessageData {
            user: request.user.clone(),
            mention: mention_user.to_string(),
            prompt: if let Some(dream) = request.dream.as_ref() {
                dream.clone()
            } else {
                request.raw.clone()
            },
            is_dream: request.dream.is_some(),
            enhanced: None,
            comment: None,
            gallery_url: None,
            error: None,
            changelog: None,
            queue_pos: None,
            gen_pct: None,
        };

        // When generating, we first create an interaction response in which we
        // display event data such as queue #s.
        // Once the generation is complete, we send a followup message with the
        // results.

        // However, we might want to stick a changelog entry in there.
        status_data.changelog =
            changelog::get_new_changelog_entry(&self.context, &request.user).await?;

        async fn update_statusbox(
            ctx: &Context,
            data: &DiscordMessageData,
            boxx: &mut Message,
        ) -> Result<()> {
            let status_text = format_message(data);
            boxx.edit(&ctx.http, |message| message.content(&status_text))
                .await
                .context("Updating statusbox")
        }

        update_statusbox(ctx, &status_data, &mut statusbox).await?;

        while let Some(event) = stream.next().await {
            trace!("Event: {:?}", event);
            match event {
                GenerationEvent::GPTCompleted(c) => {
                    if c.dream.is_some() {
                        status_data.enhanced = Some(c.raw.clone());
                    }
                    c.comment.clone_into(&mut status_data.comment);
                    update_statusbox(ctx, &status_data, &mut statusbox).await?;
                }
                GenerationEvent::Parsed(_) => (
                    // TODO: Implement this.
                ),
                GenerationEvent::Queued(n) => {
                    if n > 0 {
                        status_data.queue_pos = Some(n);
                    } else {
                        status_data.queue_pos = None;
                    }
                    update_statusbox(ctx, &status_data, &mut statusbox).await?;
                }
                GenerationEvent::Generating(percent) => {
                    status_data.queue_pos = None;
                    status_data.gen_pct = Some(percent);
                    update_statusbox(ctx, &status_data, &mut statusbox).await?;
                }
                GenerationEvent::Error(e) => {
                    status_data.error = Some(e.to_string());
                    update_statusbox(ctx, &status_data, &mut statusbox).await?;
                }
                GenerationEvent::Completed(c) => {
                    status_data.queue_pos = None;
                    status_data.gen_pct = None;
                    // TODO: Add gallery url once the ROcket server is up.

                    // Add images to the database & upload them.
                    let gallery_geometry = utils::gallery_geometry(c.images.len());
                    let urls = self.context.db.add_image_batch(&c).await?;

                    // Create the final message, with:
                    // - One row with a delete, restyle, and retry button.
                    // - NxM rows of upscale buttons (up to 4x4).
                    let text = format_message(&status_data);
                    let image_url = urls[0].clone();

                    statusbox
                        .channel_id
                        .send_message(&ctx.http, |message| {
                            message
                                .add_embed(|e| e.image(&image_url))
                                .content(text)
                                .components(|c| {
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
                    statusbox
                        .delete(&ctx.http)
                        .await
                        .context("Deleting status message")?;
                    debug!("Deleted statusbox");
                }
            }
        }

        Ok(())
    }

    async fn handle_submit(
        &self,
        ctx: &Context,
        interaction: &ModalSubmitInteraction,
    ) -> Result<()> {
        let _ = interaction.defer(&ctx.http).await;
        let is_private = interaction.guild_id.is_none();
        // Basically just editing.
        match interaction.data.custom_id.as_str() {
            "edit.submit" => {
                debug!("Received edit submission");
                // Grab the prompt from the input text.
                let prompt = interaction
                    .data
                    .components
                    .first()
                    .context("Expected a component")?
                    .components
                    .first()
                    .context("Expected a component")?;
                match prompt {
                    ActionRowComponent::InputText(text) => {
                        let text = text.value.clone();
                        // Now we can generate.
                        let request = generator::UserRequest {
                            user: interaction.user.to_string(),
                            raw: text,
                            dream: None,
                            source: generator::Source::Discord,
                            comment: None,
                            private: is_private,
                        };
                        let statusbox = interaction
                            .create_followup_message(&ctx.http, |message| {
                                message.content("Dreaming...")
                            })
                            .await
                            .context("Creating initial statusbox")?;
                        self.do_generate(
                            ctx,
                            statusbox,
                            request,
                            interaction.user.mention(),
                            is_private,
                        )
                        .await?;
                    }
                    _ => {
                        bail!("Expected an input component");
                    }
                }
            }
            unknown => {
                bail!("Unknown modal submission: {}", unknown);
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
        info!("Received command \"{command}\"");
        if command.starts_with("help") {
            let _ = component.defer(&ctx.http).await;
            // Display help. Though, which help?
            info!("Received help request for {}", params);
            let (text, further_help) = help::handler(&self.context, "/", params)
                .await
                .context("While creating help")?;
            // We'll stick the extra topics on the end, as components.
            let mut components = vec![];
            let mut row = CreateActionRow::default();
            for topic in further_help {
                row = row
                    .create_button(|b| {
                        b.style(ButtonStyle::Primary)
                            .label(topic)
                            .custom_id(format!("{}.{}", command, topic))
                    })
                    .clone();
                if row.0.len() >= 5 {
                    components.push(row);
                    row = CreateActionRow::default();
                }
            }
            if !row.0.is_empty() {
                components.push(row);
            }
            // Send the message, as a followup.
            // Or possibly multiple followups. Let's start by splitting it.
            let texts = utils::segment_lines_condensed(&text, 1800);
            if let Some((last, prefix)) = texts.split_last() {
                // Send the non-last messages.
                for text in prefix {
                    println!("Sending help message: {}", text);
                    component
                        .create_followup_message(&ctx.http, |message| {
                            message.content(text).ephemeral(true)
                        })
                        .await
                        .context("Sending help message")?;
                }
                // And the last message, with components.
                component
                    .create_followup_message(&ctx.http, |message| {
                        message
                            .content(last)
                            .components(|c| {
                                components.into_iter().fold(c, |c, r| c.add_action_row(r))
                            })
                            .ephemeral(true)
                    })
                    .await
                    .context("Sending help message")?;
            }
            return Ok(());
        }
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
                let _ = component.defer(&ctx.http).await;
                debug!("Upscaling: {:?}", params);
                // Anyway, this sums up as "Find the url in the message, and replace it with the requested invidual image.
                // However, Discord sometimes fails to install an embed. As a fallback we'll look for a textual URL.
                let url = {
                    // Is there an embed?
                    if let Some(embed) = component.message.embeds.first() {
                        if let Some(url) = embed.image.as_ref() {
                            url.url.as_ref()
                        } else {
                            // Is there a URL in the content?
                            let content = &component.message.content;
                            if let Some(url) = utils::extract_url(content) {
                                url
                            } else {
                                bail!("expected a URL in the message");
                            }
                        }
                    } else {
                        bail!("expected to find an embed or a URL in the message");
                    }
                };

                let replacement = utils::get_individual_url(url, params)?;
                debug!("Replacing {} with {}", url, replacement);
                // Send a new message with the new url.
                component
                    .create_followup_message(&ctx.http, |message| message.content(replacement))
                    .await
                    .context("Sending new message")?;
            }
            "retry" | "restyle" | "edit" => {
                // First, we need to retrieve the original generation parameters from the database.
                // All we have to work with is the UUID. That should be plenty.
                let url = component
                    .message
                    .embeds
                    .first()
                    .context("Expected an embed")?
                    .image
                    .as_ref()
                    .context("Expected an embed with an image")?
                    .url
                    .clone();
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
                    let (width, height) = utils::simplify_fraction(request.width, request.height);
                    let mut raw = format!(
                        "{} --ar {}:{} --model {}",
                        request.linguistic_prompt, width, height, request.model_name
                    );
                    if request.linguistic_prompt != request.supporting_prompt {
                        raw.push_str(&format!(" --style {}", request.supporting_prompt));
                    }
                    if !request.negative_prompt.is_empty() {
                        raw.push_str(&format!(" --no {}", request.negative_prompt));
                    }
                    if command == "edit" {
                        component
                            .create_interaction_response(&ctx.http, |f| {
                                f.kind(InteractionResponseType::Modal)
                                    .interaction_response_data(|data| {
                                        data.content("Prompt:")
                                            .title("Edit prompt")
                                            .custom_id("edit.submit")
                                            .components(|c| {
                                                c.create_action_row(|f| {
                                                    f.add_input_text({
                                                        let mut t = CreateInputText::default();
                                                        t
                                                        .placeholder("Enter a new prompt")
                                                        .value(&raw)
                                                        .custom_id("edit.prompt")
                                                        .style(component::InputTextStyle::Paragraph)
                                                        .label("Prompt");
                                                        t
                                                    })
                                                })
                                            })
                                    })
                            })
                            .await
                            .context("Sending edit modal")?;
                    } else {
                        let _ = component.defer(&ctx.http).await;
                        request.base.raw = raw;
                        let statusbox = component
                            .create_followup_message(&ctx.http, |message| {
                                message.content("Dreaming...")
                            })
                            .await
                            .context("Creating initial statusbox")?;
                        let is_private = component.guild_id.is_none();
                        self.do_generate(
                            ctx,
                            statusbox,
                            request.base,
                            component.user.mention(),
                            is_private,
                        )
                        .await?;
                    }
                } else {
                    bail!("No generation parameters found for this batch.");
                }
            }
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
            .create_button(|b| {
                b.style(ButtonStyle::Primary)
                    .label("Edit")
                    .custom_id("edit")
            })
            .create_button(|b| {
                b.style(ButtonStyle::Primary)
                    .label("Help")
                    .custom_id("help")
            })
            .clone();

        Self {
            action_buttons,
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
                if let Err(e) = self.handle_component(&ctx, &component).await {
                    let _ = component.defer(&ctx.http).await; // Deferring is optional.
                    error!("Error handling component: {:?}", e);
                    let e = format!("{:#}", e);
                    let e = utils::segment_lines(&e, 1800)[0];
                    // In this case we always send followup messages.
                    if let Err(err_err) = component
                        .create_followup_message(&ctx.http, |f| {
                            f.content(format!("Error: {:#}", e))
                        })
                        .await
                    {
                        // We couldn't send a followup.
                        error!("Error sending error message: {:?}", err_err);
                    }
                }
            }
            // Modal submit; Edit.
            Interaction::ModalSubmit(interaction) => {
                info!("Received modal submission: {:?}", interaction);
                if let Err(e) = self.handle_submit(&ctx, &interaction).await {
                    error!("Error handling modal submission: {:?}", e);
                    let e = format!("{:#}", e);
                    let e = utils::segment_lines(&e, 1800)[0];
                    // In this case we always send followup messages.
                    if let Err(err_err) = interaction
                        .create_followup_message(&ctx.http, |f| {
                            f.content(format!("Error: {:#}", e))
                        })
                        .await
                    {
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
