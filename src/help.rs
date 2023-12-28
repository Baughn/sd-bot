use anyhow::{Context, Result};
use log::info;

use std::collections::{HashMap, HashSet};

use crate::BotContext;

/// This is more of a help tree than a help file.
/// Due to Discord size limits, it's split into multiple messages. Some also
/// include data from the current bot configuration, which is more or less
/// why it's all in code in here. Regardless, you also get the option of
/// "asking it questions" by feeding the full thing to GPT-3.5.
/// 
/// Probably the right thing to do here is to return a lazily evaluated tree...
/// But that's a lot of work, and computers are fast.
/// So we just return a giant tree.

/// I think you get the picture.
#[derive(Debug)]
struct HelpText {
    text: String,
    children: HashMap<&'static str, HelpText>,
}

/// Given a help request, returns a help response.
pub async fn handler(context: &BotContext, prefix: &str, request: &str) -> Result<(String, Vec<&'static str>)> {
    let request = request.trim();
    // Let's first check if matches a predefined help topic.
    let r = root(context, prefix).await;
    let mut text = &r;
    for path_element in request.split('.') {
        let mut updated = false;
        if path_element.is_empty() {
            // If we get an empty path element, we'll just return where we are.
            break;
        }
        for (key, child) in text.children.iter() {
            if key.to_lowercase() == path_element.to_lowercase() {
                text = child;
                updated = true;
            }
        }
        if !updated {
            // We had a path element, but didn't find a match.
            // Defer to GPT.
            info!("Generating help response for {}", request);
            let response = handle_with_gpt(context, prefix, request).await.context("While creating help")?;
            return Ok((response, Vec::new()));
        }
    }
    // If we got here, we've found a predefined help topic.
    // Trim the lines, I guess.
    let trimmed = text.text.lines().map(|s| s.trim()).collect::<Vec<_>>().join("\n");

    Ok((trimmed, text.children.keys().cloned().collect()))
}

async fn handle_with_gpt(context: &BotContext, prefix: &str, request: &str) -> Result<String> {
    let mut prompt = format!(
        "Here is some documentation:\n\n{}",
        full_text(&root(context, prefix).await, 1, ""));
    prompt += "\nGiven the above, answer the following question. Be as concise as possible, but no more. If the question isn't about image generation, then _ONLY_ respond with a request to use !ask instead of !help. \n\n";
    context.prompt_generator.gpt3_5(&prompt, request).await
}

/// Recursive function that just collapses the tree.
fn full_text(text: &HelpText, depth: usize, position: &str) -> String {
    let mut ret = format!("{} {}: ", "#".repeat(depth), position); 
    ret += &text.text;
    for (key, child) in text.children.iter() {
        ret += &full_text(child, depth + 1, &format!("{}.{}", position, key));
    }
    ret
}


