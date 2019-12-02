use std::net::{SocketAddrV4, Ipv4Addr, Shutdown};
use std::net::SocketAddr;
use async_std::net::TcpStream;
use async_std::sync::{channel, Receiver, Sender};
use async_std::task;
use std::time::Duration;
use async_std::io::{Read, Write};
use async_std::prelude::*;
use std::collections::HashMap;
use super::timer;
use common::protocol::{sc, HEARTBEAT_INTERVAL_MS, VERIFY_DATA, ALIVE_TIMEOUT_TIME_MS, pack_cs_heartbeat_msg, pack_cs_open_port_msg, pack_cs_connect_msg, pack_cs_connect_domain_msg, pack_cs_shutdown_write_msg, pack_cs_data_msg, pack_cs_close_port_msg};
use log::{info};
use time::{get_time, Timespec};

pub enum Destination {
    Address {
        address: SocketAddr
    },
    DomainName {
        name: Vec<u8>,
        port: u16,
    },
    Unknown,
}


const VER: u8 = 5;
const RSV: u8 = 0;

const CMD_CONNECT: u8 = 1;
const METHOD_NO_AUTH: u8 = 0;
const METHOD_NO_ACCEPT: u8 = 0xFF;

const ATYP_IPV4: u8 = 1;
const ATYP_DOMAINNAME: u8 = 3;
const ATYP_IPV6: u8 = 4;


async fn respond(stream: &mut TcpStream, method: u8) -> std::io::Result<()> {
    let buf = [VER, method];
    stream.write_all(&buf).await
}


pub async fn read_destination(stream: &mut TcpStream) -> std::io::Result<Destination> {
    let mut buf = [0u8; 2];
    stream.read_exact(&mut buf).await?;

    if buf[0] != VER {
        respond(stream, METHOD_NO_ACCEPT).await?;
        return Ok(Destination::Unknown);
    }

    let mut methods = vec![0; buf[1] as usize];
    stream.read_exact(&mut methods).await?;

    if !methods.into_iter().any(|method| method == METHOD_NO_AUTH) {
        respond(stream, METHOD_NO_ACCEPT).await?;
        return Ok(Destination::Unknown);
    }

    respond(stream, METHOD_NO_AUTH).await?;

    let mut buf = [0u8; 4];
    stream.read_exact(&mut buf).await?;


    if buf[1] != CMD_CONNECT {
        return Ok(Destination::Unknown);
    }

    let destination = match buf[3] {
        ATYP_IPV4 => {
            let mut ipv4_addr = [0u8; 6];
            stream.read_exact(&mut ipv4_addr).await?;

            let port = unsafe { *(ipv4_addr.as_ptr().offset(4) as *const u16) };
            Destination::Address {
                address: SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::new(ipv4_addr[3], ipv4_addr[2], ipv4_addr[1], ipv4_addr[0]),
                    u16::from_be(port),
                ))
            }
        }

        ATYP_DOMAINNAME => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;

            let len = len[0] as usize;
            let mut buf = vec![0u8; len + 2];
            stream.read_exact(&mut buf).await?;

            let port = unsafe { *(buf.as_ptr().offset(len as isize) as *const u16) };
            buf.truncate(len);
            Destination::DomainName { name: buf, port: u16::from_be(port) }
        }

        ATYP_IPV6 => Destination::Unknown,
        _ => Destination::Unknown,
    };

    Ok(destination)
}


pub struct Tunnel {
    entry_id_current: u32,
    sender: Sender<Message>,
}


impl Tunnel {
    pub async fn new_entry(&mut self) -> Entry {
        let entry_id = self.entry_id_current;
        self.entry_id_current += 1;
        let (entry_sender, entry_receiver) = channel(999);
        self.sender.send(Message::CSEntryOpen(entry_id, entry_sender)).await;

        (
            Entry {
                id: entry_id,
                tunnel_sender: self.sender.clone(),
                entry_receiver,
            }
        )
    }
}


pub struct Entry {
    id: u32,
    tunnel_sender: Sender<Message>,
    entry_receiver: Receiver<EntryMessage>,
}

impl Entry {
    pub async fn drop(&self) {
        self.tunnel_sender.send(Message::EntryClose(self.id)).await;
    }

    pub async fn write(&self, buf: Vec<u8>) {
        self.tunnel_sender.send(Message::CSData(self.id, buf)).await;
    }

    pub async fn connect_ip(&self, address: Vec<u8>) {
        self.tunnel_sender.send(Message::CSConnectIp(self.id, address)).await;
    }

    pub async fn connect_domain_name(&self, domain_name: Vec<u8>, port: u16) {
        self.tunnel_sender
            .send(Message::CSConnectDomainName(self.id, domain_name, port))
            .await;
    }

    pub async fn shutdown_write(&self) {
        self.tunnel_sender.send(Message::CSShutdownWrite(self.id)).await;
    }

