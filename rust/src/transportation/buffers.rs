use log::error;
use std::{collections::VecDeque, error::Error, sync::Arc};
use std::{
    future::Future,
    io::{Error as StdIoError, ErrorKind},
};
use tokio::sync::Mutex;

use crate::error::NetworkError;

use super::{
    endpoint::{DeviceEndpoint, RemoteEndpoint},
    TransportationId,
};

/// The buffer for 2 side in the transport.
/// The buffer is internal mutable.
/// The remote buffer contains the buffered data that read from device and going transfer to remote.
/// The device buffer contains the buffered data that read from remote and going transfer to device.
pub(crate) enum Buffer {
    Tcp {
        device: Arc<Mutex<VecDeque<u8>>>,
        remote: Arc<Mutex<VecDeque<u8>>>,
    },
    Udp {
        device: Arc<Mutex<VecDeque<Vec<u8>>>>,
        remote: Arc<Mutex<VecDeque<Vec<u8>>>>,
    },
}

impl Buffer {
    pub(crate) fn new_tcp_buffer() -> Self {
        Buffer::Tcp {
            device: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
            remote: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
        }
    }

    pub(crate) fn new_udp_buffer() -> Self {
        Buffer::Udp {
            device: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
            remote: Arc::new(Mutex::new(VecDeque::with_capacity(65536))),
        }
    }

    pub(crate) async fn push_device_data_to_remote(&self, data: &[u8]) {
        match self {
            Buffer::Tcp { remote, .. } => {
                let mut remote = remote.lock().await;
                remote.extend(data);
            }
            Buffer::Udp { remote, .. } => {
                let mut remote = remote.lock().await;
                remote.push_back(data.to_vec())
            }
        }
    }

    pub(crate) async fn push_remote_data_to_device(&self, data: &[u8]) {
        match self {
            Buffer::Tcp { device, .. } => {
                let mut device = device.lock().await;
                device.extend(data)
            }
            Buffer::Udp { device, .. } => {
                let mut device = device.lock().await;
                device.push_back(data.to_vec())
            }
        }
    }

    pub(crate) async fn consume_device_buffer_with<'b, F, Fut>(&self, trans_id: TransportationId, device_endpoint: Arc<DeviceEndpoint<'b>>, mut write_fn: F)
    where
        F: FnMut(TransportationId, Arc<DeviceEndpoint<'b>>, Vec<u8>) -> Fut,
        Fut: Future<Output = Result<usize, NetworkError>>,
    {
        match self {
            Buffer::Tcp {
                device: device_buffer,
                ..
            } => {
                let device_buffer_owned = {
                    let mut device_buffer = device_buffer.lock().await;
                    device_buffer.make_contiguous().to_vec()
                };
                match write_fn(trans_id, device_endpoint, device_buffer_owned).await {
                    Ok(consumed) => {
                        let mut device_buffer = device_buffer.lock().await;
                        device_buffer.drain(..consumed);
                    }
                    Err(error) => {
                        if let Some(source_error) = error.source() {
                            if let Some(io_error) = source_error.downcast_ref::<StdIoError>() {
                                if io_error.kind() != ErrorKind::WouldBlock {
                                    error!(">>>> Fail to write buffer data to device tcp because of error: {io_error:?}")
                                }
                            }
                        };
                    }
                }
            }
            Buffer::Udp {
                device: all_datagrams,
                ..
            } => {
                let all_datagrams_owned = {
                    let mut all_datagrams = all_datagrams.lock().await;
                    all_datagrams.make_contiguous().to_vec()
                };
                let mut consumed: usize = 0;
                // write udp packets one by one
                for datagram in all_datagrams_owned {
                    if let Err(error) = write_fn(trans_id, device_endpoint.clone(), datagram.to_vec()).await {
                        error!(">>>> Fail to write buffer data to device udp because of error: {error:?}");
                        break;
                    }
                    consumed += 1;
                }
                let mut all_datagrams = all_datagrams.lock().await;
                all_datagrams.drain(..consumed);
            }
        }
    }

    pub(crate) async fn consume_remote_buffer_with<F, Fut>(&self, trans_id: TransportationId, remote_endpoint: Arc<RemoteEndpoint>, mut write_fn: F)
    where
        F: FnMut(TransportationId, Arc<RemoteEndpoint>, Vec<u8>) -> Fut,
        Fut: Future<Output = Result<usize, NetworkError>>,
    {
        match self {
            Buffer::Tcp {
                remote: remote_buffer,
                ..
            } => {
                let remote_buffer_owned = {
                    let mut remote_buffer = remote_buffer.lock().await;
                    remote_buffer.make_contiguous().to_vec()
                };
                match write_fn(trans_id, remote_endpoint, remote_buffer_owned).await {
                    Ok(consumed) => {
                        let mut remote_buffer = remote_buffer.lock().await;
                        remote_buffer.drain(..consumed);
                    }
                    Err(error) => {
                        if let Some(source_error) = error.source() {
                            if let Some(io_error) = source_error.downcast_ref::<StdIoError>() {
                                if io_error.kind() != ErrorKind::WouldBlock {
                                    error!(">>>> Fail to write buffer data to remote tcp because of error: {io_error:?}")
                                }
                            }
                        };
                    }
                }
            }
            Buffer::Udp {
                remote: all_datagrams,
                ..
            } => {
                let all_datagrams_owned = {
                    let mut all_datagrams = all_datagrams.lock().await;
                    all_datagrams.make_contiguous().to_vec()
                };
                let mut consumed: usize = 0;
                // write udp packets one by one
                for datagram in all_datagrams_owned {
                    let datagram_owned = datagram.to_vec();
                    if let Err(error) = write_fn(trans_id, remote_endpoint.clone(), datagram_owned).await {
                        error!(">>>> Fail to write buffer data to remote udp because of error: {error:?}");
                        break;
                    }
                    consumed += 1;
                }
                let mut all_datagrams = all_datagrams.lock().await;
                all_datagrams.drain(..consumed);
            }
        }
    }
}
