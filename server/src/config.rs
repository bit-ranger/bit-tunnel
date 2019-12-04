#[derive(Clone)]
pub struct Config{
    key: Vec<u8>,
    listen_address: String
}

impl Config{

    pub fn new(key: Vec<u8>,
               listen_address: String) -> Config
    {
        Config{
            key,
            listen_address
        }
    }

    pub fn get_key(&self) -> &[u8]{
        return self.key.as_slice();
    }

    pub fn get_listen_address(&self) -> &str{
        return self.listen_address.as_str();
    }
}