use crate::error::NetworkError;

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer as TcpSocketBuffer};

use smoltcp::socket::udp::{PacketBuffer as UdpSocketBuffer, PacketMetadata, Socket as UdpSocket};
use smoltcp::wire::IpEndpoint;
use std::net::SocketAddr;

pub(crate) enum TransportProtocol {
    Tcp,
    Udp,
}

pub(crate) struct DeviceEndpoint<'socketset, 'buf> {
    socket_handle: SocketHandle,
    transport_protocol: TransportProtocol,
    local_endpoint: IpEndpoint,
    socketset: &'socketset mut SocketSet<'buf>,
}

impl<'socketset, 'buf> DeviceEndpoint<'socketset, 'buf> {
    pub(crate) fn new(
        transport_protocol: TransportProtocol,
        local_address: SocketAddr,
        remote_address: SocketAddr,
        socketset: &mut SocketSet<'buf>,
    ) -> Option<Self> {
        let local_endpoint = IpEndpoint::from(local_address);
        let remote_endpoint = IpEndpoint::from(remote_address);
        let socket_handle = match transport_protocol {
            TransportProtocol::Tcp => {
                let socket = Self::create_tcp_socket(remote_endpoint)?;
                socketset.add(socket)
            }
            TransportProtocol::Udp => {
                let socket = Self::create_udp_socket(remote_endpoint)?;
                socketset.add(socket)
            }
        };

        let socket = Self {
            socket_handle,
            transport_protocol,
            local_endpoint,
            socketset,
        };

        Some(socket)
    }

    fn create_tcp_socket<'a>(endpoint: IpEndpoint) -> Option<TcpSocket<'a>> {
        let mut socket = TcpSocket::new(
            TcpSocketBuffer::new(vec![0; 1024 * 1024]),
            TcpSocketBuffer::new(vec![0; 1024 * 1024]),
        );

        if socket.listen(endpoint).is_err() {
            log::error!("failed to listen on socket, endpoint=[{}]", endpoint);
            return None;
        }

        socket.set_ack_delay(None);

        Some(socket)
    }

    fn create_udp_socket<'a>(endpoint: IpEndpoint) -> Option<UdpSocket<'a>> {
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
            log::error!("failed to bind socket, endpoint=[{}]", endpoint);
            return None;
        }

        Some(socket)
    }

    pub(crate) fn can_send(&self) -> bool {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socket = self.socketset.get_mut::<TcpSocket>(self.socket_handle);
                socket.may_send()
            }
            TransportProtocol::Udp => {
                let socket = self.socketset.get_mut::<UdpSocket>(self.socket_handle);
                socket.can_send()
            }
        }
    }

    pub(crate) fn send(&mut self, data: &[u8]) -> Result<usize, NetworkError> {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socket = self.socketset.get_mut::<TcpSocket>(self.socket_handle);
                socket
                    .send_slice(data)
                    .map_err(NetworkError::SendTcpDataToDevice)
            }
            TransportProtocol::Udp => {
                let socket = self.socketset.get_mut::<UdpSocket>(self.socket_handle);
                socket
                    .send_slice(data, self.local_endpoint)
                    .and(Ok(data.len()))
                    .map_err(NetworkError::SendUdpDataToDevice)
            }
        }
    }

    pub(crate) fn can_receive(&self) -> bool {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socket = self.socketset.get_mut::<TcpSocket>(self.socket_handle);
                socket.can_recv()
            }
            TransportProtocol::Udp => {
                let socket = self.socketset.get_mut::<UdpSocket>(self.socket_handle);
                socket.can_recv()
            }
        }
    }

    pub(crate) fn receive(&mut self, data: &mut [u8]) -> Result<usize, NetworkError> {
        match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socket = self.socketset.get_mut::<TcpSocket>(self.socket_handle);
                socket
                    .recv_slice(data)
                    .map_err(NetworkError::ReceiveTcpDataFromDevice)
            }
            TransportProtocol::Udp => {
                let socket = self.socketset.get_mut::<UdpSocket>(self.socket_handle);
                socket
                    .recv_slice(data)
                    .and_then(|r| Ok(r.0))
                    .map_err(NetworkError::ReceiveUdpDataFromDevice)
            }
        }
    }

    pub(crate) fn close(&mut self) {
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
}
