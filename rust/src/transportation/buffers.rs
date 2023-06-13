use log::error;
use std::io::{Error as StdIoError, ErrorKind};
use std::{collections::VecDeque, error::Error};

use crate::error::NetworkError;

pub(crate) enum Buffers {
    Tcp(TcpBuffers),
    Udp(UdpBuffers),
}

impl Buffers {
    pub(crate) fn push_data(&mut self, event: IncomingDataEvent<'_>) {
        match self {
            Buffers::Tcp(tcp_buf) => tcp_buf.push_data(event),
            Buffers::Udp(udp_buf) => udp_buf.push_data(event),
        }
    }

    pub(crate) fn write_data<F>(&mut self, direction: OutgoingDirection, mut write_fn: F)
    where
        F: FnMut(&[u8]) -> Result<usize, NetworkError>,
    {
        match self {
            Buffers::Tcp(tcp_buf) => {
                let buffer = tcp_buf.peek_data(&direction).to_vec();
                match write_fn(&buffer[..]) {
                    Ok(consumed) => {
                        tcp_buf.consume_data(&direction, consumed);
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
            Buffers::Udp(udp_buf) => {
                let all_datagrams = udp_buf.peek_data(&direction);
                let mut consumed: usize = 0;
                // write udp packets one by one
                for datagram in all_datagrams {
                    if let Err(error) = write_fn(&datagram[..]) {
                        error!(">>>> Fail to write buffer data to remote udp because of error: {error:?}");
                        break;
                    }
                    consumed += 1;
                }
                udp_buf.consume_data(&direction, consumed);
            }
        }
    }
}

pub(crate) struct TcpBuffers {
    device: VecDeque<u8>,
    remote: VecDeque<u8>,
}

impl TcpBuffers {
    pub(crate) fn new() -> TcpBuffers {
        TcpBuffers {
            device: Default::default(),
            remote: Default::default(),
        }
    }

    pub(crate) fn peek_data(&mut self, direction: &OutgoingDirection) -> &[u8] {
        let buffer = match direction {
            OutgoingDirection::ToRemote => &mut self.remote,
            OutgoingDirection::ToDevice => &mut self.device,
        };
        buffer.make_contiguous()
    }

    pub(crate) fn consume_data(&mut self, direction: &OutgoingDirection, size: usize) {
        let buffer = match direction {
            OutgoingDirection::ToRemote => &mut self.remote,
            OutgoingDirection::ToDevice => &mut self.device,
        };
        buffer.drain(0..size);
    }

    pub(crate) fn push_data(&mut self, event: IncomingDataEvent<'_>) {
        let direction = event.direction;
        let buffer = event.buffer;
        match direction {
            IncomingDirection::FromRemote => {
                self.device.extend(buffer.iter());
            }
            IncomingDirection::FromDevice => {
                self.remote.extend(buffer.iter());
            }
        }
    }
}

pub(crate) struct UdpBuffers {
    device: VecDeque<Vec<u8>>,
    remote: VecDeque<Vec<u8>>,
}

impl UdpBuffers {
    pub(crate) fn new() -> UdpBuffers {
        UdpBuffers {
            device: Default::default(),
            remote: Default::default(),
        }
    }

    pub(crate) fn peek_data(&mut self, direction: &OutgoingDirection) -> &[Vec<u8>] {
        let buffer = match direction {
            OutgoingDirection::ToRemote => &mut self.remote,
            OutgoingDirection::ToDevice => &mut self.device,
        };
        buffer.make_contiguous()
    }

    pub(crate) fn consume_data(&mut self, direction: &OutgoingDirection, size: usize) {
        let buffer = match direction {
            OutgoingDirection::ToRemote => &mut self.remote,
            OutgoingDirection::ToDevice => &mut self.device,
        };
        buffer.drain(0..size);
    }

    pub(crate) fn push_data(&mut self, event: IncomingDataEvent<'_>) {
        let direction = event.direction;
        let buffer = event.buffer;
        match direction {
            IncomingDirection::FromRemote => self.device.push_back(buffer.to_vec()),
            IncomingDirection::FromDevice => self.remote.push_back(buffer.to_vec()),
        }
    }
}

#[derive(Eq, PartialEq, Debug)]
pub(crate) enum IncomingDirection {
    FromRemote,
    FromDevice,
}

#[derive(Eq, PartialEq, Debug)]
pub(crate) enum OutgoingDirection {
    ToRemote,
    ToDevice,
}

pub(crate) struct DataEvent<'a, T> {
    pub direction: T,
    pub buffer: &'a [u8],
}

pub(crate) type IncomingDataEvent<'a> = DataEvent<'a, IncomingDirection>;
