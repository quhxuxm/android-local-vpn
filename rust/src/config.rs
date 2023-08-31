#[derive(Debug)]
pub(crate) struct PpaassVpnServerConfig {
    user_token: String,
    proxy_address: String,
    thread_number: usize,
    proxy_connection_buffer_size: usize,
    smoltcp_tcp_rx_buffer_size: usize,
    smoltcp_tcp_tx_buffer_size: usize,
    client_endpoint_tcp_recv_buffer_size: usize,
    client_endpoint_udp_recv_buffer_size: usize,
}

impl PpaassVpnServerConfig {
    pub(crate) fn new() -> Self {
        Self {
            user_token: "user1".to_string(),
            proxy_address: "64.176.193.76:80".to_string(),
            thread_number: 256,
            proxy_connection_buffer_size: 65536,
            smoltcp_tcp_rx_buffer_size: 65536,
            smoltcp_tcp_tx_buffer_size: 65536,
            client_endpoint_tcp_recv_buffer_size: 65536,
            client_endpoint_udp_recv_buffer_size: 32,
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

    pub(crate) fn get_smoltcp_tcp_rx_buffer_size(&self) -> usize {
        self.smoltcp_tcp_rx_buffer_size
    }

    pub(crate) fn get_smoltcp_tcp_tx_buffer_size(&self) -> usize {
        self.smoltcp_tcp_tx_buffer_size
    }

    pub(crate) fn get_client_endpoint_tcp_recv_buffer_size(&self) -> usize {
        self.client_endpoint_tcp_recv_buffer_size
    }
    pub(crate) fn get_client_endpoint_udp_recv_buffer_size(&self) -> usize {
        self.client_endpoint_udp_recv_buffer_size
    }
}
