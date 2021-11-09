use std::collections::VecDeque;
use std::fs::remove_file;

use tinyroute::server::{Listener, Server, TcpListener, UdsListener};
use tinyroute::{Agent, Message, Router, ToAddress, spawn, block_on};
use tinyroute::task::JoinHandle;

use tinylog::config::Config;
use tinylog::{print_error, LogEntry, Request, Saved, Filter, Unsaved};

// -----------------------------------------------------------------------------
//     - Logger -
//     Logs entries (what else should it do?)
// -----------------------------------------------------------------------------
struct Logger {
    entries: VecDeque<LogEntry<Saved>>,
    next_id: usize,
}

impl Logger {
    fn with_capacity(cap: usize) -> Self {
        Self { entries: VecDeque::with_capacity(cap), next_id: 0 }
    }

    fn push(&mut self, entry: LogEntry<Unsaved>) -> &LogEntry<Saved> {
        // If the log is full, remove the first entry
        if self.entries.len() == self.entries.capacity() {
            self.entries.pop_front();
        }

        let id = self.next_id;
        let entry = entry.persist(id);
        self.entries.push_back(entry);
        let entry = self.entries.back().expect("this should not fail as we have just pushed an entry into the queue");
        self.next_id += 1;
        entry
    }

    fn tail(&mut self, count: isize, filter: Option<Filter>) -> Vec<&LogEntry<Saved>> {
        let entries = self.entries.iter().filter(|e| match filter {
            Some(ref f) => f.apply(e),
            None => true,
        });
        let start = (self.entries.len() as isize - count).max(0) as usize;
        entries.skip(start).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Address {
    Log,
    TcpServer,
    UdsServer,
    Con(usize),
}

impl ToAddress for Address {
    fn from_bytes(bytes: &[u8]) -> Option<Address> {
        match bytes {
            b"log" => Some(Address::Log),
            _ => None,
        }
    }

    fn to_string(&self) -> String {
        format!("{:?}", self)
    }
}

async fn start_log(mut agent: Agent<Address, Address>) {
    let mut logger = Logger::with_capacity(1024 * 4);
    let mut subscribers: Vec<(Address, Option<Filter>)> = Vec::new();

    while let Ok(msg) = agent.recv().await {
        match msg {
            Message::RemoteMessage { sender, bytes, .. } => {
                let request = match serde_json::from_slice::<Request>(&bytes) {
                    Ok(entry) => entry,
                    Err(e) => {
                        print_error!(module_path!(), "{}: Failed to parse: {}", sender.to_string(), e);
                        continue;
                    }
                };

                match request {
                    Request::Subscribe(filter) if !subscribers.iter().any(|(s, _f)| s == &sender) => {
                        if let Err(e) = agent.track(sender).await {
                            print_error!(module_path!(), "Router down: {}", e);
                            break;
                        }
                        subscribers.push((sender, filter));
                    }
                    Request::Tail(count, filter) => {
                        let tail = logger.tail(count, filter).into_iter().map(serde_json::to_vec).filter_map(Result::ok);
                        for bytes in tail {
                            if let Err(e) = agent.send_remote(Some(sender), &bytes).await {
                                print_error!(module_path!(), "Failed to send entry: {}", e);
                                continue;
                            }
                        }
                    }
                    Request::Subscribe(_) => {}
                    Request::Log(entry) => {
                        let saved_entry = logger.push(entry);
                        let bytes = match serde_json::to_vec(&saved_entry) {
                            Ok(b) => b,
                            Err(e) => {
                                print_error!(module_path!(), "Failed to serialize entry: {}", e);
                                continue;
                            }
                        };
                        let subs = subscribers.iter().filter_map(|(s, filter)| 
                            match filter {
                                None => Some(*s),
                                Some(f) if f.apply(saved_entry) => Some(*s),
                                Some(_) => None
                            }
                        )
                        .collect::<Vec<_>>();

                        if let Err(e) = agent.send_remote(subs, &bytes).await {
                            print_error!(module_path!(), "Failed to send entry: {}", e);
                            continue;
                        }
                    }
                }
            }
            Message::AgentRemoved(address) => subscribers.retain(|(addr, _)| addr != &address),
            Message::Value(_, _) => {}
            Message::Shutdown => break,
        }
    }
}

async fn spawn_server<L: Listener + Send + 'static>(
    router: &mut Router<Address>,
    listener: L,
    address: Address,
    upper_half: bool,
) -> anyhow::Result<JoinHandle<()>> {
    let server_agent = router.new_agent(1024, address)?;

    let server = Server::new(listener, server_agent);

    let server_handle = spawn(async move {
        let mut id = match upper_half {
            true => usize::MAX / 2,
            false => 0,
        };
        let address_gen = || {
            id += 1;
            Address::Con(id)
        };
        if let Err(e) = server.run(None, address_gen).await {
            print_error!(module_path!(), "Failed to start logging server: {}", e);
        }
    });

    Ok(server_handle)
}

// -----------------------------------------------------------------------------
//     - Uds server -
// -----------------------------------------------------------------------------
async fn spawn_uds(router: &mut Router<Address>, socket_path: &str) -> anyhow::Result<JoinHandle<()>> {
    let _ = remove_file(socket_path);
    let listener = UdsListener::bind(socket_path).await?;
    spawn_server(router, listener, Address::UdsServer, false).await
}

// -----------------------------------------------------------------------------
//     - Tcp server -
// -----------------------------------------------------------------------------
async fn spawn_tcp(router: &mut Router<Address>, addr: &str) -> anyhow::Result<JoinHandle<()>> {
    let listener = TcpListener::bind(addr).await?;
    spawn_server(router, listener, Address::TcpServer, true).await
}

// -----------------------------------------------------------------------------
//     - Main -
// -----------------------------------------------------------------------------
async fn async_main() -> anyhow::Result<()> {
    // Config
    let config = Config::new()?;
    config.print();

    // Router
    let mut router = Router::new();

    // Agents
    let log_agent = router.new_agent(1024, Address::Log)?;

    let mut server_handles = vec![];

    // Tcp server
    if config.enable_tcp {
        match config.tcp_addr {
            Some(addr) => server_handles.push(spawn_tcp(&mut router, &addr).await?),
            None => print_error!(module_path!(), "Socket address missing from config"),
        }
    }

    // Uds server
    if config.enable_uds {
        match config.socket {
            Some(addr) => server_handles.push(spawn_uds(&mut router, &addr).await?),
            None => print_error!(module_path!(), "Socket path missing from config"),
        }
    }

    let handle = spawn(start_log(log_agent));
    router.run().await;
    for handle in server_handles {
        let _ = handle.await;
    }
    let _ = handle.await;

    Ok(())
}

fn main() {
    let _ = block_on(async_main());
}
