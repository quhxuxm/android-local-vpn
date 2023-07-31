mod common;

use std::{collections::VecDeque, future::Future, sync::Arc};

use anyhow::Result;

use smoltcp::iface::Interface;
use smoltcp::iface::{SocketHandle, SocketSet};
use tokio::sync::{mpsc::Sender, Mutex, Notify};

use crate::{config::PpaassVpnServerConfig, values::ClientFileTxPacket};
use crate::{device::SmoltcpDevice, error::RemoteEndpointError};
use crate::{error::ClientEndpointError, transport::ControlProtocol};

use self::common::{
    close_client_tcp, close_client_udp, new_tcp, new_udp, recv_from_client_tcp, recv_from_client_udp,
    send_to_client_tcp, send_to_client_udp,
};

use super::{remote::RemoteEndpoint, TransportId};

pub(crate) enum ClientEndpoint<'buf> {
    Tcp {
        transport_id: TransportId,
        smoltcp_socket_handle: SocketHandle,
        smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
        smoltcp_iface: Arc<Mutex<Interface>>,
        smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
        recv_buffer: Arc<Mutex<VecDeque<u8>>>,
        recv_buffer_notify: Arc<Notify>,
        client_file_tx_sender: Sender<ClientFileTxPacket>,
        closed: Mutex<bool>,
        _config: &'static PpaassVpnServerConfig,
    },
    Udp {
        transport_id: TransportId,
        smoltcp_socket_handle: SocketHandle,
        smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
        smoltcp_iface: Arc<Mutex<Interface>>,
        smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
        recv_buffer: Arc<Mutex<VecDeque<Vec<u8>>>>,
        recv_buffer_notify: Arc<Notify>,
        client_file_tx_sender: Sender<ClientFileTxPacket>,
        closed: Mutex<bool>,
        _config: &'static PpaassVpnServerConfig,
    },
}

impl<'buf> ClientEndpoint<'buf> {
    pub(crate) fn new(
        transport_id: TransportId,
        client_file_tx_sender: Sender<ClientFileTxPacket>,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<(ClientEndpoint<'_>, Arc<Notify>), ClientEndpointError> {
        match transport_id.control_protocol {
            ControlProtocol::Tcp => new_tcp(transport_id, client_file_tx_sender, config),
            ControlProtocol::Udp => new_udp(transport_id, client_file_tx_sender, config),
        }
    }

    pub(crate) async fn consume_recv_buffer<F, Fut>(
        &self,
        remote: Arc<RemoteEndpoint>,
        mut consume_fn: F,
    ) -> Result<bool>
    where
        F: FnMut(TransportId, Vec<u8>, Arc<RemoteEndpoint>) -> Fut,
        Fut: Future<Output = Result<usize, RemoteEndpointError>>,
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
                let recv_buffer_data = recv_buffer.make_contiguous().to_vec();
                let consume_size = consume_fn(*transport_id, recv_buffer_data, remote).await?;
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
                smoltcp_socket_handle,
                smoltcp_socket_set,
                smoltcp_iface,
                smoltcp_device,
                client_file_tx_sender,
                closed,
                ..
            } => {
                close_client_tcp(
                    smoltcp_device,
                    smoltcp_iface,
                    smoltcp_socket_set,
                    *smoltcp_socket_handle,
                    *transport_id,
                    client_file_tx_sender,
                    closed,
                )
                .await;
            }
            Self::Udp {
                transport_id,
                smoltcp_socket_handle,
                smoltcp_socket_set,
                smoltcp_iface,
                smoltcp_device,
                client_file_tx_sender,
                closed,
                ..
            } => {
                close_client_udp(
                    smoltcp_device,
                    smoltcp_iface,
                    smoltcp_socket_set,
                    *smoltcp_socket_handle,
                    *transport_id,
                    client_file_tx_sender,
                    closed,
                )
                .await;
            }
        }
    }

    pub(crate) async fn send_to_smoltcp(&self, data: Vec<u8>) -> Result<usize, ClientEndpointError> {
        match self {
            Self::Tcp {
                transport_id,
                smoltcp_socket_handle,
                smoltcp_socket_set,
                smoltcp_iface,
                smoltcp_device,
                client_file_tx_sender,
                ..
            } => {
                send_to_client_tcp(
                    smoltcp_device,
                    smoltcp_iface,
                    smoltcp_socket_set,
                    smoltcp_socket_handle,
                    data,
                    transport_id,
                    client_file_tx_sender,
                )
                .await
            }
            Self::Udp {
                transport_id,
                smoltcp_socket_handle,
                smoltcp_socket_set,
                smoltcp_iface,
                smoltcp_device,
                client_file_tx_sender,
                ..
            } => {
                send_to_client_udp(
                    smoltcp_device,
                    smoltcp_iface,
                    smoltcp_socket_set,
                    smoltcp_socket_handle,
                    transport_id,
                    data,
                    client_file_tx_sender,
                )
                .await
            }
        }
    }

    pub(crate) async fn receive_from_client(&self, client_data: Vec<u8>) {
        match self {
            Self::Tcp {
                transport_id,
                smoltcp_socket_handle,
                smoltcp_socket_set,
                smoltcp_iface,
                smoltcp_device,
                client_file_tx_sender,
                recv_buffer,
                recv_buffer_notify,
                ..
            } => {
                recv_from_client_tcp(
                    smoltcp_device,
                    smoltcp_iface,
                    smoltcp_socket_set,
                    client_data,
                    *smoltcp_socket_handle,
                    *transport_id,
                    client_file_tx_sender,
                    recv_buffer,
                    recv_buffer_notify,
                )
                .await;
            }
            Self::Udp {
                transport_id,
                smoltcp_socket_handle,
                smoltcp_socket_set,
                smoltcp_iface,
                smoltcp_device,
                client_file_tx_sender,
                recv_buffer,
                recv_buffer_notify,
                ..
            } => {
                recv_from_client_udp(
                    smoltcp_device,
                    smoltcp_iface,
                    smoltcp_socket_set,
                    client_data,
                    *smoltcp_socket_handle,
                    *transport_id,
                    client_file_tx_sender,
                    recv_buffer,
                    recv_buffer_notify,
                )
                .await;
            }
        };
    }
}
