use std::io::Write as _;
use std::fmt::Write as _;
use std::time::Duration;

use chrono::{DateTime, Local};
pub use log::{Level, LevelFilter};
use log::{Log, Metadata, Record};
use serde::{Deserialize, Serialize};
use termcolor::{ColorChoice, ColorSpec, StandardStream, WriteColor};
use tinyroute::client::{connect, Client, ClientMessage, ClientReceiver, ClientSender, TcpClient, UdsClient};
use tinyroute::frame::Frame;
use tinyroute::{Agent, Message, ToAddress, sleep};

pub use termcolor::Color;

pub mod config;
mod errors;

pub use crate::errors::{Error, Result};

// -----------------------------------------------------------------------------
//     - Internal printing macros -
// -----------------------------------------------------------------------------
#[macro_export]
macro_rules! print_log {
    ($lvl:expr, $module_path:expr, $($arg:tt)*) => ({
        let lvl = $lvl;
        let (color, prompt) = match lvl {
            log::Level::Error => ($crate::Color::Red, "ERROR"),
            log::Level::Warn => ($crate::Color::Yellow, "WARN"),
            log::Level::Info => ($crate::Color::Green, "INFO"),
            log::Level::Debug => ($crate::Color::Green, "DEBUG"),
            log::Level::Trace => ($crate::Color::Green, "TRACE"),
        };

        $crate::with_prompt(
            color,
            prompt,
            $module_path,
            format_args!($($arg)*)
        );
    });
}

#[macro_export]
macro_rules! print_error {
    ($module_path:expr, $($arg:tt)+) => {
        $crate::print_log!(log::Level::Error, $module_path, $($arg)+)
    }
}

#[macro_export]
macro_rules! print_warn {
    ($module_path:expr, $($arg:tt)+) => {
        $crate::print_log!(log::Level::Warn, $module_path, $($arg)+)
    }
}

#[macro_export]
macro_rules! print_info {
    ($module_path:expr, $($arg:tt)+) => {
        $crate::print_log!(log::Level::Info, $module_path, $($arg)+)
    }
}

pub fn with_prompt(color: Color, prompt: &str, module_path: &str, s: std::fmt::Arguments) {
    let mut stdout = StandardStream::stdout(ColorChoice::Always);
    stdout.set_color(ColorSpec::new().set_fg(Some(color))).unwrap();

    write!(&mut stdout, "{} ", prompt).expect("failed to write to stdout");
    stdout.reset().expect("failed to write to stdout");
    writeln!(&mut stdout, "{} | {}", module_path, s).expect("failed to write to stdout");
}

/// Filter log messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Filter {
    pub level: Option<LevelFilter>,
    pub modules: Vec<String>,
}

impl Filter {
    pub fn empty() -> Filter {
        Filter { level: None, modules: Vec::new() }
    }

    pub fn apply(&self, entry: &LogEntry<Saved>) -> bool {
        if let Some(level) = self.level {
            if entry.level > level {
                return false;
            }
        }

        if !self.modules.is_empty() && !self.modules.iter().any(|m| entry.module.contains(m)) {
            return false;
        }

        true
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Saved(usize);

#[derive(Debug, Serialize, Deserialize)]
pub struct Unsaved;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Log(LogEntry<Unsaved>),
    Subscribe(Option<Filter>),
    Tail(isize, Option<Filter>),
    // Get(usize),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry<T> {
    pub level: Level,
    pub message: String,
    pub module: String,
    pub timestamp: DateTime<Local>,
    pub file: Option<String>,
    pub line: Option<u32>,
    state: T,
}

impl LogEntry<Saved> {
    pub fn print(&self, print_full: bool) {
        let mut message = self.message.clone();
        if !print_full {
            message.truncate(120);
        }

        let mut output = format!("{:04} | {}", self.state.0, self.timestamp.format("%H:%M:%S"));

        if let (Some(file), Some(line)) = (&self.file, self.line) {
            write!(&mut output, "| {}:{}", file, line).expect("Failed to write to a string?!?!");
        }

        write!(&mut output, "| {}", message).expect("Failed to write to a string?!?!");

        match self.level {
            Level::Error => print_error!(&self.module, "{}", output),
            Level::Warn => print_warn!(&self.module, "{}", output),
            Level::Info => print_info!(&self.module, "{}", output),
            Level::Debug => print_info!(&self.module, "{}", output),
            Level::Trace => print_info!(&self.module, "{}", output),
        }
    }
}

impl LogEntry<Unsaved> {
    pub fn persist(self, id: usize) -> LogEntry<Saved> {
        LogEntry {
            level: self.level,
            message: self.message,
            timestamp: self.timestamp,
            module: self.module,
            line: self.line,
            file: self.file,
            state: Saved(id),
        }
    }

