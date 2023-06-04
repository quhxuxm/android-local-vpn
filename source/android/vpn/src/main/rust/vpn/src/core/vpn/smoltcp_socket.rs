// This is free and unencumbered software released into the public domain.
//
// Anyone is free to copy, modify, publish, use, compile, sell, or
// distribute this software, either in source code form or as a compiled
// binary, for any purpose, commercial or non-commercial, and by any
// means.
//
// In jurisdictions that recognize copyright laws, the author or authors
// of this software dedicate any and all copyright interest in the
// software to the public domain. We make this dedication for the benefit
// of the public at large and to the detriment of our heirs and
// successors. We intend this dedication to be an overt act of
// relinquishment in perpetuity of all present and future rights to this
// software under copyright law.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
// IN NO EVENT SHALL THE AUTHORS BE LIABLE FOR ANY CLAIM, DAMAGES OR
// OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
// ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
// OTHER DEALINGS IN THE SOFTWARE.
//
// For more information, please refer to <https://unlicense.org>

use super::{buffers::WriteError, vpn_device::VpnDevice};
use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer as TcpSocketBuffer};

use smoltcp::socket::udp::{PacketBuffer as UdpSocketBuffer, PacketMetadata, Socket as UdpSocket};
use smoltcp::wire::IpEndpoint;
use std::net::SocketAddr;

pub(crate) enum TransportProtocol {
    Tcp,
    Udp,
}

pub(crate) struct Socket {
    socket_handle: SocketHandle,
    transport_protocol: TransportProtocol,
    local_endpoint: IpEndpoint,
}

impl Socket {
    pub(crate) fn new(
        transport_protocol: TransportProtocol,
        local_address: SocketAddr,
        remote_address: SocketAddr,
        interface: &mut Interface,
        vpn_device: &mut VpnDevice,
        sockets: &mut SocketSet,
    ) -> Option<Socket> {
        let local_endpoint = IpEndpoint::from(local_address);

        let remote_endpoint = IpEndpoint::from(remote_address);

        let socket_handle = match transport_protocol {
            TransportProtocol::Tcp => {
                let socket = Self::create_tcp_socket(remote_endpoint).unwrap();
                sockets.add(socket)
            }
            TransportProtocol::Udp => {
                let socket = Self::create_udp_socket(remote_endpoint).unwrap();
                sockets.add(socket)
            }
        };

        let socket = Socket {
            socket_handle,
            transport_protocol,
            local_endpoint,
        };

        Some(socket)
    }

    fn create_tcp_socket<'buf>(endpoint: IpEndpoint) -> Option<TcpSocket<'buf>> {
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

    fn create_udp_socket<'buf>(endpoint: IpEndpoint) -> Option<UdpSocket<'buf>> {
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

    pub(crate) fn get<'socket, 'buf>(&self, sockets: &'socket mut SocketSet<'buf>) -> SocketInstance<'socket, 'buf> {
        let socket = match self.transport_protocol {
            TransportProtocol::Tcp => {
                let socket = sockets.get_mut::<TcpSocket>(self.socket_handle);
                SocketType::Tcp(socket)
            }
            TransportProtocol::Udp => {
                let socket = sockets.get_mut::<UdpSocket>(self.socket_handle);
                SocketType::Udp(socket, self.local_endpoint)
            }
        };

        SocketInstance { instance: socket }
    }
}

pub(crate) struct SocketInstance<'socket, 'buf> {
    instance: SocketType<'socket, 'buf>,
}

enum SocketType<'socket, 'buf> {
    Tcp(&'socket mut TcpSocket<'buf>),
    Udp(&'socket mut UdpSocket<'buf>, IpEndpoint),
}

impl<'socket, 'buf> SocketInstance<'socket, 'buf> {
    pub(crate) fn can_send(&self) -> bool {
        match &self.instance {
            SocketType::Tcp(socket) => socket.may_send(),
            SocketType::Udp(_, _) => true,
        }
    }

    pub(crate) fn send(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        match &mut self.instance {
            SocketType::Tcp(socket) => socket
                .send_slice(data)
                .map_err(|e| WriteError::SmoltcpTcpSendErr(e)),
            SocketType::Udp(socket, local_endpoint) => socket
                .send_slice(data, *local_endpoint)
                .and(Ok(data.len()))
                .map_err(|e| WriteError::SmoltcpUdpSendErr(e)),
        }
    }

    pub(crate) fn can_receive(&self) -> bool {
        match &self.instance {
            SocketType::Tcp(socket) => socket.can_recv(),
            SocketType::Udp(socket, _) => socket.can_recv(),
        }
    }

    pub(crate) fn receive(&mut self, data: &mut [u8]) -> Result<usize, anyhow::Error> {
        match &mut self.instance {
            SocketType::Tcp(socket) => socket
                .recv_slice(data)
                .map_err(|e| anyhow::anyhow!("{e:?}")),
            SocketType::Udp(socket, _) => socket
                .recv_slice(data)
                .and_then(|result| Ok(result.0))
                .map_err(|e| anyhow::anyhow!("{e:?}")),
        }
    }

    pub(crate) fn close(&mut self) {
        match &mut self.instance {
            SocketType::Tcp(socket) => socket.close(),
            SocketType::Udp(socket, _) => socket.close(),
        }
    }
}
