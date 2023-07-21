

use anyhow::{Result, bail, Context};

use config::{BotConfigModule};
use futures::{prelude::*, stream::FuturesUnordered};
use generator::ImageGeneratorModule;
use log::{info};


use crate::{generator::{UserRequest}, gpt::GPTPromptGeneratorModule, db::DatabaseModule};

mod db;
// mod discord;
mod config;
mod generator;
mod gpt;
mod irc;
mod utils;

// async fn dispatch_and_retry(command: QueuedCommand) -> Result<()> {
//     let retry_strategy = ExponentialBackoff::from_millis(50).max_delay(Duration::from_secs(2)).take(5);
//     let result = Retry::spawn(retry_strategy, || async {
//         trace!("Dispatching command: {:?}", command.command);
//         let result = dispatch(&command.command).await;
//         if let Err(ref e) = result {
//             warn!("Failed to dispatch command: {:?}", e);
//         }
//         result
//     }).await;
//     trace!("Dispatched command: {:?}", command.command);
//     let result = match result {
//         Ok(result) => result,
//         Err(e) => {
//             CommandResult::Failure(format!("Failed to dispatch command: {:?}", e))
//         },
//     };
//     if let Err(e) = command.sender.send(result) {
//         error!("Failed to send command result: {:?}", e);
//     }
//     Ok(())
// }


#[derive(Clone)]
pub struct BotContext {
    pub config: BotConfigModule,
    pub db: DatabaseModule,
    pub prompt_generator: GPTPromptGeneratorModule,
    pub image_generator: ImageGeneratorModule,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    // Initialize context.
    let config = BotConfigModule::new()
        .context("failed to initialize config")?;
    config.with_config(|c| info!("Loaded config: {:?}", c)).await;

    // Start backends.
    let db = DatabaseModule::new(&config).await?;
    let prompt_generator = GPTPromptGeneratorModule::new(config.clone());
    let image_generator = ImageGeneratorModule::new(config.clone(), prompt_generator.clone())?;
    
    let context = BotContext {
        config: config.clone(),
        db: db.clone(),
        prompt_generator: prompt_generator.clone(),
        image_generator: image_generator.clone(),
    };

    // Run smoke-test.
    tokio::task::spawn(async move {
        let s = image_generator.generate(UserRequest {
            user: "warmup".to_owned(),
            raw: "warmup --steps 1".to_owned(),
            dream: None,
            source: generator::Source::Unknown,
        }).await;
        s.for_each(|e| {
            info!("Smoke-test result: {:?}", e);
            future::ready(())
        }).await;
    }).await?;

    // Start IRC client(s)
    let mut irc_tasks = config.with_config(|c| {
        c.irc.iter().map(|irc_config| {
            irc::IrcTask::new(irc_config.clone(), context.clone())
        }).collect::<Vec<_>>()
    }).await;
    let mut irc_runners = irc_tasks
        .iter_mut().map(|t| t.run())
        .collect::<FuturesUnordered<_>>();

    // Await all futures. (Run tasks until one completes, i.e. crashes.)
    tokio::select! {
        err = irc_runners.next() => {
            bail!("IRC client failed: {:?}", err);
        },
    }
}

