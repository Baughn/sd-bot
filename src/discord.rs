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
    generator::{self, GenerationEvent},
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
        let mut status_text;
        let stream = match cmd {
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
                    status_text = format!("Dreaming based on `{}`", prompt);
                    self.context
                        .image_generator
                        .generate(generator::UserRequest {
                            user: command.user.to_string(),
                            raw: prompt.to_string(),
                            dream: Some(prompt.to_string()),
                            source: generator::Source::Discord,
                        })
                        .await
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
                status_text = format!("Dreaming about `{}`", raw);
                self.context
                    .image_generator
                    .generate(generator::UserRequest {
                        user: command.user.to_string(),
                        raw,
                        dream: None,
                        source: generator::Source::Discord,
                    })
                    .await
            },
            x => bail!("Unknown command: {}", x),
        };
        let mut stream = Box::pin(stream);
        // When generating, we first create an interaction response in which we
        // display event data such as queue #s.
        // Once the generation is complete, we send a followup message with the
        // results.
        command
            .edit_original_interaction_response(&ctx.http, |message| message.content(&status_text))
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
                    command
                        .edit_original_interaction_response(&ctx.http, |message| {
                            message.content(&status_text)
                        })
                        .await?;
                }
                GenerationEvent::Parsed(_) => (
                    // TODO: Implement this.
                ),
                GenerationEvent::Queued(n) => {
                    if n > 0 {
                        status_text.push_str(&format!("\nQueued at position {n}"));
                        command
                            .edit_original_interaction_response(&ctx.http, |message| {
                                message.content(&status_text)
                            })
                            .await?;
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
                    command
                        .edit_original_interaction_response(&ctx.http, |message| {
                            message.content(&status_text)
                        })
                        .await?;
                }
                GenerationEvent::Error(e) => {
                    // There was an error. We'll send it as a followup message.
                    let err = format!("{} Error: {:#}", mention_user, e);
                    let err = utils::segment_one(&err, 1800);
                    command
                        .create_followup_message(&ctx.http, |message| message.content(err))
                        .await?;
                    // Leave the original message as-is; it has the prompt.
                }
                GenerationEvent::Completed(c) => {
                    // Before anything else, let's update the status message.
                    status_text = format!(
                        "Dreamed about `{}`\nGenerated {} images; now uploading",
                        c.base.base.raw,
                        c.images.len()
                    );
                    command
                        .edit_original_interaction_response(&ctx.http, |message| {
                            message.content(&status_text)
                        })
                        .await?;
                    // Create a gallery of the images.
                    let gallery_geometry = utils::gallery_geometry(c.images.len());
                    let overview = utils::overview_of_pictures(&c.images)?;
                    let all: Vec<Vec<u8>> = std::iter::once(overview)
                        .chain(c.images.into_iter())
                        .collect();
                    // Send the results to the user.
                    let urls = utils::upload_images(&self.context.config, all)
                        .await
                        .context("failed to upload images")?;

                    let mut text = vec![
                        format!(
                            "Dreams of `{}` | For {}",
                            c.base.base.raw,
                            command.user.mention()
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
                    command
                        .create_followup_message(&ctx.http, |message| {
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
                    // When all is said and done, delete the original message.
                    command
                        .delete_original_interaction_response(&ctx.http)
                        .await
                        .context("Deleting original message")?;
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
                        f.content(format!("Error: {}", e))
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
        let command_prefix = self
            .context
            .config
            .with_config(|c| c.command_prefix.clone())
            .await;
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
                    o.name("style")
                     .description("Style preset (EXPERIMENTAL)")
                     .kind(CommandOptionType::String)
                     .required(false)
                     .add_string_choice("Shōnen Anime", "Shōnen Anime, action-oriented, Akira Toriyama (Dragon Ball), youthful, vibrant, dynamic")
                     .add_string_choice("Shōjo Anime", "Shōjo Anime, Romantic, Naoko Takeuchi (Sailor Moon), emotional, detailed backgrounds, soft colors")
                     .add_string_choice("Seinen Anime", "Seinen Anime, Mature, Hajime Isayama (Attack on Titan), complex themes, realistic, detailed")
                     .add_string_choice("Abstract Expressionism", "Abstract Expressionism, Abstract, Jackson Pollock, spontaneous, dynamic, emotional")
                     .add_string_choice("Art Nouveau", "Art Nouveau, Decorative, Alphonse Mucha, organic forms, intricate, flowing")
                     .add_string_choice("Baroque", "Baroque, Dramatic, Caravaggio, high contrast, ornate, realism")
                     .add_string_choice("Classical", "Classical, Proportionate, Leonardo da Vinci, balanced, harmonious, detailed")
                     .add_string_choice("Contemporary", "Contemporary, Innovative, Ai Weiwei, conceptual, diverse mediums, social commentary")
                     .add_string_choice("Cubism", "Cubism, Geometric, Pablo Picasso, multi-perspective, abstract, fragmented")
                     .add_string_choice("Fantasy", "Fantasy, Imaginative, J.R.R. Tolkien, mythical creatures, dreamlike, detailed")
                     .add_string_choice("Film Noir", "Film Noir, Monochromatic, Orson Welles, high contrast, dramatic shadows, mystery")
                     .add_string_choice("Impressionism", "Impressionism, Painterly, Claude Monet, light effects, outdoor scenes, everyday life")
                     .add_string_choice("Minimalist", "Minimalist, Simplified, Agnes Martin, bare essentials, geometric, neutral colors")
                     .add_string_choice("Modern", "Modern, Avant-garde, Piet Mondrian, non-representational, experimental, abstract")
                     .add_string_choice("Neo-Gothic", "Neo-Gothic, Dark, H.R. Giger, intricate detail, macabre, architectural elements")
                     .add_string_choice("Pixel Art", "Pixel Art, Retro, Shigeru Miyamoto, 8-bit, digital, geometric")
                     .add_string_choice("Pop Art", "Pop Art, Colorful, Andy Warhol, mass culture, ironic, bold")
                     .add_string_choice("Post-Impressionism", "Post-Impressionism, Expressive, Vincent Van Gogh, symbolic, bold colors, heavy brushstrokes")
                     .add_string_choice("Renaissance", "Renaissance, Realistic, Michelangelo, perspective, humanism, religious themes")
                     .add_string_choice("Retro / Vintage", "Retro / Vintage, Nostalgic, Norman Rockwell, past styles, soft colors, romantic")
                     .add_string_choice("Romanticism", "Romanticism, Emotional, Caspar David Friedrich, nature, dramatic, imaginative")
                     .add_string_choice("Surrealism", "Surrealism, Dreamlike, Salvador Dalí, irrational, bizarre, subconscious")
                     .add_string_choice("Steampunk", "Steampunk, Futuristic, H.G. Wells, industrial, Victorian, mechanical")
                     .add_string_choice("Street Art", "Street Art, Public, Keith Haring, social commentary, bold colors, mural")
                     .add_string_choice("Watercolor", "Watercolor, Translucent, J.M.W. Turner, lightness, fluid, landscape")
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
                    o.name("model")
                     .description("Model")
                     .kind(CommandOptionType::String)
                     .required(false)
                     .add_string_choice("Flexible (default)", "default")
                })
            })
        }).await;

        if let Err(e) = commands {
            panic!("Error registering commands: {:?}", e);
        }
    }
}
