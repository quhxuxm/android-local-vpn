use crate::{
    device::PpaassVpnDevice,
    error::NetworkError,
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

use smoltcp::socket::udp::{PacketBuffer as UdpSocketBuffer, PacketMetadata, Socket as UdpSocket};
use smoltcp::wire::IpEndpoint;
use std::net::SocketAddr;

pub struct DeviceEndpoint<'buf> {
    socket_handle: SocketHandle,
    transport_protocol: TransportProtocol,
    local_endpoint: IpEndpoint,
    socketset: SocketSet<'buf>,
    interface: Interface,
    device: PpaassVpnDevice,
    trans_id: TransportationId,
}

impl<'buf> DeviceEndpoint<'buf> {
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
            socketset,
            interface,
            device,
        };

        Some(socket)
    }

    fn prepare_iface_and_device(trans_id: TransportationId) -> Result<(Interface, PpaassVpnDevice), NetworkError> {
        let mut routes = Routes::new();
        let default_gateway_ipv4 = Ipv4Address::new(0, 0, 0, 1);
        routes.add_default_ipv4_route(default_gateway_ipv4).unwrap();
        let mut interface_config = Config::default();
        interface_config.random_seed = rand::random::<u64>();
        let mut vpn_device = PpaassVpnDevice::new(trans_id);
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
                NetworkError::DeviceEndpointCreation
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

    pub fn can_send(&self) -> bool {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socket = self.socketset.get::<TcpSocket>(self.socket_handle);
                socket.may_send()
            }
            TransportProtocol::Udp => {
                let socket = self.socketset.get::<UdpSocket>(self.socket_handle);
                socket.can_send()
            }
        }
    }

    pub fn send(&mut self, data: &[u8]) -> Result<usize, NetworkError> {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socket = self.socketset.get_mut::<TcpSocket>(self.socket_handle);
                debug!(
                    "<<<< Transportation {} send tcp data to smoltcp stack: {}",
                    self.trans_id,
                    pretty_hex::pretty_hex(&data)
                );
                socket
                    .send_slice(data)
                    .map_err(NetworkError::SendTcpDataToDevice)
            }
            TransportProtocol::Udp => {
                let socket = self.socketset.get_mut::<UdpSocket>(self.socket_handle);
                debug!(
                    "<<<< Transportation {} send udp data to smoltcp stack: {}",
                    self.trans_id,
                    pretty_hex::pretty_hex(&data)
                );
                socket
                    .send_slice(data, self.local_endpoint)
                    .and(Ok(data.len()))
                    .map_err(NetworkError::SendUdpDataToDevice)
            }
        }
    }

    pub fn can_receive(&self) -> bool {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socket = self.socketset.get::<TcpSocket>(self.socket_handle);
                socket.can_recv()
            }
            TransportProtocol::Udp => {
                let socket = self.socketset.get::<UdpSocket>(self.socket_handle);
                socket.can_recv()
            }
        }
    }

    pub fn receive(&mut self, data: &mut [u8]) -> Result<usize, NetworkError> {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socket = self.socketset.get_mut::<TcpSocket>(self.socket_handle);

                let result = socket
                    .recv_slice(data)
                    .map_err(NetworkError::ReceiveTcpDataFromDevice);

                debug!(
                    ">>>> Transportation {} receive tcp data from smoltcp stack: {}",
                    self.trans_id,
                    pretty_hex::pretty_hex(&data)
                );
                result
            }
            TransportProtocol::Udp => {
                let socket = self.socketset.get_mut::<UdpSocket>(self.socket_handle);
                let result = socket
                    .recv_slice(data)
                    .map(|r| r.0)
                    .map_err(NetworkError::ReceiveUdpDataFromDevice);
                debug!(
                    ">>>> Transportation {} receive udp data from smoltcp stack: {}",
                    self.trans_id,
                    pretty_hex::pretty_hex(&data)
                );
                result
            }
        }
    }

    pub fn close(&mut self) {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socket = self.socketset.get_mut::<TcpSocket>(self.socket_handle);
                socket.close();
            }
            TransportProtocol::Udp => {
                let socket = self.socketset.get_mut::<UdpSocket>(self.socket_handle);
                socket.close();
            }
        }
    }

    pub fn poll(&mut self) -> bool {
        self.interface
            .poll(Instant::now(), &mut self.device, &mut self.socketset)
    }

    pub fn push_rx_to_device(&mut self, rx_data: Vec<u8>) {
        self.device.push_rx(rx_data);
    }

    pub fn pop_tx_from_device(&mut self) -> Option<Vec<u8>> {
        self.device.pop_tx()
    }
}
