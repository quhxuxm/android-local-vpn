use std::{collections::VecDeque, future::Future, sync::Arc};

use anyhow::{anyhow, Result};
use log::error;
use smoltcp::{iface::Interface, socket::udp::Socket as SmoltcpUdpSocket, time::Instant};
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp::Socket as SmoltcpTcpSocket,
};
use tokio::sync::{mpsc::Sender, Mutex, Notify};

use crate::device::SmoltcpDevice;
use crate::transport::Transport;
use crate::values::ClientFileTxPacket;

use super::{
    common::{create_smoltcp_tcp_socket, create_smoltcp_udp_socket, prepare_smoltcp_iface_and_device},
    remote::RemoteEndpoint,
    TransportId,
};

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
    },
}

impl<'buf> ClientEndpoint<'buf> {
    pub(crate) fn new_tcp(transport_id: TransportId, client_file_tx_sender: Sender<ClientFileTxPacket>) -> Result<(ClientEndpoint<'buf>, Arc<Notify>)> {
        let (smoltcp_iface, smoltcp_device) = prepare_smoltcp_iface_and_device(transport_id)?;
        let mut smoltcp_socket_set = SocketSet::new(Vec::with_capacity(1024));
        let smoltcp_tcp_socket = create_smoltcp_tcp_socket(transport_id, transport_id.destination.into())?;
        let smoltcp_socket_handle = smoltcp_socket_set.add(smoltcp_tcp_socket);
        let recv_buffer_notify = Arc::new(Notify::new());
        Ok((
            Self::Tcp {
                transport_id,
                smoltcp_socket_handle,
                smoltcp_socket_set: Arc::new(Mutex::new(smoltcp_socket_set)),
                smoltcp_iface: Arc::new(Mutex::new(smoltcp_iface)),
                smoltcp_device: Arc::new(Mutex::new(smoltcp_device)),
                recv_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                recv_buffer_notify: Arc::clone(&recv_buffer_notify),
                client_file_tx_sender,
            },
            recv_buffer_notify,
        ))
    }

    pub(crate) fn new_udp(transport_id: TransportId, client_file_tx_sender: Sender<ClientFileTxPacket>) -> Result<(ClientEndpoint<'buf>, Arc<Notify>)> {
        let (smoltcp_iface, smoltcp_device) = prepare_smoltcp_iface_and_device(transport_id)?;
        let mut smoltcp_socket_set = SocketSet::new(Vec::with_capacity(1024));
        let smoltcp_udp_socket = create_smoltcp_udp_socket(transport_id, transport_id.destination.into())?;
        let smoltcp_socket_handle = smoltcp_socket_set.add(smoltcp_udp_socket);
        let recv_buffer_notify = Arc::new(Notify::new());
        Ok((
            Self::Udp {
                transport_id,
                smoltcp_socket_handle,
                smoltcp_socket_set: Arc::new(Mutex::new(smoltcp_socket_set)),
                smoltcp_iface: Arc::new(Mutex::new(smoltcp_iface)),
                smoltcp_device: Arc::new(Mutex::new(smoltcp_device)),
                recv_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                recv_buffer_notify: Arc::clone(&recv_buffer_notify),
                client_file_tx_sender,
            },
            recv_buffer_notify,
        ))
    }

