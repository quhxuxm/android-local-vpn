use std::{collections::VecDeque, future::Future, os::fd::AsRawFd, sync::Arc};

use anyhow::{anyhow, Result};
use log::{debug, error};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpSocket, UdpSocket,
    },
    sync::{Mutex, Notify},
};

use crate::protect_socket;
use crate::transport::ControlProtocol;

use super::{client::ClientEndpoint, value::InternetProtocol, TransportId};

const PROXY_ADDRESS: &str = "64.176.193.76:80";

pub(crate) enum RemoteEndpoint {
    Tcp {
        transport_id: TransportId,
        remote_tcp_stream_read: Mutex<OwnedReadHalf>,
        remote_tcp_stream_write: Mutex<OwnedWriteHalf>,
        recv_buffer: Arc<Mutex<VecDeque<u8>>>,
        recv_buffer_notify: Arc<Notify>,
        closed: Mutex<bool>,
    },
    Udp {
        transport_id: TransportId,
        remote_udp_socket: UdpSocket,
        recv_buffer: Arc<Mutex<VecDeque<Vec<u8>>>>,
        recv_buffer_notify: Arc<Notify>,
        closed: Mutex<bool>,
    },
}

impl RemoteEndpoint {
    pub(crate) async fn new(transport_id: TransportId) -> Result<(Self, Arc<Notify>)> {
        match transport_id.control_protocol {
            ControlProtocol::Tcp => Self::new_tcp(transport_id).await,
            ControlProtocol::Udp => Self::new_udp(transport_id).await,
        }
    }

    async fn new_tcp(transport_id: TransportId) -> Result<(Self, Arc<Notify>)> {
        let tcp_socket = match transport_id.internet_protocol {
            InternetProtocol::Ipv4 => TcpSocket::new_v4()?,
            InternetProtocol::Ipv6 => TcpSocket::new_v6()?,
        };
        let raw_socket_fd = tcp_socket.as_raw_fd();
        protect_socket(raw_socket_fd)?;
        let remote_tcp_stream = tcp_socket.connect(transport_id.destination).await?;
        let (remote_tcp_stream_read, remote_tcp_stream_write) = remote_tcp_stream.into_split();
        let recv_buffer_notify = Arc::new(Notify::new());
        Ok((
            Self::Tcp {
                transport_id,
                remote_tcp_stream_read: Mutex::new(remote_tcp_stream_read),
                remote_tcp_stream_write: Mutex::new(remote_tcp_stream_write),
                recv_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                recv_buffer_notify: Arc::clone(&recv_buffer_notify),
                closed: Mutex::new(false),
            },
            recv_buffer_notify,
        ))
    }

