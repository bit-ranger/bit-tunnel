use std::collections::HashMap;
use std::net::Shutdown;
use std::str::from_utf8;
use std::time::Duration;
use std::vec::Vec;

use async_std::io::{Read, Write};
use async_std::net::TcpStream;
use async_std::prelude::*;
use async_std::sync::{channel, Receiver, Sender};
use async_std::task;

use time::{get_time, Timespec};
use common::protocol::{cs, VERIFY_DATA, HEARTBEAT_INTERVAL_MS, pack_sc_heartbeat, pack_sc_entry_close, pack_sc_eof, pack_sc_connect_ok, pack_sc_data};
use common::timer;
use log::{info};
use common::cryptor::Cryptor;
use crate::config::Config;


pub struct TcpTunnel;
impl TcpTunnel {
    pub fn new(config: &Config, client_stream: TcpStream) {
        let config  = config.clone();
        task::spawn(async move {
            TcpTunnel::task(&config, client_stream).await;
        });
    }

    async fn task(config: &Config, client_stream: TcpStream) {
        let (tunnel_sender, tunnel_receiver) = channel(10000);

        let mut entry_map = EntryMap::new();
        let (client_stream0, client_stream1) = &mut (&client_stream, &client_stream);
        let r = async {
            let _ = client_stream_to_tunnel( config, client_stream0, tunnel_sender.clone()).await;
            tunnel_sender.send(Message::SC(Sc::CloseTunnel)).await;
            let _ = client_stream.shutdown(Shutdown::Both);
        };
        let w = async {
            let _ = tunnel_to_client_stream(config, tunnel_sender.clone(), tunnel_receiver, &mut entry_map, client_stream1)
                .await;
            let _ = client_stream.shutdown(Shutdown::Both);
        };
        let _ = r.join(w).await;

        for (_, value) in entry_map.iter() {
            value.sender.send(EntryMessage::Close).await;
        }
    }
}


pub struct Entry {
    id: u32,
    tunnel_sender: Sender<Message>,
    entry_receiver: Receiver<EntryMessage>,
}




impl Entry {

    async fn connect_ok(&self, buf: Vec<u8>) {
        self.tunnel_sender.send(Message::SC(Sc::ConnectOk(self.id, buf))).await;
    }

    async fn write(&self, buf: Vec<u8>) {
        self.tunnel_sender.send(Message::SC(Sc::Data(self.id, buf))).await;
    }

    async fn eof(&self) {
        self.tunnel_sender.send(Message::SC(Sc::Eof(self.id))).await;
    }

    async fn close(&self) {
        self.tunnel_sender.send(Message::SC(Sc::EntryClose(self.id))).await;
    }

    async fn read(&self) -> EntryMessage {
        match self.entry_receiver.recv().await {
            Some(msg) => msg,
            None => EntryMessage::Close,
        }
    }
}

struct EntryInternal {
    sender: Sender<EntryMessage>,
}

type EntryMap = HashMap<u32, EntryInternal>;


async fn dest_stream_to_entry(dest_stream: &mut &TcpStream, entry: &Entry) {
    loop {
        let mut buf = vec![0; 1024];
        match dest_stream.read(&mut buf).await {
            Ok(0) => {
                let _ = dest_stream.shutdown(Shutdown::Read);
                entry.eof().await;
                break;
            }

            Ok(n) => {
                buf.truncate(n);
                entry.write(buf).await;
            }

            Err(_) => {
                let _ = dest_stream.shutdown(Shutdown::Both);
                entry.close().await;
                break;
            }
        }
    }
}

async fn entry_to_dest_stream(entry: &Entry, dest_stream: &mut &TcpStream) {
    loop {
        match entry.read().await {
            EntryMessage::Data(buf) => {
                if dest_stream.write_all(&buf).await.is_err() {
                    let _ = dest_stream.shutdown(Shutdown::Both);
                    break;
                }
            }

            EntryMessage::Eof => {
                let _ = dest_stream.shutdown(Shutdown::Write);
                break;
            }

            _ => {
                let _ = dest_stream.shutdown(Shutdown::Both);
                break;
            }
        }
    }
}