    pub async fn close(&self) {
        self.tunnel_sender.send(Message::CSEntryClose(self.id)).await;
    }

    pub async fn read(&self) -> EntryMessage {
        match self.entry_receiver.recv().await {
            Some(msg) => msg,
            None => EntryMessage::Close,
        }
    }
}

pub struct TcpTunnel;

impl TcpTunnel {
    pub fn new(tunnel_id: u32, server_address: String) -> Tunnel {
        let (s, r) = channel(10000);
        let s1 = s.clone();

        task::spawn(async move {
            loop {
                TcpTunnel::task(
                    tunnel_id,
                    server_address.clone(),
                    s.clone(),
                    r.clone(),
                ).await;
            }
        });

        Tunnel {
            entry_id_current: 1,
            sender: s1,
        }
    }

    async fn task(
        tunnel_id: u32,
        server_address: String,
        tunnel_sender: Sender<Message>,
        tunnel_receiver: Receiver<Message>,
    ) {
        let server_stream = match TcpStream::connect(&server_address).await {
            Ok(server_stream) => server_stream,

            Err(_) => {
                task::sleep(Duration::from_millis(1000)).await;
                return;
            }
        };

        let mut entry_map = EntryMap::new();
        let (server_stream0, server_stream1) = &mut (&server_stream, &server_stream);
        let r = async {
            let _ = server_stream_to_tunnel(server_stream0, tunnel_sender.clone()).await;
            let _ = server_stream.shutdown(Shutdown::Both);
        };
        let w = async {
            let _ = tunnel_to_server_stream(tunnel_id, tunnel_receiver.clone(), &mut entry_map, server_stream1).await;
            let _ = server_stream.shutdown(Shutdown::Both);
        };
        let _ = r.join(w).await;

        info!("Tcp tunnel {} broken", tunnel_id);

        for (_, value) in entry_map.iter() {
            value.sender.send(EntryMessage::Close).await;
        }
    }
}


///从server_stream读数据, 向tunnel写数据
async fn server_stream_to_tunnel<R: Read + Unpin>(
    server_stream: &mut R,
    tunnel_sender: Sender<Message>,
) -> std::io::Result<()> {

    loop {
        let mut op = [0u8; 1];
        server_stream.read_exact(&mut op).await?;
        let op = op[0];

        if op == sc::HEARTBEAT_RSP {
            tunnel_sender.send(Message::SCHeartbeat).await;
            continue;
        }

        let mut id = [0u8; 4];
        server_stream.read_exact(&mut id).await?;
        let id = u32::from_be(unsafe { *(id.as_ptr() as *const u32) });

        match op {
            sc::CLOSE_PORT => {
                tunnel_sender.send(Message::SCClosePort(id)).await;
            }

            sc::SHUTDOWN_WRITE => {
                tunnel_sender.send(Message::SCShutdownWrite(id)).await;
            }

            sc::CONNECT_OK | sc::DATA => {
                let mut len = [0u8; 4];
                server_stream.read_exact(&mut len).await?;
                let len = u32::from_be(unsafe { *(len.as_ptr() as *const u32) });

                let mut buf = vec![0; len as usize];
                server_stream.read_exact(&mut buf).await?;

                let data = buf;

                if op == sc::CONNECT_OK {
                    tunnel_sender.send(Message::SCConnectOk(id, data)).await;
                } else {
                    tunnel_sender.send(Message::SCData(id, data)).await;
                }
            }

            _ => break,
        }
    }

    Ok(())
}

///从channel读数据, 向tunnel写数据
async fn tunnel_to_server_stream<W: Write + Unpin>(
    tunnel_id: u32,
    tunnel_receiver: Receiver<Message>,
    entry_map: &mut EntryMap,
    server_stream: &mut W,
) -> std::io::Result<()> {
    let mut alive_time = get_time();

    let duration = Duration::from_millis(HEARTBEAT_INTERVAL_MS as u64);
    let timer_stream = timer::interval(duration, Message::Heartbeat);
    let mut msg_stream = timer_stream.merge(tunnel_receiver);

    server_stream.write_all(&VERIFY_DATA).await?;

    loop {
        match msg_stream.next().await {
            Some(Message::Heartbeat) => {
                let duration = get_time() - alive_time;
                if duration.num_milliseconds() > ALIVE_TIMEOUT_TIME_MS {
                    break;
                }

                server_stream.write_all(&pack_cs_heartbeat_msg()).await?;
            }

            Some(msg) => {
                process_tunnel_message(tunnel_id, msg, &mut alive_time, entry_map, server_stream)
                    .await?;
            }

            None => break,
        }
    }

    Ok(())
}

