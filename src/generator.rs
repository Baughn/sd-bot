// These types describe the various stages of generation.
// Each is logically a superset of the previous, and the final struct includes the output.

use std::{
    fmt::Debug,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{bail, Context, Ok, Result};
use async_stream::try_stream;
use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
    select,
    stream::{empty, FusedStream},
    FutureExt, SinkExt, Stream, StreamExt,
};
use lazy_static::lazy_static;
use log::{debug, info, trace, warn};
use rand::seq::SliceRandom;
use reqwest::RequestBuilder;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_retry::{strategy::ExponentialBackoff, Retry};
use tokio_tungstenite as ws;
use uuid::Uuid;

use crate::{
    config::{BotBackend, BotConfig, BotConfigModule},
    db::DatabaseModule,
    gpt::PromptGeneratorModule,
    utils,
};

/// Used to determine if a model name is close enough to a real model name.
/// And for the tests.
const SIMILARITY_THRESHOLD: f64 = 0.7;

/// generate() is the entry point for the generator.
/// It returns a stream of these.
#[derive(Debug)]
pub enum GenerationEvent {
    /// GPT-4 generation has been completed.
    /// (This event may be skipped.)
    GPTCompleted(UserRequest),
    /// Parsing has completed successfully
    /// and the request is ready to be sent to the backend.
    Parsed(ParsedRequest),
    /// The request is queued for processing, at position N.
    Queued(u32),
    /// Generation has started, and is N% complete (0-100).
    Generating(u32),
    /// Generation has completed.
    Completed(CompletedRequest),
    /// Something broke.
    /// The generator has stopped.
    Error(anyhow::Error),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserRequest {
    // The name of the user who made the request.
    pub user: String,
    pub dream: Option<String>,
    pub raw: String,
    pub source: Source,
    #[serde(default)]
    pub private: bool,
    pub comment: Option<String>, // Sometimes filled in by GPT-4.
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Source {
    Discord,
    Irc,
    Unknown,
}

// ParsedRequest adds the computed resolution, aesthetic values, etc.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParsedRequest {
    pub base: UserRequest,
    pub model_name: String,        // --model, -m
    pub linguistic_prompt: String, // Default.
    pub supporting_prompt: String, // --style -s
    pub negative_prompt: String,   // --no
    pub use_pos_default: bool,     // --np
    pub use_neg_default: bool,     // --nn
    pub guidance_scale: f32,       // --scale
    pub aesthetic_scale: f32,      // --aesthetic, -a
    pub steps: Option<u32>,        // --steps
    pub count: u32,                // --count, -c
    pub seed: u32,                 // --seed
    // Width and height cannot be set directly; they are derived from --ar.
    pub width: u32,
    pub height: u32,
}

impl Default for ParsedRequest {
    fn default() -> Self {
        Self {
            base: UserRequest {
                user: "".to_string(),
                dream: None,
                raw: "".to_string(),
                source: Source::Unknown,
                comment: None,
                private: true,
            },
            model_name: "default".to_string(),
            linguistic_prompt: "".to_string(),
            supporting_prompt: "".to_string(),
            negative_prompt: "".to_string(),
            use_pos_default: true,
            use_neg_default: true,
            guidance_scale: 5.5,
            aesthetic_scale: 20.0,
            steps: None,
            count: 4,
            seed: 0,
            width: 1024,
            height: 1024,
        }
    }
}

pub struct CompletedRequest {
    pub base: ParsedRequest,
    pub images: Vec<JpegBlob>,
    pub uuid: Uuid,
}

impl Debug for CompletedRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompletedRequest")
            .field("base", &self.base)
            .field("images", &self.images.len().to_string())
            .finish()
    }
}

type JpegBlob = Vec<u8>;

