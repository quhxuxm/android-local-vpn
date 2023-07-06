use crate::{
    device::SmoltcpDevice,
    transportation::{TransportProtocol, TransportationId},
};

use log::{debug, error};
use smoltcp::{
    iface::{Config, Routes},
    socket::tcp::{Socket as TcpSocket, SocketBuffer as TcpSocketBuffer},
    wire::{IpAddress, IpCidr, Ipv4Address},
};
use smoltcp::{
    iface::{Interface, SocketHandle, SocketSet},
    time::Instant,
};

use anyhow::anyhow;
use anyhow::Result;
use smoltcp::socket::udp::{PacketBuffer as UdpSocketBuffer, PacketMetadata, Socket as UdpSocket};
use smoltcp::wire::IpEndpoint;
use std::{collections::VecDeque, future::Future, net::SocketAddr, sync::Arc};
use tokio::{
    io::AsyncWrite,
    sync::{Mutex, RwLock},
};

use super::RemoteEndpoint;

pub struct LocalEndpoint<'buf> {
    socket_handle: SocketHandle,
    transport_protocol: TransportProtocol,
    local_endpoint: IpEndpoint,
    socketset: Arc<Mutex<SocketSet<'buf>>>,
    interface: Mutex<Interface>,
    device: Mutex<SmoltcpDevice>,
    trans_id: TransportationId,
    local_recv_buf: Arc<Mutex<VecDeque<u8>>>,
}

impl<'buf> LocalEndpoint<'buf> {
    pub(crate) fn new(
        trans_id: TransportationId,
        transport_protocol: TransportProtocol,
        local_address: SocketAddr,
        remote_address: SocketAddr,
    ) -> Option<Self> {
        let (interface, device) = Self::prepare_iface_and_device(trans_id).ok()?;
        let mut socketset = SocketSet::new(vec![]);
        let local_endpoint = IpEndpoint::from(local_address);
        let remote_endpoint = IpEndpoint::from(remote_address);
        let socket_handle = match transport_protocol {
            TransportProtocol::Tcp => {
                let socket = Self::create_tcp_socket(trans_id, remote_endpoint)?;
                socketset.add(socket)
            }
            TransportProtocol::Udp => {
                let socket = Self::create_udp_socket(trans_id, remote_endpoint)?;
                socketset.add(socket)
            }
        };

        let socket = Self {
            trans_id,
            socket_handle,
            transport_protocol,
            local_endpoint,
            socketset: Arc::new(Mutex::new(socketset)),
            interface: Mutex::new(interface),
            device: Mutex::new(device),
            local_recv_buf: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
        };

        Some(socket)
    }

    fn prepare_iface_and_device(trans_id: TransportationId) -> Result<(Interface, SmoltcpDevice)> {
        let mut routes = Routes::new();
        let default_gateway_ipv4 = Ipv4Address::new(0, 0, 0, 1);
        routes.add_default_ipv4_route(default_gateway_ipv4).unwrap();
        let mut interface_config = Config::default();
        interface_config.random_seed = rand::random::<u64>();
        let mut vpn_device = SmoltcpDevice::new(trans_id);
        let mut interface = Interface::new(interface_config, &mut vpn_device);
        interface.set_any_ip(true);
        interface.update_ip_addrs(|ip_addrs| {
            if let Err(e) = ip_addrs.push(IpCidr::new(IpAddress::v4(0, 0, 0, 1), 0)) {
                error!(">>>> Transportation {trans_id} fail to add ip address to interface in device endpoint because of error: {e:?}")
            }
        });
        interface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(0, 0, 0, 1))
            .map_err(|e| {
                error!(">>>> Transportation {trans_id} fail to add default ipv4 route because of error: {e:?}");
                anyhow!("{e:?}")
            })?;

