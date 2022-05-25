use std::thread;

use tinylog::config::Config;
use tinylog::{print_error, Filter, LevelFilter, Request};

fn start_client(mut client: tinylog::LogClient, filter: Option<Filter>) -> anyhow::Result<()> {
    client.send(Request::Subscribe(filter.clone()))?;
    client.send(Request::Tail(100, filter.clone()))?;

    let show_path = filter.map(|f| f.show_path).unwrap_or(true);
    while let Ok(entry) = client.recv() {
        entry.print(true, show_path);
    }

    Ok(())
}

fn run(config: Config, filter: Option<Filter>) -> anyhow::Result<()> {
    let mut handles = vec![];

    // Tcp client
    {
        let filter = filter.clone();
        if config.enable_tcp {
            match config.tcp_addr {
                Some(addr) => handles.push(thread::spawn(move|| {
                    let cli = tinylog::LogClient::connect_tcp(&addr, false);
                    if let Err(e) = start_client(cli, filter) {
                        print_error!(module_path!(), "Failed to start log client: {}", e);
                    }
                })),
                None => print_error!(module_path!(), "Socket address missing from config"),
            }
        }
    }

    // Uds client
    // Only enable the uds client if the tcp client is disabled
    if config.enable_uds && !config.enable_tcp {
        match config.socket {
            Some(socket) => handles.push(thread::spawn(move || {
                let cli = tinylog::LogClient::connect_uds(&socket, false);
                if let Err(e) = start_client(cli, filter) {
                    print_error!(module_path!(), "Failed to start log client: {}", e);
                }
            })),
            None => print_error!(module_path!(), "Socket address missing from config"),
        }
    }

    for handle in handles {
        let _ = handle.join();
    }

    Ok(())
}

fn filter_from_args() -> Option<Filter> {
    let mut args = std::env::args().skip(1);
    let mut filter = Filter::empty();

    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "-m" => filter.modules.push(args.next()?),
            "-p" => filter.show_path = false,
            "-l" => {
                let level = args.next()?;
                let level = match level.as_ref() {
                    "info" => Some(LevelFilter::Info),
                    "warn" => Some(LevelFilter::Warn),
                    "error" => Some(LevelFilter::Error),
                    "debug" => Some(LevelFilter::Debug),
                    "trace" => Some(LevelFilter::Trace),
                    _ => None,
                }?;
                filter.level = Some(level);
            }
            _ => {}
        }
    }

    Some(filter)
}

fn main() -> anyhow::Result<()> {
    let config = Config::new()?;
    config.print();

    let filter = filter_from_args();

    loop {
        if let Err(e) = run(config.clone(), filter.clone()) {
            print_error!(module_path!(), "Failed to start log client: {}", e);
        }
    }
}