    pub fn new(
        level: Level,
        module: impl Into<String>,
        message: impl Into<String>,
        file: Option<String>,
        line: Option<u32>,
    ) -> Self {
        Self {
            timestamp: Local::now(),
            level,
            state: Unsaved,
            module: module.into(),
            message: message.into(),
            file,
            line,
        }
    }
}

pub struct Logger<A: ToAddress, C: Client> {
    agent: Agent<LogEntry<Unsaved>, A>,
    client: C,
}

impl<A: ToAddress, C: Client> Logger<A, C> {
    pub async fn new(agent: Agent<LogEntry<Unsaved>, A>, client: C) -> Result<Self> {
        let inst = Self { agent, client };

        Ok(inst)
    }

    pub async fn run(self) {
        let Self { mut agent, client } = self;
        let (tx, _rx) = connect(client, None);

        while let Ok(Message::Value(log_entry, _)) = agent.recv().await {
            let entry = Request::Log(log_entry);
            let bytes = match serde_json::to_vec(&entry) {
                Err(e) => {
                    print_error!(module_path!(), "Failed to serialize log entry: {}", e);
                    continue;
                }
                Ok(b) => b,
            };

            let prefix = b"log|";
            let mut payload = Vec::with_capacity(prefix.len() + bytes.len());
            payload.extend_from_slice(prefix);
            payload.extend(bytes);
            let framed_mesage = Frame::frame_message(&payload);
            if let Err(e) = tx.send(ClientMessage::Payload(framed_mesage)) {
                print_error!(module_path!(), "Failed to send client message: {}", e);
            }
        }
    }
}

// -----------------------------------------------------------------------------
//     - Logger -
// -----------------------------------------------------------------------------
pub struct LogClient {
    rx: ClientReceiver,
    tx: ClientSender,
    no_stdout: bool,
}

impl LogClient {
    pub async fn connect_uds(socket_path: &str, no_stdout: bool) -> Self {
        let uds_client = {
            loop {
                match UdsClient::connect(socket_path).await {
                    Ok(cli) => break cli,
                    Err(e) => {
                        if !no_stdout {
                            print_error!(module_path!(), "Failed to connect: {}", e);
                        }
                        sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        };

        let (tx, rx) = connect(uds_client, None);
        LogClient { tx, rx, no_stdout }
    }

    pub async fn connect_tcp(addr: &str, no_stdout: bool) -> Self {
        let tcp_client = {
            loop {
                match TcpClient::connect(addr).await {
                    Ok(cli) => break cli,
                    Err(e) => {
                        if !no_stdout {
                            print_error!(module_path!(), "Failed to connect: {}", e);
                        }
                        sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        };

        let (tx, rx) = connect(tcp_client, None);
        LogClient { tx, rx, no_stdout }
    }

    pub fn send(&mut self, request: Request) -> Result<()> {
        let bytes = serde_json::to_vec(&request)?;
        let msg = ClientMessage::channel_payload(b"log", &bytes);
        if let Err(e) = self.tx.send(msg) {
            if !self.no_stdout {
                print_error!(module_path!(), "Failed to send request: {}", e);
            }
        }

        Ok(())
    }

    pub async fn recv(&mut self) -> Result<LogEntry<Saved>> {
        let bytes = self.rx.recv_async().await.map_err(tinyroute::errors::Error::RecvErr)?;
        let entry = serde_json::from_slice::<LogEntry<Saved>>(&bytes)?;
        Ok(entry)
    }

    pub fn try_recv(&mut self) -> Result<LogEntry<Saved>> {
        let bytes = self.rx.try_recv().map_err(tinyroute::errors::Error::TryRecvErr)?;
        Ok(serde_json::from_slice::<LogEntry<Saved>>(&bytes)?)
    }
}

struct TinyLogger {
    client: std::sync::Mutex<LogClient>,
}

impl Log for TinyLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let log_entry = LogEntry::new(
                record.level(),
                record.module_path().unwrap_or("").to_string(),
                format!("{}", record.args()),
                record.file().map(|s| s.into()),
                record.line(),
            );

            if let Err(e) = self.client.lock().map(|mut c| c.send(Request::Log(log_entry))) {
                if !self.client.lock().map(|c| c.no_stdout).unwrap_or(false) {
                    eprintln!("Failed to aquire client lock: {}", e);
                }
            }
        }
    }

    fn flush(&self) {}
}

// -----------------------------------------------------------------------------
//     - Logging connection -
// -----------------------------------------------------------------------------
pub async fn init_logger(no_stdout: bool) -> anyhow::Result<()> {
    let client = LogClient::connect_tcp("127.0.0.1:5566", no_stdout).await;
    // let client = LogClient::connect_uds("/tmp/tinylog.sock", no_stdout).await;
    let tiny_logger = Box::new(TinyLogger { client: std::sync::Mutex::new(client) });
    let tiny_logger = Box::leak(tiny_logger);

    let max_level = LevelFilter::Info;

    let logger = log::set_logger(tiny_logger);

    if logger.is_ok() {
        log::set_max_level(max_level);
    }

    Ok(())
}
