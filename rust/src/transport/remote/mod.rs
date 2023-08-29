mod common;

use std::{
    collections::VecDeque,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::Result;
use futures_util::stream::{SplitSink, SplitStream};

use tokio::sync::RwLock;
use tokio::{
    net::TcpStream,
    sync::{Mutex, Notify},
};

use crate::{
    config::PpaassVpnServerConfig, error::RemoteEndpointError,
    util::AgentRsaCryptoFetcher,
};
use crate::{error::ClientEndpointError, transport::ControlProtocol};

use self::common::{
    close_remote_tcp, close_remote_udp, new_tcp, new_udp, read_from_remote_tcp,
    read_from_remote_udp, write_to_remote_tcp, write_to_remote_udp,
};
use super::{client::ClientEndpoint, TransportId};
use ppaass_common::{proxy::PpaassProxyConnection, PpaassAgentMessage};

type ProxyConnectionWrite = SplitSink<
    PpaassProxyConnection<
        'static,
        TcpStream,
        AgentRsaCryptoFetcher,
        TransportId,
    >,
    PpaassAgentMessage,
>;

type ProxyConnectionRead = SplitStream<
    PpaassProxyConnection<
        'static,
        TcpStream,
        AgentRsaCryptoFetcher,
        TransportId,
    >,
>;

pub(crate) type RemoteTcpRecvBuf = (RwLock<VecDeque<u8>>, Notify);
pub(crate) type RemoteUdpRecvBuf = (RwLock<VecDeque<Vec<u8>>>, Notify);

pub(crate) enum RemoteEndpoint {
    Tcp {
        transport_id: TransportId,
        proxy_connection_read: Mutex<ProxyConnectionRead>,
        proxy_connection_write: Mutex<ProxyConnectionWrite>,
        recv_buffer: Arc<RemoteTcpRecvBuf>,
        config: &'static PpaassVpnServerConfig,
        no_more_remote_data: AtomicBool,
    },
    Udp {
        transport_id: TransportId,
        proxy_connection_read: Mutex<ProxyConnectionRead>,
        proxy_connection_write: Mutex<ProxyConnectionWrite>,
        recv_buffer: Arc<RemoteUdpRecvBuf>,
        config: &'static PpaassVpnServerConfig,
        no_more_remote_data: AtomicBool,
    },
}

impl RemoteEndpoint {
    /// Create new remote endpoint
    pub(crate) async fn new(
        transport_id: TransportId,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<RemoteEndpoint, RemoteEndpointError> {
        match transport_id.control_protocol {
            ControlProtocol::Tcp => {
                new_tcp(transport_id, agent_rsa_crypto_fetcher, config).await
            }
            ControlProtocol::Udp => {
                new_udp(transport_id, agent_rsa_crypto_fetcher, config).await
            }
        }
    }

    pub(crate) fn is_no_more_remote_data(&self) -> bool {
        match self {
            RemoteEndpoint::Tcp {
                no_more_remote_data,
                ..
            } => no_more_remote_data.load(Ordering::Relaxed),
            RemoteEndpoint::Udp {
                no_more_remote_data,
                ..
            } => no_more_remote_data.load(Ordering::Relaxed),
        }
    }

    pub(crate) async fn read_from_remote(
        &self,
    ) -> Result<(), RemoteEndpointError> {
        match self {
            Self::Tcp {
                transport_id,
                proxy_connection_read,
                recv_buffer,
                no_more_remote_data,
                ..
            } => {
                read_from_remote_tcp(
                    *transport_id,
                    proxy_connection_read,
                    recv_buffer,
                    no_more_remote_data,
                )
                .await
            }
            Self::Udp {
                transport_id,
                proxy_connection_read,
                recv_buffer,
                no_more_remote_data,
                ..
            } => {
                read_from_remote_udp(
                    *transport_id,
                    proxy_connection_read,
                    recv_buffer,
                    no_more_remote_data,
                )
                .await
            }
        }
    }

    pub(crate) async fn write_to_remote(
        &self,
        data: Vec<u8>,
    ) -> Result<usize, RemoteEndpointError> {
        match self {
            Self::Tcp {
                transport_id,
                proxy_connection_write,
                config,
                ..
            } => {
                write_to_remote_tcp(
                    config,
                    data,
                    transport_id,
                    proxy_connection_write,
                )
                .await
            }
            Self::Udp {
                transport_id,
                proxy_connection_write,
                config,
                ..
            } => {
                write_to_remote_udp(
                    config,
                    data,
                    transport_id,
                    proxy_connection_write,
                )
                .await
            }
        }
    }

    pub(crate) async fn consume_recv_buffer<'buf, F, Fut>(
        &self,
        remote: Arc<ClientEndpoint<'buf>>,
        mut consume_fn: F,
    ) -> Result<(), ClientEndpointError>
    where
        F: FnMut(TransportId, Vec<u8>, Arc<ClientEndpoint<'buf>>) -> Fut,
        Fut: Future<Output = Result<usize, ClientEndpointError>>,
    {
        match self {
            Self::Tcp {
                transport_id,
                recv_buffer,
                ..
            } => {
                if recv_buffer.0.read().await.len() == 0 {
                    return Ok(());
                }
                let mut recv_buffer = recv_buffer.0.write().await;
                let consume_size = consume_fn(
                    *transport_id,
                    recv_buffer.make_contiguous().to_vec(),
                    remote,
                )
                .await?;
                recv_buffer.drain(..consume_size);
                Ok(())
            }
            Self::Udp {
                transport_id,
                recv_buffer,
                ..
            } => {
                if recv_buffer.0.read().await.len() == 0 {
                    return Ok(());
                }
                let mut recv_buffer = recv_buffer.0.write().await;
                let mut consume_size = 0;
                for udp_data in recv_buffer.iter() {
                    consume_fn(
                        *transport_id,
                        udp_data.to_vec(),
                        Arc::clone(&remote),
                    )
                    .await?;
                    consume_size += 1;
                }
                recv_buffer.drain(..consume_size);
                Ok(())
            }
        }
    }

    pub(crate) async fn close(&self) {
        match self {
            Self::Tcp {
                transport_id,
                proxy_connection_write,
                recv_buffer,
                ..
            } => {
                recv_buffer.1.notify_waiters();
                close_remote_tcp(*transport_id, proxy_connection_write).await;
            }
            Self::Udp { recv_buffer, .. } => {
                recv_buffer.1.notify_waiters();
                close_remote_udp().await;
            }
        }
    }

    pub(crate) async fn awaiting_recv_buf(&self) {
        match self {
            RemoteEndpoint::Tcp { recv_buffer, .. } => {
                recv_buffer.1.notified().await
            }
            RemoteEndpoint::Udp { recv_buffer, .. } => {
                recv_buffer.1.notified().await
            }
        }
    }
}
