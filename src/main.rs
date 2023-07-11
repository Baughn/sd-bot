use std::{path, io::{Write}, vec, os::unix::prelude::PermissionsExt, time::Duration, future::pending, pin::Pin, collections::HashMap};

use anyhow::{Result, bail, Context};

use futures::{prelude::*};
use irc::{client::prelude::*};
use log::{info, warn, error, debug, trace};
use reqwest::RequestBuilder;
use serde::{Serialize, Deserialize};
use tokio::sync::{mpsc, oneshot};
use tokio_retry::{strategy::ExponentialBackoff, Retry};

mod discord;

const BACKEND: &str = "http://localhost:8188";
const WEBHOST: &str = "brage.info";
const WEBHOST_DIR: &str = "GAN/ganbot2";
const CLIENT_ID: &str = "ganbot2";


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendCommand {
    model_name: String, // --model, -m
    linguistic_prompt: String, // Default.
    supporting_prompt: String, // --style -s
    negative_prompt: String, // --no
    use_pos_default: bool, // --np
    use_neg_default: bool, // --nn
    guidance_scale: f32, // --scale
    aesthetic_scale: f32, // --aesthetic, -a
    steps: u32, // --steps
    count: u32, // --count, -c
    seed: u32, // --seed
    // Width and height cannot be set directly; they are derived from --ar.
    width: u32, 
    height: u32,
}

impl BackendCommand {
    pub fn from_dream(dream: &str) -> Result<Self> {
        // This parses the !dream IRC/Discord command.
        // It's basically a command line, but with special handling for --style and --no.
        // TODO: Load some of this from config.json.
        let mut model_name = "default";
        let mut linguistic_prompt = vec![];
        let mut supporting_prompt = vec![];
        let mut negative_prompt = vec![];
        let mut use_pos_default = true;
        let mut use_neg_default = true;
        let mut guidance_scale = 8.0;
        let mut aesthetic_scale = 8.0;
        let mut steps = 50;
        let mut count = 2;
        // Default seed is the POSIX timestamp.
        let mut seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as u32;
        let mut width = 1280;
        let mut height = 816;
        
        let mut last_option = None;
        let mut last_value = None;
        let mut reading_supporting_prompt = false;
        let mut reading_negative_prompt = false;

        for token in dream.split_whitespace() {
            if last_option.is_some() {
                // Did we previously see an option (that was missing its argument)?
                last_value = Some(token);
            } else if token == "--no" {
                reading_supporting_prompt = false;
                reading_negative_prompt = true;
            } else if token == "--style" {
                reading_negative_prompt = false;
                reading_supporting_prompt = true;
            } else if token == "--prompt" {
                reading_supporting_prompt = false;
                reading_negative_prompt = false;
            } else if let Some(token) = token.strip_prefix("--") {
                // It's an option, but does it include the value?
                let mut parts = token.splitn(2, '=');
                last_option = Some(parts.next().unwrap());
                last_value = parts.next();
            } else if let Some(token) = token.strip_prefix('-') {
                // It's a short-form option, which never includes the value.
                last_option = Some(token);
            } else {
                // It's part of one of the prompts.
                if reading_supporting_prompt {
                    supporting_prompt.push(token);
                } else if reading_negative_prompt {
                    negative_prompt.push(token);
                } else {
                    linguistic_prompt.push(token);
                }
            }
            // Did we just finish reading an option?
            if let Some(value) = last_value {
                match last_option.unwrap() {
                    "model" | "m" => model_name = value,
                    "style" | "s" => supporting_prompt.push(value),
                    "np" => use_pos_default = false,
                    "nn" => use_neg_default = false,
                    "scale" => guidance_scale = value.parse().context("Scale must be a number")?,
                    "aesthetic" | "a" => aesthetic_scale = value.parse().context("Aesthetic scale must be a number")?,
                    "steps" => steps = value.parse().context("Steps must be a number")?,
                    "count" | "c" => count = value.parse().context("Count must be a number")?,
                    "seed" => seed = value.parse().context("Seed must be a number")?,
                    "ar" => {
                        let mut parts = value.splitn(2, ':');
                        let ar_x = parts.next().context("AR must be in the form W:H")?;
                        let ar_y = parts.next().context("AR must be in the form W:H")?;
                        let ar_x: f32 = ar_x.parse().context("AR must be in the form W:H")?;
                        let ar_y: f32 = ar_y.parse().context("AR must be in the form W:H")?;
                        // Width and height are derived from AR.
                        // The total number of pixels is fixed at 1 megapixel.
                        let ar: f32 = ar_x / ar_y;
                        width = (1024.0 * ar.sqrt()).round() as u32;
                        height = (1024.0 / ar.sqrt()).round() as u32;
                        // Shrink dimensions so that they're multiples of 8.
                        width -= width % 8;
                        height -= height % 8;
                    },
                    x => bail!("Unknown option: {}", x),
                }
                last_option = None;
                last_value = None;
            }
        }

        // Do some final validation.
        if linguistic_prompt.is_empty() {
            bail!("Linguistic prompt is required");
        }
        if supporting_prompt.is_empty() {
            bail!("Style prompt is required; use --style with `comic book, artistic` or something similar, e.g. `cat --style anime`. Style prompts should describe the genre, not the specific image.");
        }
        if !(1.0..=30.0).contains(&guidance_scale) {
            bail!("Scale must be between 1 and 30");
        }
        if !(1.0..=30.0).contains(&aesthetic_scale) {
            bail!("Aesthetic scale must be between 1 and 30");
        }
        if steps < 1 || count < 1 {
            bail!("We're done! Wasn't that fast?")
        }
        if count > 16 {
            bail!("Count must be 16 or less");
        }

        let command = BackendCommand {
            model_name: model_name.to_string(),
            linguistic_prompt: linguistic_prompt.join(" "),
            supporting_prompt: supporting_prompt.join(" "),
            negative_prompt: negative_prompt.join(" "),
            use_pos_default,
            use_neg_default,
            guidance_scale,
            aesthetic_scale,
            steps,
            count,
            seed,
            width,
            height,
        };
        info!("Generated command configuration: {:?}", command);
        Ok(command)
    }
}


