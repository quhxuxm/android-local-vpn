use std::{collections::VecDeque, os::fd::AsRawFd, sync::Arc, time::Duration};

use crate::{
    config::PpaassVpnServerConfig,
    error::RemoteEndpointError,
    transport::{value::InternetProtocol, ControlProtocol, TransportId},
    util::{
        protect_socket, AgentPpaassMessagePayloadEncryptionSelector,
        AgentRsaCryptoFetcher,
    },
};
use anyhow::anyhow;
use dns_parser::Packet as DnsPacket;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, trace};
use ppaass_common::{
    generate_uuid,
    proxy::PpaassProxyConnection,
    tcp::{ProxyTcpData, ProxyTcpInit, ProxyTcpInitResultType},
    udp::UdpData,
    PpaassMessageGenerator, PpaassMessagePayloadEncryptionSelector,
    PpaassMessageProxyPayloadType, PpaassProxyMessage,
    PpaassProxyMessagePayload,
};

use tokio::sync::RwLock;
use tokio::{
    net::{TcpSocket, TcpStream},
    sync::Mutex,
};

use super::{
    ProxyConnectionRead, ProxyConnectionWrite, RemoteEndpoint,
    RemoteTcpRecvBuf, RemoteUdpRecvBuf,
};

pub(crate) async fn write_to_remote_udp(
    config: &PpaassVpnServerConfig,
    data: Vec<u8>,
    transport_id: &TransportId,
    proxy_connection_write: &Mutex<ProxyConnectionWrite>,
) -> Result<usize, RemoteEndpointError> {
    let payload_encryption =
        AgentPpaassMessagePayloadEncryptionSelector::select(
            config.get_user_token(),
            Some(generate_uuid().into_bytes()),
        );
    trace!(
        ">>>> Transport {transport_id}, [UDP PROCESS] send udp data to remote: {}",
        pretty_hex::pretty_hex(&data)
    );
    if let Ok(dns_request) = DnsPacket::parse(&data) {
        trace!(">>>> Transport {transport_id}, [UDP PROCESS] read dns request from client: {dns_request:?}")
    };

    let data_len = data.len();
    let udp_data = PpaassMessageGenerator::generate_agent_udp_data_message(
        config.get_user_token(),
        payload_encryption,
        transport_id.source.into(),
        transport_id.destination.into(),
        data,
    )?;
    proxy_connection_write.lock().await.send(udp_data).await?;
    Ok(data_len)
}

pub(crate) async fn write_to_remote_tcp(
    config: &PpaassVpnServerConfig,
    data: Vec<u8>,
    transport_id: &TransportId,
    proxy_connection_write: &Mutex<ProxyConnectionWrite>,
) -> Result<usize, RemoteEndpointError> {
    let payload_encryption =
        AgentPpaassMessagePayloadEncryptionSelector::select(
            config.get_user_token(),
            Some(generate_uuid().into_bytes()),
        );
    let data_len = data.len();
    let tcp_data = PpaassMessageGenerator::generate_agent_tcp_data_message(
        config.get_user_token(),
        payload_encryption,
        transport_id.source.into(),
        transport_id.destination.into(),
        data,
    )?;
    proxy_connection_write.lock().await.send(tcp_data).await?;
    Ok(data_len)
}

pub(crate) async fn read_from_remote_udp(
    transport_id: TransportId,
    proxy_connection_read: &Mutex<ProxyConnectionRead>,
    recv_buffer: &RemoteUdpRecvBuf,
) -> Result<bool, RemoteEndpointError> {
    match proxy_connection_read.lock().await.next().await {
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
                "<<<< Transport {transport_id}, [UDP PROCESS] read remote udp data to remote receive buffer: {}",
                pretty_hex::pretty_hex(&udp_relay_data)
            );
            if let Ok(dns_response) = DnsPacket::parse(&udp_relay_data) {
                trace!("<<<< Transport {transport_id}, [UDP PROCESS] read dns response from remote: {dns_response:?}")
            };
            recv_buffer.write().await.push_back(udp_relay_data.to_vec());
            Ok(false)
        }
        Some(Err(e)) => {
            error!("<<<< Transport {transport_id} fail to read remote udp data because of error: {e:?}");
            Err(e.into())
        }
    }
}

pub(crate) async fn read_from_remote_tcp(
    transport_id: TransportId,
    proxy_connection_read: &Mutex<ProxyConnectionRead>,
    recv_buffer: &RemoteTcpRecvBuf,
) -> Result<bool, RemoteEndpointError> {
    match proxy_connection_read.lock().await.next().await {
        None => Ok(true),
        Some(Ok(PpaassProxyMessage {
            payload: proxy_message_payload,
            ..
        })) => {
            let ProxyTcpData {
                data: tcp_relay_data,
                ..
            } = proxy_message_payload.data.as_slice().try_into()?;
            trace!(
                "<<<< Transport {transport_id} read remote tcp data to remote endpoint receive buffer: {}",
                pretty_hex::pretty_hex(&tcp_relay_data)
            );
            recv_buffer.write().await.extend(tcp_relay_data);

            Ok(false)
        }
        Some(Err(e)) => {
            error!("<<<< Transport {transport_id} fail to read remote tcp data because of error: {e:?}");

            Err(e.into())
        }
    }
}