impl ParsedRequest {
    pub fn parse_aspect_ratio(stride: u32, target: u32, value: &str) -> Result<(u32, u32)> {
        let mut parts = value.splitn(2, ':');
        let ar_x = parts.next().context("AR must be in the form W:H")?;
        let ar_y = parts.next().context("AR must be in the form W:H")?;
        let ar_x: f32 = ar_x.parse().context("AR must be in the form W:H")?;
        let ar_y: f32 = ar_y.parse().context("AR must be in the form W:H")?;
        // Width and height are derived from AR.
        // The total number of pixels is fixed at 1 megapixel for most models.
        let target = target as f32;
        let ar: f32 = ar_x / ar_y;
        let mut width = (target * ar.sqrt()).round() as u32;
        let mut height = (target / ar.sqrt()).round() as u32;
        // Make sure aspect ratio is less than 1:4.
        if !(0.25..=4.0).contains(&ar) {
            bail!("Aspect ratio must be between 1:4 and 4:1");
        }
        // Shrink dimensions so that they're multiples of stride.
        width -= width % stride;
        height -= height % stride;

        Ok((width, height))
    }

    pub fn from_request(config: &BotConfig, request: UserRequest) -> Result<Self> {
        // This parses the !dream IRC/Discord command.
        // It's basically a command line, but with special handling for --style and --no.
        // TODO: Load some of this from config.json.
        let mut parsed = ParsedRequest {
            // Default seed is the POSIX timestamp.
            base: request.clone(),
            seed: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32,
            ..Default::default()
        };
        // Parsing outputs.
        let mut linguistic_prompt = vec![];
        let mut supporting_prompt = vec![];
        let mut negative_prompt = vec![];
        let mut override_wh = None;

        // Parsing state.
        let mut last_option = None;
        let mut last_value = None;
        let mut reading_supporting_prompt = false;
        let mut reading_negative_prompt = false;

        // For a final pre-processing step, turn — (em dash) into -- (double dash).
        // Because Apple.
        let raw = request.raw.replace('—', "--");
        let mut ar = "1:1";

        for token in raw.split_whitespace() {
            let mut add_to_prompt = |token| {
                if reading_supporting_prompt {
                    supporting_prompt.push(token);
                } else if reading_negative_prompt {
                    negative_prompt.push(token);
                } else {
                    linguistic_prompt.push(token);
                }
            };
            if last_option.is_some() {
                // Did we previously see an option (that was missing its argument)?
                last_value = Some(token);
            } else if token == "-" || token == "--" {
                // These should be treated as part of the prompt.
                add_to_prompt(token);
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
                add_to_prompt(token);
            }
            // Did we just finish reading an option?
            // First, check for flags that take no arguments.
            match last_option {
                Some("np") => {
                    parsed.use_pos_default = false;
                    last_option = None;
                }
                Some("nn") => {
                    parsed.use_neg_default = false;
                    last_option = None;
                }
                _ => {}
            };
            // Then, check for flags that do take arguments.
            if let Some(value) = last_value {
                match last_option.unwrap() {
                    "model" | "m" => value.clone_into(&mut parsed.model_name),
                    "style" | "s" => supporting_prompt.push(value),
                    "scale" => {
                        parsed.guidance_scale = value.parse().context("Scale must be a number")?
                    }
                    "aesthetic" | "a" => {
                        parsed.aesthetic_scale =
                            value.parse().context("Aesthetic scale must be a number")?
                    }
                    "steps" => {
                        parsed.steps = Some(value.parse().context("Steps must be a number")?)
                    }
                    "count" | "c" => {
                        parsed.count = value.parse().context("Count must be a number")?
                    }
                    "seed" => parsed.seed = value.parse().context("Seed must be a number")?,
                    "w" => {
                        let w = value.parse().context("Width must be an integer")?;
                        if let Some((_, h)) = override_wh {
                            override_wh = Some((w, h));
                        } else {
                            override_wh = Some((w, parsed.height));
                        }
                    }
                    "h" => {
                        let h = value.parse().context("Height must be an integer")?;
                        if let Some((w, _)) = override_wh {
                            override_wh = Some((w, h));
                        } else {
                            override_wh = Some((parsed.width, h));
                        }
                    }
                    "ar" => {
                        ar = value;
                    }
                    x => bail!("Unknown option: {}", x),
                }
                last_option = None;
                last_value = None;
            }
        }

        // Do some final validation.
        if !config.models.contains_key(&parsed.model_name) {
            // That model doesn't exist, so... do a distance check.
            let mut best_similarity = 0.0;
            let mut best_model = None;
            for model in config.aliases.keys().chain(config.models.keys()) {
                let similarity = strsim::jaro_winkler(&parsed.model_name, model);
                if similarity > best_similarity {
                    best_similarity = similarity;
                    best_model = Some(model);
                }
            }
            if let Some(best_model) = best_model {
                if best_similarity < SIMILARITY_THRESHOLD {
                    bail!(
                        "Unknown model: {}. Did you mean {}?",
                        parsed.model_name,
                        best_model
                    );
                } else {
                    parsed.model_name.clone_from(best_model);
                }
            }
        }
        while let Some(alias) = config.aliases.get(&parsed.model_name) {
            parsed.model_name.clone_from(alias);
        }
        let model_config = config
            .models
            .get(&parsed.model_name)
            .context("no such model")?;

        if let Some((w, h)) = override_wh {
            (parsed.width, parsed.height) = (w, h);
        } else {
            let base_resolution = model_config.base_resolution.unwrap_or(1024);
            (parsed.width, parsed.height) =
                ParsedRequest::parse_aspect_ratio(64, base_resolution, ar)
                    .ok()
                    .or_else(|| ParsedRequest::parse_aspect_ratio(64, 1024, "1:1").ok())
                    .unwrap();
        }

        // Final sanity checks.
        if linguistic_prompt.is_empty() {
            bail!("Linguistic prompt is required");
        }
        if !(1.0..=80.0).contains(&parsed.guidance_scale) {
            bail!("Scale must be between 1 and 80");
        }
        if !(1.0..=30.0).contains(&parsed.aesthetic_scale) {
            bail!("Aesthetic scale must be between 1 and 30");
        }
        if let Some(s) = parsed.steps {
            if s < 1 {
                bail!("We're done! Wasn't that fast?")
            }
        }
        if parsed.count > 9 {
            parsed.count = 9;
        }
        if parsed.max_batch_size() < 1 || (parsed.width * parsed.height > 2048 * 2048) {
            bail!("Resolution is too high");
        }

        // Generate the final command.
        let parsed = ParsedRequest {
            linguistic_prompt: linguistic_prompt.join(" "),
            supporting_prompt: supporting_prompt.join(" "),
            negative_prompt: negative_prompt.join(" "),
            ..parsed
        };
        info!("Parsed configuration: {:?}", parsed);
        Ok(parsed)
    }

