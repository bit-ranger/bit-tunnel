use client::tunnel::*;
use async_std::net::TcpListener;
use async_std::net::TcpStream;
use async_std::prelude::*;
use async_std::task;
use std::env;
use std::net::Shutdown;
use std::net::ToSocketAddrs;
use std::str::from_utf8;
use std::vec::Vec;
use client::{socks5};
use common::logger;
use log::{info};
use client::config::Config;


async fn local_stream_to_entry(local_stream: &mut &TcpStream, entry: &Entry) {
    loop {
        let mut buf = vec![0; 1024];
        match local_stream.read(&mut buf).await {
            Ok(0) => {
                let _ = local_stream.shutdown(Shutdown::Read);
                entry.eof().await;
                break;
            }

            Ok(n) => {
                buf.truncate(n);
                entry.write(buf).await;
            }

            Err(_) => {
                let _ = local_stream.shutdown(Shutdown::Both);
                entry.close().await;
                break;
            }
        }
    }
}

async fn entry_to_local_stream(entry: &Entry, local_stream: &mut &TcpStream) {
    loop {
        let buf = match entry.read().await {
            EntryMessage::Data(buf) => buf,

            EntryMessage::Eof => {
                let _ = local_stream.shutdown(Shutdown::Write);
                break;
            }

            _ => {
                let _ = local_stream.shutdown(Shutdown::Both);
                break;
            }
        };

        if local_stream.write_all(&buf).await.is_err() {
            let _ = local_stream.shutdown(Shutdown::Both);
            break;
        }
    }
}

async fn run_entry(
    mut stream: TcpStream,
    entry: Entry,
) {
    match socks5::read_dest(&mut stream).await {
        Ok(socks5::Destination::Ip4 {
               address
           }) => {
            let mut buf = Vec::new();
            let _ = std::io::Write::write_fmt(&mut buf, format_args!("{}", address));
            entry.connect_ip4(buf).await;
        }

        Ok(socks5::Destination::DomainName{
            name,
            port,
        }) => {
            entry.connect_domain_name(name, port).await;
        }

        _ => {
            return entry.close().await;
        }
    }

    let address = match entry.read().await {
        EntryMessage::ConnectOk(buf) => from_utf8(&buf).unwrap().to_socket_addrs().unwrap().nth(0),

        _ => None,
    };

    let success = match address {
        Some(address) => socks5::write_dest_connected(&mut stream, address)
            .await
            .is_ok(),
        None => socks5::write_dest_unreached(&mut stream).await.is_ok() && false,
    };

    if success {
        let (stream0, stream1) = &mut (&stream, &stream);
        let r = local_stream_to_entry(stream0, &entry);
        let w = entry_to_local_stream(&entry, stream1);
        let _ = r.join(w).await;
    } else {
        let _ = stream.shutdown(Shutdown::Both);
    }

    entry.close().await;
}

fn run_tunnels(
    config: &Config
) {
    task::block_on(async move {
        let mut tunnels = Vec::new();
        for ti in 0..config.get_tunnel_max() {
            let tunnel = TcpTunnel::new(config, ti);
            tunnels.push(tunnel);
        }

        let mut index = 0;
        let listener = TcpListener::bind(config.get_listen_address()).await.unwrap();
        let mut incoming = listener.incoming();

        while let Some(stream) = incoming.next().await {
            match stream {
                Ok(stream) => {
                    {
                        let tunnel: &mut Tunnel = tunnels.get_mut(index).unwrap();
                        let entry = tunnel.open_entry().await;
                        task::spawn(async move {
                            run_entry(stream, entry).await;
                        });
                    }

                    index = (index + 1) % tunnels.len();
                }

                Err(_) => {}
            }
        }
    });
}

fn main() {
    let args: Vec<_> = env::args().collect();
    let program = args[0].clone();

    let mut opts = getopts::Options::new();
    opts.reqopt("s", "server", "server address", "server-address");
    opts.reqopt("m", "tunnel-max", "tunnel max", "tunnel-max");
    opts.optopt("l", "listen", "listen address", "listen-address");
    opts.optopt("", "log", "log path", "log-path");
    opts.optopt("k", "key", "key", "key");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => m,
        Err(_) => {
            println!("{}", opts.short_usage(&program));
            return;
        }
    };

    let server_addr = matches.opt_str("s").unwrap();
    let tunnel_max = matches.opt_str("c").unwrap_or(String::from("2"));
    let log_path = matches.opt_str("log").unwrap_or(String::from("/var/log/bit-tunnel/client.log"));
    let listen_addr = matches.opt_str("l").unwrap_or("127.0.0.1:1080".to_string());
    let key = matches.opt_str("k").unwrap_or("123456".to_string());

    let tunnel_max: u32 = match tunnel_max.parse() {
        Err(_) | Ok(0) => {
            println!("tunnel-count must greater than 0");
            return;
        }
        Ok(count) => count,
    };

    logger::init(log::Level::Info, log_path, 1, 2000000).unwrap();
    info!("starting up");

    let config = Config::new(
        key.into_bytes(),
        listen_addr,
        server_addr,
        tunnel_max
    );

    run_tunnels(&config);
}