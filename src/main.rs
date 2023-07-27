use anyhow::{bail, Context, Result};

use clap::Parser;
use config::BotConfigModule;
use futures::{prelude::*, stream::FuturesUnordered};
use generator::ImageGeneratorModule;
use log::{info, error};

use crate::{
    db::DatabaseModule,
    gpt::GPTPromptGeneratorModule,
};

mod changelog;
mod config;
mod db;
mod discord;
mod generator;
mod gpt;
mod help;
mod irc;
mod utils;
#[derive(Clone)]

pub struct BotContext {
    pub config: BotConfigModule,
    pub db: DatabaseModule,
    pub prompt_generator: GPTPromptGeneratorModule,
    pub image_generator: ImageGeneratorModule,
}

#[derive(Parser, Debug)]
struct CommandLineFlags {
    #[arg(long, short)]
    pub config_path: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = CommandLineFlags::parse();

    // Immediately crash on panic.
    std::panic::set_hook(Box::new(|panic_info| {
        error!("Panic: {:?}", panic_info);
        std::process::exit(1);
    }));

    // Initialize context.
    let config = BotConfigModule::new(args.config_path).context("failed to initialize config")?;
    config
        .with_config(|c| info!("Loaded config: {:?}", c))
        .await;

    // Start backends.
    let db = DatabaseModule::new(config.clone()).await?;
    let prompt_generator = GPTPromptGeneratorModule::new(config.clone());
    let image_generator = ImageGeneratorModule::new(config.clone(), prompt_generator.clone())?;

    let context = BotContext {
        config: config.clone(),
        db: db.clone(),
        prompt_generator: prompt_generator.clone(),
        image_generator: image_generator.clone(),
    };

    // // Run smoke-test.
    // tokio::task::spawn(async move {
    //     let s = image_generator.generate(UserRequest {
    //         user: "warmup".to_owned(),
    //         raw: "warmup --steps 1".to_owned(),
    //         dream: None,
    //         source: generator::Source::Unknown,
    //     }).await;
    //     s.for_each(|e| {
    //         info!("Smoke-test result: {:?}", e);
    //         if let GenerationEvent::Error(e) = e {
    //             panic!("Smoke-test error: {}", e);
    //         }
    //         future::ready(())
    //     }).await;
    // }).await?;

    // Start IRC client(s)
    let mut irc_tasks = config
        .with_config(|c| {
            c.irc
                .iter()
                .map(|irc_config| irc::IrcTask::new(irc_config.clone(), context.clone()))
                .collect::<Vec<_>>()
        })
        .await;
    let mut irc_runners = irc_tasks
        .iter_mut()
        .map(|t| t.run())
        .collect::<FuturesUnordered<_>>();

    // Start Discord client
    let mut discord_task = discord::DiscordTask::new(context.clone())?;

    // Await all futures. (Run tasks until one completes, i.e. crashes.)
    tokio::select! {
        err = irc_runners.next() => {
            bail!("IRC client failed: {:?}", err);
        },
        err = discord_task.run() => {
            bail!("Discord client failed: {:?}", err);
        },
    }
}