    /// In general we have a limit of 4 Megapixels per request.
    /// There's no benefit to Flux from batching images, so don't.
    fn max_batch_size(&self) -> u32 {
        if self.model_name.contains("flux") {
            1
        } else {
            let max_pixels = (4 * 1024 * 1024) as f32;
            let pixels_per_image = (self.width * self.height) as f32;
            (max_pixels / pixels_per_image) as u32
        }
    }

    /// Builds a query string for the backend.
    /// Since the backend is ComfyUI, this is a bit of a pain. We need to:
    /// - Load the model config from models.json. This can fail, if the model doesn't exist anymore.
    /// - Load the workflow "JSON" from the model config. This actually has __STANDIN__ placeholders for the parameters.
    /// - Replace the placeholders with the actual parameters.
    /// - Confirm that the result is valid JSON.
    /// - Take the text, and pass it to /prompt as POST data.
    fn build_query(
        &self,
        config: &BotConfig,
        batch_size: u32,
        seed_offset: u32,
    ) -> Result<RequestBuilder> {
        // First, check for aliases.
        let model_name = config
            .aliases
            .get(&self.model_name)
            .cloned()
            .unwrap_or(self.model_name.clone());
        // Then, load the model config.
        let model_config = config
            .models
            .get(&model_name)
            .ok_or_else(|| anyhow::anyhow!("no such model: {}", model_name))?;
        // Load the workflow.
        let workflow =
            std::fs::read_to_string(&model_config.workflow).context("failed to read workflow")?;
        // Replace the placeholders.
        let linguistic_prompt = if self.use_pos_default && !model_config.default_positive.is_empty()
        {
            model_config.default_positive.clone() + ", " + &self.linguistic_prompt
        } else {
            self.linguistic_prompt.clone()
        };
        let negative_prompt = if self.use_neg_default {
            self.negative_prompt.clone() + ", " + &model_config.default_negative
        } else {
            self.negative_prompt.clone()
        };
        let steps = self
            .steps
            .unwrap_or(model_config.default_steps.unwrap_or(30));
        let steps_cutover = (steps as f32 * 0.5) as u32;
        info!("Generating {} images in {} steps", self.count, steps);
        info!("Linguistic prompt: {}", linguistic_prompt);
        info!("Supporting prompt: {}", self.supporting_prompt);
        info!("Negative prompt: {}", negative_prompt);

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

        let combined_prompt = if self.supporting_prompt.is_empty() {
            linguistic_prompt.clone()
        } else {
            let supporting_prompt = &self.supporting_prompt;
            let style_connector = model_config.style_connector.as_deref().unwrap_or(": ");
            format!("{linguistic_prompt}. {style_connector}{supporting_prompt}")
        };

        let workflow = workflow
            .replace(
                "__REFINER_CHECKPOINT__",
                &json_encode_string(
                    model_config
                        .refiner
                        .as_ref()
                        .unwrap_or(&model_config.baseline),
                ),
            )
            .replace(
                "__BASE_CHECKPOINT__",
                &json_encode_string(&model_config.baseline),
            )
            .replace(
                "__VAE__",
                &json_encode_string(model_config.vae.as_ref().unwrap_or(&"".to_string())),
            )
            .replace("__NEGATIVE_PROMPT__", &json_encode_string(&negative_prompt))
            .replace("__PROMPT_A__", &json_encode_string(&linguistic_prompt))
            .replace("__PROMPT_B__", &json_encode_string(&self.supporting_prompt))
            .replace("__COMBINED_PROMPT__", &json_encode_string(&combined_prompt))
            .replace("__STEPS_TOTAL__", &steps.to_string())
            .replace("__STEPS_HALF__", &(steps / 2).to_string())
            .replace("__FIRST_PASS_END_AT_STEP__", &steps_cutover.to_string())
            .replace("__WIDTH__", &self.width.to_string())
            .replace("__HEIGHT__", &self.height.to_string())
            .replace("__WIDTH_d2__", &(self.width / 2).to_string())
            .replace("__HEIGHT_d2__", &(self.height / 2).to_string())
            .replace("__2xWIDTH__", &(self.width * 2).to_string())
            .replace("__2xHEIGHT__", &(self.height * 2).to_string())
            .replace("__4xWIDTH__", &(self.width * 4).to_string())
            .replace("__4xHEIGHT__", &(self.height * 4).to_string())
            .replace("__SEED__", &(self.seed + seed_offset).to_string())
            .replace("__BASE_CFG__", &self.guidance_scale.to_string())
            .replace("__REFINER_CFG__", &self.guidance_scale.to_string())
            .replace("__BATCH_SIZE__", &batch_size.to_string())
            .replace("__POSITIVE_A_SCORE__", &self.aesthetic_scale.to_string())
            .replace("__NEGATIVE_A_SCORE__", "1.0");
        // Confirm that the result is valid JSON.
        let workflow: serde_json::Value =
            serde_json::from_str(&workflow).context("failed to parse augmented workflow")?;
        #[derive(Debug, Serialize)]
        struct Request {
            prompt: serde_json::Value,
            client_id: String,
        }
        let request = Request {
            prompt: workflow,
            client_id: config.backend.client_id.clone(),
        };
        // Take the text, and pass it to /prompt as POST data.
        let request = serde_json::to_string(&request).context("failed to serialize request")?;
        let request = reqwest::Client::new()
            .post(format!(
                "http://{}:{}/prompt",
                config.backend.host, config.backend.port
            ))
            .body(request);
        Ok(request)
    }

