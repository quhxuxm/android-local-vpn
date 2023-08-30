mod common;

use std::{collections::VecDeque, future::Future, sync::Arc};

use anyhow::Result;

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::udp::Socket as SmoltcpUdpSocket;
use smoltcp::{
    iface::Interface, socket::tcp::Socket as SmoltcpTcpSocket,
    socket::tcp::State,
};
use tokio::sync::{mpsc::Sender, Mutex, MutexGuard, Notify, RwLock};

use crate::transport::client::common::abort_client_tcp;
use crate::{config::PpaassVpnServerConfig, util::ClientOutputPacket};
use crate::{device::SmoltcpDevice, error::RemoteEndpointError};
use crate::{error::ClientEndpointError, transport::ControlProtocol};

use self::common::{
    close_client_tcp, close_client_udp, new_tcp, new_udp, recv_from_client_tcp,
    recv_from_client_udp, send_to_client_tcp, send_to_client_udp,
};

use super::{remote::RemoteEndpoint, TransportId};

pub(crate) type ClientTcpRecvBuf = (RwLock<VecDeque<u8>>, Notify);
pub(crate) type ClientUdpRecvBuf = (RwLock<VecDeque<Vec<u8>>>, Notify);

pub(crate) struct ClientEndpointCtlLockGuard<'lock, 'buf> {
    pub(crate) smoltcp_socket_set: MutexGuard<'lock, SocketSet<'buf>>,
    pub(crate) smoltcp_iface: MutexGuard<'lock, Interface>,
    pub(crate) smoltcp_device: MutexGuard<'lock, SmoltcpDevice>,
}

pub(crate) struct ClientEndpointCtl<'buf> {
    smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
    smoltcp_iface: Arc<Mutex<Interface>>,
    smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
}

impl<'buf> ClientEndpointCtl<'buf> {
    fn new(
        smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
        smoltcp_iface: Arc<Mutex<Interface>>,
        smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
    ) -> Self {
        Self {
            smoltcp_socket_set,
            smoltcp_iface,
            smoltcp_device,
        }
    }
    pub(crate) async fn lock<'lock>(
        &'lock self,
    ) -> ClientEndpointCtlLockGuard<'lock, 'buf> {
        let smoltcp_device = self.smoltcp_device.lock().await;
        let smoltcp_iface = self.smoltcp_iface.lock().await;
        let smoltcp_socket_set = self.smoltcp_socket_set.lock().await;
        ClientEndpointCtlLockGuard {
            smoltcp_socket_set,
            smoltcp_iface,
            smoltcp_device,
        }
    }
}

#[derive(Debug)]
pub(crate) enum ClientEndpointUdpState {
    Open,
    Closed,
}

#[derive(Debug)]
pub(crate) enum ClientEndpointState {
    Tcp(State),
    Udp(ClientEndpointUdpState),
}

pub(crate) enum ClientEndpoint<'buf> {
    Tcp {
        transport_id: TransportId,
        ctl: ClientEndpointCtl<'buf>,
        smoltcp_socket_handle: SocketHandle,
        recv_buffer: Arc<ClientTcpRecvBuf>,
        client_output_tx: Sender<ClientOutputPacket>,
        _config: &'static PpaassVpnServerConfig,
    },
    Udp {
        transport_id: TransportId,
        smoltcp_socket_handle: SocketHandle,
        ctl: ClientEndpointCtl<'buf>,
        recv_buffer: Arc<ClientUdpRecvBuf>,
        client_output_tx: Sender<ClientOutputPacket>,
        _config: &'static PpaassVpnServerConfig,
    },
}

impl<'buf> ClientEndpoint<'buf> {
    pub(crate) fn new(
        transport_id: TransportId,
        client_output_tx: Sender<ClientOutputPacket>,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<ClientEndpoint<'_>, ClientEndpointError> {
        match transport_id.control_protocol {
            ControlProtocol::Tcp => {
                new_tcp(transport_id, client_output_tx, config)
            }
            ControlProtocol::Udp => {
                new_udp(transport_id, client_output_tx, config)
            }
        }
    }

