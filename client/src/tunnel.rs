use std::net::{Shutdown};
use async_std::net::TcpStream;
use async_std::channel::{bounded, Receiver, Sender};
use async_std::task;
use std::time::Duration;
use async_std::io::{Read, Write};
use async_std::prelude::*;
use std::collections::HashMap;
use common::timer;
use common::protocol::{sc, HEARTBEAT_INTERVAL_MS, VERIFY_DATA, pack_cs_heartbeat, pack_cs_entry_open, pack_cs_connect_ip4, pack_cs_connect_domain_name, pack_cs_eof, pack_cs_data, pack_cs_entry_close};
use log::{info, error, warn};
use time::{get_time, Timespec};
use crate::config::Config;
use common::cryptor::Cryptor;
use std::str::from_utf8;


pub struct Tunnel {
    entry_id_current: u32,
    sender: Sender<Message>,
}


impl Tunnel {
    pub async fn open_entry(&mut self) -> Entry {
        let entry_id = self.entry_id_current;
        self.entry_id_current += 1;
        let (entry_sender, entry_receiver) = bounded(999);
        let _ = self.sender.send(Message::CS(Cs::EntryOpen(entry_id, entry_sender))).await;

        Entry {
            id: entry_id,
            tunnel_sender: self.sender.clone(),
            entry_receiver,
        }

    }
}


pub struct Entry {
    id: u32,
    tunnel_sender: Sender<Message>,
    entry_receiver: Receiver<EntryMessage>,
}

impl Entry {

    pub async fn write(&self, buf: Vec<u8>) {
        let _ = self.tunnel_sender.send(Message::CS(Cs::Data(self.id, buf))).await;
    }

    pub async fn connect_ip4(&self, address: Vec<u8>) {
        let _ = self.tunnel_sender.send(Message::CS(Cs::ConnectIp4(self.id, address))).await;
    }

    pub async fn connect_domain_name(&self, domain_name: Vec<u8>, port: u16) {
        let _ = self.tunnel_sender
            .send(Message::CS(Cs::ConnectDomainName(self.id, domain_name, port)))
            .await;
    }

    pub async fn eof(&self) {
        let _ = self.tunnel_sender.send(Message::CS(Cs::Eof(self.id))).await;
    }

    pub async fn close(&self) {
        let _ = self.tunnel_sender.send(Message::CS(Cs::EntryClose(self.id))).await;
    }

    pub async fn read(&self) -> EntryMessage {
        match self.entry_receiver.recv().await {
            Ok(msg) => msg,
            Err(_) => EntryMessage::Close,
        }
    }
}

pub struct TcpTunnel;

