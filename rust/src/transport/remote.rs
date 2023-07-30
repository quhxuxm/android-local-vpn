use std::{collections::VecDeque, future::Future, os::fd::AsRawFd, sync::Arc};

use anyhow::{anyhow, Result};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use log::{debug, error};
use ppaass_common::{
    generate_uuid,
    tcp::{TcpData, TcpInitResponse, TcpInitResponseType},
    udp::UdpData,
    PpaassConnection, PpaassMessage, PpaassMessageGenerator, PpaassMessagePayloadEncryptionSelector, PpaassMessageProxyPayload, PpaassMessageProxyPayloadType,
};

use tokio::{
    net::{TcpSocket, TcpStream},
    sync::{Mutex, Notify},
};

use crate::{protect_socket, util::AgentRsaCryptoFetcher, PROXY_ADDRESS, USER_TOKE};
use crate::{transport::ControlProtocol, util::AgentPpaassMessagePayloadEncryptionSelector};

use super::{client::ClientEndpoint, value::InternetProtocol, TransportId};

type ProxyConnectionWrite = SplitSink<PpaassConnection<'static, TcpStream, AgentRsaCryptoFetcher, TransportId>, PpaassMessage>;

type ProxyConnectionRead = SplitStream<PpaassConnection<'static, TcpStream, AgentRsaCryptoFetcher, TransportId>>;

pub(crate) enum RemoteEndpoint {
    Tcp {
        transport_id: TransportId,
        proxy_connection_read: Mutex<ProxyConnectionRead>,
        proxy_connection_write: Mutex<ProxyConnectionWrite>,
        recv_buffer: Arc<Mutex<VecDeque<u8>>>,
        recv_buffer_notify: Arc<Notify>,
        closed: Mutex<bool>,
    },
    Udp {
        transport_id: TransportId,
        proxy_connection_read: Mutex<ProxyConnectionRead>,
        proxy_connection_write: Mutex<ProxyConnectionWrite>,
        recv_buffer: Arc<Mutex<VecDeque<Vec<u8>>>>,
        recv_buffer_notify: Arc<Notify>,
        closed: Mutex<bool>,
    },
}

impl RemoteEndpoint {
    pub(crate) async fn new(transport_id: TransportId, agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher) -> Result<(RemoteEndpoint, Arc<Notify>)> {
        match transport_id.control_protocol {
            ControlProtocol::Tcp => Self::new_tcp(transport_id, agent_rsa_crypto_fetcher).await,
            ControlProtocol::Udp => Self::new_udp(transport_id, agent_rsa_crypto_fetcher).await,
        }
    }

    async fn new_tcp(transport_id: TransportId, agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher) -> Result<(RemoteEndpoint, Arc<Notify>)> {
        let tcp_socket = match transport_id.internet_protocol {
            InternetProtocol::Ipv4 => TcpSocket::new_v4()?,
            InternetProtocol::Ipv6 => TcpSocket::new_v6()?,
        };
        let raw_socket_fd = tcp_socket.as_raw_fd();
        protect_socket(raw_socket_fd)?;
        let proxy_connection = Self::init_proxy_connection(tcp_socket, transport_id, agent_rsa_crypto_fetcher).await?;
        // let remote_tcp_stream = tcp_socket.connect(transport_id.destination).await?;
        let (proxy_connection_write, proxy_connection_read) = proxy_connection.split();
        let recv_buffer_notify = Arc::new(Notify::new());
        Ok((
            Self::Tcp {
                transport_id,
                proxy_connection_read: Mutex::new(proxy_connection_read),
                proxy_connection_write: Mutex::new(proxy_connection_write),
                recv_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                recv_buffer_notify: Arc::clone(&recv_buffer_notify),
                closed: Mutex::new(false),
            },
            recv_buffer_notify,
        ))
    }

    async fn new_udp(transport_id: TransportId, agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher) -> Result<(RemoteEndpoint, Arc<Notify>)> {
        let tcp_socket = match transport_id.internet_protocol {
            InternetProtocol::Ipv4 => TcpSocket::new_v4()?,
            InternetProtocol::Ipv6 => TcpSocket::new_v6()?,
        };
        let raw_socket_fd = tcp_socket.as_raw_fd();

        protect_socket(raw_socket_fd)?;

        let proxy_connection = Self::init_proxy_connection(tcp_socket, transport_id, agent_rsa_crypto_fetcher).await?;
        let (proxy_connection_write, proxy_connection_read) = proxy_connection.split();

        let recv_buffer_notify = Arc::new(Notify::new());
        Ok((
            Self::Udp {
                transport_id,
                proxy_connection_read: Mutex::new(proxy_connection_read),
                proxy_connection_write: Mutex::new(proxy_connection_write),
                recv_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                recv_buffer_notify: Arc::clone(&recv_buffer_notify),
                closed: Mutex::new(false),
            },
            recv_buffer_notify,
        ))
    }

    pub(crate) async fn init_proxy_connection(
        tcp_socket: TcpSocket,
        transport_id: TransportId,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
    ) -> Result<PpaassConnection<'static, TcpStream, AgentRsaCryptoFetcher, TransportId>> {
        let proxy_tcp_stream = tcp_socket.connect(PROXY_ADDRESS.parse()?).await?;
        let mut proxy_connection = PpaassConnection::new(
            transport_id,
            proxy_tcp_stream,
            agent_rsa_crypto_fetcher,
            true,
            65536,
        );
        let payload_encryption = AgentPpaassMessagePayloadEncryptionSelector::select(USER_TOKE, Some(generate_uuid().into_bytes()));

