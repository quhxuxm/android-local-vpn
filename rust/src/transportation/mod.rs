mod common;
mod value;

use crate::device::SmoltcpDevice;
use crate::protect_socket;
use crate::types::TransportationsRepository;
use crate::util::log_ip_packet;
use log::{debug, error, trace};

use std::os::fd::AsRawFd;
use std::{collections::VecDeque, sync::Arc};
use std::{fs::File, io::Write, sync::atomic::AtomicBool};

use crate::transportation::common::{create_smoltcp_tcp_socket, create_smoltcp_udp_socket, prepare_smoltcp_iface_and_device};
use anyhow::{anyhow, Result};
use pretty_hex::pretty_hex;
use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::socket::tcp::Socket as SmoltcpTcpSocket;
use smoltcp::socket::udp::Socket as SmoltcpUdpSocket;
use smoltcp::time::Instant;
use smoltcp::wire::IpEndpoint;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpSocket, UdpSocket};
use tokio::sync::{Mutex, Notify};

pub(crate) use self::value::InternetProtocol;
pub(crate) use self::value::TransportProtocol;
pub(crate) use self::value::TransportationId;

pub(crate) enum Transportation<'buf>
where
    'buf: 'static,
{
    Tcp {
        trans_id: TransportationId,
        smoltcp_socket_handle: SocketHandle,

        smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
        smoltcp_interface: Arc<Mutex<Interface>>,
        smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
        smoltcp_recv_buf: Arc<Mutex<VecDeque<u8>>>,
        smoltcp_recv_buf_notify: Arc<Notify>,
        remote_tcp_read: Arc<Mutex<OwnedReadHalf>>,
        remote_tcp_write: Arc<Mutex<OwnedWriteHalf>>,
        remote_recv_buf: Arc<Mutex<VecDeque<u8>>>,
        remote_recv_buf_notify: Arc<Notify>,
        client_file_write: Arc<Mutex<File>>,
        closed: AtomicBool,
    },
    Udp {
        trans_id: TransportationId,
        smoltcp_socket_handle: SocketHandle,

        smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
        smoltcp_interface: Arc<Mutex<Interface>>,
        smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
        smoltcp_recv_buf: Arc<Mutex<VecDeque<u8>>>,
        smoltcp_recv_buf_notify: Arc<Notify>,
        remote_recv_buf: Arc<Mutex<VecDeque<Vec<u8>>>>,
        remote_recv_buf_notify: Arc<Notify>,
        remote_udp_socket: Arc<UdpSocket>,
        client_file_write: Arc<Mutex<File>>,
        closed: AtomicBool,
    },
}