    pub(crate) async fn get_state(&self) -> ClientEndpointState {
        match self {
            ClientEndpoint::Tcp {
                ctl,
                smoltcp_socket_handle,
                ..
            } => {
                let ClientEndpointCtlLockGuard {
                    mut smoltcp_socket_set,
                    ..
                } = ctl.lock().await;
                let smoltcp_socket = smoltcp_socket_set
                    .get_mut::<SmoltcpTcpSocket>(*smoltcp_socket_handle);
                ClientEndpointState::Tcp(smoltcp_socket.state())
            }
            ClientEndpoint::Udp {
                smoltcp_socket_handle,
                ctl,
                ..
            } => {
                let ClientEndpointCtlLockGuard {
                    mut smoltcp_socket_set,
                    ..
                } = ctl.lock().await;
                let smoltcp_socket = smoltcp_socket_set
                    .get_mut::<SmoltcpUdpSocket>(*smoltcp_socket_handle);
                ClientEndpointState::Udp(if smoltcp_socket.is_open() {
                    ClientEndpointUdpState::Open
                } else {
                    ClientEndpointUdpState::Closed
                })
            }
        }
    }

    pub(crate) async fn consume_recv_buffer<F, Fut>(
        &self,
        remote: Arc<RemoteEndpoint>,
        mut consume_fn: F,
    ) -> Result<()>
    where
        F: FnMut(TransportId, Vec<u8>, Arc<RemoteEndpoint>) -> Fut,
        Fut: Future<Output = Result<usize, RemoteEndpointError>>,
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
                let recv_buffer_data = recv_buffer.make_contiguous().to_vec();
                let consume_size =
                    consume_fn(*transport_id, recv_buffer_data, remote).await?;
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

    pub(crate) async fn send_to_smoltcp(
        &self,
        data: Vec<u8>,
    ) -> Result<usize, ClientEndpointError> {
        match self {
            Self::Tcp {
                transport_id,
                smoltcp_socket_handle,
                ctl,
                client_output_tx,
                ..
            } => {
                send_to_client_tcp(
                    ctl,
                    *smoltcp_socket_handle,
                    data,
                    *transport_id,
                    client_output_tx,
                )
                .await
            }
            Self::Udp {
                transport_id,
                smoltcp_socket_handle,
                ctl,
                client_output_tx,
                ..
            } => {
                send_to_client_udp(
                    ctl,
                    *smoltcp_socket_handle,
                    *transport_id,
                    data,
                    client_output_tx,
                )
                .await
            }
        }
    }

    pub(crate) async fn receive_from_client(
        &self,
        client_data: Vec<u8>,
    ) -> Result<(), ClientEndpointError> {
        match self {
            Self::Tcp {
                transport_id,
                smoltcp_socket_handle,
                ctl,
                client_output_tx,
                recv_buffer,
                ..
            } => {
                recv_from_client_tcp(
                    ctl,
                    client_data,
                    *smoltcp_socket_handle,
                    *transport_id,
                    client_output_tx,
                    recv_buffer,
                )
                .await
            }
            Self::Udp {
                transport_id,
                smoltcp_socket_handle,
                ctl,
                client_output_tx,
                recv_buffer,
                ..
            } => {
                recv_from_client_udp(
                    ctl,
                    client_data,
                    *smoltcp_socket_handle,
                    *transport_id,
                    client_output_tx,
                    recv_buffer,
                )
                .await
            }
        }
    }

    pub(crate) async fn abort(&self) {
        match self {
            Self::Tcp {
                transport_id,
                smoltcp_socket_handle,
                ctl,
                client_output_tx,
                recv_buffer,
                ..
            } => {
                abort_client_tcp(
                    ctl,
                    *smoltcp_socket_handle,
                    *transport_id,
                    client_output_tx,
                )
                .await;
                recv_buffer.1.notify_waiters();
            }
            Self::Udp {
                transport_id,
                ctl,
                client_output_tx,
                recv_buffer,
                ..
            } => {
                close_client_udp(ctl, *transport_id, client_output_tx).await;
                recv_buffer.1.notify_waiters();
            }
        }
    }
    pub(crate) async fn close(&self) {
        match self {
            Self::Tcp {
                transport_id,
                smoltcp_socket_handle,
                ctl,
                client_output_tx,
                recv_buffer,
                ..
            } => {
                close_client_tcp(
                    ctl,
                    *smoltcp_socket_handle,
                    *transport_id,
                    client_output_tx,
                )
                .await;
                recv_buffer.1.notify_waiters();
            }
            Self::Udp {
                transport_id,
                ctl,
                client_output_tx,
                recv_buffer,
                ..
            } => {
                close_client_udp(ctl, *transport_id, client_output_tx).await;
                recv_buffer.1.notify_waiters();
            }
        }
    }

    pub(crate) async fn awaiting_recv_buf(&self) {
        match self {
            ClientEndpoint::Tcp { recv_buffer, .. } => {
                recv_buffer.1.notified().await
            }
            ClientEndpoint::Udp { recv_buffer, .. } => {
                recv_buffer.1.notified().await
            }
        }
    }
}