async fn entry_task(entry: Entry) {
    let dest_stream = match entry.read().await {
        EntryMessage::ConnectIp(buf) => {
            TcpStream::connect(from_utf8(&buf).unwrap()).await.ok()
        }

        EntryMessage::ConnectDomainName(domain_name, port) => {
            info!("connect {}", from_utf8(&domain_name).unwrap());
            TcpStream::connect((from_utf8(&domain_name).unwrap(), port))
                .await
                .ok()
        }

        _ => None,
    };

    let dest_stream = match dest_stream {
        Some(s) => s,
        None => return entry.close().await,
    };

    match dest_stream.local_addr() {
        Ok(address) => {
            let mut buf = Vec::new();
            let _ = std::io::Write::write_fmt(&mut buf, format_args!("{}", address));
            entry.connect_ok(buf).await;
        }

        Err(_) => {
            return entry.close().await;
        }
    }

    let (dest_stream0, dest_stream1) = &mut (&dest_stream, &dest_stream);
    let w = dest_stream_to_entry(dest_stream0, &entry);
    let r = entry_to_dest_stream(&entry, dest_stream1);
    let _ = r.join(w).await;

    entry.close().await;
}



async fn client_stream_to_tunnel<R: Read + Unpin>(
    config: &Config,
    client_stream: &mut R,
    tunnel_sender: Sender<Message>
) -> std::io::Result<()> {

    let mut ctr = vec![0; Cryptor::ctr_size()];
    client_stream.read_exact(&mut ctr).await?;

    let mut cryptor = Cryptor::with_ctr(config.get_key(), ctr);

    let mut buf = vec![0; VERIFY_DATA.len()];
    client_stream.read_exact(&mut buf).await?;

    let data = cryptor.decrypt(&buf);
    if &data != &VERIFY_DATA {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }

    loop {
        let mut op = [0u8; 1];
        client_stream.read_exact(&mut op).await?;
        let op = op[0];

        if op == cs::HEARTBEAT {
            tunnel_sender.send(Message::CS(Cs::Heartbeat)).await;
            continue;
        }

        let mut id = [0u8; 4];
        client_stream.read_exact(&mut id).await?;
        let id = u32::from_be(unsafe { *(id.as_ptr() as *const u32) });

        match op {
            cs::ENTRY_OPEN => {
                tunnel_sender.send(Message::CS(Cs::EntryOpen(id))).await;
            }

            cs::ENTRY_CLOSE => {
                tunnel_sender.send(Message::CS(Cs::EntryClose(id))).await;
            }

            cs::EOF => {
                tunnel_sender.send(Message::CS(Cs::Eof(id))).await;
            }

            cs::CONNECT_DOMAIN_NAME => {
                let mut len = [0u8; 4];
                client_stream.read_exact(&mut len).await?;
                let len = u32::from_be(unsafe { *(len.as_ptr() as *const u32) });

                let mut buf = vec![0; len as usize];
                client_stream.read_exact(&mut buf).await?;

                let pos = (len - 2) as usize;
                let domain_name = cryptor.decrypt(&buf[0..pos]);
                let port = u16::from_be(unsafe { *(buf[pos..].as_ptr() as *const u16) });

                tunnel_sender
                    .send(Message::CS(Cs::ConnectDomainName(id, domain_name, port)))
                    .await;
            }

            cs::CONNECT_IP4 => {
                let mut len = [0u8; 4];
                client_stream.read_exact(&mut len).await?;
                let len = u32::from_be(unsafe { *(len.as_ptr() as *const u32) });

                let mut buf = vec![0; len as usize];
                client_stream.read_exact(&mut buf).await?;

                let data = cryptor.decrypt(&buf);
                tunnel_sender
                    .send(Message::CS(Cs::ConnectIp4(id, data)))
                    .await;
            }

            _ => {
                let mut len = [0u8; 4];
                client_stream.read_exact(&mut len).await?;
                let len = u32::from_be(unsafe { *(len.as_ptr() as *const u32) });

                let mut buf = vec![0; len as usize];
                client_stream.read_exact(&mut buf).await?;

                let data = cryptor.decrypt(&buf);
                tunnel_sender.send(Message::CS(Cs::Data(id, data))).await;
            }
        }
    }
}