impl<'buf> Transportation<'buf>
where
    'buf: 'static,
{
    pub(crate) async fn new(trans_id: TransportationId, client_file_write: Arc<Mutex<File>>) -> Option<Transportation<'buf>> {
        let (smoltcp_interface, smoltcp_device) = prepare_smoltcp_iface_and_device(trans_id).ok()?;
        let mut smoltcp_socket_set = SocketSet::new(vec![]);
        let remote_endpoint = IpEndpoint::from(trans_id.destination);

        match trans_id.transport_protocol {
            TransportProtocol::Tcp => {
                let socket = create_smoltcp_tcp_socket(trans_id, remote_endpoint)?;
                let smoltcp_socket_handle = smoltcp_socket_set.add(socket);
                let remote_tcp_socket = match trans_id.internet_protocol {
                    InternetProtocol::Ipv4 => TcpSocket::new_v4().ok()?,
                    InternetProtocol::Ipv6 => TcpSocket::new_v6().ok()?,
                };
                let remote_tcp_socket_fd = remote_tcp_socket.as_raw_fd();
                protect_socket(remote_tcp_socket_fd).ok()?;
                let remote_tcp_stream = match remote_tcp_socket.connect(trans_id.destination).await {
                    Ok(tcp_stream) => tcp_stream,
                    Err(e) => {
                        error!(
                            ">>>> Transportation {trans_id} fail to connect remote address [{}] for tcp because of error: {e:}",
                            trans_id.destination
                        );
                        return None;
                    }
                };
                let (remote_tcp_read, remote_tcp_write) = remote_tcp_stream.into_split();
                Some(Transportation::Tcp {
                    trans_id,
                    client_file_write,
                    smoltcp_socket_handle,

                    smoltcp_socket_set: Arc::new(Mutex::new(smoltcp_socket_set)),
                    smoltcp_recv_buf_notify: Arc::new(Default::default()),
                    smoltcp_interface: Arc::new(Mutex::new(smoltcp_interface)),
                    smoltcp_device: Arc::new(Mutex::new(smoltcp_device)),
                    smoltcp_recv_buf: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                    remote_tcp_read: Arc::new(Mutex::new(remote_tcp_read)),
                    remote_tcp_write: Arc::new(Mutex::new(remote_tcp_write)),
                    remote_recv_buf: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                    remote_recv_buf_notify: Arc::new(Default::default()),
                    closed: Default::default(),
                })
            }
            TransportProtocol::Udp => {
                let socket = create_smoltcp_udp_socket(trans_id, remote_endpoint)?;
                let smoltcp_socket_handle = smoltcp_socket_set.add(socket);
                let remote_udp_socket = UdpSocket::bind("0.0.0.0:0").await.ok()?;
                let remote_udp_socket_fd = remote_udp_socket.as_raw_fd();
                protect_socket(remote_udp_socket_fd).ok()?;
                if let Err(e) = remote_udp_socket.connect(trans_id.destination).await {
                    error!(
                        ">>>> Transportation {trans_id} fail to connect remote address [{}] for udp because of error: {e:}",
                        trans_id.destination
                    );
                    return None;
                };
                Some(Transportation::Udp {
                    trans_id,
                    client_file_write,
                    smoltcp_socket_handle,

                    smoltcp_socket_set: Arc::new(Mutex::new(smoltcp_socket_set)),
                    smoltcp_recv_buf_notify: Arc::new(Default::default()),
                    smoltcp_interface: Arc::new(Mutex::new(smoltcp_interface)),
                    smoltcp_device: Arc::new(Mutex::new(smoltcp_device)),
                    smoltcp_recv_buf: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                    remote_recv_buf: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
                    remote_recv_buf_notify: Arc::new(Default::default()),
                    remote_udp_socket: Arc::new(remote_udp_socket),
                    closed: Default::default(),
                })
            }
        }
    }

    pub(crate) async fn start(&self) -> Result<()> {
        match self {
            Self::Tcp {
                trans_id,
                client_file_write,
                smoltcp_socket_set,
                smoltcp_device,
                smoltcp_interface,
                smoltcp_socket_handle,
                smoltcp_recv_buf,
                smoltcp_recv_buf_notify,
                remote_tcp_read,
                remote_tcp_write,
                remote_recv_buf,
                remote_recv_buf_notify,
                ..
            } => {
                {
                    let smoltcp_recv_buf = Arc::clone(smoltcp_recv_buf);
                    let remote_tcp_write = Arc::clone(remote_tcp_write);
                    let smoltcp_recv_buf_notify = Arc::clone(smoltcp_recv_buf_notify);
                    tokio::spawn(async move {
                        loop {
                            smoltcp_recv_buf_notify.notified().await;
                            let mut smoltcp_recv_buf = smoltcp_recv_buf.lock().await;
                            if smoltcp_recv_buf.is_empty() {
                                continue;
                            }
                            let data_to_remote = smoltcp_recv_buf.make_contiguous();

                            match remote_tcp_write.lock().await.write(data_to_remote).await {
                                Ok(consumed) => {
                                    smoltcp_recv_buf.drain(..consumed);
                                }
                                Err(e) => {
                                    error!(">>>> Fail to write local receive buffer data because of error: {e:?}")
                                }
                            }
                        }
                    });
                }
                {
                    let trans_id = *trans_id;
                    let remote_recv_buf_notify = Arc::clone(remote_recv_buf_notify);
                    let remote_recv_buf = Arc::clone(remote_recv_buf);
                    let remote_tcp_read = Arc::clone(remote_tcp_read);

                    // Spawn a task to read remote data to remote receive buffer.
                    tokio::spawn(async move {
                        let mut remote_data_buf = [0u8; 65535];
                        loop {
                            let mut remote_tcp_read = remote_tcp_read.lock().await;
                            match remote_tcp_read.read(&mut remote_data_buf).await {
                                Ok(0) => {
                                    debug!("<<<< Transportation {trans_id} complete to read remote tcp data");
                                    remote_recv_buf_notify.notify_one();
                                    break;
                                }
                                Ok(size) => {
                                    let remote_data_buf = &remote_data_buf[..size];
                                    let mut remote_recv_buf = remote_recv_buf.lock().await;
                                    remote_recv_buf.extend(remote_data_buf);
                                    debug!(
                                        "<<<< Transportation {trans_id} read remote tcp data, current remote receive buffer size: {}, current remote data: {}",
                                        remote_recv_buf.len(),
                                        pretty_hex(&remote_data_buf)
                                    );
                                    remote_recv_buf_notify.notify_one();
                                }
                                Err(e) => {
                                    error!("<<<< Transportation {trans_id} fail to read remote tcp data because of error: {e:?}");
                                    remote_recv_buf_notify.notify_one();
                                    break;
                                }
                            };
                        }
                    });
                }
                // Spawn a task to transfer remote receive data buffer to client.
                let trans_id = *trans_id;
                let smoltcp_socket_handle = *smoltcp_socket_handle;
                let remote_recv_buf_notify = Arc::clone(remote_recv_buf_notify);
                let client_file_write = Arc::clone(client_file_write);
                let smoltcp_socket_set = Arc::clone(smoltcp_socket_set);
                let smoltcp_interface = Arc::clone(smoltcp_interface);
                let smoltcp_device = Arc::clone(smoltcp_device);
                let remote_recv_buf = Arc::clone(remote_recv_buf);

                tokio::spawn(async move {
                    loop {
                        remote_recv_buf_notify.notified().await;

                        let mut remote_recv_buf = remote_recv_buf.lock().await;
                        if let Err(e) = Self::consume_tcp_remote_recv_buf(
                            trans_id,
                            &mut remote_recv_buf,
                            Arc::clone(&client_file_write),
                            smoltcp_socket_handle,
                            Arc::clone(&smoltcp_socket_set),
                            Arc::clone(&smoltcp_interface),
                            Arc::clone(&smoltcp_device),
                        )
                        .await
                        {
                            error!("<<<< Transportation {trans_id} fail to consume tcp remote receive buffer because of error: {e:?}")
                        };
                    }
                });
            }
            Self::Udp {
                trans_id,
                client_file_write,
                smoltcp_socket_set,
                smoltcp_device,
                smoltcp_interface,
                smoltcp_socket_handle,
                smoltcp_recv_buf,
                smoltcp_recv_buf_notify,
                remote_udp_socket,
                remote_recv_buf,
                remote_recv_buf_notify,
                ..
            } => {
                {
                    let smoltcp_recv_buf = Arc::clone(smoltcp_recv_buf);
                    let remote_udp_socket = Arc::clone(remote_udp_socket);
                    let smoltcp_recv_buf_notify = Arc::clone(smoltcp_recv_buf_notify);
                    tokio::spawn(async move {
                        loop {
                            smoltcp_recv_buf_notify.notified().await;
                            let mut smoltcp_recv_buf = smoltcp_recv_buf.lock().await;
                            if smoltcp_recv_buf.is_empty() {
                                continue;
                            }
                            let data_to_remote = smoltcp_recv_buf.make_contiguous();

                            match remote_udp_socket.send(data_to_remote).await {
                                Ok(consumed) => {
                                    smoltcp_recv_buf.drain(..consumed);
                                }
                                Err(e) => {
                                    error!(">>>> Fail to write local receive buffer data because of error: {e:?}")
                                }
                            }
                        }
                    });
                }
                {
                    let remote_udp_socket = Arc::clone(remote_udp_socket);
                    let remote_recv_buffer = Arc::clone(remote_recv_buf);
                    let remote_recv_buf_notify = Arc::clone(remote_recv_buf_notify);
                    let trans_id = *trans_id;

                    // Spawn a task to transfer remote data to buffer.
                    tokio::spawn(async move {
                        let mut remote_data_buf = [0u8; 65535];
                        match remote_udp_socket.recv(&mut remote_data_buf).await {
                            Ok(0) => {
                                debug!("<<<< Transportation {trans_id} complete to read remote udp data");
                                remote_recv_buf_notify.notify_one();
                            }
                            Ok(size) => {
                                let remote_data_buf = &remote_data_buf[..size];
                                let mut remote_recv_buffer = remote_recv_buffer.lock().await;
                                remote_recv_buffer.push_back(remote_data_buf.to_vec());
                                debug!(
                                    "<<<< Transportation {trans_id} read remote udp data, current remote receive buffer size: {}, current remote data: {}",
                                    remote_recv_buffer.len(),
                                    pretty_hex(&remote_data_buf)
                                );
                                remote_recv_buf_notify.notify_one();
                            }
                            Err(e) => {
                                error!("<<<< Transportation {trans_id} fail to read remote udp data because of error: {e:?}");
                                remote_recv_buf_notify.notify_one();
                            }
                        };
                    });
                }
                // Spawn a task to transfer remote receive data buffer to client.
                let trans_id = *trans_id;
                let smoltcp_socket_handle = *smoltcp_socket_handle;
                let remote_recv_buf_notify = Arc::clone(remote_recv_buf_notify);
                let client_file_write = Arc::clone(client_file_write);
                let smoltcp_socket_set = Arc::clone(smoltcp_socket_set);
                let smoltcp_interface = Arc::clone(smoltcp_interface);
                let smoltcp_device = Arc::clone(smoltcp_device);
                let remote_recv_buf = Arc::clone(remote_recv_buf);
                tokio::spawn(async move {
                    loop {
                        remote_recv_buf_notify.notified().await;
                        let mut remote_recv_buf = remote_recv_buf.lock().await;
                        if let Err(e) = Self::consume_udp_remote_recv_buf(
                            trans_id,
                            &mut remote_recv_buf,
                            Arc::clone(&client_file_write),
                            smoltcp_socket_handle,
                            Arc::clone(&smoltcp_socket_set),
                            Arc::clone(&smoltcp_interface),
                            Arc::clone(&smoltcp_device),
                        )
                        .await
                        {
                            error!("<<<< Transportation {trans_id} fail to consume tcp remote receive buffer because of error: {e:?}")
                        };
                    }
                });
            }
        }
        Ok(())
    }

    pub(crate) async fn consume_tcp_remote_recv_buf(
        trans_id: TransportationId,
        remote_recv_buf: &mut VecDeque<u8>,
        client_file_write: Arc<Mutex<File>>,
        smoltcp_socket_handle: SocketHandle,
        smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
        smoltcp_interface: Arc<Mutex<Interface>>,
        smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
    ) -> Result<()> {
        if remote_recv_buf.is_empty() {
            return Ok(());
        }
        let consumed_size = {
            let mut smoltcp_socket_set = smoltcp_socket_set.lock().await;
            let smoltcp_tcp_socket = smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(smoltcp_socket_handle);
            if !smoltcp_tcp_socket.may_send() {
                return Ok(());
            }
            let remote_recv_buf_content = remote_recv_buf.make_contiguous().to_vec();
            trace!(
                "<<<< Transportation {trans_id} send tcp data to smoltcp stack: {}",
                pretty_hex(&remote_recv_buf_content)
            );
            smoltcp_tcp_socket
                .send_slice(&remote_recv_buf_content)
                .map_err(|e| {
                    error!("<<<< Transportation {trans_id} fail to send tcp data to smoltcp stack because of error: {e:?}");
                    anyhow!("Transportation {trans_id} fail to send tcp data to smoltcp stack because of error: {e:?}")
                })?
        };
        let mut smoltcp_interface = smoltcp_interface.lock().await;
        let mut smoltcp_device = smoltcp_device.lock().await;
        let mut smoltcp_socket_set = smoltcp_socket_set.lock().await;
        if !smoltcp_interface.poll(
            Instant::now(),
            &mut *smoltcp_device,
            &mut smoltcp_socket_set,
        ) {
            return Ok(());
        };
        while let Some(data) = smoltcp_device.pop_tx() {
            let log = log_ip_packet(&data);
            debug!("<<<< Transportation {trans_id} write the tx to client on consume remote receive buffer:\n{log}\n",);
            let mut client_file_write = client_file_write.lock().await;
            client_file_write.write_all(&data)?;
        }
        remote_recv_buf.drain(..consumed_size);
        Ok(())
    }

    pub(crate) async fn consume_udp_remote_recv_buf(
        trans_id: TransportationId,
        remote_recv_buf: &mut VecDeque<Vec<u8>>,
        client_file_write: Arc<Mutex<File>>,
        smoltcp_socket_handle: SocketHandle,
        smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
        smoltcp_interface: Arc<Mutex<Interface>>,
        smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
    ) -> Result<()> {
        if remote_recv_buf.is_empty() {
            return Ok(());
        }
        let mut consumed: usize = 0;
        // write udp packets one by one
        for datagram in remote_recv_buf.make_contiguous() {
            let mut smoltcp_socket_set = smoltcp_socket_set.lock().await;
            let smoltcp_udp_socket = smoltcp_socket_set.get_mut::<SmoltcpUdpSocket>(smoltcp_socket_handle);
            if !smoltcp_udp_socket.can_send() {
                break;
            }

            trace!(
                "<<<< Transportation {trans_id} send udp data to smoltcp stack: {}",
                pretty_hex(datagram)
            );
            if let Err(e) = smoltcp_udp_socket
                .send_slice(datagram, trans_id.source.into())
                .map_err(|e| {
                    error!("<<<< Transportation {trans_id} fail to send udp data to smoltcp stack because of error: {e:?}");
                    anyhow!("Transportation {trans_id} fail to send udp data to smoltcp stack because of error: {e:?}")
                })
            {
                continue;
            };

            let mut smoltcp_interface = smoltcp_interface.lock().await;
            let mut smoltcp_device = smoltcp_device.lock().await;
            if !smoltcp_interface.poll(
                Instant::now(),
                &mut *smoltcp_device,
                &mut smoltcp_socket_set,
            ) {
                continue;
            };
            consumed += 1;
        }
        remote_recv_buf.drain(..consumed);
        Ok(())
    }

    pub(crate) async fn send_to_smoltcp(&self, data: &[u8]) {
        match self {
            Transportation::Tcp {
                trans_id,
                smoltcp_socket_handle,
                smoltcp_socket_set,
                smoltcp_interface,
                smoltcp_device,
                smoltcp_recv_buf,
                smoltcp_recv_buf_notify,
                client_file_write,
                ..
            } => {
                let mut smoltcp_device = smoltcp_device.lock().await;
                let mut smoltcp_interface = smoltcp_interface.lock().await;
                let mut smoltcp_socket_set = smoltcp_socket_set.lock().await;
                smoltcp_device.push_rx(data.to_vec());
                if !smoltcp_interface.poll(
                    Instant::now(),
                    &mut *smoltcp_device,
                    &mut smoltcp_socket_set,
                ) {
                    return;
                };
                let smoltcp_tcp_socket = smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(*smoltcp_socket_handle);
                while smoltcp_tcp_socket.may_recv() {
                    let mut smoltcp_data = [0u8; 65536];
                    match smoltcp_tcp_socket.recv_slice(&mut smoltcp_data) {
                        Ok(0) => {
                            smoltcp_recv_buf_notify.notify_waiters();
                            break;
                        }
                        Ok(size) => {
                            trace!(">>>> Transportation {trans_id} receive {size} bytes from smoltcp.");
                            let mut smoltcp_recv_buf = smoltcp_recv_buf.lock().await;
                            smoltcp_recv_buf.extend(&smoltcp_data[..size]);
                            smoltcp_recv_buf_notify.notify_waiters();
                        }
                        Err(e) => {
                            error!(">>>> Transportation {trans_id} fail to receive data from smoltcp because of error {e:?}.");
                            smoltcp_recv_buf_notify.notify_waiters();
                        }
                    };
                }

                while let Some(data) = smoltcp_device.pop_tx() {
                    let log = log_ip_packet(&data);
                    debug!("<<<< Transportation {trans_id} write the tx to client on consume remote receive buffer:\n{log}\n",);
                    let mut client_file_write = client_file_write.lock().await;
                    if let Err(e) = client_file_write.write_all(&data) {
                        break;
                    };
                }
            }
            Transportation::Udp {
                trans_id,
                smoltcp_socket_handle,
                smoltcp_socket_set,
                smoltcp_interface,
                smoltcp_device,
                smoltcp_recv_buf,
                client_file_write,
                smoltcp_recv_buf_notify,
                ..
            } => {
                let mut smoltcp_device = smoltcp_device.lock().await;
                let mut smoltcp_interface = smoltcp_interface.lock().await;
                let mut smoltcp_socket_set = smoltcp_socket_set.lock().await;
                smoltcp_device.push_rx(data.to_vec());
                if !smoltcp_interface.poll(
                    Instant::now(),
                    &mut *smoltcp_device,
                    &mut smoltcp_socket_set,
                ) {
                    return;
                };
                let smoltcp_udp_socket = smoltcp_socket_set.get_mut::<SmoltcpUdpSocket>(*smoltcp_socket_handle);
                while smoltcp_udp_socket.can_recv() {
                    let mut smoltcp_data = [0u8; 65536];
                    match smoltcp_udp_socket.recv_slice(&mut smoltcp_data) {
                        Ok((0, _)) => {
                            smoltcp_recv_buf_notify.notify_waiters();
                            break;
                        }
                        Ok((size, _)) => {
                            trace!(">>>> Transportation {trans_id} receive {size} bytes from smoltcp.");
                            let mut smoltcp_recv_buf = smoltcp_recv_buf.lock().await;
                            smoltcp_recv_buf.extend(&smoltcp_data[..size]);
                            smoltcp_recv_buf_notify.notify_waiters();
                        }
                        Err(e) => {
                            error!(">>>> Transportation {trans_id} fail to receive data from smoltcp because of error {e:?}.");
                            smoltcp_recv_buf_notify.notify_waiters();
                        }
                    };
                }
                while let Some(data) = smoltcp_device.pop_tx() {
                    let log = log_ip_packet(&data);
                    debug!("<<<< Transportation {trans_id} write the tx to client on consume remote receive buffer:\n{log}\n",);
                    let mut client_file_write = client_file_write.lock().await;
                    if let Err(e) = client_file_write.write_all(&data) {
                        break;
                    };
                }
            }
        }
    }
}
