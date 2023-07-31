mod common;

use std::{collections::VecDeque, future::Future, sync::Arc};

use anyhow::Result;
use futures_util::stream::{SplitSink, SplitStream};

use ppaass_common::{PpaassConnection, PpaassMessage};

use tokio::{
    net::TcpStream,
    sync::{Mutex, Notify},
};

use crate::transport::ControlProtocol;
use crate::{config::PpaassVpnServerConfig, error::RemoteEndpointError, util::AgentRsaCryptoFetcher};

use self::common::{
    close_remote_tcp, close_remote_udp, new_tcp, new_udp, read_from_remote_tcp, read_from_remote_udp,
    write_to_remote_tcp, write_to_remote_udp,
};

use super::{client::ClientEndpoint, TransportId};

type ProxyConnectionWrite =
    SplitSink<PpaassConnection<'static, TcpStream, AgentRsaCryptoFetcher, TransportId>, PpaassMessage>;

type ProxyConnectionRead = SplitStream<PpaassConnection<'static, TcpStream, AgentRsaCryptoFetcher, TransportId>>;

pub(crate) enum RemoteEndpoint {
    Tcp {
        transport_id: TransportId,
        proxy_connection_read: Mutex<ProxyConnectionRead>,
        proxy_connection_write: Mutex<ProxyConnectionWrite>,
        recv_buffer: Arc<Mutex<VecDeque<u8>>>,
        recv_buffer_notify: Arc<Notify>,
        closed: Mutex<bool>,
        config: &'static PpaassVpnServerConfig,
    },
    Udp {
        transport_id: TransportId,
        proxy_connection_read: Mutex<ProxyConnectionRead>,
        proxy_connection_write: Mutex<ProxyConnectionWrite>,
        recv_buffer: Arc<Mutex<VecDeque<Vec<u8>>>>,
        recv_buffer_notify: Arc<Notify>,
        closed: Mutex<bool>,
        config: &'static PpaassVpnServerConfig,
    },
}

impl RemoteEndpoint {
    /// Create new remote endpoint
    pub(crate) async fn new(
        transport_id: TransportId,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<(RemoteEndpoint, Arc<Notify>), RemoteEndpointError> {
        match transport_id.control_protocol {
            ControlProtocol::Tcp => new_tcp(transport_id, agent_rsa_crypto_fetcher, config).await,
            ControlProtocol::Udp => new_udp(transport_id, agent_rsa_crypto_fetcher, config).await,
        }
    }

    pub(crate) async fn read_from_remote(&self) -> Result<bool, RemoteEndpointError> {
        match self {
            Self::Tcp {
                transport_id,
                proxy_connection_read,
                recv_buffer,
                recv_buffer_notify,
                ..
            } => {
                read_from_remote_tcp(
                    *transport_id,
                    proxy_connection_read,
                    recv_buffer_notify,
                    recv_buffer,
                )
                .await
            }
            Self::Udp {
                transport_id,
                proxy_connection_read,
                recv_buffer,
                recv_buffer_notify,
                ..
            } => {
                read_from_remote_udp(
                    *transport_id,
                    proxy_connection_read,
                    recv_buffer_notify,
                    recv_buffer,
                )
                .await
            }
        }
    }

    pub(crate) async fn write_to_remote(&self, data: Vec<u8>) -> Result<usize, RemoteEndpointError> {
        match self {
            Self::Tcp {
                transport_id,
                proxy_connection_write,
                config,
                ..
            } => write_to_remote_tcp(config, data, transport_id, proxy_connection_write).await,
            Self::Udp {
                transport_id,
                proxy_connection_write,
                config,
                ..
            } => write_to_remote_udp(config, data, transport_id, proxy_connection_write).await,
        }
    }

    pub(crate) async fn consume_recv_buffer<'buf, F, Fut>(
        &self,
        remote: Arc<ClientEndpoint<'buf>>,
        mut consume_fn: F,
    ) -> Result<bool>
    where
        F: FnMut(TransportId, Vec<u8>, Arc<ClientEndpoint<'buf>>) -> Fut,
        Fut: Future<Output = Result<usize>>,
    {
        match self {
            Self::Tcp {
                transport_id,
                recv_buffer,
                closed,
                ..
            } => {
                let mut recv_buffer = recv_buffer.lock().await;
                if recv_buffer.len() == 0 {
                    let closed = closed.lock().await;
                    return Ok(*closed);
                }
                let consume_size = consume_fn(
                    *transport_id,
                    recv_buffer.make_contiguous().to_vec(),
                    remote,
                )
                .await?;
                recv_buffer.drain(..consume_size);
                Ok(false)
            }
            Self::Udp {
                transport_id,
                recv_buffer,
                closed,
                ..
            } => {
                let mut consume_size = 0;
                let mut recv_buffer = recv_buffer.lock().await;
                if recv_buffer.len() == 0 {
                    let closed = closed.lock().await;
                    return Ok(*closed);
                }
                for udp_data in recv_buffer.iter() {
                    consume_fn(*transport_id, udp_data.to_vec(), Arc::clone(&remote)).await?;
                    consume_size += 1;
                }
                recv_buffer.drain(..consume_size);
                Ok(false)
            }
        }
    }

    pub(crate) async fn close(&self) {
        match self {
            Self::Tcp {
                transport_id,
                proxy_connection_write,
                recv_buffer_notify,
                closed,
                ..
            } => {
                close_remote_tcp(
                    *transport_id,
                    proxy_connection_write,
                    recv_buffer_notify,
                    closed,
                )
                .await;
            }
            Self::Udp {
                recv_buffer_notify,
                closed,
                ..
            } => {
                close_remote_udp(recv_buffer_notify, closed).await;
            }
        }
    }
}