async fn tunnel_to_client_stream<W: Write + Unpin>(
    config: &Config,
    tunnel_sender: Sender<Message>,
    tunnel_receiver: Receiver<Message>,
    entry_map: &mut EntryMap,
    client_stream: &mut W,
) -> std::io::Result<()> {
    let mut alive_time = get_time();
    let duration = Duration::from_millis(HEARTBEAT_INTERVAL_MS as u64);
    let timer_stream = timer::interval(duration, Message::SC(Sc::Heartbeat));
    let mut msg_stream = timer_stream.merge(tunnel_receiver);

    let mut cryptor = Cryptor::new(config.get_key());
    client_stream.write_all(cryptor.ctr_as_slice()).await?;

    loop {
        match msg_stream.next().await {

            Some(Message::SC(Sc::CloseTunnel)) => break,

            Some(msg) => {
                process_tunnel_message(
                    msg,
                    &tunnel_sender,
                    &mut alive_time,
                    entry_map,
                    client_stream,
                    &mut cryptor
                )
                .await?;
            }

            None => break,
        }
    }

    Ok(())
}

async fn process_tunnel_message<W: Write + Unpin>(
    msg: Message,
    tunnel_sender: &Sender<Message>,
    alive_time: &mut Timespec,
    entry_map: &mut EntryMap,
    client_stream: &mut W,
    cryptor: &mut Cryptor
) -> std::io::Result<()> {
    match msg {

        Message::CS(cs) => {
            match cs {
                Cs::Heartbeat => {
                    *alive_time = get_time();
                    client_stream.write_all(&pack_sc_heartbeat()).await?;
                }

                Cs::EntryOpen(id) => {
                    *alive_time = get_time();
                    let (es, er) = channel(1000);
                    entry_map.insert(id, EntryInternal {sender: es});

                    let entry = Entry{
                        id,
                        tunnel_sender: tunnel_sender.clone(),
                        entry_receiver: er
                    };

                    task::spawn(async move {
                        entry_task(entry).await;
                    });
                }

                Cs::EntryClose(id) => {
                    *alive_time = get_time();

                    if let Some(value) = entry_map.get(&id) {
                        value.sender.send(EntryMessage::Close).await;
                    };

                    entry_map.remove(&id);
                }

                Cs::Eof(id) => {
                    *alive_time = get_time();

                    if let Some(value) = entry_map.get(&id) {
                        value.sender.send(EntryMessage::Eof).await;
                    };
                }

                Cs::ConnectDomainName(id, domain_name, port) => {
                    *alive_time = get_time();

                    if let Some(value) = entry_map.get(&id) {
                        value
                            .sender
                            .send(EntryMessage::ConnectDomainName(domain_name, port))
                            .await;
                    };
                }

                Cs::ConnectIp4(id, address) => {
                    *alive_time = get_time();

                    if let Some(value) = entry_map.get(&id) {
                        value
                            .sender
                            .send(EntryMessage::ConnectIp(address))
                            .await;
                    };
                }

                Cs::Data( id, buf) => {
                    *alive_time = get_time();

                    if let Some(value) = entry_map.get(&id) {
                        value.sender.send(EntryMessage::Data( buf)).await;
                    };
                }

            }
        }

        Message::SC(sc) => {

            match sc {
                Sc::EntryClose(id) => {
                    if let Some(value) = entry_map.get(&id) {
                        value.sender.send(EntryMessage::Close).await;
                        client_stream.write_all(&pack_sc_entry_close(id)).await?;
                    };

                    entry_map.remove(&id);
                }

                Sc::Eof(id) => {
                    client_stream.write_all(&pack_sc_eof(id)).await?;
                }

                Sc::ConnectOk(id, buf) => {
                    let data = cryptor.encrypt(&buf);
                    client_stream.write_all(&pack_sc_connect_ok(id, &data)).await?;
                }

                Sc::Data(id, buf) => {
                    let data = cryptor.encrypt(&buf);
                    client_stream.write_all(&pack_sc_data(id, &data)).await?;
                }

                _ => {}
            }


        }
    }

    Ok(())
}



#[derive(Clone)]
enum Message {
    CS(Cs),
    SC(Sc)
}

#[derive(Clone)]
enum Cs {
    EntryOpen(u32),
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
    CloseTunnel
}

pub enum EntryMessage{
    ConnectIp(Vec<u8>),
    ConnectDomainName(Vec<u8>, u16),
    Data(Vec<u8>),
    Eof,
    Close,
}