#[derive(Debug, Serialize, Deserialize)]
struct BackendResponse {
    images: Option<Vec<String>>,
    detail: Option<String>,
}

type JpegBlob = Vec<u8>;

#[derive(Debug)]
pub enum CommandResult {
    Failure(String),
    Success(Vec<JpegBlob>),
}

pub struct QueuedCommand {
    command: BackendCommand,
    sender: oneshot::Sender<CommandResult>,
}

// Dispatches an image generation command to the backend via HTTP.
async fn dispatch(command: &BackendCommand) -> Result<CommandResult> {
    // How many images can we generate at once?
    // In general, we have a limit of 3 MPixels per request.
    let max_batch_size: u32 = {
        let max_pixels = (3 * 1024 * 1024) as f32;
        let pixels_per_image = (command.width * command.height) as f32;
        (max_pixels / pixels_per_image) as u32
    };
    let mut remaining = command.count;
    let mut final_images = Vec::with_capacity(command.count as usize);
    let mut seed_offset = 0;
    while remaining > 0 {
        #[derive(Deserialize)]
        struct ComfyUIResponse {
            prompt_id: String,
            #[allow(dead_code)]
            number: u32,
        }
        let batch_size = std::cmp::min(remaining, max_batch_size);
        remaining -= batch_size;
        debug!("Generating {} images", batch_size);
        let request = build_query(
            batch_size, 
            BackendCommand {
                seed: command.seed + seed_offset,
                ..command.clone()
            }).context("failed to build query")?;
        seed_offset += 1;
        let response = request.send().await.context("failed to send request")?;
        let text = response.text().await.context("failed to read response")?;
        trace!("Response: {}", text);
        let parsed = serde_json::from_str::<ComfyUIResponse>(&text).context("failed to parse response")?;
        let prompt_id = parsed.prompt_id;
        debug!("Got prompt ID {}", prompt_id);
        // Now, we need to poll the history endpoint until it's done.
        let mut filenames = None;
        for _ in 0..2000 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let client = reqwest::Client::new();
            let history: serde_json::Value = client.get(format!("{}/history/{}", BACKEND, prompt_id))
                .send()
                .await
                .context("failed to poll history")?
                .json().await.context("failed to parse history")?;
            // If the history is empty, we're not done yet.
            if history.as_object().map(|o| o.is_empty()).unwrap_or(false) {
                continue;
            }
            // Otherwise, we're done.
            //
            // Extract the filenames from the history; these are the images we want.
            // They're at history[prompt_id].outputs.<number>.images[<index>].filename.
            let outputs = history.get(prompt_id)
                .and_then(|o| o.get("outputs")).context("history missing outputs")?;
            let outputs = outputs.as_object().context("outputs not an object")?;
            // This should just contain a single key, which is the number. We only care about the value.
            let suboutput = outputs.iter().next().context("outputs empty")?.1;
            let images = suboutput.get("images").context("suboutput missing images")?;
            let images = images.as_array().context("images not an array")?;
            filenames = images.iter().map(|i| {
                let filename = i.get("filename")?;
                let filename = filename.as_str()?;
                Some(filename.to_owned())
            }).collect::<Option<Vec<_>>>();
            break;
        }
        // If we didn't get any filenames, we timed out.
        let filenames = filenames.ok_or_else(|| anyhow::anyhow!("timed out waiting for images"))?;
        // Now, we need to download the images.
        let client = reqwest::Client::new();
        for filename in filenames {
            let image = client.get(format!("{}/view", BACKEND))
                .query(&[("filename", filename)])
                .send()
                .await
                .context("failed to download image")?
                .bytes().await.context("failed to read image")?;
            final_images.push(image.into());
        }
    }
    Ok(CommandResult::Success(final_images))

}

