use std::{collections::VecDeque, future::Future, io::ErrorKind, os::fd::AsRawFd, sync::Arc};

use crate::protect_socket;

use super::{client::ClientEndpoint, value::InternetProtocol, TransportId};
use anyhow::{anyhow, Result};
use log::{debug, error};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpSocket, TcpStream, UdpSocket},
    sync::{Mutex, Notify},
};

pub(crate) enum RemoteEndpoint {
    Tcp {
        transport_id: TransportId,
        remote_tcp_stream: Mutex<TcpStream>,
        recv_buffer: Arc<Mutex<VecDeque<u8>>>,
        recv_buffer_notify: Arc<Notify>,
    },
    Udp {
        transport_id: TransportId,
        remote_udp_socket: UdpSocket,
        recv_buffer: Arc<Mutex<VecDeque<Vec<u8>>>>,
        recv_buffer_notify: Arc<Notify>,
    },
}

impl RemoteEndpoint {
    pub(crate) async fn new_tcp(transport_id: TransportId) -> Result<(Self, Arc<Notify>)> {
        let tcp_socket = match transport_id.internet_protocol {
            InternetProtocol::Ipv4 => TcpSocket::new_v4()?,
            InternetProtocol::Ipv6 => TcpSocket::new_v6()?,
        };
        let raw_socket_fd = tcp_socket.as_raw_fd();
        protect_socket(raw_socket_fd)?;
        let remote_tcp_stream = tcp_socket.connect(transport_id.destination).await?;
        let recv_buffer_notify = Arc::new(Notify::new());
        Ok((
            Self::Tcp {
                transport_id,
                remote_tcp_stream: Mutex::new(remote_tcp_stream),
                recv_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                recv_buffer_notify: Arc::clone(&recv_buffer_notify),
            },
            recv_buffer_notify,
        ))
    }

    pub(crate) async fn new_udp(transport_id: TransportId) -> Result<(Self, Arc<Notify>)> {
        let remote_udp_socket = UdpSocket::bind(transport_id.source).await?;
        let raw_socket_fd = remote_udp_socket.as_raw_fd();
        protect_socket(raw_socket_fd)?;
        remote_udp_socket.connect(transport_id.destination).await?;
        let recv_buffer_notify = Arc::new(Notify::new());
        Ok((
            Self::Udp {
                transport_id,
                remote_udp_socket,
                recv_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                recv_buffer_notify: Arc::clone(&recv_buffer_notify),
            },
            recv_buffer_notify,
        ))
    }

    pub(crate) async fn read_from_remote(&self) -> Result<bool> {
        match self {
            RemoteEndpoint::Tcp {
                transport_id,
                remote_tcp_stream,
                recv_buffer,
                recv_buffer_notify,
            } => {
                let remote_tcp_stream = remote_tcp_stream.lock().await;
                let mut data = [0u8; 65536];
                match remote_tcp_stream.try_read(&mut data) {
                    Ok(0) => {
                        recv_buffer_notify.notify_waiters();
                        Ok(true)
                    }
                    Ok(size) => {
                        let mut recv_buffer = recv_buffer.lock().await;
                        let remote_data = &data[..size];
                        debug!(
                            "<<<< Transport {transport_id} read remote tcp data to remote receive buffer: {}",
                            pretty_hex::pretty_hex(&remote_data)
                        );
                        recv_buffer.extend(remote_data);
                        recv_buffer_notify.notify_waiters();
                        Ok(false)
                    }
                    Err(e) => {
                        if e.kind() == ErrorKind::WouldBlock {
                            Ok(false)
                        } else {
                            Err(anyhow!("{e:?}"))
                        }
                    }
                }
            }
            RemoteEndpoint::Udp {
                transport_id,
                remote_udp_socket,
                recv_buffer,
                recv_buffer_notify,
            } => {
                let mut data = [0u8; 65536];
                match remote_udp_socket.recv(&mut data).await? {
                    0 => {
                        recv_buffer_notify.notify_waiters();
                        Ok(true)
                    }
                    size => {
                        let mut recv_buffer = recv_buffer.lock().await;
                        let remote_data = &data[..size];
                        debug!(
                            "<<<< Transport {transport_id} read remote udp data to remote receive buffer: {}",
                            pretty_hex::pretty_hex(&remote_data)
                        );
                        recv_buffer.push_back(remote_data.to_vec());
                        recv_buffer_notify.notify_waiters();
                        Ok(true)
                    }
                }
            }
        }
    }

    pub(crate) async fn write_to_remote(&self, data: Vec<u8>) -> Result<usize> {
        match self {
            RemoteEndpoint::Tcp {
                transport_id,
                remote_tcp_stream,
                ..
            } => {
                let mut remote_tcp_stream = remote_tcp_stream.lock().await;
                let write_result = remote_tcp_stream.write(&data).await.map_err(|e| {
                    error!(">>>> Transport {transport_id} fail to write tcp data to remote because of error:{e:?}");
                    anyhow!("{e:?}")
                });
                remote_tcp_stream.flush().await?;
                write_result
            }
            RemoteEndpoint::Udp {
                transport_id,
                remote_udp_socket,
                ..
            } => remote_udp_socket.send(&data).await.map_err(|e| {
                error!(">>>> Transport {transport_id} fail to write tcp data to remote because of error:{e:?}");
                anyhow!("{e:?}")
            }),
        }
    }

    pub(crate) async fn consume_recv_buffer<'buf, F, Fut>(&self, remote: Arc<ClientEndpoint<'buf>>, mut consume_fn: F) -> Result<()>
    where
        F: FnMut(TransportId, Vec<u8>, Arc<ClientEndpoint<'buf>>) -> Fut,
        Fut: Future<Output = Result<usize>>,
    {
        match self {
            RemoteEndpoint::Tcp {
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
            RemoteEndpoint::Udp {
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
}
