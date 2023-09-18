use std::net::AddrParseError;

use crate::transport::TransportId;
use ppaass_common::{CommonError, PpaassMessageProxyProtocol};
use smoltcp::{
    iface::RouteTableFull as SmoltcpRouteTableFullError,
    socket::tcp::{
        ListenError as SmoltcpTcpListenError, RecvError as SmoltcpTcpRecvError,
        SendError as SmoltcpTcpSendError,
    },
};

use smoltcp::socket::udp::{
    BindError as SmoltcpUdpBindError, RecvError as SmoltcpUdpRecvError,
    SendError as SmoltcpUdpSendError,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum TransportError {
    #[error("Remote endpoint error happen:{0:?}")]
    RemoteEndpoint(#[from] RemoteEndpointError),
    #[error("Client endpoint error happen:{0:?}")]
    ClientEndpoint(#[from] ClientEndpointError),
}

#[derive(Error, Debug)]
pub(crate) enum RemoteEndpointError {
    #[error("Std io error happen: {0}")]
    StdIo(#[from] std::io::Error),
    #[error("Connect to remote timeout: {0}")]
    ConnectionTimeout(u64),
    #[error("Receive remote data timeout: {0}")]
    ReceiveTimeout(u64),
    #[error("Transport {transport_id} fail to protect remote socket fd [{socket_fd}] because of error: {message}")]
    ProtectRemoteSocket {
        transport_id: TransportId,
        socket_fd: i32,
        message: String,
    },
    #[error("Proxy fail to initialize connection, transport: {0:?}")]
    ProxyFailToInitializeConnection(TransportId),
    #[error("Proxy common error happen: {0:?}")]
    ProxyCommon(#[from] CommonError),
    #[error("Proxy connection exhausted, transport: {0:?}")]
    ProxyExhausted(TransportId),
    #[error("Invalid proxy message payload type: {0:?}")]
    InvalidProxyProtocol(PpaassMessageProxyProtocol),
    #[error("Fail to parse address: {0:?}")]
    AddressParse(#[from] AddrParseError),
    #[error("Other error happen: {0:?}")]
    Other(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
pub(crate) enum ClientEndpointError {
    #[error("Receive client data timeout: {0}")]
    ReceiveTimeout(u64),
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
    #[error("Smoltcp router table error happen: {0:?}")]
    SmoltcpRouterTableError(#[from] SmoltcpRouteTableFullError),
}