#[derive(Debug, Serialize, Deserialize)]
struct BotConfig {
    aliases: HashMap<String, String>,
    models: HashMap<String, BotModelConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BotModelConfig {
    workflow: String,
    baseline: String,
    refiner: String,
    default_positive: String,
    default_negative: String,
}

// Builds a query string for the backend.
// Since the backend is ComfyUI, this is a bit of a pain. We need to:
// - Load the model config from models.json. This can fail, if the model doesn't exist.
// - Load the workflow "JSON" from the model config. This actually has __STANDIN__ placeholders for the parameters.
// - Replace the placeholders with the actual parameters.
// - Confirm that the result is valid JSON.
// - Take the text, and pass it to /prompt as POST data.
fn build_query(batch_size: u32, command: BackendCommand) -> Result<RequestBuilder> {
    let config = std::fs::read_to_string("config.json").context("failed to read config.json")?;
    let config: BotConfig = serde_json::from_str(&config).context("failed to parse config.json")?;
    // First, check for aliases.
    let model_name = config.aliases.get(&command.model_name).cloned().unwrap_or(command.model_name.clone());
    // Then, load the model config.
    let model_config = config.models.get(&model_name).ok_or_else(|| anyhow::anyhow!("no such model: {}", model_name))?;
    // Load the workflow.
    let workflow = std::fs::read_to_string(&model_config.workflow).context("failed to read workflow")?;
    // Replace the placeholders.
    let supporting_prompt = command.supporting_prompt.clone() + if command.use_pos_default { &model_config.default_positive } else { "" };
    let negative_prompt = command.negative_prompt.clone() + if command.use_neg_default { &model_config.default_negative } else { "" };
    let steps_cutover = (command.steps as f32 * 0.66) as u32;

    fn json_encode_string(s: &str) -> String {
        let mut result = String::new();
        for c in s.chars() {
            match c {
                '"' => result.push_str("\\\""),
                '\\' => result.push_str("\\\\"),
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                '\t' => result.push_str("\\t"),
                _ => result.push(c),
            }
        }
        result
    }

    let workflow = workflow
        .replace("__REFINER_CHECKPOINT__", &json_encode_string(&model_config.refiner))
        .replace("__BASE_CHECKPOINT__", &json_encode_string(&model_config.baseline))
        .replace("__NEGATIVE_PROMPT__", &json_encode_string(&negative_prompt))
        .replace("__PROMPT_A__", &json_encode_string(&command.linguistic_prompt))
        .replace("__PROMPT_B__", &json_encode_string(&supporting_prompt))
        .replace("__STEPS_TOTAL__", &command.steps.to_string())
        .replace("__FIRST_PASS_END_AT_STEP__", &steps_cutover.to_string())
        .replace("__WIDTH__", &command.width.to_string())
        .replace("__HEIGHT__", &command.height.to_string())
        .replace("__4xWIDTH__", &(command.width * 4).to_string())
        .replace("__4xHEIGHT__", &(command.height * 4).to_string())
        .replace("__SEED__", &command.seed.to_string())
        .replace("__BASE_CFG__", &command.guidance_scale.to_string())
        .replace("__REFINER_CFG__", &command.guidance_scale.to_string())
        .replace("__BATCH_SIZE__", &batch_size.to_string())
        .replace("__POSITIVE_A_SCORE__", &command.aesthetic_scale.to_string())
        .replace("__NEGATIVE_A_SCORE__", "1.0");
    // Confirm that the result is valid JSON.
    let workflow: serde_json::Value = serde_json::from_str(&workflow).context("failed to parse augmented workflow")?;
    #[derive(Debug, Serialize)]
    struct Request {
        prompt: serde_json::Value,
        client_id: String,
    }
    let request = Request {
        prompt: workflow,
        client_id: CLIENT_ID.to_string(),
    };
    // Take the text, and pass it to /prompt as POST data.
    let request = serde_json::to_string(&request).context("failed to serialize request")?;
    let request = reqwest::Client::new().post(format!("{}/prompt", BACKEND))
        .body(request);
    Ok(request)
}


async fn dispatch_and_retry(command: QueuedCommand) -> Result<()> {
    let retry_strategy = ExponentialBackoff::from_millis(500).max_delay(Duration::from_secs(5)).take(5);
    let result = Retry::spawn(retry_strategy, || async {
        trace!("Dispatching command: {:?}", command.command);
        let result = dispatch(&command.command).await;
        if let Err(ref e) = result {
            warn!("Failed to dispatch command: {:?}", e);
        }
        result
    }).await;
    trace!("Dispatched command: {:?}", command.command);
    let result = match result {
        Ok(result) => result,
        Err(e) => {
            CommandResult::Failure(format!("Failed to dispatch command: {:?}", e))
        },
    };
    if let Err(e) = command.sender.send(result) {
        error!("Failed to send command result: {:?}", e);
    }
    Ok(())
}


fn select_command(commands: &mut Vec<QueuedCommand>, last_model: &str) -> Option<QueuedCommand> {
    let mut index = None;
    for (i, command) in commands.iter().enumerate() {
        if command.command.model_name == *last_model {
            index = Some(i);
            break;
        }
    }
    if let Some(index) = index {
        Some(commands.remove(index))
    } else {
        commands.pop()
    }
}

// Image generation dispatcher.
// Implements a simple prioritization scheme that mostly just avoids unnecessary model reloads.
async fn dispatcher(mut commands: mpsc::Receiver<QueuedCommand>) -> Result<()> {
    // This is a state machine, with three states:
    // - Idle: The backend is idle, and new commands should be sent immediately.
    // - Busy: The backend is busy, and new commands should be queued.
    // - Ready: The backend has finished a command, and a new command should be sent if one is available.
    //
    // Idle -> Busy: When a new command is received.
    // Busy -> Busy: When a new command is received while the backend is busy.
    // Busy -> Ready: When the backend finishes a command.
    // Ready -> Idle: When no new commands are available.
    enum State {
        Idle,
        Busy,
        Ready,
    }
    let mut state = State::Idle;
    let mut queue: Vec<QueuedCommand> = Vec::new();
    let mut last_model: String = "default".to_owned();
    let mut executing: Pin<Box<dyn Future<Output = Result<()>>>> = Box::pin(pending());

    loop {
        match state {
            State::Idle => {
                let command = commands.recv().await.context("Command channel closed")?;
                queue.push(command);
                state = State::Ready;
                debug!("Transitioning to Ready state");
            },
            State::Busy => {
                // Wait for either completion of the current command or a new command.
                tokio::select! {
                    command = commands.recv() => {
                        let command = command.context("Command channel closed")?;
                        queue.push(command);
                    },
                    result = &mut executing => {
                        result.context("Command execution failed")?;
                        state = State::Ready;
                    }
                };
            },
            State::Ready => {
                let command = select_command(&mut queue, &last_model);
                if let Some(command) = command {
                    state = State::Busy;
                    last_model = command.command.model_name.clone();
                    executing = Box::pin(dispatch_and_retry(command));
                } else {
                    state = State::Idle;
                }
            }
        }
    }
}

// Handles a !dream command.
async fn handle_dream(target: &str, nickname: &str, msg: &str, dispatcher: &mut mpsc::Sender<QueuedCommand>) -> Result<String> {
    info!("!dream from {} in {}: {}", nickname, target, msg);
    let (sender, receiver) = oneshot::channel();
    // Parse the command.
    let prompt = msg.trim_start_matches("!dream").trim();
    let command = QueuedCommand {
        command: BackendCommand::from_dream(prompt)?,
        sender,
    };
    dispatcher.send(command).await.context("failed to send command")?;
    let result = receiver.await.context("failed to receive command result")?;
    match result {
        CommandResult::Failure(e) => {
            bail!("failed to generate image: {}", e);
        },
        CommandResult::Success(images) => {
            let url = upload_images(images).await.context("failed to upload images")?;
            Ok(url)
        },
    }
}

pub async fn upload_images(images: Vec<Vec<u8>>) -> Result<String> {
    let uuid = uuid::Uuid::new_v4();
    let mut urls = Vec::new();
    for (i, data) in images.iter().enumerate() {
        let filename = format!("{}.{}.jpg", uuid, i);
        info!("Uploading {} bytes to {}", data.len(), filename);
        // Save the image to a temporary file.
        let tmp = tempfile::NamedTempFile::new().context("failed to create temporary file")?;
        tmp.as_file().write_all(data).context("failed to write temporary file")?;
        tmp.as_file().set_permissions(PermissionsExt::from_mode(0o644)).context("failed to chmod temporary file")?;
        // We'll just call scp directly. It's not like we're going to be uploading a lot of images.
        let mut command = tokio::process::Command::new("scp");
        command
            .env_remove("LD_PRELOAD")  // SSH doesn't like tcmalloc.
            .arg("-F").arg("None") // Don't read ~/.ssh/config.
            .arg("-p")  // Preserve access bits.
            .arg(tmp.path())
            .arg(format!("{}:web/{}/{}", WEBHOST, WEBHOST_DIR, filename));
        debug!("Running {:?}", &command);
        let status = command
            .status().await
            .context("failed to run scp")?;
        if !status.success() {
            bail!("scp failed: {}", status);
        }
        
        urls.push(format!("https://{}/{}/{}", WEBHOST, WEBHOST_DIR, filename));
    }

    Ok(urls.join(" "))
}

async fn irc_client(dispatcher: mpsc::Sender<QueuedCommand>) -> Result<()> {
    let config = Config::load(path::Path::new("irc.toml")).context("failed to load irc.toml")?;
    let mut client = Client::from_config(config).await.context("failed to connect to IRC")?;
    client.identify().context("failed to identify to IRC")?;

    let mut stream = client.stream()?;
    while let Some(message) = stream.next().await.transpose()? {
        if let Command::PRIVMSG(ref target, ref msg) = message.command {
            let target = target.to_owned();
            let msg = msg.trim().to_owned();
            let sender = client.sender();
            let nickname = message.source_nickname().context("No nickname")?.to_owned();
            let mut dispatcher = dispatcher.clone();
            if !target.starts_with('#') {
                client.send_privmsg(nickname, "/msg is currently not supported, please use a channel.").context("failed to send message")?;
                continue;
            }
            tokio::spawn(async move {
                if msg.starts_with("!dream") {
                    let answer: Result<String> = handle_dream(&target, &nickname, &msg, &mut dispatcher).await;
                    match answer {
                        Ok(answer) => {
                            let answer = format!("{}: {}", nickname, answer);
                            sender.send_privmsg(target, answer).expect("failed to send answer");
                        },
                        Err(e) => {
                            let e = e.to_string();
                            let els: Vec<&str> = e.lines().collect();
                            if els.len() > 1 {
                                for line in els {
                                    error!("Error: {}", line);
                                    sender.send_privmsg(&nickname, line).expect("failed to send error message");
                                    tokio::time::sleep(Duration::from_millis(500)).await;
                                }
                            } else {
                                error!("Error: {e:?}");
                                sender.send_privmsg(&target, format!("{nickname}: {e:?}")).expect("failed to send error message");
                            }
                        }
                    }
                }
            });
        }
    }

    bail!("IRC client exited");
}


#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let (dispatcher_tx, dispatcher_rx) = mpsc::channel(32);

    // Await all futures.
    tokio::select! {
        err = dispatcher(dispatcher_rx) => {
            bail!("Dispatcher failed: {:?}", err);
        },
        irc = irc_client(dispatcher_tx.clone()) => {
            bail!("IRC client failed: {:?}", irc);
        },
        discord = discord::client(dispatcher_tx.clone()) => {
            bail!("Discord client failed: {:?}", discord);
        },
    }
}
