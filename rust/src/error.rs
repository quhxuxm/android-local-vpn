use crate::transport::TransportId;
use anyhow::Error as AnyhowError;
use ppaass_common::CommonError;
use smoltcp::socket::tcp::{
    ListenError as SmoltcpTcpListenError, RecvError as SmoltcpTcpRecvError,
    SendError as SmoltcpTcpSendError,
};
use smoltcp::socket::udp::{
    BindError as SmoltcpUdpBindError, RecvError as SmoltcpUdpRecvError,
    SendError as SmoltcpUdpSendError,
};
use std::io::Error as StdIoError;
use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum AgentError {
    #[error("Remote endpoint error happen:{0:?}")]
    RemoteEndpoint(#[from] RemoteEndpointError),
    #[error("Client endpoint error happen:{0:?}")]
    ClientEndpoint(#[from] ClientEndpointError),
}

#[derive(Error, Debug)]
pub(crate) enum RemoteEndpointError {
    #[error("I/O error happen: {0:?}")]
    Io(#[from] StdIoError),
    #[error("Connect to remote timeout: {0}")]
    ConnectionTimeout(u64),
    #[error("Transport {transport_id} fail to protect remote socket fd [{socket_fd}] because of error: {message}")]
    ProtectRemoteSocket {
        transport_id: TransportId,
        socket_fd: i32,
        message: String,
    },
    #[error("Proxy common error happen: {0:?}")]
    ProxyCommon(#[from] CommonError),
    #[error("Other error happen: {0:?}")]
    Other(#[from] AnyhowError),
}

#[derive(Error, Debug)]
pub(crate) enum ClientEndpointError {
    #[error("I/O error happen: {0:?}")]
    Io(#[from] StdIoError),
    #[error("Smoltcp tcp listen error happen: {0:?}")]
    SmoltcpTcpListenError(#[from] SmoltcpTcpListenError),
    #[error("Smoltcp tcp receive error happen: {0:?}")]
    SmoltcpTcpReceiveError(#[from] SmoltcpTcpRecvError),
    #[error("Smoltcp tcp send error happen: {0:?}")]
    SmoltcpTcpSendError(#[from] SmoltcpTcpSendError),
    #[error("Smoltcp udp bind error happen: {0:?}")]
    SmoltcpUdpBindError(#[from] SmoltcpUdpBindError),
    #[error("Smoltcp udp receive error happen: {0:?}")]
    SmoltcpUdpReceiveError(#[from] SmoltcpUdpRecvError),
    #[error("Smoltcp udp send error happen: {0:?}")]
    SmoltcpUdpSendError(#[from] SmoltcpUdpSendError),
    #[error("Other error happen: {0:?}")]
    Other(#[from] AnyhowError),
}
