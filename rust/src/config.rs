pub(crate) const MTU: usize = 65535;

#[derive(Debug)]
pub(crate) struct PpaassVpnServerConfig {
    user_token: String,
    proxy_address: String,
    thread_number: usize,
    proxy_connection_buffer_size: usize,
    smoltcp_tcp_rx_buffer_size: usize,
    smoltcp_tcp_tx_buffer_size: usize,
    smoltcp_udp_packet_size: usize,
    smoltcp_udp_rx_packet_buffer_number: usize,
    smoltcp_udp_tx_packet_buffer_number: usize,
    client_endpoint_tcp_recv_buffer_size: usize,
    client_endpoint_udp_recv_buffer_size: usize,
    remote_tcp_recv_timeout: u64,
    remote_udp_recv_timeout: u64,
    client_tcp_recv_timeout: u64,
    client_udp_recv_timeout: u64,
}

impl PpaassVpnServerConfig {
    pub(crate) fn new() -> Self {
        Self {
            user_token: "user1".to_string(),
            proxy_address: "64.176.193.76:80".to_string(),
            thread_number: 512,
            proxy_connection_buffer_size: 1024 * 1024,
            smoltcp_tcp_rx_buffer_size: 1024 * 1024,
            smoltcp_tcp_tx_buffer_size: 1024 * 1024,
            smoltcp_udp_packet_size: 65536,
            client_endpoint_tcp_recv_buffer_size: 1024 * 1024,
            client_endpoint_udp_recv_buffer_size: 1024 * 1024,
            smoltcp_udp_rx_packet_buffer_number: 128,
            smoltcp_udp_tx_packet_buffer_number: 128,
            remote_tcp_recv_timeout: 10,
            remote_udp_recv_timeout: 5,
            client_tcp_recv_timeout: 10,
            client_udp_recv_timeout: 5,
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

    pub(crate) fn get_smoltcp_udp_rx_packet_buffer_number(&self) -> usize {
        self.smoltcp_udp_rx_packet_buffer_number
    }

    pub(crate) fn get_smoltcp_udp_tx_packet_buffer_number(&self) -> usize {
        self.smoltcp_udp_tx_packet_buffer_number
    }

    pub(crate) fn get_smoltcp_udp_packet_size(&self) -> usize {
        self.smoltcp_udp_packet_size
    }

    pub(crate) fn get_remote_tcp_recv_timeout(&self) -> u64 {
        self.remote_tcp_recv_timeout
    }

    pub(crate) fn get_remote_udp_recv_timeout(&self) -> u64 {
        self.remote_udp_recv_timeout
    }

    pub(crate) fn get_client_tcp_recv_timeout(&self) -> u64 {
        self.client_tcp_recv_timeout
    }

    pub(crate) fn get_client_udp_recv_timeout(&self) -> u64 {
        self.client_udp_recv_timeout
    }
}
