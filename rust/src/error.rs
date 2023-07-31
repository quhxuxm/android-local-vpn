use thiserror::Error;

use crate::transport::TransportId;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("Transport {transport_id} fail to protect remote socket fd [{socket_fd}] because of error: {message}")]
    ProtectRemoteSocket {
        transport_id: TransportId,
        socket_fd: i32,
        message: String,
    },
}
