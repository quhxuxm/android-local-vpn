use std::{future::Future, os::fd::AsRawFd, sync::Arc, time::Duration};

use bytes::{Buf, Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};

use log::{debug, error, trace};
use tokio::{net::TcpSocket, sync::RwLock, time::timeout};
use tokio::{net::TcpStream, sync::Mutex};

use crate::{
    config::PpaassVpnServerConfig,
    error::RemoteEndpointError,
    transport::InternetProtocol,
    util::{
        protect_socket, AgentPpaassMessagePayloadEncryptionSelector,
        AgentRsaCryptoFetcher,
    },
};
use crate::{error::ClientEndpointError, transport::client::ClientTcpEndpoint};

use super::{ProxyConnectionRead, ProxyConnectionWrite, TransportId};
use ppaass_common::{
    generate_uuid,
    proxy::PpaassProxyConnection,
    tcp::{ProxyTcpData, ProxyTcpInit, ProxyTcpInitResultType},
    PpaassMessageGenerator, PpaassMessagePayloadEncryptionSelector,
    PpaassMessageProxyProtocol, PpaassMessageProxyTcpPayloadType,
    PpaassProxyMessage, PpaassProxyMessagePayload,
};

type RemoteTcpRecvBuf = RwLock<BytesMut>;

pub(crate) struct RemoteTcpEndpoint {
    transport_id: TransportId,
    proxy_connection_read: Mutex<ProxyConnectionRead>,
    proxy_connection_write: Mutex<ProxyConnectionWrite>,
    recv_buffer: Arc<RemoteTcpRecvBuf>,
    config: &'static PpaassVpnServerConfig,
}

impl RemoteTcpEndpoint {
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
            recv_buffer: Arc::new(RwLock::new(BytesMut::with_capacity(65536))),
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
        let proxy_tcp_stream = timeout(
        Duration::from_secs(10),
        tcp_socket.connect(proxy_addresses.parse()?),
    )
    .await
    .map_err(|_| {
        error!(">>>> Transport {transport_id} connect to proxy timeout in 10 seconds.");
        RemoteEndpointError::ConnectionTimeout(10)
    })??;
        let mut proxy_connection = PpaassProxyConnection::new(
            transport_id,
            proxy_tcp_stream,
            agent_rsa_crypto_fetcher,
            true,
            config.get_proxy_connection_buffer_size(),
        );
        let payload_encryption =
            AgentPpaassMessagePayloadEncryptionSelector::select(
                config.get_user_token(),
                Some(Bytes::from(generate_uuid().into_bytes())),
            );

        let tcp_init_request =
            PpaassMessageGenerator::generate_agent_tcp_init_message(
                config.get_user_token(),
                transport_id.source.into(),
                transport_id.destination.into(),
                payload_encryption,
            )?;
        proxy_connection.send(tcp_init_request).await?;
        let PpaassProxyMessage {
            payload: PpaassProxyMessagePayload { protocol, data },
            ..
        } = proxy_connection
            .next()
            .await
            .ok_or(RemoteEndpointError::ProxyExhausted(transport_id))??;

        let tcp_init_response = match protocol {
            PpaassMessageProxyProtocol::Tcp(
                PpaassMessageProxyTcpPayloadType::Init,
            ) => data.try_into()?,
            other_protocol => {
                error!(">>>> Transport {transport_id} receive invalid message from proxy, payload type: {other_protocol:?}");
                return Err(RemoteEndpointError::InvalidProxyProtocol(
                    other_protocol,
                ));
            }
        };
        let ProxyTcpInit { result_type, .. } = tcp_init_response;
        match result_type {
            ProxyTcpInitResultType::Success => {
                debug!(">>>> Transport {transport_id} success to initialize proxy tcp connection.");
            }
            ProxyTcpInitResultType::Fail => {
                error!(">>>> Transport {transport_id} fail to initialize proxy tcp connection.");
                return Err(
                    RemoteEndpointError::ProxyFailToInitializeConnection(
                        transport_id,
                    ),
                );
            }
        }

        Ok(proxy_connection)
    }

    pub(crate) async fn read_from_remote(
        &self,
    ) -> Result<bool, RemoteEndpointError> {
        match self.proxy_connection_read.lock().await.next().await {
            None => Ok(true),
            Some(Ok(proxy_message)) => {
                let PpaassProxyMessage { payload, .. } = proxy_message;
                let ProxyTcpData {
                    data: tcp_relay_data,
                    ..
                } = payload.data.try_into()?;
                trace!(
                "<<<< Transport {} read remote tcp data to remote endpoint receive buffer: {}", self.transport_id, pretty_hex::pretty_hex(&tcp_relay_data));
                self.recv_buffer.write().await.extend(tcp_relay_data);
                Ok(false)
            }
            Some(Err(e)) => {
                error!("<<<< Transport {} fail to read remote tcp data because of error: {e:?}", self.transport_id);
                Err(e.into())
            }
        }
    }

    pub(crate) async fn write_to_remote(
        &self,
        data: Bytes,
    ) -> Result<usize, RemoteEndpointError> {
        let payload_encryption =
            AgentPpaassMessagePayloadEncryptionSelector::select(
                self.config.get_user_token(),
                Some(Bytes::from(generate_uuid().into_bytes())),
            );
        let data_len = data.len();
        let tcp_data = PpaassMessageGenerator::generate_agent_tcp_data_message(
            self.config.get_user_token(),
            payload_encryption,
            self.transport_id.source.into(),
            self.transport_id.destination.into(),
            data,
        )?;
        self.proxy_connection_write
            .lock()
            .await
            .send(tcp_data)
            .await?;
        Ok(data_len)
    }

    pub(crate) async fn consume_recv_buffer<'c, 'buf, F, Fut>(
        &self,
        remote: &'c ClientTcpEndpoint<'buf>,
        mut consume_fn: F,
    ) -> Result<(), ClientEndpointError>
    where
        F: FnMut(TransportId, Bytes, &'c ClientTcpEndpoint<'buf>) -> Fut,
        Fut: Future<Output = Result<usize, ClientEndpointError>>,
    {
        if self.recv_buffer.read().await.is_empty() {
            return Ok(());
        }
        let mut recv_buffer = self.recv_buffer.write().await;
        let recv_buffer_clone = recv_buffer.clone();
        let consume_size =
            consume_fn(self.transport_id, recv_buffer_clone.freeze(), remote)
                .await?;
        recv_buffer.advance(consume_size);
        Ok(())
    }

    pub(crate) async fn close(&self) {
        let mut proxy_connection_write =
            self.proxy_connection_write.lock().await;
        if let Err(e) = proxy_connection_write.close().await {
            error!(">>>> Transport {} fail to close tcp remote endpoint because of error: {e:?}", self.transport_id);
        };
    }
}
