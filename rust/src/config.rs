#[derive(Debug)]
pub(crate) struct PpaassVpnServerConfig {
    user_token: String,
    proxy_address: String,
}

impl PpaassVpnServerConfig {
    pub(crate) fn new() -> Self {
        Self {
            user_token: "user1".to_string(),
            proxy_address: "64.176.193.76:80".to_string(),
        }
    }

    pub(crate) fn get_user_token(&self) -> &str {
        &self.user_token
    }

    pub(crate) fn get_proxy_address(&self) -> &str {
        &self.proxy_address
    }
}