async fn process_tunnel_message<W: Write + Unpin>(
    tid: u32,
    msg: Message,
    alive_time: &mut Timespec,
    entry_map: &mut EntryMap,
    server_stream: &mut W,
) -> std::io::Result<()> {
    match msg {
        Message::CSEntryOpen(id, entry_sender) => {
            entry_map.insert(
                id,
                EntryInternal {
                    count: 2,
                    sender: entry_sender,
                    host: String::new(),
                    port: 0,
                },
            );

            server_stream.write_all(&pack_cs_open_port_msg(id)).await?;
        }

        Message::CSConnectIp(id, buf) => {
            let data = buf;
            server_stream.write_all(&pack_cs_connect_msg(id, &data)).await?;
        }

        Message::CSConnectDomainName(id, buf, port) => {
            let host = String::from_utf8(buf.clone()).unwrap_or(String::new());
            info!("{}.{}: connecting {}:{}", tid, id, host, port);

            if let Some(value) = entry_map.get_mut(&id) {
                value.host = host;
                value.port = port;
            }

            let packed_buffer = pack_cs_connect_domain_msg(id, &buf, port);
            server_stream.write_all(&packed_buffer).await?;
        }

        Message::CSShutdownWrite(id) => {
            match entry_map.get(&id) {
                Some(entry) => {
                    info!(
                        "{}.{}: client shutdown write {}:{}",
                        tid, id, entry.host, entry.port
                    );
                }

                None => {
                    info!("{}.{}: client shutdown write unknown server", tid, id);
                }
            }

            server_stream.write_all(&pack_cs_shutdown_write_msg(id)).await?;
        }

        Message::CSData(id, buf) => {
            server_stream.write_all(&pack_cs_data_msg(id, &buf)).await?;
        }

        Message::CSEntryClose(id) => {
            match entry_map.get(&id) {
                Some(value) => {
                    info!("{}.{}: client close {}:{}", tid, id, value.host, value.port);
                    value.sender.send(EntryMessage::Close).await;
                    server_stream.write_all(&pack_cs_close_port_msg(id)).await?;
                }

                None => {
                    info!("{}.{}: client close unknown server", tid, id);
                }
            }

            entry_map.remove(&id);
        }

        Message::SCHeartbeat => {
            *alive_time = get_time();
        }

        Message::SCClosePort(id) => {
            *alive_time = get_time();

            match entry_map.get(&id) {
                Some(value) => {
                    info!("{}.{}: server close {}:{}", tid, id, value.host, value.port);

                    value.sender.send(EntryMessage::Close).await;
                }

                None => {
                    info!("{}.{}: server close unknown client", tid, id);
                }
            }

            entry_map.remove(&id);
        }

        Message::SCShutdownWrite(id) => {
            *alive_time = get_time();

            match entry_map.get(&id) {
                Some(entry) => {
                    info!(
                        "{}.{}: server shutdown write {}:{}",
                        tid, id, entry.host, entry.port
                    );

                    entry.sender.send(EntryMessage::ShutdownWrite).await;
                }

                None => {
                    info!("{}.{}: server shutdown write unknown client", tid, id);
                }
            }
        }

        Message::SCConnectOk(id, buf) => {
            *alive_time = get_time();

            match entry_map.get(&id) {
                Some(value) => {
                    info!("{}.{}: connect {}:{} ok", tid, id, value.host, value.port);

                    value.sender.send(EntryMessage::ConnectOk(buf)).await;
                }

                None => {
                    info!("{}.{}: connect unknown server ok", tid, id);
                }
            }
        }

        //向channel写数据
        Message::SCData(id, buf) => {
            *alive_time = get_time();
            if let Some(value) = entry_map.get(&id) {
                value.sender.send(EntryMessage::Data(buf)).await;
            };
        }

        Message::EntryClose(id) => {
            if let Some(value) = entry_map.get_mut(&id) {
                value.count = value.count - 1;
                if value.count == 0 {
                    info!(
                        "{}.{}: drop tunnel port {}:{}",
                        tid, id, value.host, value.port
                    );
                    entry_map.remove(&id);
                }
            } else {
                info!("{}.{}: drop unknown tunnel port", tid, id);
            }
        }

        _ => {}
    }

    Ok(())
}


type EntryMap = HashMap<u32, EntryInternal>;

struct EntryInternal {
    host: String,
    port: u16,
    count: u32,
    sender: Sender<EntryMessage>,
}


#[derive(Clone)]
enum Message {
    CSEntryOpen(u32, Sender<EntryMessage>),
    CSEntryClose(u32),
    CSConnectIp(u32, Vec<u8>),
    CSConnectDomainName(u32, Vec<u8>, u16),
    CSShutdownWrite(u32),
    CSData(u32, Vec<u8>),

    SCHeartbeat,
    SCClosePort(u32),
    SCShutdownWrite(u32),
    SCConnectOk(u32, Vec<u8>),
    SCData(u32, Vec<u8>),

    Heartbeat,
    EntryClose(u32),
}

enum EntryMessage {
    ConnectOk(Vec<u8>),
    Data(Vec<u8>),
    ShutdownWrite,
    Close,
}