/// Create new tcp remote endpoint
pub(crate) async fn new_tcp(
    transport_id: TransportId,
    agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
    config: &'static PpaassVpnServerConfig,
) -> Result<RemoteEndpoint, RemoteEndpointError> {
    let tcp_socket = match transport_id.internet_protocol {
        InternetProtocol::Ipv4 => TcpSocket::new_v4()?,
        InternetProtocol::Ipv6 => TcpSocket::new_v6()?,
    };
    let raw_socket_fd = tcp_socket.as_raw_fd();
    protect_socket(transport_id, raw_socket_fd)?;
    let proxy_connection = init_proxy_connection(
        tcp_socket,
        transport_id,
        agent_rsa_crypto_fetcher,
        config,
    )
    .await?;
    let (proxy_connection_write, proxy_connection_read) =
        proxy_connection.split();
    Ok(RemoteEndpoint::Tcp {
        transport_id,
        proxy_connection_read: Mutex::new(proxy_connection_read),
        proxy_connection_write: Mutex::new(proxy_connection_write),
        recv_buffer: Arc::new(RwLock::new(VecDeque::with_capacity(65536))),
        config,
    })
}

/// Create new udp remote endpoint
pub(crate) async fn new_udp(
    transport_id: TransportId,
    agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
    config: &'static PpaassVpnServerConfig,
) -> Result<RemoteEndpoint, RemoteEndpointError> {
    let tcp_socket = match transport_id.internet_protocol {
        InternetProtocol::Ipv4 => TcpSocket::new_v4()?,
        InternetProtocol::Ipv6 => TcpSocket::new_v6()?,
    };
    let raw_socket_fd = tcp_socket.as_raw_fd();
    protect_socket(transport_id, raw_socket_fd)?;
    let proxy_connection = init_proxy_connection(
        tcp_socket,
        transport_id,
        agent_rsa_crypto_fetcher,
        config,
    )
    .await?;
    let (proxy_connection_write, proxy_connection_read) =
        proxy_connection.split();
    Ok(RemoteEndpoint::Udp {
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
            Some(generate_uuid().into_bytes()),
        );

    if let ControlProtocol::Tcp = transport_id.control_protocol {
        let tcp_init_request =
            PpaassMessageGenerator::generate_agent_tcp_init_message(
                config.get_user_token(),
                transport_id.source.into(),
                transport_id.destination.into(),
                payload_encryption,
            )?;
        proxy_connection.send(tcp_init_request).await?;
        let PpaassProxyMessage {
            payload:
                PpaassProxyMessagePayload {
                    payload_type: proxy_msg_payload_type,
                    data: proxy_msg_payload_data,
                },
            ..
        } = proxy_connection.next().await.ok_or(anyhow!(
            "Transport {transport_id} nothing read from proxy."
        ))??;

        let tcp_init_response = match proxy_msg_payload_type {
            PpaassMessageProxyPayloadType::TcpInit => {
                proxy_msg_payload_data.as_slice().try_into()?
            }
            _ => {
                error!(">>>> Transport {transport_id} receive invalid message from proxy, payload type: {proxy_msg_payload_type:?}");
                return Err(anyhow!(
                    "Invalid proxy message payload type: {proxy_msg_payload_type:?}"
                )
                .into());
            }
        };
        let ProxyTcpInit {
            result_type: tcp_init_response_type,
            ..
        } = tcp_init_response;
        match tcp_init_response_type {
            ProxyTcpInitResultType::Success => {
                debug!(">>>> Transport {transport_id} success to initialize proxy tcp connection.");
            }
            ProxyTcpInitResultType::Fail => {
                error!(">>>> Transport {transport_id} fail to initialize proxy tcp connection.");
                return Err(
                    anyhow!("Fail to initialize proxy tcp connection").into()
                );
            }
        }
    }

    Ok(proxy_connection)
}

pub(crate) async fn close_remote_tcp(
    transport_id: TransportId,
    proxy_connection_write: &Mutex<ProxyConnectionWrite>,
) {
    let mut proxy_connection_write = proxy_connection_write.lock().await;
    if let Err(e) = proxy_connection_write.close().await {
        error!(
            ">>>> Transport {transport_id} fail to close tcp remote endpoint because of error: {e:?}"
        )
    };
}

pub(crate) async fn close_remote_udp(
    transport_id: TransportId,
    proxy_connection_write: &Mutex<ProxyConnectionWrite>,
) {
    let mut proxy_connection_write = proxy_connection_write.lock().await;
    if let Err(e) = proxy_connection_write.close().await {
        error!(
            ">>>> Transport {transport_id} fail to close udp remote endpoint because of error: {e:?}"
        )
    };
}
