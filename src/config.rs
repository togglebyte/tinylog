use serde::Deserialize;
use figment::Figment;
use figment::providers::{Format, Toml, Env};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Config {
    pub tcp_addr: Option<String>,
    pub ws_addr: Option<String>,
    pub socket: Option<String>,
    pub enable_uds: bool,
    pub enable_tcp: bool,
    pub enable_ws: bool,
}

impl Config {
    pub fn new() -> anyhow::Result<Self> {
        let inst = Figment::new()
            .merge(Toml::file("config.toml"))
            .merge(Env::prefixed("TINYLOG_"))
            .extract()?;

        Ok(inst)
    }

    pub fn print(&self) {
        println!("-----------------------------------");
        println!(" TinyLog config");
        println!(" Version: {}", env!("CARGO_PKG_VERSION"));
        println!("-----------------------------------");
        println!("Tcp enabled         {}", self.enable_tcp);
        println!("Tcp address         {}", self.tcp_addr.clone().unwrap_or_else(|| "[not set]".into()));
        println!("Uds enabled         {}", self.enable_uds);
        println!("Uds socket          {}", self.socket.clone().unwrap_or_else(|| "[not set]".into()));
        println!("Websockets enabled  {}", self.enable_uds);
        println!("Websockets address  {}", self.ws_addr.clone().unwrap_or_else(|| "[not set]".into()));
        println!("-----------------------------------");
    }
}
