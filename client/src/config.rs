#[derive(Clone)]
pub struct Config{
    key: Vec<u8>,
    listen_address: String,
    server_address: String,
    tunnel_max: u32
}

impl Config{

    pub fn new(key: Vec<u8>,
               listen_address: String,
               server_address: String,
               tunnel_max: u32) -> Config
    {
        Config{
            key,
            listen_address,
            server_address,
            tunnel_max
        }
    }

    pub fn get_key(&self) -> &[u8]{
        return self.key.as_slice();
    }

    pub fn get_listen_address(&self) -> &str{
        return self.listen_address.as_str();
    }

    pub fn get_server_address(&self) -> &str{
        return self.server_address.as_str();
    }

    pub fn get_tunnel_max(&self) -> u32{
        return self.tunnel_max;
    }

}