impl TcpTunnel {
    pub fn new(config: &Config, tunnel_id: u32) -> Tunnel {
        let (s, r) = bounded(10000);
        let s1 = s.clone();
        let config = config.clone();
        task::spawn(async move {

            loop {
                TcpTunnel::task(
                    &config,
                    tunnel_id,
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
        config: &Config,
        tunnel_id: u32,
        tunnel_sender: Sender<Message>,
        tunnel_receiver: Receiver<Message>,
    ) {
        info!("{}: tunnel connecting", tunnel_id);
        let server_stream = match TcpStream::connect(config.get_server_address()).await {
            Ok(server_stream) => server_stream,

            Err(_) => {
                task::sleep(Duration::from_millis(1000)).await;
                return;
            }
        };

        let (server_stream0, server_stream1) = &mut (&server_stream, &server_stream);

        let write_tid = server_stream0.write_all(&tunnel_id.to_be_bytes()).await;
        if let Err(e) = write_tid {
            error!("tunnel connect error, {}", e);
            return;
        }
        info!("{}: tunnel connect ok", tunnel_id);

        let mut entry_map = EntryMap::new();

        let r = async {
            let _ = server_stream_to_tunnel(config, server_stream0, tunnel_sender.clone()).await;
            let _ = server_stream.shutdown(Shutdown::Both);
        };
        let w = async {
            let _ = tunnel_to_server_stream(config, tunnel_id, tunnel_receiver.clone(), &mut entry_map, server_stream1).await;
            let _ = server_stream.shutdown(Shutdown::Both);
        };
        let _ = r.join(w).await;

        warn!("{}: tunnel broken", tunnel_id);

        for (_, value) in entry_map.iter() {
            let _ = value.sender.send(EntryMessage::Close).await;
        }
    }
}


///从server_stream读数据, 向tunnel写数据
async fn server_stream_to_tunnel<R: Read + Unpin>(
    config: &Config,
    server_stream: &mut R,
    tunnel_sender: Sender<Message>,
) -> std::io::Result<()> {

    let mut ctr = vec![0; Cryptor::ctr_size()];
    server_stream.read_exact(&mut ctr).await?;

    let mut cryptor = Cryptor::with_ctr(config.get_key(), ctr);

    loop {
        let mut op = [0u8; 1];
        server_stream.read_exact(&mut op).await?;
        let op = op[0];

        if op == sc::HEARTBEAT {
            let _ = tunnel_sender.send(Message::SC(Sc::Heartbeat)).await;
            continue;
        }

        let mut id = [0u8; 4];
        server_stream.read_exact(&mut id).await?;
        let id = u32::from_be(unsafe { *(id.as_ptr() as *const u32) });

        match op {
            sc::ENTRY_CLOSE => {
                let _ = tunnel_sender.send(Message::SC(Sc::EntryClose(id))).await;
            }

            sc::EOF => {
                let _ = tunnel_sender.send(Message::SC(Sc::Eof(id))).await;
            }

            sc::CONNECT_OK | sc::DATA => {
                let mut len = [0u8; 4];
                server_stream.read_exact(&mut len).await?;
                let len = u32::from_be(unsafe { *(len.as_ptr() as *const u32) });

                let mut buf = vec![0; len as usize];
                server_stream.read_exact(&mut buf).await?;

                let data = cryptor.decrypt(&buf);

                if op == sc::CONNECT_OK {
                    let _ = tunnel_sender.send(Message::SC(Sc::ConnectOk(id, data))).await;
                } else {
                    let _ = tunnel_sender.send(Message::SC(Sc::Data(id, data))).await;
                }
            }

            _ => break,
        }
    }

    Ok(())
}

///从channel读数据, 向tunnel写数据
async fn tunnel_to_server_stream<W: Write + Unpin>(
    config: &Config,
    tunnel_id: u32,
    tunnel_receiver: Receiver<Message>,
    entry_map: &mut EntryMap,
    server_stream: &mut W
) -> std::io::Result<()> {

    let mut alive_time = get_time();
    let duration = Duration::from_millis(HEARTBEAT_INTERVAL_MS as u64);
    let timer_stream = timer::interval(duration, Message::CS(Cs::Heartbeat));
    let mut msg_stream = timer_stream.merge(tunnel_receiver);

    let mut cryptor = Cryptor::new(config.get_key());

    server_stream.write_all(cryptor.ctr_as_slice()).await?;
    server_stream.write_all(cryptor.encrypt(&VERIFY_DATA).as_slice()).await?;

    loop {
        match msg_stream.next().await {
            Some(msg) => {
                process_tunnel_message(tunnel_id, msg, &mut alive_time, entry_map, server_stream, &mut cryptor)
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
    cryptor: &mut Cryptor
) -> std::io::Result<()> {
    match msg {

        Message::CS(cs) => {
            match cs{

                Cs::Heartbeat => {
                    server_stream.write_all(&pack_cs_heartbeat()).await?;
                }

                Cs::EntryOpen(id, entry_sender) => {
                    entry_map.insert(
                        id,
                        EntryInternal {
                            sender: entry_sender,
                            host: String::new(),
                            port: 0,
                        },
                    );

                    server_stream.write_all(&pack_cs_entry_open(id)).await?;
                }

                Cs::ConnectIp4(id, buf) => {
                    let addr = from_utf8(&buf).unwrap();
                    info!("{}.{}: connecting {}", tid, id, addr);

                    let data = cryptor.encrypt(&buf);
                    server_stream.write_all(&pack_cs_connect_ip4(id, &data)).await?;
                }

                Cs::ConnectDomainName(id, buf, port) => {
                    let host = String::from_utf8(buf.clone()).unwrap_or(String::new());
                    info!("{}.{}: connecting {}:{}", tid, id, host, port);

                    if let Some(value) = entry_map.get_mut(&id) {
                        value.host = host;
                        value.port = port;
                    }
                    let data = cryptor.encrypt(&buf);
                    let packed_buffer = pack_cs_connect_domain_name(id, &data, port);
                    server_stream.write_all(&packed_buffer).await?;
                }

                Cs::Eof(id) => {
                    match entry_map.get(&id) {
                        Some(entry) => {
                            info!(
                                "{}.{}: client eof {}:{}",
                                tid, id, entry.host, entry.port
                            );
                        }

                        None => {
                            info!("{}.{}: client eof repeated", tid, id);
                        }
                    }

                    server_stream.write_all(&pack_cs_eof(id)).await?;
                }

                Cs::Data(id, buf) => {
                    let data = cryptor.encrypt(&buf);
                    server_stream.write_all(&pack_cs_data(id, &data)).await?;
                }

                Cs::EntryClose(id) => {
                    match entry_map.get(&id) {
                        Some(value) => {
                            info!("{}.{}: client close {}:{}", tid, id, value.host, value.port);
                            let _ = value.sender.send(EntryMessage::Close).await;
                            server_stream.write_all(&pack_cs_entry_close(id)).await?;
                        }

                        None => {
                            info!("{}.{}: client close repeated", tid, id);
                        }
                    }

                    entry_map.remove(&id);
                }


            }
        }

        Message::SC(sc) => {
            match sc {
                Sc::Heartbeat => {
                    *alive_time = get_time();
                }

                Sc::EntryClose(id) => {
                    *alive_time = get_time();

                    match entry_map.get(&id) {
                        Some(entry) => {
                            info!("{}.{}: server close {}:{}", tid, id, entry.host, entry.port);

                            let _ = entry.sender.send(EntryMessage::Close).await;
                        }

                        None => {
                            info!("{}.{}: server close repeated", tid, id);
                        }
                    }

                    entry_map.remove(&id);
                }

                Sc::Eof(id) => {
                    *alive_time = get_time();

                    match entry_map.get(&id) {
                        Some(entry) => {
                            info!(
                                "{}.{}: server eof {}:{}",
                                tid, id, entry.host, entry.port
                            );

                            let _ = entry.sender.send(EntryMessage::Eof).await;
                        }

                        None => {
                            info!("{}.{}: server eof unknown client", tid, id);
                        }
                    }
                }

                Sc::ConnectOk(id, buf) => {
                    *alive_time = get_time();

                    match entry_map.get(&id) {
                        Some(value) => {
                            info!("{}.{}: connect ok {}:{}", tid, id, value.host, value.port);

                            let _ = value.sender.send(EntryMessage::ConnectOk(buf)).await;
                        }

                        None => {
                            info!("{}.{}: connect unknown server ok", tid, id);
                        }
                    }
                }

                //向channel写数据
                Sc::Data(id, buf) => {
                    *alive_time = get_time();
                    if let Some(value) = entry_map.get(&id) {
                        let _ = value.sender.send(EntryMessage::Data(buf)).await;
                    };
                }
            }
        }
    }

    Ok(())
}


type EntryMap = HashMap<u32, EntryInternal>;

struct EntryInternal {
    host: String,
    port: u16,
    sender: Sender<EntryMessage>,
}


#[derive(Clone)]
enum Message {
    CS(Cs),
    SC(Sc)
}

#[derive(Clone)]
enum Cs {
    EntryOpen(u32, Sender<EntryMessage>),
    EntryClose(u32),
    ConnectIp4(u32, Vec<u8>),
    ConnectDomainName(u32, Vec<u8>, u16),
    Eof(u32),
    Data(u32, Vec<u8>),
    Heartbeat,
}

#[derive(Clone)]
enum Sc{
    EntryClose(u32),
    Eof(u32),
    ConnectOk(u32, Vec<u8>),
    Data(u32, Vec<u8>),
    Heartbeat,
}

pub enum EntryMessage{
    ConnectOk(Vec<u8>),
    Data(Vec<u8>),
    Eof,
    Close,
}

