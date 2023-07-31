use std::{collections::VecDeque, os::fd::AsRawFd, sync::Arc};

use crate::{
    config::PpaassVpnServerConfig,
    error::RemoteEndpointError,
    transport::{value::InternetProtocol, ControlProtocol, TransportId},
    util::{protect_socket, AgentPpaassMessagePayloadEncryptionSelector, AgentRsaCryptoFetcher},
};
use anyhow::anyhow;
use dns_parser::Packet as DnsPacket;
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use log::{debug, error};
use ppaass_common::{
    generate_uuid,
    tcp::{TcpData, TcpInitResponse, TcpInitResponseType},
    udp::UdpData,
    PpaassConnection, PpaassMessage, PpaassMessageGenerator, PpaassMessagePayloadEncryptionSelector,
    PpaassMessageProxyPayload, PpaassMessageProxyPayloadType,
};
use tokio::{
    net::{TcpSocket, TcpStream},
    sync::{Mutex, Notify},
};

use super::RemoteEndpoint;

pub(crate) async fn write_to_remote_udp(
    config: &PpaassVpnServerConfig,
    data: Vec<u8>,
    transport_id: &TransportId,
    proxy_connection_write: &Mutex<
        SplitSink<PpaassConnection<'_, TcpStream, AgentRsaCryptoFetcher, TransportId>, PpaassMessage>,
    >,
) -> Result<usize, RemoteEndpointError> {
    let payload_encryption = AgentPpaassMessagePayloadEncryptionSelector::select(
        config.get_user_token(),
        Some(generate_uuid().into_bytes()),
    );
    debug!(
        ">>>> Transport {transport_id}, [UDP PROCESS] send udp data to remote: {}",
        pretty_hex::pretty_hex(&data)
    );
    if let Ok(dns_request) = DnsPacket::parse(&data) {
        debug!(">>>> Transport {transport_id}, [UDP PROCESS] read dns request from client: {dns_request:?}")
    };

    let data_len = data.len();
    let udp_data = PpaassMessageGenerator::generate_udp_data(
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
    proxy_connection_write: &Mutex<
        SplitSink<PpaassConnection<'_, TcpStream, AgentRsaCryptoFetcher, TransportId>, PpaassMessage>,
    >,
) -> Result<usize, RemoteEndpointError> {
    let payload_encryption = AgentPpaassMessagePayloadEncryptionSelector::select(
        config.get_user_token(),
        Some(generate_uuid().into_bytes()),
    );
    let data_len = data.len();
    let tcp_data = PpaassMessageGenerator::generate_tcp_data(
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
    proxy_connection_read: &Mutex<SplitStream<PpaassConnection<'_, TcpStream, AgentRsaCryptoFetcher, TransportId>>>,
    recv_buffer_notify: &Notify,
    recv_buffer: &Arc<Mutex<VecDeque<Vec<u8>>>>,
) -> Result<bool, RemoteEndpointError> {
    let mut proxy_connection_read = proxy_connection_read.lock().await;
    match proxy_connection_read.next().await {
        None => {
            recv_buffer_notify.notify_waiters();
            Ok(true)
        }
        Some(Ok(PpaassMessage {
            payload: proxy_message_payload,
            ..
        })) => {
            let UdpData {
                data: udp_relay_data,
                ..
            } = proxy_message_payload.as_slice().try_into()?;
            let mut recv_buffer = recv_buffer.lock().await;
            debug!(
                "<<<< Transport {transport_id}, [UDP PROCESS] read remote udp data to remote receive buffer: {}",
                pretty_hex::pretty_hex(&udp_relay_data)
            );

            if let Ok(dns_response) = DnsPacket::parse(&udp_relay_data) {
                debug!("<<<< Transport {transport_id}, [UDP PROCESS] read dns response from remote: {dns_response:?}")
            };
            recv_buffer.push_back(udp_relay_data.to_vec());
            recv_buffer_notify.notify_waiters();
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
    proxy_connection_read: &Mutex<SplitStream<PpaassConnection<'_, TcpStream, AgentRsaCryptoFetcher, TransportId>>>,
    recv_buffer_notify: &Notify,
    recv_buffer: &Arc<Mutex<VecDeque<u8>>>,
) -> Result<bool, RemoteEndpointError> {
    let mut proxy_connection_read = proxy_connection_read.lock().await;
    match proxy_connection_read.next().await {
        None => {
            recv_buffer_notify.notify_waiters();
            Ok(true)
        }
        Some(Ok(PpaassMessage {
            payload: proxy_message_payload,
            ..
        })) => {
            let TcpData {
                data: tcp_relay_data,
                ..
            } = proxy_message_payload.as_slice().try_into()?;
            debug!(
                "<<<< Transport {transport_id} read remote tcp data to remote endpoint receive buffer: {}",
                pretty_hex::pretty_hex(&tcp_relay_data)
            );
            recv_buffer.lock().await.extend(tcp_relay_data);
            recv_buffer_notify.notify_waiters();
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
) -> Result<(RemoteEndpoint, Arc<Notify>), RemoteEndpointError> {
    let tcp_socket = match transport_id.internet_protocol {
        InternetProtocol::Ipv4 => TcpSocket::new_v4()?,
        InternetProtocol::Ipv6 => TcpSocket::new_v6()?,
    };
    let raw_socket_fd = tcp_socket.as_raw_fd();
    protect_socket(transport_id, raw_socket_fd)?;
    let proxy_connection = init_proxy_connection(tcp_socket, transport_id, agent_rsa_crypto_fetcher, config).await?;
    let (proxy_connection_write, proxy_connection_read) = proxy_connection.split();
    let recv_buffer_notify = Arc::new(Notify::new());
    Ok((
        RemoteEndpoint::Tcp {
            transport_id,
            proxy_connection_read: Mutex::new(proxy_connection_read),
            proxy_connection_write: Mutex::new(proxy_connection_write),
            recv_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
            recv_buffer_notify: Arc::clone(&recv_buffer_notify),
            closed: Mutex::new(false),
            config,
        },
        recv_buffer_notify,
    ))
}

/// Create new udp remote endpoint
pub(crate) async fn new_udp(
    transport_id: TransportId,
    agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
    config: &'static PpaassVpnServerConfig,
) -> Result<(RemoteEndpoint, Arc<Notify>), RemoteEndpointError> {
    let tcp_socket = match transport_id.internet_protocol {
        InternetProtocol::Ipv4 => TcpSocket::new_v4()?,
        InternetProtocol::Ipv6 => TcpSocket::new_v6()?,
    };
    let raw_socket_fd = tcp_socket.as_raw_fd();
    protect_socket(transport_id, raw_socket_fd)?;
    let proxy_connection = init_proxy_connection(tcp_socket, transport_id, agent_rsa_crypto_fetcher, config).await?;
    let (proxy_connection_write, proxy_connection_read) = proxy_connection.split();

    let recv_buffer_notify = Arc::new(Notify::new());
    Ok((
        RemoteEndpoint::Udp {
            transport_id,
            proxy_connection_read: Mutex::new(proxy_connection_read),
            proxy_connection_write: Mutex::new(proxy_connection_write),
            recv_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
            recv_buffer_notify: Arc::clone(&recv_buffer_notify),
            closed: Mutex::new(false),
            config,
        },
        recv_buffer_notify,
    ))
}

/// Initialize proxy connection
async fn init_proxy_connection(
    tcp_socket: TcpSocket,
    transport_id: TransportId,
    agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
    config: &'static PpaassVpnServerConfig,
) -> Result<PpaassConnection<'static, TcpStream, AgentRsaCryptoFetcher, TransportId>, RemoteEndpointError> {
    let proxy_addresses = config.get_proxy_address();
    let proxy_tcp_stream = tcp_socket
        .connect(
            proxy_addresses
                .parse()
                .map_err(|e| anyhow!("Faill to parse proxy address because of error: {e:?}"))?,
        )
        .await?;
    let mut proxy_connection = PpaassConnection::new(
        transport_id,
        proxy_tcp_stream,
        agent_rsa_crypto_fetcher,
        true,
        config.get_proxy_connection_buffer_size(),
    );
    let payload_encryption = AgentPpaassMessagePayloadEncryptionSelector::select(
        config.get_user_token(),
        Some(generate_uuid().into_bytes()),
    );

    if let ControlProtocol::Tcp = transport_id.control_protocol {
        let tcp_init_request = PpaassMessageGenerator::generate_tcp_init_request(
            config.get_user_token(),
            transport_id.source.into(),
            transport_id.destination.into(),
            payload_encryption,
        )?;
        proxy_connection.send(tcp_init_request).await?;
        let PpaassMessage {
            payload: proxy_msg_payload,
            ..
        } = proxy_connection
            .next()
            .await
            .ok_or(anyhow!("Transport {transport_id} nothing read from proxy."))??;

        let PpaassMessageProxyPayload {
            payload_type: proxy_msg_payload_type,
            data: proxy_msg_payload_data,
        } = proxy_msg_payload.as_slice().try_into()?;
        let tcp_init_response = match proxy_msg_payload_type {
            PpaassMessageProxyPayloadType::TcpInit => proxy_msg_payload_data.as_slice().try_into()?,
            _ => {
                error!(">>>> Transport {transport_id} receive invalid message from proxy, payload type: {proxy_msg_payload_type:?}");
                return Err(anyhow!("Invalid proxy message payload type: {proxy_msg_payload_type:?}").into());
            }
        };
        let TcpInitResponse {
            response_type: tcp_init_response_type,
            ..
        } = tcp_init_response;
        match tcp_init_response_type {
            TcpInitResponseType::Success => {
                debug!(">>>> Transport {transport_id} success to initialize proxy tcp connection.");
            }
            TcpInitResponseType::Fail => {
                error!(">>>> Transport {transport_id} fail to initialize proxy tcp connection.");
                return Err(anyhow!("Fail to initialize proxy tcp connection").into());
            }
        }
    }

    Ok(proxy_connection)
}

pub(crate) async fn close_remote_tcp(
    transport_id: TransportId,
    proxy_connection_write: &Mutex<
        SplitSink<PpaassConnection<'_, TcpStream, AgentRsaCryptoFetcher, TransportId>, PpaassMessage>,
    >,
    recv_buffer_notify: &Arc<Notify>,
    closed: &Mutex<bool>,
) {
    let mut proxy_connection_write = proxy_connection_write.lock().await;
    if let Err(e) = proxy_connection_write.close().await {
        error!(">>>> Transport {transport_id} fail to close remote endpoint because of error: {e:?}")
    };
    recv_buffer_notify.notify_waiters();
    let mut closed = closed.lock().await;
    *closed = true;
}

pub(crate) async fn close_remote_udp(recv_buffer_notify: &Arc<Notify>, closed: &Mutex<bool>) {
    recv_buffer_notify.notify_waiters();
    let mut closed = closed.lock().await;
    *closed = true;
}