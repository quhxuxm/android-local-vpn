use std::collections::VecDeque;

use anyhow::Result;
use log::error;
use smoltcp::{iface::Interface, socket::udp::Socket as SmoltcpUdpSocket, time::Instant};
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp::Socket as SmoltcpTcpSocket,
};
use tokio::sync::mpsc::{channel, Receiver, Sender};

use crate::device::SmoltcpDevice;

use super::{
    common::{create_smoltcp_tcp_socket, create_smoltcp_udp_socket, prepare_smoltcp_iface_and_device},
    TransportId,
};
pub(crate) enum ClientEndpoint<'buf> {
    Tcp {
        transport_id: TransportId,
        smoltcp_socket_handle: SocketHandle,
        smoltcp_socket_set: SocketSet<'buf>,
        smoltcp_iface: Interface,
        smoltcp_device: SmoltcpDevice,
        client_endpoint_output_rx: Sender<Vec<u8>>,
        recv_buffer: VecDeque<u8>,
    },
    Udp {
        transport_id: TransportId,
        smoltcp_socket_handle: SocketHandle,
        smoltcp_socket_set: SocketSet<'buf>,
        smoltcp_iface: Interface,
        smoltcp_device: SmoltcpDevice,
        client_endpoint_output_rx: Sender<Vec<u8>>,
        recv_buffer: VecDeque<Vec<u8>>,
    },
}

impl<'buf> ClientEndpoint<'buf> {
    pub(crate) fn new_tcp(transport_id: TransportId) -> Result<(ClientEndpoint<'buf>, Receiver<Vec<u8>>)> {
        let (smoltcp_iface, smoltcp_device) = prepare_smoltcp_iface_and_device(transport_id)?;
        let mut smoltcp_socket_set = SocketSet::new(Vec::with_capacity(1024));
        let smoltcp_tcp_socket = create_smoltcp_tcp_socket(transport_id, transport_id.destination.into())?;
        let smoltcp_socket_handle = smoltcp_socket_set.add(smoltcp_tcp_socket);
        let (client_endpoint_output_rx, client_endpoint_output_tx) = channel::<Vec<u8>>(1024);
        Ok((
            Self::Tcp {
                transport_id,
                smoltcp_socket_handle,
                smoltcp_socket_set,
                smoltcp_iface,
                smoltcp_device,
                client_endpoint_output_rx,
                recv_buffer: VecDeque::with_capacity(65536),
            },
            client_endpoint_output_tx,
        ))
    }

    pub(crate) fn new_udp(transport_id: TransportId) -> Result<(ClientEndpoint<'buf>, Receiver<Vec<u8>>)> {
        let (smoltcp_iface, smoltcp_device) = prepare_smoltcp_iface_and_device(transport_id)?;
        let mut smoltcp_socket_set = SocketSet::new(Vec::with_capacity(1024));
        let smoltcp_udp_socket = create_smoltcp_udp_socket(transport_id, transport_id.destination.into())?;
        let smoltcp_socket_handle = smoltcp_socket_set.add(smoltcp_udp_socket);
        let (client_endpoint_output_rx, client_endpoint_output_tx) = channel::<Vec<u8>>(1024);
        Ok((
            Self::Udp {
                transport_id,
                smoltcp_socket_handle,
                smoltcp_socket_set,
                smoltcp_iface,
                smoltcp_device,
                client_endpoint_output_rx,
                recv_buffer: VecDeque::with_capacity(65536),
            },
            client_endpoint_output_tx,
        ))
    }

    pub(crate) async fn receive(&mut self, client_data: Vec<u8>) {
        match self {
            ClientEndpoint::Tcp {
                transport_id,
                smoltcp_socket_handle,
                ref mut smoltcp_socket_set,
                smoltcp_iface,
                smoltcp_device,
                client_endpoint_output_rx,
                ref mut recv_buffer,
            } => {
                smoltcp_device.push_rx(client_data);
                if smoltcp_iface.poll(Instant::now(), smoltcp_device, smoltcp_socket_set) {
                    let smoltcp_tcp_socket = smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(*smoltcp_socket_handle);
                    while let Some(output) = smoltcp_device.pop_tx() {
                        if let Err(e) = client_endpoint_output_rx.send(output).await {
                            error!("<<<< Transport {transport_id} fail to transfer smoltcp tcp data for outupt because of error: {e:?}")
                        };
                    }
                    while smoltcp_tcp_socket.may_recv() {
                        let mut data = [0u8; 65536];
                        let data = match smoltcp_tcp_socket.recv_slice(&mut data) {
                            Ok(size) => &data[..size],
                            Err(e) => break,
                        };
                        recv_buffer.extend(data);
                    }
                }
            }
            ClientEndpoint::Udp {
                transport_id,
                smoltcp_socket_handle,
                ref mut smoltcp_socket_set,
                smoltcp_iface,
                smoltcp_device,
                client_endpoint_output_rx,
                ref mut recv_buffer,
            } => {
                smoltcp_device.push_rx(client_data);
                if smoltcp_iface.poll(Instant::now(), smoltcp_device, smoltcp_socket_set) {
                    let mut smoltcp_udp_socket = smoltcp_socket_set.get_mut::<SmoltcpUdpSocket>(*smoltcp_socket_handle);
                    while let Some(output) = smoltcp_device.pop_tx() {
                        if let Err(e) = client_endpoint_output_rx.send(output).await {
                            error!("<<<< Transport {transport_id} fail to transfer smoltcp udp data for outupt because of error: {e:?}")
                        };
                    }
                    while smoltcp_udp_socket.can_recv() {
                        let mut data = [0u8; 65536];
                        let data = match smoltcp_udp_socket.recv_slice(&mut data) {
                            Ok((size, _)) => &data[..size],
                            Err(e) => break,
                        };
                        recv_buffer.push_back(data.to_vec());
                    }
                }
            }
        };
    }
}