    fn score(&self, previous_request: &ParsedRequest) -> f32 {
        let mut score = 0.0;
        // On an average picture, this subtracts 2.
        score -= (self.count as f32) / (Self::default().count as f32);
        // Prefer to alternate between users.
        if self.base.user != previous_request.base.user {
            score += 2.0;
        }
        // Prefer *not* to alternate models.
        if self.model_name != previous_request.model_name {
            score -= 3.0;
        }
        score
    }
}

struct ImageGenerator {
    db: DatabaseModule,
    config: BotConfigModule,
    command_sender: UnboundedSender<(ParsedRequest, UnboundedSender<GenerationEvent>)>,
    load: Arc<AtomicUsize>,
    prompt_generator: PromptGeneratorModule,
}

type EventStream = Pin<Box<dyn Send + FusedStream<Item = GenerationEvent>>>;

#[derive(Clone)]
pub struct ImageGeneratorModule(Arc<RwLock<ImageGenerator>>);

impl ImageGeneratorModule {
    pub fn new(
        db: DatabaseModule,
        config: BotConfigModule,
        prompt_generator: PromptGeneratorModule,
    ) -> Result<Self> {
        let (tx, rx) = unbounded();
        let generator = ImageGeneratorModule(Arc::new(RwLock::new(ImageGenerator {
            db,
            config,
            command_sender: tx,
            load: Arc::new(AtomicUsize::new(0)),
            prompt_generator,
        })));

        tokio::task::spawn(generator.clone().run(rx));
        Ok(generator)
    }

