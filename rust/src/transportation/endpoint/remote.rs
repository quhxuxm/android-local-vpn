use crate::{
    protect_socket,
    transportation::{InternetProtocol, TransportProtocol, Transportation, TransportationId},
};
use anyhow::anyhow;
use log::{debug, error};

use anyhow::Result;

use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    os::unix::io::AsRawFd,
    sync::Arc,
};
use std::{future::Future, net::SocketAddr};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpSocket, UdpSocket,
    },
    sync::{Mutex, Notify},
};

use super::LocalEndpoint;

pub(crate) enum RemoteEndpoint {
    Tcp {
        tcp_read: Arc<Mutex<OwnedReadHalf>>,
        tcp_write: Arc<Mutex<OwnedWriteHalf>>,
        remote_recv_buf: Arc<Mutex<VecDeque<u8>>>,
        trans_id: TransportationId,
        able_to_consume_remote_recv_buf_notify: Arc<Notify>,
    },
    Udp {
        udp_socket: Arc<UdpSocket>,
        remote_recv_buf: Arc<Mutex<VecDeque<Vec<u8>>>>,
        trans_id: TransportationId,
        able_to_consume_remote_recv_buf_notify: Arc<Notify>,
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
            TransportProtocol::Tcp => {
                let remote_tcp_socket = match internet_protocol {
                    InternetProtocol::Ipv4 => TcpSocket::new_v4().ok()?,
                    InternetProtocol::Ipv6 => TcpSocket::new_v6().ok()?,
                };
                let remote_tcp_socket_fd = remote_tcp_socket.as_raw_fd();
                protect_socket(remote_tcp_socket_fd).ok()?;
                let tcp_stream = remote_tcp_socket.connect(remote_address).await.ok()?;
                let (tcp_read, tcp_write) = tcp_stream.into_split();
                Some(RemoteEndpoint::Tcp {
                    tcp_read: Arc::new(Mutex::new(tcp_read)),
                    tcp_write: Arc::new(Mutex::new(tcp_write)),
                    remote_recv_buf: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                    trans_id,
                    able_to_consume_remote_recv_buf_notify: Arc::new(Notify::new()),
                })
            }
            TransportProtocol::Udp => {
                let remote_udp_socket = UdpSocket::bind("0.0.0.0:0").await.ok()?;
                let remote_udp_socket_fd = remote_udp_socket.as_raw_fd();
                protect_socket(remote_udp_socket_fd).ok()?;
                remote_udp_socket.connect(remote_address).await.ok()?;
                Some(RemoteEndpoint::Udp {
                    udp_socket: Arc::new(remote_udp_socket),
                    remote_recv_buf: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                    trans_id,
                    able_to_consume_remote_recv_buf_notify: Arc::new(Notify::new()),
                })
            }
        }
    }

    pub(crate) async fn waiting_for_consume_notify(&self) {
        match self {
            RemoteEndpoint::Tcp {
                able_to_consume_remote_recv_buf_notify,
                ..
            } => able_to_consume_remote_recv_buf_notify.notified().await,
            RemoteEndpoint::Udp {
                able_to_consume_remote_recv_buf_notify,
                ..
            } => able_to_consume_remote_recv_buf_notify.notified().await,
        }
    }

    pub(crate) fn start_read_remote(&self, transportations: Arc<Mutex<HashMap<TransportationId, Arc<Transportation<'_>>>>>) {
        match self {
            RemoteEndpoint::Tcp {
                tcp_read,
                remote_recv_buf,
                trans_id,
                able_to_consume_remote_recv_buf_notify,
                ..
            } => {
                let trans_id = *trans_id;
                let tcp_read = Arc::clone(tcp_read);
                let rx_buffer = Arc::clone(remote_recv_buf);
                let able_to_consume_remote_recv_buf_notify = Arc::clone(able_to_consume_remote_recv_buf_notify);
                tokio::spawn(async move {
                    let mut data = [0u8; 65536];
                    loop {
                        let read_result = {
                            let mut tcp_read = tcp_read.lock().await;
                            tcp_read.read(&mut data).await
                        };
                        match read_result {
                            Ok(0) => {
                                let transportation = {
                                    let mut transportations = transportations.lock().await;
                                    transportations.remove(&trans_id)
                                };
                                if let Some(transportation) = transportation {
                                    transportation.close_local_endpoint().await;
                                }
                                break;
                            }
                            Ok(size) => {
                                let data = &data[..size];
                                let mut rx_buffer = rx_buffer.lock().await;
                                rx_buffer.extend(data);
                                able_to_consume_remote_recv_buf_notify.notify_one();
                            }
                            Err(e) => {
                                // if e.kind() == ErrorKind::WouldBlock {
                                //     continue;
                                // }
                                error!("<<<< Transportation {trans_id} fail to read remote endpoint tcp data because of error: {e:?}");
                                let transportation = {
                                    let mut transportations = transportations.lock().await;
                                    transportations.remove(&trans_id)
                                };
                                if let Some(transportation) = transportation {
                                    transportation.close_local_endpoint().await;
                                }
                                break;
                            }
                        };
                    }
                });
            }
            RemoteEndpoint::Udp {
                udp_socket,
                trans_id,
                remote_recv_buf,
                able_to_consume_remote_recv_buf_notify,
                ..
            } => {
                let trans_id = *trans_id;
                let udp_socket = Arc::clone(udp_socket);
                let rx_buffer = Arc::clone(remote_recv_buf);
                let able_to_consume_remote_recv_buf_notify = Arc::clone(able_to_consume_remote_recv_buf_notify);
                tokio::spawn(async move {
                    let mut data = [0u8; 65535];
                    match udp_socket.recv(&mut data).await {
                        Ok(0) => {
                            debug!("<<<< Transportation {trans_id} nothing read from remote endpoint udp data.");
                            let transportation = {
                                let mut transportations = transportations.lock().await;
                                transportations.remove(&trans_id)
                            };
                            if let Some(transportation) = transportation {
                                transportation.close_local_endpoint().await;
                            }
                        }
                        Ok(size) => {
                            let data = &data[..size];
                            let mut rx_buffer = rx_buffer.lock().await;
                            rx_buffer.push_back(data.to_vec());
                            able_to_consume_remote_recv_buf_notify.notify_one();
                        }
                        Err(e) => {
                            error!("<<<< Transportation {trans_id} fail to read remote endpoint udp data because of error: {e:?}");
                            let transportation = {
                                let mut transportations = transportations.lock().await;
                                transportations.remove(&trans_id)
                            };
                            if let Some(transportation) = transportation {
                                transportation.close_local_endpoint().await;
                            }
                        }
                    };
                });
            }
        }
    }

    pub(crate) async fn close(&self) -> Result<()> {
        match self {
            RemoteEndpoint::Tcp {
                tcp_write,
                trans_id,
                ..
            } => {
                debug!(">>>> Transportation {trans_id} going to close remote tcp stream.");
                tcp_write.lock().await.shutdown().await.map_err(|e| {
                    error!(">>>> Transportation {trans_id} fail to close remote tcp stream because of error: {e:?}");
                    anyhow!(e)
                })?
            }
            RemoteEndpoint::Udp { trans_id, .. } => {
                debug!(">>>> Transportation {trans_id} nothing to do for close remote udp socket.");
            }
        }
        Ok(())
    }

    pub(crate) async fn write_to_remote(&self, bytes: &[u8]) -> Result<usize> {
        match self {
            Self::Tcp {
                tcp_write,
                trans_id,
                ..
            } => {
                debug!(
                    ">>>> Transportation {trans_id} write data to remote tcp stream: {}",
                    pretty_hex::pretty_hex(&bytes)
                );
                Ok(tcp_write.lock().await.write(bytes).await?)
            }
            Self::Udp {
                udp_socket,
                trans_id,
                ..
            } => {
                debug!(
                    ">>>> Transportation {trans_id} write data to remote udp socket: {}",
                    pretty_hex::pretty_hex(&bytes)
                );
                Ok(udp_socket.send(bytes).await?)
            }
        }
    }

    pub(crate) async fn consume_remote_recv_buf_with<'buf, F, Fut>(
        &self,
        trans_id: TransportationId,
        client_file_write: Arc<Mutex<File>>,
        local_endpoint: Arc<LocalEndpoint<'buf>>,
        mut consume_fn: F,
    ) where
        F: FnMut(TransportationId, Arc<Mutex<File>>, Arc<LocalEndpoint<'buf>>, Vec<u8>) -> Fut,
        Fut: Future<Output = Result<usize>>,
    {
        match self {
            Self::Tcp {
                remote_recv_buf, ..
            } => {
                let mut remote_recv_buf = remote_recv_buf.lock().await;
                if remote_recv_buf.is_empty() {
                    return;
                }
                match consume_fn(
                    trans_id,
                    client_file_write,
                    local_endpoint,
                    remote_recv_buf.make_contiguous().to_vec(),
                )
                .await
                {
                    Ok(consumed) => {
                        remote_recv_buf.drain(..consumed);
                    }
                    Err(e) => {
                        error!(">>>> Fail to consume remote receive buffer data for tcp because of error: {e:?}");
                    }
                }
            }
            Self::Udp {
                remote_recv_buf, ..
            } => {
                let mut remote_recv_buf = remote_recv_buf.lock().await;
                if remote_recv_buf.is_empty() {
                    return;
                }
                let mut consumed: usize = 0;
                // write udp packets one by one
                for datagram in remote_recv_buf.make_contiguous() {
                    if let Err(e) = consume_fn(
                        trans_id,
                        Arc::clone(&client_file_write),
                        Arc::clone(&local_endpoint),
                        datagram.to_owned(),
                    )
                    .await
                    {
                        error!(">>>> Fail to consume remote receive buffer data for udp because of error: {e:?}");
                        break;
                    }
                    consumed += 1;
                }
                remote_recv_buf.drain(..consumed);
            }
        }
    }
}
