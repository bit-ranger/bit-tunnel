use std::net::{SocketAddrV4, Ipv4Addr, Shutdown};
use std::net::SocketAddr;
use async_std::net::TcpStream;
use async_std::sync::{channel, Receiver, Sender};
use async_std::task;
use std::time::Duration;
use async_std::io::{Read, Write};
use std::collections::HashMap;

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


async fn respond_reject(stream: &mut TcpStream, method: u8) -> std::io::Result<()> {
    let buf = [VER, method];
    stream.write_all(&buf).await
}


pub async fn read_destination(stream: &mut TcpStream) -> std::io::Result<Destination> {
    let mut buf = [0u8; 2];
    stream.read_exact(&mut buf).await?;

    if buf[0] != VER {
        respond_reject(stream, METHOD_NO_ACCEPT).await?;
        return Ok(Destination::Unknown);
    }

    let mut methods = vec![0; buf[1] as usize];
    stream.read_exact(&mut methods).await?;

    if !methods.into_iter().any(|method| method == METHOD_NO_AUTH) {
        respond_reject(stream, METHOD_NO_ACCEPT).await?;
        return Ok(Destination::Unknown);
    }

    respond_reject(stream, METHOD_NO_AUTH).await?;

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


#[derive(Clone)]
enum TunnelMsg {
    CSOpenPort(u32, Sender<TunnelPortMsg>),
    CSConnect(u32, Vec<u8>),
    CSConnectDN(u32, Vec<u8>, u16),
    CSShutdownWrite(u32),
    CSClosePort(u32),
    CSData(u32, Vec<u8>),

    SCHeartbeat,
    SCClosePort(u32),
    SCShutdownWrite(u32),
    SCConnectOk(u32, Vec<u8>),
    SCData(u32, Vec<u8>),

    Heartbeat,
    TunnelPortDrop(u32),
}

pub enum TunnelPortMsg {
    ConnectOk(Vec<u8>),
    Data(Vec<u8>),
    ShutdownWrite,
    ClosePort,
}

pub struct TunnelWritePort {
    tunnel_id: u32,
    core_sender: Sender<TunnelMsg>,
}

pub struct TunnelReadPort {
    tunnel_id: u32,
    core_sender: Sender<TunnelMsg>,
    tunnel_receiver: Receiver<TunnelPortMsg>,
}

impl TunnelWritePort {
    pub async fn write(&self, buf: Vec<u8>) {
        self.core_sender.send(TunnelMsg::CSData(self.tunnel_id, buf)).await;
    }

    pub async fn connect(&self, address: Vec<u8>) {
        self.core_sender.send(TunnelMsg::CSConnect(self.tunnel_id, address)).await;
    }

    pub async fn connect_domain_name(&self, domain_name: Vec<u8>, port: u16) {
        self.core_sender
            .send(TunnelMsg::CSConnectDN(self.tunnel_id, domain_name, port))
            .await;
    }

    pub async fn shutdown_write(&self) {
        self.core_sender.send(TunnelMsg::CSShutdownWrite(self.tunnel_id)).await;
    }

    pub async fn close(&self) {
        self.core_sender.send(TunnelMsg::CSClosePort(self.tunnel_id)).await;
    }

    pub async fn drop(&self) {
        self.core_sender.send(TunnelMsg::TunnelPortDrop(self.tunnel_id)).await;
    }
}

impl TunnelReadPort {
    pub async fn read(&self) -> TunnelPortMsg {
        match self.tunnel_receiver.recv().await {
            Some(msg) => msg,
            None => TunnelPortMsg::ClosePort,
        }
    }

    pub async fn drop(&self) {
        self.core_sender.send(TunnelMsg::TunnelPortDrop(self.tunnel_id)).await;
    }
}

pub struct Tunnel {
    id: u32,
    core_sender: Sender<TunnelMsg>,
}

impl Tunnel {
    pub async fn open_port(&mut self) -> (TunnelWritePort, TunnelReadPort) {
        let core_tx1 = self.core_sender.clone();
        let core_tx2 = self.core_sender.clone();
        let id = self.id;
        self.id += 1;

        let (ts, tr) = channel(999);
        self.core_sender.send(TunnelMsg::CSOpenPort(id, ts)).await;

        (
            TunnelWritePort {
                tunnel_id: id,
                core_sender: core_tx1,
            },
            TunnelReadPort {
                tunnel_id: id,
                core_sender: core_tx2,
                tunnel_receiver: tr,
            },
        )
    }
}
