use log::error;
use std::io::{Error as StdIoError, ErrorKind};
use std::{collections::VecDeque, error::Error};

use crate::error::NetworkError;

pub(crate) enum Buffer {
    Tcp {
        device: VecDeque<u8>,
        remote: VecDeque<u8>,
    },
    Udp {
        device: VecDeque<Vec<u8>>,
        remote: VecDeque<Vec<u8>>,
    },
}

impl Buffer {
    pub(crate) fn new_tcp_buffer() -> Self {
        Buffer::Tcp {
            device: VecDeque::with_capacity(65536),
            remote: VecDeque::with_capacity(65536),
        }
    }

    pub(crate) fn new_udp_buffer() -> Self {
        Buffer::Udp {
            device: VecDeque::with_capacity(65536),
            remote: VecDeque::with_capacity(65536),
        }
    }

    pub(crate) fn push_device_data_to_remote(&mut self, data: &[u8]) {
        match self {
            Buffer::Tcp { remote, .. } => remote.extend(data),
            Buffer::Udp { remote, .. } => remote.push_back(data.to_vec()),
        }
    }

    pub(crate) fn push_remote_data_to_device(&mut self, data: &[u8]) {
        match self {
            Buffer::Tcp { device, .. } => device.extend(data),
            Buffer::Udp { device, .. } => device.push_back(data.to_vec()),
        }
    }

    pub(crate) fn consume_device_buffer<F>(&mut self, mut write_fn: F)
    where
        F: FnMut(&[u8]) -> Result<usize, NetworkError>,
    {
        match self {
            Buffer::Tcp {
                device: device_buffer,
                ..
            } => {
                let device_buffer_slice = device_buffer.make_contiguous();
                match write_fn(device_buffer_slice) {
                    Ok(consumed) => {
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
                let all_datagrams_slice = all_datagrams.make_contiguous();
                let mut consumed: usize = 0;
                // write udp packets one by one
                for datagram in all_datagrams_slice {
                    if let Err(error) = write_fn(&datagram[..]) {
                        error!(">>>> Fail to write buffer data to device udp because of error: {error:?}");
                        break;
                    }
                    consumed += 1;
                }
                all_datagrams.drain(..consumed);
            }
        }
    }

    pub(crate) fn consume_remote_buffer<F>(&mut self, mut write_fn: F)
    where
        F: FnMut(&[u8]) -> Result<usize, NetworkError>,
    {
        match self {
            Buffer::Tcp {
                remote: remote_buffer,
                ..
            } => {
                let remote_buffer_slice = remote_buffer.make_contiguous();
                match write_fn(remote_buffer_slice) {
                    Ok(consumed) => {
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
                let all_datagrams_slice = all_datagrams.make_contiguous();
                let mut consumed: usize = 0;
                // write udp packets one by one
                for datagram in all_datagrams_slice {
                    if let Err(error) = write_fn(&datagram[..]) {
                        error!(">>>> Fail to write buffer data to remote udp because of error: {error:?}");
                        break;
                    }
                    consumed += 1;
                }
                all_datagrams.drain(..consumed);
            }
        }
    }
}