    /// Waits until nothing is being generated, then returns.
    pub async fn wait_until_idle(&self) {
        loop {
            let load = self.0.read().await.load.load(Ordering::Relaxed);
            if load == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Generates a single batch of images.
    async fn generate_batch(
        backend: &BotBackend,
        request: RequestBuilder,
    ) -> Result<Vec<JpegBlob>> {
        #[derive(Deserialize)]
        struct ComfyUIResponse {
            prompt_id: String,
            #[allow(dead_code)]
            number: u32,
        }

        #[derive(Deserialize)]
        struct ComfyUIError {
            error: ComfyUIErrorDetails,
        }

        #[derive(Deserialize)]
        struct ComfyUIErrorDetails {
            message: String,
            details: String,
        }

        let response = request.send().await.context("failed to send request")?;
        let text = response.text().await.context("failed to read response")?;
        trace!("Response: {}", text);
        let parsed = match serde_json::from_str::<ComfyUIResponse>(&text) {
            Result::Ok(parsed) => parsed,
            Err(e) => {
                if let Result::Ok(parsed) = serde_json::from_str::<ComfyUIError>(&text) {
                    let msg = if parsed.error.details.is_empty() {
                        parsed.error.message
                    } else {
                        format!("{}: {}", parsed.error.message, parsed.error.details)
                    };
                    warn!("Backend error: {}, {}", msg, text);
                    bail!("backend error: {}", msg);
                } else {
                    warn!("Failed to parse response: {}", text);
                    bail!("failed to parse backend response: {}", e);
                }
            }
        };

        let prompt_id = parsed.prompt_id;
        debug!("Got prompt ID {}", prompt_id);
        // Now, we need to poll the history endpoint until it's done.
        // We limit the traffic by reading the websocket, only polling when it update or if
        // it's been a second.
        let mut filenames: Option<Vec<String>> = None;
        let mut ws_client = ws::connect_async(format!(
            "ws://{}:{}/ws?clientId={}",
            backend.host, backend.port, prompt_id
        ))
        .await
        .context("failed to connect to websocket")?
        .0;
        for iteration in 0..10 {
            // Loop across websocket messages until we get one indicating completion.
            loop {
                select! {
                    msg = ws_client.next() => {
                        if let Some(Result::Ok(tungstenite::protocol::Message::Text(msg))) = msg {
                            // Parse as JSON.
                            let msg: serde_json::Value = serde_json::from_str(&msg).context("failed to parse websocket message")?;
                            if msg.get("type").and_then(|t| t.as_str()) == Some("status") {
                                break;
                            }
                        } else {
                            warn!("Got unexpected websocket message: {:?}", msg);
                            break;
                        }
                    },
                    _ = futures_time::task::sleep(futures_time::time::Duration::from_secs(90)).fuse() => {
                        warn!("Websocket sleep timed out");
                        // Really this should never happen, but try to recover anyway.
                        break;
                    },
                }
            }
            trace!("Polling history");
            let client = reqwest::Client::new();
            let history: serde_json::Value = client
                .get(format!(
                    "http://{}:{}/history/{}",
                    backend.host, backend.port, prompt_id
                ))
                .send()
                .await
                .context("failed to poll history")?
                .json()
                .await
                .context("failed to parse history")?;
            // If the history is empty, we're not done yet.
            if history.as_object().map(|o| o.is_empty()).unwrap_or(false) {
                continue;
            }
            // Otherwise, we're done.
            trace!("History: {:?}", history);
            // Extract the filenames from the history; these are the images we want.
            // They're at history[prompt_id].outputs.<number>.images[<index>].filename.
            let outputs = history
                .get(prompt_id)
                .and_then(|o| o.get("outputs"))
                .context("history missing outputs")?;
            let outputs = outputs.as_object().context("outputs not an object")?;
            // This can contain multiple outputs, but we only care about whichever one
            // has a non-empty images array.
            // We'll just take the first one that has images.
            let maybe_filenames = outputs.iter().find_map(|(_, v)| {
                let images = v.get("images")?;
                let images = images.as_array()?;
                if images.is_empty() {
                    None
                } else {
                    let filenames = images
                        .iter()
                        .map(|i| {
                            let filename = i.get("filename")?;
                            let filename = filename.as_str()?;
                            Some(filename.to_owned())
                        })
                        .collect::<Option<Vec<_>>>();
                    Some(filenames)
                }
            });
            if let Some(fns) = maybe_filenames {
                filenames = fns;
                debug!("Got filenames: {:?}", filenames);
            } else {
                bail!("no images in history");
            }
            break;
        }
        // If we didn't get any filenames, we timed out.
        let filenames = filenames.ok_or_else(|| anyhow::anyhow!("timed out waiting for images"))?;
        // Now, we need to download the images.
        let client = reqwest::Client::new();
        let mut final_images = Vec::new();
        for filename in filenames {
            let image = client
                .get(format!("http://{}:{}/view", backend.host, backend.port))
                .query(&[("filename", filename)])
                .send()
                .await
                .context("failed to download image")?
                .bytes()
                .await
                .context("failed to read image")?;
            final_images
                .push(utils::convert_to_jpeg(image.into()).context("failed to convert image")?);
        }

        Ok(final_images)
    }

    /// Runs the generator loop for a single request.
    /// Locks self for an instant at startup.
    async fn do_generate(
        &self,
        request: ParsedRequest,
    ) -> impl FusedStream<Item = GenerationEvent> {
        let config = { self.0.read().await.config.snapshot().await };
        try_stream! {
            let backend = &config.backend;
            let mut remaining = request.count;
            let mut seed_offset = 0;
            let mut final_images = Vec::new();
            while remaining > 0 {
                // Calculate % remaining.
                let percent = 100.0 * (1.0 - (remaining as f64 / request.count as f64));
                yield GenerationEvent::Generating(percent as u32);

                let batch_size = if request.model_name == "pixart" {
                    // HACK WARNING
                    1
                } else {
                    std::cmp::min(remaining, request.max_batch_size())
                };

                let retry_strategy = ExponentialBackoff::from_millis(50).max_delay(std::time::Duration::from_secs(2)).take(5);
                let images = Retry::spawn(retry_strategy, || async {
                    debug!("Generating batch of {} images", batch_size);
                    let request = request.build_query(&config, batch_size, seed_offset).context("Failed to build query")?;
                    Self::generate_batch(backend, request).await.context("Failed to generate batch")
                }).await.context("Ran out of retries")?;

                final_images.extend(images);

                remaining -= batch_size;
                seed_offset += batch_size;
            }
            let completed_request = CompletedRequest {
                base: request,
                images: final_images,
                uuid: uuid::Uuid::new_v4(),
            };
            yield GenerationEvent::Completed(completed_request);
        }.map(|r| r.unwrap_or_else(GenerationEvent::Error))
    }

    /// Returns the highest-scoring request in the queue, if any.
    fn highest_scoring<T>(
        queue: &mut Vec<(ParsedRequest, T)>,
        previous_request: &ParsedRequest,
    ) -> Option<(ParsedRequest, T)> {
        if queue.is_empty() {
            return None;
        }
        let mut highest_score = f32::NEG_INFINITY;
        let mut highest_index = 0;
        for (i, (request, _)) in queue.iter().enumerate() {
            // TODO: This is a very simple scoring function.
            // It should be improved.
            // Currently: Prefers older requests, prefers requests with the same model, prefers requests with different users.
            let score = request.score(previous_request) - i as f32;
            if score >= highest_score {
                highest_score = score;
                highest_index = i;
            }
        }
        let (request, tx) = queue.remove(highest_index);
        Some((request, tx))
    }

    /// Core of the generator.
    /// This background task picks pictures off the queue, prioritizes them based on a cost metric, and generates them.
    /// It sends updates back to the requester.
    pub async fn run(
        self,
        mut command_receiver: UnboundedReceiver<(ParsedRequest, UnboundedSender<GenerationEvent>)>,
    ) {
        // Queue of pictures-to-be-generated.
        let mut queue: Vec<(ParsedRequest, UnboundedSender<GenerationEvent>)> = vec![];
        // Stream for the currently generating picture, if any.
        let mut current_gen: EventStream = Box::pin(empty());
        let mut current_tx: Option<UnboundedSender<GenerationEvent>> = None;
        // Previously generated picture... if any.
        let mut previous_request: Option<ParsedRequest> = None;
        loop {
            // Update the load.
            let current_load = queue.len() + if current_tx.is_some() { 1 } else { 0 };
            self.0
                .write()
                .await
                .load
                .store(current_load, Ordering::Relaxed);

            // Wait for a new command or the current generation to finish.
            if current_tx.is_none() {
                // We're not generating anything right now, so we can pick something off the queue.
                if let Some((request, tx)) =
                    Self::highest_scoring(&mut queue, &previous_request.unwrap_or_default())
                {
                    // We found something to generate.
                    current_gen = Box::pin(self.do_generate(request.clone()).await);
                    current_tx = Some(tx);
                    previous_request = Some(request);
                } else {
                    // Nothing to generate.
                    previous_request = None;
                }
            }
            select! {
                // New event on the generating stream.
                event = current_gen.next() => {
                    match event {
                        Some(event) => {
                            // Send the event to the requester.
                            if let Some(mut tx) = current_tx.as_ref() {
                                tx.send(event).await.expect("failed to send event");
                            }
                        },
                        None => {
                            // Generation is done.
                            current_tx = None;
                            current_gen = Box::pin(empty());
                        },
                    }
                },
                // New picture to generate.
                command = command_receiver.next() => {
                    if let Some((request, mut tx)) = command {
                        let qsz = queue.len() + if current_tx.is_some() { 1 } else { 0 };
                        tx.send(GenerationEvent::Queued(qsz as u32)).await.expect("failed to send queued event");
                        queue.push((request, tx));
                    } else {
                        panic!("command channel closed");
                    }
                }
            }
        }
    }

    pub async fn generate(
        &self,
        mut request: UserRequest,
        is_private: bool,
    ) -> impl Stream<Item = GenerationEvent> + '_ {
        let db_for_completion = self.0.read().await.db.clone();
        let (tx, rx) = unbounded();
        try_stream! {
            if let Some(ref dream) = request.dream {
                // This is a dream request. We need to generate a prompt for it.
                debug!("Generating prompt for {:?}", request);
                let raw = self.0.read().await.prompt_generator.generate(&request.user, dream).await?;
                request.raw = raw.to_string();
                request.comment = Some(raw.comment);
                yield GenerationEvent::GPTCompleted(request.clone());
            } else {
                // This is an explicit request. Generate some fun commentary, but only in channels.
                // TODO: Use 3.5mini for this.
                //if !request.private {
                //    let comment = self.0.read().await.prompt_generator.comment(&request.user, &request.raw).await?;
                //    request.comment = Some(comment);
                //    yield GenerationEvent::GPTCompleted(request.clone());
                //}
            }

            // TODO: Snapshot the config here, keep it for the scope of the request.
            let parsed = ParsedRequest::from_request(&self.0.read().await.config.snapshot().await, request)?;
            // Check if the user is making too many private requests.
            self.0.read().await.db.check_privacy_limit(&parsed, is_private)
                .await
                .context("While checking privacy limit")?;
            yield GenerationEvent::Parsed(parsed.clone());

            self.0.write().await.command_sender.send((parsed.clone(), tx)).await.expect("failed to send command");
        }.map(|r| r.unwrap_or_else(GenerationEvent::Error))
         .chain(rx)
         .then(move |ev| {
            let db = db_for_completion.clone();
            async move {
                if let GenerationEvent::Completed(ref ev) = ev {
                    db.update_user_stats(&ev.base, is_private).await.expect("failed to update user stats");
                }
                ev
            }
        })
    }
}

lazy_static! {
    pub static ref STYLES: Vec<(&'static str, &'static str)> = vec![
        ("Shōnen Anime", "Shōnen Anime, action-oriented, Akira Toriyama (Dragon Ball), youthful, vibrant, dynamic"),
        ("Shōjo Anime", "Shōjo Anime, Romantic, Naoko Takeuchi (Sailor Moon), emotional, detailed backgrounds, soft colors"),
        ("Seinen Anime", "Seinen Anime, Mature, Hajime Isayama (Attack on Titan), complex themes, realistic, detailed"),
        ("Abstract Expressionism", "Abstract Expressionism, Abstract, Jackson Pollock, spontaneous, dynamic, emotional"),
        ("Art Nouveau", "Art Nouveau, Decorative, Alphonse Mucha, organic forms, intricate, flowing"),
        ("Baroque", "Baroque, Dramatic, Caravaggio, high contrast, ornate, realism"),
        ("Classical", "Classical, Proportionate, Leonardo da Vinci, balanced, harmonious, detailed"),
        ("Contemporary", "Contemporary, Innovative, Ai Weiwei, conceptual, diverse mediums, social commentary"),
        ("Cubism", "Cubism, Geometric, Pablo Picasso, multi-perspective, abstract, fragmented"),
        ("Fantasy", "Fantasy, Imaginative, J.R.R. Tolkien, mythical creatures, dreamlike, detailed"),
        ("Film Noir", "Film Noir, Monochromatic, Orson Welles, high contrast, dramatic shadows, mystery"),
        ("Impressionism", "Impressionism, Painterly, Claude Monet, light effects, outdoor scenes, everyday life"),
        ("Minimalist", "Minimalist, Simplified, Agnes Martin, bare essentials, geometric, neutral colors"),
        ("Modern", "Modern, Avant-garde, Piet Mondrian, non-representational, experimental, abstract"),
        ("Neo-Gothic", "Neo-Gothic, Dark, H.R. Giger, intricate detail, macabre, architectural elements"),
        ("Pixel Art", "Pixel Art, Retro, Shigeru Miyamoto, 8-bit, digital, geometric"),
        ("Pop Art", "Pop Art, Colorful, Andy Warhol, mass culture, ironic, bold"),
        ("Post-Impressionism", "Post-Impressionism, Expressive, Vincent Van Gogh, symbolic, bold colors, heavy brushstrokes"),
        ("Renaissance", "Renaissance, Realistic, Michelangelo, perspective, humanism, religious themes"),
        ("Retro / Vintage", "Retro / Vintage, Nostalgic, Norman Rockwell, past styles, soft colors, romantic"),
        ("Romanticism", "Romanticism, Emotional, Caspar David Friedrich, nature, dramatic, imaginative"),
        ("Surrealism", "Surrealism, Dreamlike, Salvador Dalí, irrational, bizarre, subconscious"),
        ("Steampunk", "Steampunk, Futuristic, H.G. Wells, industrial, Victorian, mechanical"),
        ("Street Art", "Street Art, Public, Keith Haring, social commentary, bold colors, mural"),
        ("Watercolor", "Watercolor, Translucent, J.M.W. Turner, lightness, fluid, landscape"),
    ];
}

pub fn choose_random_style() -> &'static str {
    let mut rng = rand::thread_rng();
    let (_, style) = STYLES.choose(&mut rng).unwrap();
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    fn most_similar(model: &str) -> (f64, String) {
        let models = [
            "MeinaMix_v11",
            "MeinaHentai_v4",
            "cetusMix_whalefall_v2",
            "SDXL_0.9",
        ];
        let mut best_similarity = 0.0;
        let mut best_model = None;
        for other in &models {
            let similarity = strsim::jaro_winkler(model, other);
            if similarity > best_similarity {
                best_similarity = similarity;
                best_model = Some(other);
            }
        }
        (best_similarity, best_model.unwrap().to_string())
    }

    fn correct(model: &str, target: &str) {
        let (similarity, best_model) = most_similar(model);
        assert!(
            similarity >= SIMILARITY_THRESHOLD,
            "model {} is too different from {}: {}",
            model,
            best_model,
            similarity
        );
        assert_eq!(best_model, target);
    }

    /// This test exists to determine the appropriate threshold, chiefly.
    #[test]
    fn test_model_similarity_threshold() {
        correct("MeinaMix_v11", "MeinaMix_v11");
        correct("meinamix", "MeinaMix_v11");
        correct("cetusmix", "cetusMix_whalefall_v2");
        correct("SDXL", "SDXL_0.9");
    }
}
