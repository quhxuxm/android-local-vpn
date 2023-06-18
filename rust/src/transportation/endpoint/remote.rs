use crate::{
    error::NetworkError,
    protect_socket,
    transportation::{InternetProtocol, TransportProtocol, TransportationId},
};
use log::{debug, error};

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use tokio::{
    io::{AsyncWriteExt, Interest, Ready},
    net::{TcpSocket, TcpStream, UdpSocket},
    sync::RwLock,
};

pub(crate) enum RemoteEndpoint {
    Tcp {
        tcp_stream: RwLock<TcpStream>,
        trans_id: TransportationId,
    },
    Udp {
        udp_socket: UdpSocket,
        trans_id: TransportationId,
    },
}

impl RemoteEndpoint {
    pub(crate) async fn new(
        trans_id: TransportationId,
        transport_protocol: TransportProtocol,
        internet_protocol: InternetProtocol,
        remote_address: SocketAddr,
    ) -> Option<Self> {
        match transport_protocol {
            TransportProtocol::Tcp => match internet_protocol {
                InternetProtocol::Ipv4 => {
                    let remote_tcp_socket = TcpSocket::new_v4().ok()?;
                    let remote_tcp_socket_fd = remote_tcp_socket.as_raw_fd();
                    protect_socket(remote_tcp_socket_fd).ok()?;
                    Some(RemoteEndpoint::Tcp {
                        tcp_stream: RwLock::new(remote_tcp_socket.connect(remote_address).await.ok()?),
                        trans_id,
                    })
                }
                InternetProtocol::Ipv6 => {
                    let remote_tcp_socket = TcpSocket::new_v6().ok()?;
                    let remote_tcp_socket_fd = remote_tcp_socket.as_raw_fd();
                    protect_socket(remote_tcp_socket_fd).ok()?;
                    Some(RemoteEndpoint::Tcp {
                        tcp_stream: RwLock::new(remote_tcp_socket.connect(remote_address).await.ok()?),
                        trans_id,
                    })
                }
            },
            TransportProtocol::Udp => {
                let remote_udp_socket = UdpSocket::bind("0.0.0.0:0").await.ok()?;
                let remote_udp_socket_fd = remote_udp_socket.as_raw_fd();
                protect_socket(remote_udp_socket_fd).ok()?;
                remote_udp_socket.connect(remote_address).await.ok()?;
                Some(RemoteEndpoint::Udp {
                    udp_socket: remote_udp_socket,
                    trans_id,
                })
            }
        }
    }

    pub(crate) async fn poll(&self) -> Result<Ready, NetworkError> {
        match self {
            Self::Tcp { tcp_stream, .. } => tcp_stream
                .read()
                .await
                .ready(Interest::READABLE | Interest::WRITABLE)
                .await
                .map_err(NetworkError::PollSource),
            Self::Udp { udp_socket, .. } => udp_socket
                .ready(Interest::READABLE)
                .await
                .map_err(NetworkError::PollSource),
        }
    }

    pub(crate) async fn close(&self) -> Result<(), NetworkError> {
        match self {
            RemoteEndpoint::Tcp {
                tcp_stream,
                trans_id,
            } => {
                debug!(">>>> Transportation {trans_id} going to shutdown remote tcp stream.");
                tcp_stream
                    .write()
                    .await
                    .shutdown()
                    .await
                    .map_err(NetworkError::DeregisterSource)?
            }
            RemoteEndpoint::Udp { trans_id, .. } => {
                debug!(">>>> Transportation {trans_id} nothing to do for close udp socket.");
            }
        }
        Ok(())
    }

    pub(crate) async fn write(&self, bytes: &[u8]) -> Result<usize, NetworkError> {
        match self {
            Self::Tcp {
                tcp_stream,
                trans_id,
            } => {
                debug!(
                    ">>>> Transportation {trans_id} write data to remote tcp stream: {}",
                    pretty_hex::pretty_hex(&bytes)
                );
                tcp_stream
                    .write()
                    .await
                    .write(bytes)
                    .await
                    .map_err(NetworkError::WriteToRemote)
            }
            Self::Udp {
                udp_socket,
                trans_id,
            } => {
                debug!(
                    ">>>> Transportation {trans_id} write data to remote udp socket: {}",
                    pretty_hex::pretty_hex(&bytes)
                );
                udp_socket
                    .send(bytes)
                    .await
                    .map_err(NetworkError::WriteToRemote)
            }
        }
    }

    pub(crate) async fn read(&self) -> Result<(Vec<Vec<u8>>, bool), NetworkError> {
        let mut bytes: Vec<Vec<u8>> = Vec::new();
        let mut buffer = [0; 65536]; // maximum UDP packet size
        let mut _is_closed = false;
        loop {
            let (read_result, trans_id) = match self {
                Self::Tcp {
                    tcp_stream,
                    trans_id,
                    ..
                } => (tcp_stream.read().await.try_read(&mut buffer), trans_id),
                Self::Udp {
                    udp_socket,
                    trans_id,
                    ..
                } => (udp_socket.try_recv(&mut buffer), trans_id),
            };
            match read_result {
                Ok(0) => {
                    _is_closed = true;
                    break;
                }
                Ok(count) => {
                    // bytes.extend_from_slice(&buffer[..count]);
                    let data = buffer[..count].to_vec();
                    bytes.push(data);
                    continue;
                }
                Err(e) => {
                    if e.kind() == ErrorKind::WouldBlock {
                        continue;
                    } else {
                        error!(">>>> Transportation {trans_id} fail to read data from remote because of error: {e:?}");
                        _is_closed = true;
                        return Err(NetworkError::ReadFromRemote(e));
                    }
                }
            }
        }
        Ok((bytes, _is_closed))
    }
}