        Ok((interface, vpn_device))
    }

    fn create_tcp_socket<'a>(trans_id: TransportationId, endpoint: IpEndpoint) -> Option<TcpSocket<'a>> {
        let mut socket = TcpSocket::new(
            TcpSocketBuffer::new(vec![0; 1024 * 1024]),
            TcpSocketBuffer::new(vec![0; 1024 * 1024]),
        );

        if socket.listen(endpoint).is_err() {
            error!(
                ">>>> Transportation {trans_id} failed to listen on smoltcp tcp socket, endpoint=[{}]",
                endpoint
            );
            return None;
        }
        socket.set_ack_delay(None);
        Some(socket)
    }

    fn create_udp_socket<'a>(trans_id: TransportationId, endpoint: IpEndpoint) -> Option<UdpSocket<'a>> {
        let mut socket = UdpSocket::new(
            UdpSocketBuffer::new(
                // vec![UdpPacketMetadata::EMPTY, UdpPacketMetadata::EMPTY],
                vec![PacketMetadata::EMPTY; 1024 * 1024],
                vec![0; 1024 * 1024],
            ),
            UdpSocketBuffer::new(
                // vec![UdpPacketMetadata::EMPTY, UdpPacketMetadata::EMPTY],
                vec![PacketMetadata::EMPTY; 1024 * 1024],
                vec![0; 1024 * 1024],
            ),
        );

        if socket.bind(endpoint).is_err() {
            error!(
                ">>>> Transportation {trans_id} failed to bind smoltcp udp socket, endpoint=[{}]",
                endpoint
            );
            return None;
        }

        Some(socket)
    }

    pub async fn can_send_to_smoltcp(&self) -> bool {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socketset = self.socketset.lock().await;
                let socket = socketset.get::<TcpSocket>(self.socket_handle);
                socket.may_send()
            }
            TransportProtocol::Udp => {
                let socketset = self.socketset.lock().await;
                let socket = socketset.get::<UdpSocket>(self.socket_handle);
                socket.can_send()
            }
        }
    }

    pub async fn send_to_smoltcp(&self, data: &[u8]) -> Result<usize> {
        let trans_id = self.trans_id;
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let mut socketset = self.socketset.lock().await;
                let socket = socketset.get_mut::<TcpSocket>(self.socket_handle);
                debug!(
                    "<<<< Transportation {trans_id} send tcp data to smoltcp stack: {}",
                    pretty_hex::pretty_hex(&data)
                );
                socket.send_slice(data).map_err(|e| {
                    error!("<<<< Transportation {trans_id} fail to send tcp data to smoltcp stack because of error: {e:?}");
                    anyhow!("{e:?}")
                })
            }
            TransportProtocol::Udp => {
                let mut socketset = self.socketset.lock().await;
                let socket = socketset.get_mut::<UdpSocket>(self.socket_handle);
                debug!(
                    "<<<< Transportation {} send udp data to smoltcp stack: {}",
                    self.trans_id,
                    pretty_hex::pretty_hex(&data)
                );
                socket
                    .send_slice(data, self.local_endpoint)
                    .and(Ok(data.len()))
                    .map_err(|e| {
                        error!("<<<< Transportation {trans_id} fail to send udp data to smoltcp stack because of error: {e:?}");
                        anyhow!("{e:?}")
                    })
            }
        }
    }

    pub async fn can_receive_from_smoltcp(&self) -> bool {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socketset = self.socketset.lock().await;
                let socket = socketset.get::<TcpSocket>(self.socket_handle);
                socket.can_recv()
            }
            TransportProtocol::Udp => {
                let socketset = self.socketset.lock().await;
                let socket = socketset.get::<UdpSocket>(self.socket_handle);
                socket.can_recv()
            }
        }
    }

    pub async fn receive_from_smoltcp(&self) -> Result<()> {
        let trans_id = self.trans_id;
        while self.can_receive_from_smoltcp().await {
            match self.transport_protocol {
                TransportProtocol::Tcp => {
                    let mut data = [0u8; 65536];
                    let size = {
                        let mut socketset = self.socketset.lock().await;
                        let socket = socketset.get_mut::<TcpSocket>(self.socket_handle);
                        socket.recv_slice(&mut data).map_err(|e| {
                            error!(">>>> Transportation {trans_id} fail to receive tcp data from smoltcp stack because of error: {e:?}");
                            anyhow!("{e:?}")
                        })?
                    };
                    let data = &data[..size];
                    debug!(
                        ">>>> Transportation {} receive tcp data from smoltcp stack: {}",
                        self.trans_id,
                        pretty_hex::pretty_hex(&data)
                    );
                    let mut local_recv_buf = self.local_recv_buf.lock().await;
                    local_recv_buf.extend(data);
                }
                TransportProtocol::Udp => {
                    let mut data = [0u8; 65536];
                    let size = {
                        let mut socketset = self.socketset.lock().await;
                        let socket = socketset.get_mut::<UdpSocket>(self.socket_handle);
                        socket.recv_slice(&mut data).map(|r| r.0).map_err(|e| {
                            error!(">>>> Transportation {trans_id} fail to receive udp data from smoltcp stack because of error: {e:?}");
                            anyhow!("{e:?}")
                        })?
                    };
                    let data = &data[..size];
                    debug!(
                        ">>>> Transportation {} receive udp data from smoltcp stack: {}",
                        self.trans_id,
                        pretty_hex::pretty_hex(&data)
                    );
                    let mut local_recv_buf = self.local_recv_buf.lock().await;
                    local_recv_buf.extend(data);
                }
            }
        }
        Ok(())
    }

    pub async fn close(&self) {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let mut socketset = self.socketset.lock().await;
                let socket = socketset.get_mut::<TcpSocket>(self.socket_handle);
                socket.close();
            }
            TransportProtocol::Udp => {
                let mut socketset = self.socketset.lock().await;
                let socket = socketset.get_mut::<UdpSocket>(self.socket_handle);
                socket.close();
            }
        }
    }

    pub async fn poll(&self) -> bool {
        let mut interface = self.interface.lock().await;
        let mut device = self.device.lock().await;
        let mut socketset = self.socketset.lock().await;
        interface.poll(Instant::now(), &mut *device, &mut socketset)
    }

    pub async fn push_rx_to_device(&self, rx_data: Vec<u8>) {
        let mut device = self.device.lock().await;
        device.push_rx(rx_data);
    }

    pub async fn pop_tx_from_device(&self) -> Option<Vec<u8>> {
        let mut device = self.device.lock().await;
        device.pop_tx()
    }

    pub(crate) async fn consume_local_recv_buf_with<F, Fut>(&self, trans_id: TransportationId, remote_endpoint: Arc<RemoteEndpoint>, mut consume_fn: F)
    where
        F: FnMut(TransportationId, Arc<RemoteEndpoint>, Vec<u8>) -> Fut,
        Fut: Future<Output = Result<usize>>,
    {
        let mut local_recv_buf = self.local_recv_buf.lock().await;
        if local_recv_buf.is_empty() {
            return;
        }
        match consume_fn(
            trans_id,
            remote_endpoint,
            local_recv_buf.make_contiguous().to_vec(),
        )
        .await
        {
            Ok(consumed) => {
                local_recv_buf.drain(..consumed);
            }
            Err(e) => {
                error!(">>>> Fail to write local receive buffer data because of error: {e:?}")
            }
        }
    }
}
