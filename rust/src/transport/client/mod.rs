mod common;

use std::{
    collections::VecDeque,
    fmt::{Debug, Formatter},
    fs::File,
    future::Future,
    sync::Arc,
};

use anyhow::Result;

use smoltcp::iface::Interface;
use smoltcp::iface::{SocketHandle, SocketSet};
use tokio::sync::{Mutex, MutexGuard, Notify, RwLock};

use crate::config::PpaassVpnServerConfig;
use crate::{device::SmoltcpDevice, error::RemoteEndpointError};
use crate::{error::ClientEndpointError, transport::ControlProtocol};

use self::common::{
    close_client_tcp, close_client_udp, new_tcp, new_udp, recv_from_client_tcp, recv_from_client_udp,
    send_to_client_tcp, send_to_client_udp,
};

use super::{remote::RemoteEndpoint, TransportId};
use std::fmt::Result as FormatResult;

pub(crate) struct ClientEndpointCtlLockGuard<'lock, 'buf> {
    pub(crate) smoltcp_socket_set: MutexGuard<'lock, SocketSet<'buf>>,
    pub(crate) smoltcp_iface: MutexGuard<'lock, Interface>,
    pub(crate) smoltcp_device: MutexGuard<'lock, SmoltcpDevice>,
}

pub(crate) struct ClientEndpointCtl<'buf> {
    transport_id: TransportId,
    smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
    smoltcp_iface: Arc<Mutex<Interface>>,
    smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
}

impl Debug for ClientEndpointCtl<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
        f.debug_struct("ClientEndpointCtl")
            .field("transport_id", &self.transport_id)
            .finish()
    }
}

impl<'buf> ClientEndpointCtl<'buf> {
    pub(crate) fn new(
        transport_id: TransportId,
        smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
        smoltcp_iface: Arc<Mutex<Interface>>,
        smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
    ) -> Self {
        Self {
            transport_id,
            smoltcp_socket_set,
            smoltcp_iface,
            smoltcp_device,
        }
    }
    pub(crate) async fn lock<'lock>(&'lock self) -> ClientEndpointCtlLockGuard<'lock, 'buf> {
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
pub(crate) enum ClientEndpoint<'buf> {
    Tcp {
        transport_id: TransportId,
        ctl: ClientEndpointCtl<'buf>,
        smoltcp_socket_handle: SocketHandle,
        recv_buffer: Arc<RwLock<VecDeque<u8>>>,
        recv_buffer_notify: Arc<Notify>,
        client_file_write: Arc<Mutex<File>>,
        _config: &'static PpaassVpnServerConfig,
    },
    Udp {
        transport_id: TransportId,
        smoltcp_socket_handle: SocketHandle,
        ctl: ClientEndpointCtl<'buf>,
        recv_buffer: Arc<RwLock<VecDeque<Vec<u8>>>>,
        recv_buffer_notify: Arc<Notify>,
        client_file_write: Arc<Mutex<File>>,
        _config: &'static PpaassVpnServerConfig,
    },
}

impl<'buf> ClientEndpoint<'buf> {
    pub(crate) fn new(
        transport_id: TransportId,
        client_file_write: Arc<Mutex<File>>,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<(ClientEndpoint<'_>, Arc<Notify>), ClientEndpointError> {
        match transport_id.control_protocol {
            ControlProtocol::Tcp => new_tcp(transport_id, client_file_write, config),
            ControlProtocol::Udp => new_udp(transport_id, client_file_write, config),
        }
    }

    pub(crate) async fn consume_recv_buffer<F, Fut>(&self, remote: Arc<RemoteEndpoint>, mut consume_fn: F) -> Result<()>
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
                {
                    let recv_buffer = recv_buffer.read().await;
                    if recv_buffer.len() == 0 {
                        return Ok(());
                    }
                }
                let mut recv_buffer = recv_buffer.write().await;
                let recv_buffer_data = recv_buffer.make_contiguous().to_vec();
                let consume_size = consume_fn(*transport_id, recv_buffer_data, remote).await?;
                recv_buffer.drain(..consume_size);
                Ok(())
            }
            Self::Udp {
                transport_id,
                recv_buffer,
                ..
            } => {
                {
                    let recv_buffer = recv_buffer.read().await;
                    if recv_buffer.len() == 0 {
                        return Ok(());
                    }
                }
                let mut recv_buffer = recv_buffer.write().await;
                let mut consume_size = 0;
                for udp_data in recv_buffer.iter() {
                    consume_fn(*transport_id, udp_data.to_vec(), Arc::clone(&remote)).await?;
                    consume_size += 1;
                }
                recv_buffer.drain(..consume_size);
                Ok(())
            }
        }
    }

    pub(crate) async fn send_to_smoltcp(&self, data: Vec<u8>) -> Result<usize, ClientEndpointError> {
        match self {
            Self::Tcp {
                transport_id,
                smoltcp_socket_handle,
                ctl,
                client_file_write,
                ..
            } => {
                send_to_client_tcp(
                    ctl,
                    *smoltcp_socket_handle,
                    data,
                    *transport_id,
                    Arc::clone(client_file_write),
                )
                .await
            }
            Self::Udp {
                transport_id,
                smoltcp_socket_handle,
                ctl,
                client_file_write,
                ..
            } => {
                send_to_client_udp(
                    ctl,
                    *smoltcp_socket_handle,
                    *transport_id,
                    data,
                    Arc::clone(client_file_write),
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
                ctl,
                client_file_write,
                recv_buffer,
                recv_buffer_notify,
                ..
            } => {
                recv_from_client_tcp(
                    ctl,
                    client_data,
                    *smoltcp_socket_handle,
                    *transport_id,
                    Arc::clone(client_file_write),
                    recv_buffer,
                    recv_buffer_notify,
                )
                .await;
            }
            Self::Udp {
                transport_id,
                smoltcp_socket_handle,
                ctl,
                client_file_write,
                recv_buffer,
                recv_buffer_notify,
                ..
            } => {
                recv_from_client_udp(
                    ctl,
                    client_data,
                    *smoltcp_socket_handle,
                    *transport_id,
                    Arc::clone(client_file_write),
                    recv_buffer,
                    recv_buffer_notify,
                )
                .await;
            }
        };
    }

    pub(crate) async fn close(&self) {
        match self {
            Self::Tcp {
                transport_id,
                smoltcp_socket_handle,
                ctl,
                client_file_write,
                recv_buffer_notify,
                ..
            } => {
                recv_buffer_notify.notify_waiters();
                close_client_tcp(
                    ctl,
                    *smoltcp_socket_handle,
                    *transport_id,
                    Arc::clone(client_file_write),
                )
                .await;
            }
            Self::Udp {
                transport_id,
                ctl,
                client_file_write,
                recv_buffer_notify,
                ..
            } => {
                recv_buffer_notify.notify_waiters();
                close_client_udp(ctl, *transport_id, Arc::clone(client_file_write)).await;
            }
        }
    }
}
