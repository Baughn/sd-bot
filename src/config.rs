// This module handles config.toml.
// Every value is re-read every time it's accessed. Some are 'fusible', and cannot be changed after the first read.
// The module keeps track of the previous value, so if these are changed on disk, the bot will restart.
//
// Well, panic actually. But then it'll restart.

use std::fmt::Debug;
use std::{collections::HashMap, path::Path, sync::Arc};

use anyhow::{Context, Result};
use futures::StreamExt;
use lazy_static::lazy_static;
use log::{error, info};
use notify::{EventHandler, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

lazy_static! {
    static ref CONFIG_PATH: &'static Path = Path::new("config.toml");
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotConfig {
    pub command_prefix: String,
    pub backend: BotBackend,
    pub database: DatabaseConfig,
    #[serde(default)]
    pub irc: Vec<IrcConfig>,
    pub aliases: HashMap<String, String>,
    pub models: HashMap<String, BotModelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotBackend {
    pub client_id: String,
    pub host: String,
    pub port: u16,
    pub webhost: String,
    pub webdir: String,
    pub webdir_internal: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DatabaseConfig {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IrcConfig {
    pub server: String,
    pub port: u16,
    pub nick: String,
    pub channels: Vec<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotModelConfig {
    pub description: String,
    pub workflow: String,
    pub baseline: String,
    pub refiner: Option<String>,
    pub vae: Option<String>,
    pub default_positive: String,
    pub default_negative: String,
}

struct ConfigEventHandler {
    tx: futures::channel::mpsc::UnboundedSender<notify::Event>,
}

impl EventHandler for ConfigEventHandler {
    fn handle_event(&mut self, event: notify::Result<notify::Event>) {
        match event {
            Ok(event) => {
                if let Err(err) = self.tx.unbounded_send(event) {
                    panic!("Error sending config event: {:?}", err)
                }
            }
            Err(err) => {
                panic!("Error in config watcher: {:?}", err);
            }
        }
    }
}

#[derive(Clone)]
pub struct BotConfigModule{
    config_path: String,
    data: Arc<RwLock<BotConfig>>
}

impl BotConfigModule {
    /// Initialize the config.
    /// This reads the config from disk, and starts the updater task.
    pub fn new(config_path: String) -> Result<BotConfigModule> {
        let config = BotConfigModule {
            data: Arc::new(RwLock::new(read_config(&config_path)?)),
            config_path,
        };
        tokio::task::spawn(config.clone().updater());
        Ok(config)
    }

    /// This watches the config file for changes, and updates the config.
    async fn updater(self) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let watcher = ConfigEventHandler { tx };
        notify::recommended_watcher(watcher)
            .expect("Error creating config watcher")
            .watch(&CONFIG_PATH, notify::RecursiveMode::NonRecursive)
            .expect("Error watching config file");
        while let Some(event) = rx.next().await {
            if let notify::EventKind::Modify(_) = event.kind {
                match read_config(&self.config_path) {
                    Ok(new_config) => {
                        update_config(&mut *self.data.write().await, new_config);
                    }
                    Err(err) => {
                        error!("Error reading config: {:?}", err);
                    }
                }
            }
        }
    }

    /// Copies the current config.
    pub async fn snapshot(&self) -> BotConfig {
        self.data.read().await.clone()
    }

    /// Run some function with the current config.
    /// Please use this instead of snapshot() if possible.
    pub async fn with_config<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&BotConfig) -> T,
    {
        f(&*self.data.read().await)
    }
}

fn read_config(config_path: &str) -> Result<BotConfig> {
    let text = std::fs::read_to_string(config_path).context("Error reading config file")?;
    toml::from_str(&text).context("Error parsing config file")
}

fn update_config(old: &mut BotConfig, new: BotConfig) {
    // Check what has changed.
    // We panic if:
    // - The Discord or IRC config changes.
    // - The database path changes.
    // Otherwise, we just update the config.
    if old.irc != new.irc {
        panic!("IRC config changed");
    }
    if old.database != new.database {
        panic!("Database config changed");
    }
    info!("Config changed:\n{}", toml::to_string_pretty(&new).unwrap());
    *old = new;
}

#[cfg(test)]
pub fn testconfig() -> BotConfig {
    toml::from_str(include_str!("../testdata/config.toml")).unwrap()
}