        if let ControlProtocol::Tcp = transport_id.control_protocol {
            let tcp_init_request = PpaassMessageGenerator::generate_tcp_init_request(
                USER_TOKE,
                transport_id.source.into(),
                transport_id.destination.into(),
                payload_encryption,
            )?;
            proxy_connection.send(tcp_init_request).await?;
            let proxy_message = proxy_connection
                .next()
                .await
                .ok_or(anyhow!("No data response from proxy"))??;
            let PpaassMessage { payload, .. } = proxy_message;
            let PpaassMessageProxyPayload { payload_type, data } = payload.as_slice().try_into()?;
            let tcp_init_response = match payload_type {
                PpaassMessageProxyPayloadType::TcpInit => data.as_slice().try_into()?,
                _ => {
                    error!(">>>> Transport {transport_id} receive invalid message from proxy, payload type: {payload_type:?}");
                    return Err(anyhow!("Invalid proxy response"));
                }
            };
            let TcpInitResponse { response_type, .. } = tcp_init_response;
            match response_type {
                TcpInitResponseType::Success => {
                    debug!(">>>> Transport {transport_id} success to initialize proxy tcp connection.");
                }
                TcpInitResponseType::Fail => {
                    error!(">>>> Transport {transport_id} fail to initialize proxy tcp connection.");
                    return Err(anyhow!("Fail to initialize proxy tcp connection"));
                }
            }
        }

        Ok(proxy_connection)
    }

    pub(crate) async fn read_from_remote(&self) -> Result<bool> {
        match self {
            Self::Tcp {
                transport_id,
                proxy_connection_read,
                recv_buffer,
                recv_buffer_notify,
                ..
            } => {
                let mut proxy_connection_read = proxy_connection_read.lock().await;
                match proxy_connection_read.next().await {
                    None => {
                        recv_buffer_notify.notify_waiters();
                        Ok(true)
                    }
                    Some(Ok(PpaassMessage { payload, .. })) => {
                        let TcpData { data, .. } = payload.as_slice().try_into()?;
                        let mut recv_buffer = recv_buffer.lock().await;
                        debug!(
                            "<<<< Transport {transport_id} read remote tcp data to remote receive buffer: {}",
                            pretty_hex::pretty_hex(&data)
                        );
                        recv_buffer.extend(data);
                        recv_buffer_notify.notify_waiters();
                        Ok(false)
                    }
                    Some(Err(e)) => {
                        error!("<<<< Transport {transport_id} fail to read remote tcp data because of error: {e:?}");
                        Err(anyhow!("{e:?}"))
                    }
                }
            }
            Self::Udp {
                transport_id,
                proxy_connection_read,
                recv_buffer,
                recv_buffer_notify,
                ..
            } => {
                let mut proxy_connection_read = proxy_connection_read.lock().await;
                match proxy_connection_read.next().await {
                    None => {
                        recv_buffer_notify.notify_waiters();
                        Ok(true)
                    }
                    Some(Ok(PpaassMessage { payload, .. })) => {
                        let UdpData { data, .. } = payload.as_slice().try_into()?;
                        let mut recv_buffer = recv_buffer.lock().await;
                        debug!(
                            "<<<< Transport {transport_id} read remote udp data to remote receive buffer: {}",
                            pretty_hex::pretty_hex(&data)
                        );

                        recv_buffer.push_back(data.to_vec());
                        recv_buffer_notify.notify_waiters();
                        Ok(false)
                    }
                    Some(Err(e)) => {
                        error!("<<<< Transport {transport_id} fail to read remote udp data because of error: {e:?}");
                        Err(anyhow!("{e:?}"))
                    }
                }
            }
        }
    }

    pub(crate) async fn write_to_remote(&self, data: Vec<u8>) -> Result<usize> {
        match self {
            Self::Tcp {
                transport_id,
                proxy_connection_write,
                ..
            } => {
                let mut proxy_connection_write = proxy_connection_write.lock().await;
                let payload_encryption = AgentPpaassMessagePayloadEncryptionSelector::select(USER_TOKE, Some(generate_uuid().into_bytes()));
                let data_len = data.len();
                let tcp_data = PpaassMessageGenerator::generate_tcp_data(
                    USER_TOKE,
                    payload_encryption,
                    transport_id.source.into(),
                    transport_id.destination.into(),
                    data,
                )?;
                proxy_connection_write.send(tcp_data).await?;
                Ok(data_len)
            }
            Self::Udp {
                transport_id,
                proxy_connection_write,
                ..
            } => {
                let mut proxy_connection_write = proxy_connection_write.lock().await;
                let payload_encryption = AgentPpaassMessagePayloadEncryptionSelector::select(USER_TOKE, Some(generate_uuid().into_bytes()));
                let data_len = data.len();
                let udp_data = PpaassMessageGenerator::generate_udp_data(
                    USER_TOKE,
                    payload_encryption,
                    transport_id.source.into(),
                    transport_id.destination.into(),
                    data,
                )?;
                proxy_connection_write.send(udp_data).await?;
                Ok(data_len)
            }
        }
    }

    pub(crate) async fn consume_recv_buffer<'buf, F, Fut>(&self, remote: Arc<ClientEndpoint<'buf>>, mut consume_fn: F) -> Result<bool>
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
                let mut proxy_connection_write = proxy_connection_write.lock().await;
                if let Err(e) = proxy_connection_write.close().await {
                    error!(">>>> Transport {transport_id} fail to close remote endpoint because of error: {e:?}")
                };
                recv_buffer_notify.notify_waiters();
                let mut closed = closed.lock().await;
                *closed = true;
            }
            Self::Udp {
                recv_buffer_notify,
                closed,
                ..
            } => {
                recv_buffer_notify.notify_waiters();
                let mut closed = closed.lock().await;
                *closed = true;
            }
        }
    }
}
