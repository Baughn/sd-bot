// This module handles config.toml.
// Every value is re-read every time it's accessed. Some are 'fusible', and cannot be changed after the first read.
// The module keeps track of the previous value, so if these are changed on disk, the bot will restart.
//
// Well, panic actually. But then it'll restart.

use std::{collections::HashMap, sync::{Mutex, MutexGuard}};

use anyhow::{Context, Result};
use lazy_static::lazy_static;
use serde::{Serialize, Deserialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct BotConfig {
    pub command_prefix: String,
    pub discord: DiscordConfig,
    pub irc: Vec<IrcConfig>,
    pub aliases: HashMap<String, String>,
    pub models: HashMap<String, BotModelConfig>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscordConfig {
    // Nothing here yet.
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IrcConfig {
    pub server: String,
    pub port: u16,
    pub nick: String,
    pub channels: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BotModelConfig {
    pub workflow: String,
    pub baseline: String,
    pub refiner: String,
    pub default_positive: String,
    pub default_negative: String,
}

lazy_static! {
    static ref CONFIG: Mutex<BotConfig> = Mutex::new(read_config().expect("Error reading config.toml"));
}

fn read_config() -> Result<BotConfig> {
    let text = std::fs::read_to_string("config.toml")
        .context("Error reading config.toml")?;
    toml::from_str(&text).context("Error parsing config.toml")
}

pub fn get() -> MutexGuard<'static, BotConfig> {
    // Check if the config has changed.
    if let Ok(new_config) = read_config() {
        let mut old_config = CONFIG.lock().unwrap();
        update_config(&mut old_config, new_config);
    }
    CONFIG.lock().unwrap()
}

fn update_config(old: &mut BotConfig, new: BotConfig) {
    // Check what has changed.
    // We panic if:
    // - The command prefix changes.
    // - The Discord or IRC config changes.
    // Otherwise, we just update the config.
    if old.command_prefix != new.command_prefix {
        panic!("Command prefix changed from {} to {}", old.command_prefix, new.command_prefix);
    }
    if old.discord != new.discord {
        panic!("Discord config changed");
    }
    if old.irc != new.irc {
        panic!("IRC config changed");
    }
    *old = new;
}