    async fn new_udp(transport_id: TransportId) -> Result<(Self, Arc<Notify>)> {
        let remote_udp_socket = UdpSocket::bind("0.0.0.0:0").await?;
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
                closed: Mutex::new(false),
            },
            recv_buffer_notify,
        ))
    }

    pub(crate) async fn init_proxy_connection(&self) -> Result<()> {
        todo!()
    }

    pub(crate) async fn read_from_remote(&self) -> Result<bool> {
        match self {
            Self::Tcp {
                transport_id,
                remote_tcp_stream_read,
                recv_buffer,
                recv_buffer_notify,
                ..
            } => {
                let mut remote_tcp_stream_read = remote_tcp_stream_read.lock().await;
                let mut data = [0u8; 65536];
                match remote_tcp_stream_read.read(&mut data).await {
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
                        error!("<<<< Transport {transport_id} fail to read remote tcp data because of error: {e:?}");
                        Err(anyhow!("{e:?}"))
                    }
                }
            }
            Self::Udp {
                transport_id,
                remote_udp_socket,
                recv_buffer,
                recv_buffer_notify,
                ..
            } => {
                let mut data = [0u8; 65536];
                match remote_udp_socket.recv(&mut data).await {
                    Ok(0) => {
                        recv_buffer_notify.notify_waiters();
                        Ok(true)
                    }
                    Ok(size) => {
                        let mut recv_buffer = recv_buffer.lock().await;
                        let remote_data = &data[..size];
                        debug!(
                            "<<<< Transport {transport_id} read remote udp data to remote receive buffer: {}",
                            pretty_hex::pretty_hex(&remote_data)
                        );
                        recv_buffer.push_back(remote_data.to_vec());
                        recv_buffer_notify.notify_waiters();
                        Ok(false)
                    }
                    Err(e) => {
                        error!("<<<< Transport {transport_id} fail to read remote udp data because of error: {e:?}");
                        Err(anyhow!("{e:?}"))
                    }
                }
            }
        }
    }

    pub(crate) async fn write_to_remote(&self, data: Vec<u8>) -> Result<usize> {
        match self {
            Self::Tcp {
                transport_id,
                remote_tcp_stream_write,
                ..
            } => {
                let mut remote_tcp_stream_write = remote_tcp_stream_write.lock().await;
                let write_result = remote_tcp_stream_write.write(&data).await.map_err(|e| {
                    error!(">>>> Transport {transport_id} fail to write tcp data to remote because of error:{e:?}");
                    anyhow!("{e:?}")
                });
                remote_tcp_stream_write.flush().await?;
                write_result
            }
            Self::Udp {
                transport_id,
                remote_udp_socket,
                ..
            } => remote_udp_socket.send(&data).await.map_err(|e| {
                error!(">>>> Transport {transport_id} fail to write tcp data to remote because of error:{e:?}");
                anyhow!("{e:?}")
            }),
        }
    }

    pub(crate) async fn consume_recv_buffer<'buf, F, Fut>(&self, remote: Arc<ClientEndpoint<'buf>>, mut consume_fn: F) -> Result<bool>
    where
        F: FnMut(TransportId, Vec<u8>, Arc<ClientEndpoint<'buf>>) -> Fut,
        Fut: Future<Output = Result<usize>>,
    {
        match self {
            Self::Tcp {
                transport_id,
                recv_buffer,
                closed,
                ..
            } => {
                let mut recv_buffer = recv_buffer.lock().await;
                if recv_buffer.len() == 0 {
                    let closed = closed.lock().await;
                    return Ok(*closed);
                }
                let consume_size = consume_fn(
                    *transport_id,
                    recv_buffer.make_contiguous().to_vec(),
                    remote,
                )
                .await?;
                recv_buffer.drain(..consume_size);
                Ok(false)
            }
            Self::Udp {
                transport_id,
                recv_buffer,
                closed,
                ..
            } => {
                let mut consume_size = 0;
                let mut recv_buffer = recv_buffer.lock().await;
                if recv_buffer.len() == 0 {
                    let closed = closed.lock().await;
                    return Ok(*closed);
                }
                for udp_data in recv_buffer.iter() {
                    consume_fn(*transport_id, udp_data.to_vec(), Arc::clone(&remote)).await?;
                    consume_size += 1;
                }
                recv_buffer.drain(..consume_size);
                Ok(false)
            }
        }
    }

    pub(crate) async fn close(&self) {
        match self {
            Self::Tcp {
                transport_id,
                remote_tcp_stream_write,
                recv_buffer_notify,
                closed,
                ..
            } => {
                let mut remote_tcp_stream_write = remote_tcp_stream_write.lock().await;
                if let Err(e) = remote_tcp_stream_write.shutdown().await {
                    error!(">>>> Transport {transport_id} fail to close remote endpoint because of error: {e:?}")
                };
                recv_buffer_notify.notify_waiters();
                let mut closed = closed.lock().await;
                *closed = true;
            }
            Self::Udp {
                recv_buffer_notify,
                closed,
                ..
            } => {
                recv_buffer_notify.notify_waiters();
                let mut closed = closed.lock().await;
                *closed = true;
            }
        }
    }
}
