use std::vec::Vec;

pub const VERIFY_DATA: [u8; 8] = [0xF0u8, 0xEF, 0xE, 0x2, 0xAE, 0xBC, 0x8C, 0x78];
pub const HEARTBEAT_INTERVAL_MS: i64 = 5000;
pub const ALIVE_TIMEOUT_TIME_MS: i64 = 60000;

pub mod cs {
    pub const ENTRY_OPEN: u8 = 1;
    pub const ENTRY_CLOSE: u8 = 2;
    pub const EOF: u8 = 4;
    pub const CONNECT_IP: u8 = 5;
    pub const CONNECT_DOMAIN_NAME: u8 = 6;
    pub const DATA: u8 = 7;
    pub const HEARTBEAT: u8 = 8;
}

pub mod sc {
    pub const ENTRY_CLOSE: u8 = 1;
    pub const EOF: u8 = 3;
    pub const CONNECT_OK: u8 = 4;
    pub const DATA: u8 = 5;
    pub const HEARTBEAT: u8 = 6;
}

fn fill_cmd_id_len(buf: &mut [u8], cmd: u8, id: u32, len: u32) {
    buf[0] = cmd;
    unsafe {
        *(buf.as_ptr().offset(1) as *mut u32) = id.to_be();
        *(buf.as_ptr().offset(5) as *mut u32) = len.to_be();
    }
}

fn pack_cmd_id(cmd: u8, id: u32) -> [u8; 5] {
    let mut buf = [0u8; 5];
    buf[0] = cmd;
    unsafe {
        *(buf.as_ptr().offset(1) as *mut u32) = id.to_be();
    }
    buf
}

fn pack_cmd_id_data(cmd: u8, id: u32, data: &[u8]) -> Vec<u8> {
    let mut buf = vec![0; 9 + data.len()];
    let len = data.len() as u32;

    fill_cmd_id_len(&mut buf, cmd, id, len);
    buf[9..].copy_from_slice(data);

    buf
}

pub fn pack_cs_entry_open(id: u32) -> [u8; 5] {
    pack_cmd_id(cs::ENTRY_OPEN, id)
}

pub fn pack_cs_connect(id: u32, data: &[u8]) -> Vec<u8> {
    pack_cmd_id_data(cs::CONNECT_IP, id, data)
}

pub fn pack_cs_connect_domain_name(id: u32, domain: &[u8], port: u16) -> Vec<u8> {
    let buf_len = 11 + domain.len();
    let mut buf = vec![0; buf_len];
    let len = domain.len() as u32 + 2;

    fill_cmd_id_len(&mut buf, cs::CONNECT_DOMAIN_NAME, id, len);
    buf[9..buf_len - 2].copy_from_slice(domain);

    unsafe {
        let offset = (buf_len - 2) as isize;
        *(buf.as_ptr().offset(offset) as *mut u16) = port.to_be();
    }

    buf
}

pub fn pack_cs_eof(id: u32) -> [u8; 5] {
    pack_cmd_id(cs::EOF, id)
}

pub fn pack_cs_data(id: u32, data: &[u8]) -> Vec<u8> {
    pack_cmd_id_data(cs::DATA, id, data)
}

pub fn pack_cs_entry_close(id: u32) -> [u8; 5] {
    pack_cmd_id(cs::ENTRY_CLOSE, id)
}

pub fn pack_cs_heartbeat() -> [u8; 1] {
    let buf = [cs::HEARTBEAT];
    buf
}

pub fn pack_sc_entry_close(id: u32) -> [u8; 5] {
    pack_cmd_id(sc::ENTRY_CLOSE, id)
}

pub fn pack_sc_shutdown_write_msg(id: u32) -> [u8; 5] {
    pack_cmd_id(sc::EOF, id)
}

pub fn pack_sc_connect_ok_msg(id: u32, data: &[u8]) -> Vec<u8> {
    pack_cmd_id_data(sc::CONNECT_OK, id, data)
}

pub fn pack_sc_data_msg(id: u32, data: &[u8]) -> Vec<u8> {
    pack_cmd_id_data(sc::DATA, id, data)
}

pub fn pack_sc_heartbeat() -> [u8; 1] {
    let buf = [sc::HEARTBEAT];
    buf
}