use std::{path, sync::Arc, io::{Write, BufWriter, Cursor}, vec, net::TcpStream, fs::Permissions, os::unix::prelude::PermissionsExt, time::Duration, future::pending, pin::Pin};

use anyhow::{Result, bail, Context};
use base64::prelude::*;
use futures::{prelude::*, stream::FuturesUnordered, channel::oneshot};
use irc::{client::prelude::*, error};
use log::{info, warn, error, debug};
use serde::{Serialize, Deserialize};
use tokio::sync::{mpsc, Notify};
use tokio_retry::{strategy::ExponentialBackoff, Retry};


const BACKEND: &str = "http://localhost:8000/txt2img";
const WEBHOST: &str = "brage.info";
const WEBHOST_DIR: &str = "GAN/ganbot2";


#[derive(Debug, Serialize, Deserialize)]
struct BackendCommand {
    model_name: String,
    prompt: String,
    negative_prompt: String,
    use_pos_default: bool,
    use_neg_default: bool,
    use_refiner: bool,
    guidance_scale: f32,
    steps: u32,
    count: u32,
    seed: u32,
    width: u32,
    height: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackendResponse {
    images: Option<Vec<String>>,
    detail: Option<String>,
}

type JpegBlob = Vec<u8>;

#[derive(Debug)]
enum CommandResult {
    Failure(String),
    Success(Vec<JpegBlob>),
}

struct QueuedCommand {
    command: BackendCommand,
    sender: oneshot::Sender<CommandResult>,
}

// Dispatches an image generation command to the backend via HTTP.
async fn dispatch(command: &BackendCommand) -> Result<CommandResult> {
    let client = reqwest::Client::new();
    let response: BackendResponse = client.get(BACKEND)
        .query(&command)
        .send()
        .await
        .context("failed to send request")?
        .json().await.context("failed to parse response")?;
    
    if let Some(detail) = response.detail {
        bail!(detail);
    }
    // Convert the base64-encoded images to raw bytes.
    let images = response.images
        .ok_or_else(|| anyhow::anyhow!("no images in response"))?
        .into_iter()
        .map(|data| {
            let data = BASE64_STANDARD.decode(data.as_bytes()).context("failed to decode base64")?;
            Ok(data)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(CommandResult::Success(images))
}

async fn dispatch_and_retry(command: QueuedCommand) -> Result<()> {
    let retry_strategy = ExponentialBackoff::from_millis(500).take(5);
    let result = Retry::spawn(retry_strategy, || async {
        dispatch(&command.command).await
    }).await;
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
        command: BackendCommand {
            model_name: "default".to_owned(),
            prompt: prompt.to_owned(),
            negative_prompt: "blurry, text".to_owned(),
            use_pos_default: true,
            use_neg_default: true,
            use_refiner: true,
            guidance_scale: 8.0,
            steps: 50,
            count: 2,
            seed: 0,
            width: 1280,
            height: 720,
        },
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

async fn upload_images(images: Vec<Vec<u8>>) -> Result<String> {
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

async fn irc_client(mut dispatcher: mpsc::Sender<QueuedCommand>) -> Result<()> {
    let config = Config::load(path::Path::new("irc.toml")).context("failed to load irc.toml")?;
    let mut client = Client::from_config(config).await.context("failed to connect to IRC")?;
    client.identify().context("failed to identify to IRC")?;

    let mut stream = client.stream()?;
    while let Some(message) = stream.next().await.transpose()? {
        match message.command {
            Command::PRIVMSG(ref target, ref msg) => {
                let target = target.to_owned();
                let msg = msg.trim().to_owned();
                let sender = client.sender();
                let nickname = message.source_nickname().context("No nickname")?.to_owned();
                let mut dispatcher = dispatcher.clone();
                if !target.starts_with("#") {
                    client.send_privmsg(nickname, "/msg is currently not supported, please use a channel.").context("failed to send message")?;
                    continue;
                }
                tokio::spawn(async move {
                    if msg.starts_with("!dream") {
                        let answer: Result<String> = handle_dream(&target, &nickname, &msg, &mut dispatcher).await;
                        match answer {
                            Ok(answer) => {
                                let answer = format!("{}: {}", nickname, answer);
                                sender.send_privmsg(target, &answer).expect("failed to send answer");
                            },
                            Err(e) => {
                                for line in e.to_string().lines() {
                                    error!("Error: {}", line);
                                    sender.send_privmsg(&nickname, &line).expect("failed to send error message");
                                    tokio::time::sleep(Duration::from_millis(500)).await;
                                }
                            }
                        }
                    }
                });
            },
            _ => (),
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
        irc = irc_client(dispatcher_tx) => {
            bail!("IRC client failed: {:?}", irc);
        },
    }
}
