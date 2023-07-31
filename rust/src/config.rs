#[derive(Debug)]
pub(crate) struct PpaassVpnServerConfig {
    user_token: String,
    proxy_address: String,
    thread_number: usize,
    proxy_connection_buffer_size: usize,
}

impl PpaassVpnServerConfig {
    pub(crate) fn new() -> Self {
        Self {
            user_token: "user1".to_string(),
            proxy_address: "64.176.193.76:80".to_string(),
            thread_number: 32,
            proxy_connection_buffer_size: 65536,
        }
    }

    pub(crate) fn get_user_token(&self) -> &str {
        &self.user_token
    }

    pub(crate) fn get_proxy_address(&self) -> &str {
        &self.proxy_address
    }

    pub(crate) fn get_thread_number(&self) -> usize {
        self.thread_number
    }

    pub(crate) fn get_proxy_connection_buffer_size(&self) -> usize {
        self.proxy_connection_buffer_size
    }
}
