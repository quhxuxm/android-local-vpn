use crate::{
    protect_socket,
    transportation::{InternetProtocol, TransportProtocol, TransportationId},
};
use log::{debug, error};
use mio::net::{TcpStream, UdpSocket};
use mio::{Interest, Poll, Token};
use std::io::{ErrorKind, Read, Result, Write};
use std::net::{Shutdown, SocketAddr};
use std::os::unix::io::{AsRawFd, FromRawFd};

pub(crate) enum RemoteEndpoint {
    Tcp {
        _socket: socket2::Socket, // Need to retain so socket does not get closed.
        tcp_stream: TcpStream,
        trans_id: TransportationId,
    },
    Udp {
        _socket: socket2::Socket, // Need to retain so socket does not get closed.
        udp_socket: UdpSocket,
        trans_id: TransportationId,
    },
}

impl RemoteEndpoint {
    pub(crate) fn new(
        trans_id: TransportationId,
        transport_protocol: TransportProtocol,
        internet_protocol: InternetProtocol,
        remote_address: SocketAddr,
    ) -> Option<Self> {
        let socket = Self::create_socket(trans_id, &transport_protocol, &internet_protocol).ok()?;

        let socket_address = socket2::SockAddr::from(remote_address);

        debug!(
            ">>>> Transportation {trans_id} connecting to remote, address={:?}",
            remote_address
        );

        match socket.connect(&socket_address) {
            Ok(_) => {
                debug!(
                    ">>>> Transportation {trans_id} success connected to remote, address={:?}",
                    remote_address
                );
            }
            Err(error) => {
                if error.kind() == ErrorKind::WouldBlock || error.raw_os_error() == Some(libc::EINPROGRESS) {
                    // do nothing.
                } else {
                    error!(
                        ">>>> Transportation {trans_id} fail connect to remote [{:?}] because of error: {error:?}",
                        remote_address
                    );
                    return None;
                }
            }
        }

        match transport_protocol {
            TransportProtocol::Tcp => {
                let tcp_stream = unsafe { TcpStream::from_raw_fd(socket.as_raw_fd()) };
                Some(Self::Tcp {
                    trans_id,
                    _socket: socket,
                    tcp_stream,
                })
            }
            TransportProtocol::Udp => {
                let udp_socket = unsafe { UdpSocket::from_raw_fd(socket.as_raw_fd()) };
                Some(Self::Udp {
                    trans_id,
                    _socket: socket,
                    udp_socket,
                })
            }
        }
    }

    pub(crate) fn register_poll(&mut self, poll: &mut Poll, token: Token) -> Result<()> {
        match self {
            Self::Tcp { tcp_stream, .. } => {
                let interests = Interest::READABLE | Interest::WRITABLE;
                poll.registry().register(tcp_stream, token, interests)
            }
            Self::Udp { udp_socket, .. } => {
                let interests = Interest::READABLE;
                poll.registry().register(udp_socket, token, interests)
            }
        }
    }

    pub(crate) fn deregister_poll(&mut self, poll: &mut Poll) -> Result<()> {
        match self {
            Self::Tcp { tcp_stream, .. } => poll.registry().deregister(tcp_stream),
            Self::Udp { udp_socket, .. } => poll.registry().deregister(udp_socket),
        }
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<usize> {
        match self {
            Self::Tcp { tcp_stream, .. } => tcp_stream.write(bytes),
            Self::Udp { udp_socket, .. } => udp_socket.send(bytes),
        }
    }

    pub(crate) fn read(&mut self) -> Result<(Vec<Vec<u8>>, bool)> {
        let mut bytes: Vec<Vec<u8>> = Vec::new();
        let mut buffer = [0; 1 << 16]; // maximum UDP packet size
        let mut is_closed = false;
        loop {
            let read_result = match self {
                Self::Tcp { tcp_stream, .. } => tcp_stream.read(&mut buffer),
                Self::Udp { udp_socket, .. } => udp_socket.recv(&mut buffer),
            };
            match read_result {
                Ok(count) => {
                    if count == 0 {
                        is_closed = true;
                        break;
                    }
                    // bytes.extend_from_slice(&buffer[..count]);
                    let data = buffer[..count].to_vec();
                    bytes.push(data)
                }
                Err(error_code) => {
                    if error_code.kind() == ErrorKind::WouldBlock {
                        break;
                    } else {
                        return Err(error_code);
                    }
                }
            }
        }
        Ok((bytes, is_closed))
    }

    pub(crate) fn close(&self) {
        match &self {
            Self::Tcp {
                tcp_stream,
                trans_id,
                ..
            } => {
                if let Err(e) = tcp_stream.shutdown(Shutdown::Both) {
                    error!(">>>> Transportation {trans_id} failed to shutdown remote tcp stream because of error: {e:?}",);
                }
                debug!(">>>> Transportation {trans_id} close tcp stream.");
            }
            Self::Udp { trans_id, .. } => {
                debug!(">>>> Transportation {trans_id} close udp socket.");
            }
        }
    }

    fn create_socket(trans_id: TransportationId, transport_protocol: &TransportProtocol, internet_protocol: &InternetProtocol) -> Result<socket2::Socket> {
        let domain = match internet_protocol {
            InternetProtocol::Ipv4 => socket2::Domain::IPV4,
            InternetProtocol::Ipv6 => socket2::Domain::IPV6,
        };

        let protocol = match transport_protocol {
            TransportProtocol::Tcp => socket2::Protocol::TCP,
            TransportProtocol::Udp => socket2::Protocol::UDP,
        };

        let socket_type = match transport_protocol {
            TransportProtocol::Tcp => socket2::Type::STREAM,
            TransportProtocol::Udp => socket2::Type::DGRAM,
        };

        let socket = socket2::Socket::new(domain, socket_type, Some(protocol))?;

        socket.set_nonblocking(true)?;
        if let Err(e) = protect_socket(socket.as_raw_fd()) {
            error!(">>>> Transportation {trans_id} fail to protect outbound socket because of error: {e:?}")
        };
        Ok(socket)
    }
}
