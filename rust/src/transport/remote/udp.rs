use std::{
    collections::VecDeque, future::Future, os::fd::AsRawFd, sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, Result};

use futures_util::{SinkExt, StreamExt};

use log::{error, trace};

use tokio::{net::TcpSocket, sync::RwLock};
use tokio::{net::TcpStream, sync::Mutex};

use crate::error::ClientEndpointError;
use crate::{
    config::PpaassVpnServerConfig,
    error::RemoteEndpointError,
    transport::{client::ClientUdpEndpoint, InternetProtocol},
    util::{
        protect_socket, AgentPpaassMessagePayloadEncryptionSelector,
        AgentRsaCryptoFetcher,
    },
};

use super::{ProxyConnectionRead, ProxyConnectionWrite, TransportId};
use ppaass_common::{
    generate_uuid, proxy::PpaassProxyConnection, udp::UdpData,
    PpaassMessageGenerator, PpaassMessagePayloadEncryptionSelector,
    PpaassProxyMessage,
};

type RemoteUdpRecvBuf = RwLock<VecDeque<Vec<u8>>>;

pub(crate) struct RemoteUdpEndpoint {
    transport_id: TransportId,
    proxy_connection_read: Mutex<ProxyConnectionRead>,
    proxy_connection_write: Mutex<ProxyConnectionWrite>,
    recv_buffer: Arc<RemoteUdpRecvBuf>,
    config: &'static PpaassVpnServerConfig,
}

impl RemoteUdpEndpoint {
    /// Create new remote endpoint
    pub(crate) async fn new(
        transport_id: TransportId,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<Self, RemoteEndpointError> {
        let tcp_socket = match transport_id.internet_protocol {
            InternetProtocol::Ipv4 => TcpSocket::new_v4()?,
            InternetProtocol::Ipv6 => TcpSocket::new_v6()?,
        };
        let raw_socket_fd = tcp_socket.as_raw_fd();
        protect_socket(transport_id, raw_socket_fd)?;
        let proxy_connection = Self::init_proxy_connection(
            tcp_socket,
            transport_id,
            agent_rsa_crypto_fetcher,
            config,
        )
        .await?;
        let (proxy_connection_write, proxy_connection_read) =
            proxy_connection.split();
        Ok(Self {
            transport_id,
            proxy_connection_read: Mutex::new(proxy_connection_read),
            proxy_connection_write: Mutex::new(proxy_connection_write),
            recv_buffer: Arc::new(RwLock::new(VecDeque::with_capacity(65536))),
            config,
        })
    }

    /// Initialize proxy connection
    async fn init_proxy_connection(
        tcp_socket: TcpSocket,
        transport_id: TransportId,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<
        PpaassProxyConnection<
            'static,
            TcpStream,
            AgentRsaCryptoFetcher,
            TransportId,
        >,
        RemoteEndpointError,
    > {
        let proxy_addresses = config.get_proxy_address();
        let proxy_tcp_stream = tokio::time::timeout(
        Duration::from_secs(10),
        tcp_socket.connect(proxy_addresses.parse().map_err(|e| {
            anyhow!("Faill to parse proxy address because of error: {e:?}")
        })?),
    )
    .await
    .map_err(|_| {
        error!(">>>> Transport {transport_id} connect to proxy timeout in 10 seconds.");
        RemoteEndpointError::ConnectionTimeout(10)
    })??;
        let proxy_connection = PpaassProxyConnection::new(
            transport_id,
            proxy_tcp_stream,
            agent_rsa_crypto_fetcher,
            true,
            config.get_proxy_connection_buffer_size(),
        );
        Ok(proxy_connection)
    }

    pub(crate) async fn read_from_remote(
        &self,
    ) -> Result<bool, RemoteEndpointError> {
        match self.proxy_connection_read.lock().await.next().await {
            None => Ok(true),
            Some(Ok(PpaassProxyMessage {
                payload: proxy_message_payload,
                ..
            })) => {
                let UdpData {
                    data: udp_relay_data,
                    ..
                } = proxy_message_payload.data.try_into()?;
                trace!(
                "<<<< Transport {}, [UDP PROCESS] read remote udp data to remote receive buffer: {}",
                self.transport_id,
                pretty_hex::pretty_hex(&udp_relay_data)
            );

                self.recv_buffer
                    .write()
                    .await
                    .push_back(udp_relay_data.to_vec());
                Ok(false)
            }
            Some(Err(e)) => {
                error!("<<<< Transport {} fail to read remote udp data because of error: {e:?}", self.transport_id);
                Err(e.into())
            }
        }
    }

    pub(crate) async fn write_to_remote(
        &self,
        data: Vec<u8>,
    ) -> Result<usize, RemoteEndpointError> {
        let payload_encryption =
            AgentPpaassMessagePayloadEncryptionSelector::select(
                self.config.get_user_token(),
                Some(generate_uuid().into_bytes()),
            );
        trace!(
            ">>>> Transport {}, [UDP PROCESS] send udp data to remote: {}",
            self.transport_id,
            pretty_hex::pretty_hex(&data)
        );

        let data_len = data.len();
        let udp_data = PpaassMessageGenerator::generate_agent_udp_data_message(
            self.config.get_user_token(),
            payload_encryption,
            self.transport_id.source.into(),
            self.transport_id.destination.into(),
            data,
        )?;
        self.proxy_connection_write
            .lock()
            .await
            .send(udp_data)
            .await?;
        Ok(data_len)
    }

    pub(crate) async fn consume_recv_buffer<'c, 'buf, F, Fut>(
        &self,
        remote: &'c ClientUdpEndpoint<'buf>,
        mut consume_fn: F,
    ) -> Result<(), ClientEndpointError>
    where
        F: FnMut(TransportId, Vec<u8>, &'c ClientUdpEndpoint<'buf>) -> Fut,
        Fut: Future<Output = Result<usize, ClientEndpointError>>,
    {
        if self.recv_buffer.read().await.len() == 0 {
            return Ok(());
        }
        let mut recv_buffer = self.recv_buffer.write().await;
        let mut consume_size = 0;
        for udp_data in recv_buffer.iter() {
            consume_fn(self.transport_id, udp_data.to_vec(), remote).await?;
            consume_size += 1;
        }
        recv_buffer.drain(..consume_size);
        Ok(())
    }

    pub(crate) async fn close(&self) {
        let mut proxy_connection_write =
            self.proxy_connection_write.lock().await;
        if let Err(e) = proxy_connection_write.close().await {
            error!(">>>> Transport {} fail to close udp remote endpoint because of error: {e:?}", self.transport_id)
        };
    }
}