async fn root(context: &BotContext, prefix: &str) -> HelpText {
    HelpText {
        text: format!("Welcome to the GANBot help system. Here's what I can do:
        - `{prefix}help` - This help system.
        - `{prefix}prompt` - Image-generation from a text prompt. You can choose model, aspect ratio and so on freely. Click the button to see the full explanation.
        - `{prefix}dream` - Image-generation from a loose description, using GPT-4 to fill in the blanks. This only works with the (highly flexible) baseline SDXL model; I recommend you use the output as a guide for how to start on your own prompts.
        - `{prefix}settings` - Configure the bot's behavior. This is a work in progress.
        
        Common flags for /prompt:
        - --style — The style to feed into the model; affects everything after the flag. See the Prompting help section for more information.
        -- --no — Things to avoid; affects everything after the flag. See the Prompting help section for more information.
        - --model (-m) — The model to use. Defaults to SDXL. Every model has a different set of capabilities, but SDXL is by far the most flexible.
        - --ar — The aspect ratio to use. Defaults to 1:1.
        - --seed (-s) — The seed to use. Defaults to a random number, but you should set this to a specific value when comparing prompts
        - --count (-c) — The number of pictures to generate. You can request up to 16, but this down-prioritizes your request.
        
        
        You can also use `{prefix}help <arbitrary text>` to ask me questions. I'll try to answer them as best I can."),
        children: HashMap::from([
            ("Prompting", prompting(context)),
            ("Models", models(context).await),
            ("LoRA (specific characters) (UNIMPLEMENTED)", loras(context).await),
            ("Tips and tricks (UNIMPLEMENTED)", tips_and_tricks(context)),
        ]),
    }
}

fn prompting(_context: &BotContext) -> HelpText {
    HelpText {
        children: HashMap::new(),
        text: "Prompting is the process of narrowing down an image-generation input that gives you the picture you wanted, or at least something vaguely close. The 'ideal' prompt depends on the model you've selected, but in general there are three different types of models—and hence prompts—that you'll need to keep in mind.
        
        SDXL-based models: The default, and frankly easiest to use. SDXL understands simple english; you can tell it \"A red-haired girl standing next to a green-haired boy\" and it'll do its best to give you that. It's not perfect, Like every image-generation model, it's likely that you'll need to try a few different prompts before you get something you like. But it's a good starting point.
        
        Unlike the newer SD 1.5 models, SDXL lacks any highly refined models trained for aesthetics. The base model, flexible as it is, is equally capable of producing good and bad results. If you're not getting the quality you wanted, ask yourself if your prompt belongs as the title of a painting... or as the caption of a low-quality piece of fanart. If it's the latter, you might want to try feeding it through /dream. Or try a different model.
        
        Avoid pronouns and other placeholder words. In a sentence like \"A girl and her red jacket\", it's not smart enough to understand that the jacket belongs to the girl. \"A girl wearing a red jacket\" is better.
        
        SDXL-based models actually have two text encoders, one that understands English and one that understands tags. The second one is more finicky, but it's what --style feeds into (if you're using one), and it can give good results.
        
        The /dream command is tuned to give you good prompts for SDXL-based models.
        
        ======
        
        SD 1.5 anime-style models: These models are a bit more finicky. They're trained on a very specific set of prompts, namely tags from the Danbooru dataset. English doesn't work well; use prompts like \"blue hair, red eyes, 1girl\" instead. You can find a list of tags by searching on http://danbooru.donmai.us, but bear in mind that the model is trained on only those tags with at least a few hundred images. If you're not getting the results you want, try adding more tags.
        
        A few 1.5 anime models, most notably Counterfeit 3.0, were trained on BLIP2 captions as well and have a semi-functional understanding of English. You can try using English prompts with these models, but don't expect miracles.
        
        ======
        
        SD 1.5 photorealistic models: The lineage of these models doesn't include Danbooru, so they don't understand tags. Instead they understand English... about as well as the --style encoder for SDXL-based models. You should try sticking to *very* simple English.

        ======

        Besides all of that? ***Experiment.*** Prompting is an art, not a science. You'll get better at it with practice.
        ".to_string()
    }
}

async fn models(context: &BotContext) -> HelpText {
    // Grab a list of models.
    let (aliases, models) = context.config.with_config(|c| (c.aliases.clone(), c.models.clone())).await;
    let text = "This list of models is sorted by genre, but also workflow. You should be able to tell which models are XL and which are 1.5.\n";
    let mut text: Vec<String> = vec![
        text.into(),
        "\n".into(),
        "## Aliases\n".into(),
    ];

    // First the aliases, aka. "genres".
    let aliases = aliases.iter().map(|(alias, mut target)| {
        // First, dereference the alias.
        while let Some(deref) = aliases.get(target) {
            target = deref;
        }
        let config = models.get(target).expect("Alias points to non-existent model");
        format!("-m {} ({}) — {}\n", alias, target, config.description)
    });
    text.extend(aliases);

    // Make a list of workflows.
    let workflows = models.iter().map(|(_, v)| {
        v.workflow.as_str()
    }).collect::<HashSet<_>>();

    // Next we'll make a list of models for each workflow.
    let workflows = workflows.iter().map(|workflow| {
        let models = models.iter().filter(|(_, v)| v.workflow == *workflow).map(|(k, v)| {
            format!("-m {} — {}", k, v.description)
        }).collect::<Vec<_>>();
        format!("\n## {}\n{}", workflow, models.join("\n"))
    });
    text.extend(workflows);


    HelpText {
        children: HashMap::new(),
        text: text.join(""),
    }
}

async fn loras(_context: &BotContext) -> HelpText {
    HelpText {
        children: HashMap::new(),
        text: "Sorry, LoRAs aren't implemented yet. I'm working on it.".to_string(),
    }
}

fn tips_and_tricks(_context: &BotContext) -> HelpText {
    HelpText {
        children: HashMap::new(),
        text: "Sorry, tips and tricks aren't implemented yet. I'm working on it.\n
        \n
        Once they are, this will be where you can see the full list.\n
        But not to worry. You'll see them without going looking.\n".to_string(),
    }
}