    pub(crate) async fn consume_recv_buffer<F, Fut>(&self, remote: Arc<RemoteEndpoint>, mut consume_fn: F) -> Result<()>
    where
        F: FnMut(TransportId, Vec<u8>, Arc<RemoteEndpoint>) -> Fut,
        Fut: Future<Output = Result<usize>>,
    {
        match self {
            Self::Tcp {
                transport_id,
                recv_buffer,
                ..
            } => {
                let mut recv_buffer = recv_buffer.lock().await;
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
                let mut consume_size = 0;
                let mut recv_buffer = recv_buffer.lock().await;
                for udp_data in recv_buffer.iter() {
                    consume_fn(*transport_id, udp_data.to_vec(), Arc::clone(&remote)).await?;
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
                smoltcp_socket_handle,
                smoltcp_socket_set,
                smoltcp_iface,
                smoltcp_device,
                client_file_tx_sender,
                ..
            } => {
                let mut smoltcp_device = smoltcp_device.lock().await;
                let mut smoltcp_iface = smoltcp_iface.lock().await;
                let mut smoltcp_socket_set = smoltcp_socket_set.lock().await;
                let smoltcp_socket = smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(*smoltcp_socket_handle);
                smoltcp_socket.close();
                if smoltcp_iface.poll(
                    Instant::now(),
                    &mut *smoltcp_device,
                    &mut smoltcp_socket_set,
                ) {
                    while let Some(output) = smoltcp_device.pop_tx() {
                        let client_file_tx_packet = ClientFileTxPacket::new(*transport_id, output);
                        if let Err(e) = client_file_tx_sender.send(client_file_tx_packet).await {
                            error!("<<<< Transport {transport_id} fail to transfer smoltcp tcp data for output because of error: {e:?}")
                        };
                    }
                }
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
                let mut smoltcp_device = smoltcp_device.lock().await;
                let mut smoltcp_iface = smoltcp_iface.lock().await;
                let mut smoltcp_socket_set = smoltcp_socket_set.lock().await;
                let smoltcp_socket = smoltcp_socket_set.get_mut::<SmoltcpUdpSocket>(*smoltcp_socket_handle);
                smoltcp_socket.close();
                if smoltcp_iface.poll(
                    Instant::now(),
                    &mut *smoltcp_device,
                    &mut smoltcp_socket_set,
                ) {
                    while let Some(output) = smoltcp_device.pop_tx() {
                        let client_file_tx_packet = ClientFileTxPacket::new(*transport_id, output);
                        if let Err(e) = client_file_tx_sender.send(client_file_tx_packet).await {
                            error!("<<<< Transport {transport_id} fail to transfer smoltcp udp data for output because of error: {e:?}")
                        };
                    }
                }
            }
        }
    }

    pub(crate) async fn send_to_smoltcp(&self, data: Vec<u8>) -> Result<usize> {
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
                let mut smoltcp_device = smoltcp_device.lock().await;
                let mut smoltcp_iface = smoltcp_iface.lock().await;
                let mut smoltcp_socket_set = smoltcp_socket_set.lock().await;
                let smoltcp_socket = smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(*smoltcp_socket_handle);
                if smoltcp_socket.may_send() {
                    let send_result = smoltcp_socket
                        .send_slice(&data)
                        .map_err(|e| anyhow!("{e:?}"))?;
                    if smoltcp_iface.poll(
                        Instant::now(),
                        &mut *smoltcp_device,
                        &mut smoltcp_socket_set,
                    ) {
                        while let Some(output) = smoltcp_device.pop_tx() {
                            let client_file_tx_packet = ClientFileTxPacket::new(*transport_id, output);
                            if let Err(e) = client_file_tx_sender.send(client_file_tx_packet).await {
                                error!("<<<< Transport {transport_id} fail to transfer smoltcp tcp data for outupt because of error: {e:?}")
                            };
                        }
                    }
                    return Ok(send_result);
                }
                Ok(0)
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
                let mut smoltcp_device = smoltcp_device.lock().await;
                let mut smoltcp_iface = smoltcp_iface.lock().await;
                let mut smoltcp_socket_set = smoltcp_socket_set.lock().await;
                let smoltcp_socket = smoltcp_socket_set.get_mut::<SmoltcpUdpSocket>(*smoltcp_socket_handle);
                if smoltcp_socket.can_send() {
                    smoltcp_socket
                        .send_slice(&data, transport_id.source.into())
                        .map_err(|e| anyhow!("{e:?}"))?;
                    if smoltcp_iface.poll(
                        Instant::now(),
                        &mut *smoltcp_device,
                        &mut smoltcp_socket_set,
                    ) {
                        while let Some(output) = smoltcp_device.pop_tx() {
                            let client_file_tx_packet = ClientFileTxPacket::new(*transport_id, output);
                            if let Err(e) = client_file_tx_sender.send(client_file_tx_packet).await {
                                error!("<<<< Transport {transport_id} fail to transfer smoltcp tcp data for outupt because of error: {e:?}")
                            };
                        }
                    }
                    return Ok(1);
                }
                Ok(0)
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
            } => {
                let mut smoltcp_device = smoltcp_device.lock().await;
                let mut smoltcp_iface = smoltcp_iface.lock().await;
                let mut smoltcp_socket_set = smoltcp_socket_set.lock().await;
                smoltcp_device.push_rx(client_data);
                if smoltcp_iface.poll(
                    Instant::now(),
                    &mut *smoltcp_device,
                    &mut smoltcp_socket_set,
                ) {
                    let smoltcp_tcp_socket = smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(*smoltcp_socket_handle);
                    while let Some(output) = smoltcp_device.pop_tx() {
                        let client_file_tx_packet = ClientFileTxPacket::new(*transport_id, output);
                        if let Err(e) = client_file_tx_sender.send(client_file_tx_packet).await {
                            error!("<<<< Transport {transport_id} fail to transfer smoltcp tcp data for output because of error: {e:?}")
                        };
                    }
                    while smoltcp_tcp_socket.may_recv() {
                        let mut data = [0u8; 65536];
                        let data = match smoltcp_tcp_socket.recv_slice(&mut data) {
                            Ok(0) => {
                                if !recv_buffer.lock().await.is_empty() {
                                    recv_buffer_notify.notify_waiters();
                                }
                                break;
                            }
                            Ok(size) => &data[..size],
                            Err(e) => {
                                error!(">>>> Transport {transport_id} fail to receive tcp data from smoltcp because of error: {e:?}");
                                if !recv_buffer.lock().await.is_empty() {
                                    recv_buffer_notify.notify_waiters();
                                }
                                break;
                            }
                        };
                        recv_buffer.lock().await.extend(data);
                    }
                    if !recv_buffer.lock().await.is_empty() {
                        recv_buffer_notify.notify_waiters();
                    }
                }
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
            } => {
                let mut smoltcp_device = smoltcp_device.lock().await;
                let mut smoltcp_iface = smoltcp_iface.lock().await;
                let mut smoltcp_socket_set = smoltcp_socket_set.lock().await;
                smoltcp_device.push_rx(client_data);
                if smoltcp_iface.poll(
                    Instant::now(),
                    &mut *smoltcp_device,
                    &mut smoltcp_socket_set,
                ) {
                    let smoltcp_udp_socket = smoltcp_socket_set.get_mut::<SmoltcpUdpSocket>(*smoltcp_socket_handle);
                    while let Some(output) = smoltcp_device.pop_tx() {
                        let client_file_tx_packet = ClientFileTxPacket::new(*transport_id, output);
                        if let Err(e) = client_file_tx_sender.send(client_file_tx_packet).await {
                            error!("<<<< Transport {transport_id} fail to transfer smoltcp udp data for output because of error: {e:?}")
                        };
                    }
                    while smoltcp_udp_socket.can_recv() {
                        let mut data = [0u8; 65536];
                        let data = match smoltcp_udp_socket.recv_slice(&mut data) {
                            Ok((0, _)) => {
                                if !recv_buffer.lock().await.is_empty() {
                                    recv_buffer_notify.notify_waiters();
                                }
                                break;
                            }
                            Ok((size, _)) => &data[..size],
                            Err(e) => {
                                error!(">>>> Transport {transport_id} fail to receive udp data from smoltcp because of error: {e:?}");
                                if !recv_buffer.lock().await.is_empty() {
                                    recv_buffer_notify.notify_waiters();
                                }
                                break;
                            }
                        };
                        recv_buffer.lock().await.push_back(data.to_vec());
                    }
                    if !recv_buffer.lock().await.is_empty() {
                        recv_buffer_notify.notify_waiters();
                    }
                }
            }
        };
    }
}
