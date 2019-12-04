use std::env;

use async_std::net::TcpListener;
use async_std::prelude::*;
use async_std::task;

use server::tunnel::*;
use common::logger;
use log::{info};
use server::config::Config;

fn main() {
    let args: Vec<_> = env::args().collect();
    let program = args[0].clone();

    let mut opts = getopts::Options::new();
    opts.reqopt("l", "listen", "listen address", "listen-address");
    opts.optopt("", "log", "log path", "log-path");
    opts.optopt("k", "key", "key", "key");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => m,
        Err(_) => {
            println!("{}", opts.short_usage(&program));
            return;
        }
    };

    let listen_addr = matches.opt_str("l").unwrap();
    let log_path = matches.opt_str("log").unwrap_or(String::from("/var/log/bit-tunnel/server.log"));
    let key = matches.opt_str("k").unwrap_or("123456".to_string());

    logger::init(log::Level::Info, log_path, 1, 2000000).unwrap();
    info!("starting up");

    let config = Config::new(key.into_bytes(), listen_addr);

    task::block_on(async move {
        let listener = TcpListener::bind(config.get_listen_address()).await.unwrap();
        let mut incoming = listener.incoming();

        while let Some(stream) = incoming.next().await {
            match stream {
                Ok(stream) => {
                    TcpTunnel::new(&config, stream);
                }

                Err(_) => {}
            }
        }
    });
}
