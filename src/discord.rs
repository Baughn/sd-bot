use anyhow::{Context as anyhowCtx, bail, Result};
use log::{trace, info, error};

use serenity::{prelude::*, async_trait, model::{prelude::{*, command::{Command, CommandOptionType}, application_command::{ApplicationCommandInteraction, CommandDataOptionValue}, component::ButtonStyle}, interactions::message_component::ActionRow}, builder::CreateActionRow};
use tokio_stream::StreamExt;

use crate::{BotContext, utils, generator::{self, GenerationEvent}};



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
        let token = std::env::var("DISCORD_BOT_TOKEN")
            .context("Expected a token in the environment")?;
        Ok(Self { context, token })
    }

    pub async fn run(&mut self) -> Result<()> {
        let intents = GatewayIntents::non_privileged();

        let mut client = Client::builder(&self.token, intents)
            .event_handler(Handler::new(self.context.clone()))
            .await
            .context("Error creating client")?;

        client.start()
            .await
            .context("Discord client error")?;

        bail!("Discord client unexpectedly stopped");
    }
}

impl Handler {
    async fn handle_command(&self, ctx: &Context, command: &ApplicationCommandInteraction) -> Result<()> {
        let cprefix = self.context.config.with_config(|c| c.command_prefix.clone()).await;
        let cmd = command.data.name.trim_start_matches(&cprefix);
        let mention_user = command.user.mention();
        let mut status_text;
        let stream = match cmd {
            "dream" => {
                let prompt = command.data.options.get(0).context("Expected prompt")?.resolved.as_ref().context("Expected prompt")?;
                if let CommandDataOptionValue::String(prompt) = prompt {
                    status_text = format!("Dreaming based on `{}`", prompt);
                    self.context.image_generator.generate(generator::UserRequest {
                        user: command.user.to_string(),
                        raw: prompt.to_string(),
                        dream: Some(prompt.to_string()),
                        source: generator::Source::Discord,
                    }).await
                } else {
                    bail!("Expected parameter to be a string");
                }
            },
            "prompt" => {
                let prompt = command.data.options.get(0).context("Expected prompt")?.resolved.as_ref().context("Expected prompt")?;
                let style = command.data.options.get(1);
                info!("Style: {:?}", style);
                // TODO: Implement the options.

                if let CommandDataOptionValue::String(prompt) = prompt {
                    let raw = prompt.to_string();
                    status_text = format!("Dreaming about `{}`", raw);
                    self.context.image_generator.generate(generator::UserRequest {
                        user: command.user.to_string(),
                        raw: raw,
                        dream: None,
                        source: generator::Source::Discord,
                    }).await
                } else {
                    bail!("Expected parameter to be a string");
                }
            },
            x => bail!("Unknown command: {}", x),
        };
        let mut stream = Box::pin(stream);
        // When generating, we first create an interaction response in which we
        // display event data such as queue #s.
        // Once the generation is complete, we send a followup message with the
        // results.
        command.create_interaction_response(&ctx.http, |message| {
            message.kind(InteractionResponseType::ChannelMessageWithSource)
                    .interaction_response_data(|message| {
                        message.content(&status_text)
                    })
        }).await.context("Creating initial response")?;
        

        while let Some(event) = stream.next().await {
            trace!("Event: {:?}", event);
            match event {
                GenerationEvent::GPTCompleted(c) => {
                    status_text = format!("Dreaming about `{}`\nBased on `{}`", c.raw, c.dream.unwrap());
                    command.edit_original_interaction_response(&ctx.http, |message| {
                        message.content(&status_text)
                    }).await?;
                },
                GenerationEvent::Parsed(_) => (
                    // TODO: Implement this.
                ),
                GenerationEvent::Queued(n) => {
                    status_text.push_str(&format!("\nQueued at position {n}"));
                    command.edit_original_interaction_response(&ctx.http, |message| {
                        message.content(&status_text)
                    }).await?;
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
                    command.edit_original_interaction_response(&ctx.http, |message| {
                        message.content(&status_text)
                    }).await?;
                },
                GenerationEvent::Error(e) => {
                    // There was an error. We'll send it as a followup message.
                    let err = format!("{} Error: {:#}", mention_user, e);
                    let err = utils::segment_one(&err, 1800);
                    command.create_followup_message(&ctx.http, |message| {
                        message.content(err)
                    }).await?;
                    // Leave the original message as-is; it has the prompt.
                },
                GenerationEvent::Completed(c) => {
                    // TODO: Implement overview & upscaling/etc. buttons.
                    //let overview = utils::overview_of_pictures(&c.images)?;
                    //let all: Vec<Vec<u8>> = std::iter::once(overview).chain(c.images.into_iter()).collect();
                    //// Send the results to the user.
                    //let urls = utils::upload_images(&self.context.config, all).await
                    //    .context("failed to upload images")?;

                    let mut text = vec![
                        format!("Dreams of `{}` | For {}", c.base.base.raw, command.user.mention()),
                        format!("Seed {} | {}x{} | {} steps | Aesthetic {} | Guidance {}",
                            c.base.seed, c.base.width, c.base.height, c.base.steps, c.base.aesthetic_scale, c.base.guidance_scale)
                    ];
                    if let Some(dream) = c.base.base.dream {
                        text.push(format!("Based on `{}`", dream));
                    }
                    let urls = utils::upload_images(&self.context.config, c.images).await
                        .context("failed to upload images")?;
                    text.extend(urls);
                    // Create the final message, with:
                    // - One row with a delete, restyle, and retry button.
                    // - NxM rows of upscale buttons (up to 4x4).
                    command.create_followup_message(&ctx.http, |message| {
                        message.content(text.join("\n"))
                            .components(|c| {
                                c.add_action_row(self.action_buttons.clone())
                            })
                    }).await?;
                    // When all is said and done, delete the original message.
                    command.delete_original_interaction_response(&ctx.http).await?;
                },
            }
        }

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

        Self {
            action_buttons,
            context,
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
                    let e = format!("{:#}", e);
                    let e = utils::segment_lines(&e, 1800)[0];
                    // We might be in one of four states.
                    // - We haven't responded yet.
                    // - We've deferred the response.
                    // - We've responded with a status message.
                    // - We've responded with a result message.
                    //
                    // In general we just ignore and log errors in this error handler.
                    if command.create_interaction_response(&ctx.http, |message| {
                        message.kind(InteractionResponseType::ChannelMessageWithSource)
                                .interaction_response_data(|message| {
                                    message.content(format!("Error: {}", e))
                                })
                    }).await.is_ok() {
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
        let command_prefix = self.context.config.with_config(|c| c.command_prefix.clone()).await;
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
                     .add_string_choice("Shōnen Anime", "Action-oriented, Akira Toriyama (Dragon Ball), youthful, vibrant, dynamic")
                     .add_string_choice("Shōjo Anime", "Romantic, Naoko Takeuchi (Sailor Moon), emotional, detailed backgrounds, soft colors")
                     .add_string_choice("Seinen Anime", "Mature, Hajime Isayama (Attack on Titan), complex themes, realistic, detailed")
                     .add_string_choice("Abstract Expressionism", "Abstract, Jackson Pollock, spontaneous, dynamic, emotional")
                     .add_string_choice("Art Nouveau", "Decorative, Alphonse Mucha, organic forms, intricate, flowing")
                     .add_string_choice("Baroque", "Dramatic, Caravaggio, high contrast, ornate, realism")
                     .add_string_choice("Classical", "Proportionate, Leonardo da Vinci, balanced, harmonious, detailed")
                     .add_string_choice("Contemporary", "Innovative, Ai Weiwei, conceptual, diverse mediums, social commentary")
                     .add_string_choice("Cubism", "Geometric, Pablo Picasso, multi-perspective, abstract, fragmented")
                     .add_string_choice("Fantasy", "Imaginative, J.R.R. Tolkien, mythical creatures, dreamlike, detailed")
                     .add_string_choice("Film Noir", "Monochromatic, Orson Welles, high contrast, dramatic shadows, mystery")
                     .add_string_choice("Impressionism", "Painterly, Claude Monet, light effects, outdoor scenes, everyday life")
                     .add_string_choice("Minimalist", "Simplified, Agnes Martin, bare essentials, geometric, neutral colors")
                     .add_string_choice("Modern", "Avant-garde, Piet Mondrian, non-representational, experimental, abstract")
                     .add_string_choice("Neo-Gothic", "Dark, H.R. Giger, intricate detail, macabre, architectural elements")
                     .add_string_choice("Pixel Art", "Retro, Shigeru Miyamoto, 8-bit, digital, geometric")
                     .add_string_choice("Pop Art", "Colorful, Andy Warhol, mass culture, ironic, bold")
                     .add_string_choice("Post-Impressionism", "Expressive, Vincent Van Gogh, symbolic, bold colors, heavy brushstrokes")
                     .add_string_choice("Renaissance", "Realistic, Michelangelo, perspective, humanism, religious themes")
                     .add_string_choice("Retro / Vintage", "Nostalgic, Norman Rockwell, past styles, soft colors, romantic")
                     .add_string_choice("Romanticism", "Emotional, Caspar David Friedrich, nature, dramatic, imaginative")
                     .add_string_choice("Surrealism", "Dreamlike, Salvador Dalí, irrational, bizarre, subconscious")
                     .add_string_choice("Steampunk", "Futuristic, H.G. Wells, industrial, Victorian, mechanical")
                     .add_string_choice("Street Art", "Public, Keith Haring, social commentary, bold colors, mural")
                     .add_string_choice("Watercolor", "Translucent, J.M.W. Turner, lightness, fluid, landscape")
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


