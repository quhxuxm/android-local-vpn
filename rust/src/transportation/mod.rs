mod buffers;
mod endpoint;

use crate::error::NetworkError;
use buffers::{IncomingDataEvent, IncomingDirection, OutgoingDirection};

use endpoint::DeviceEndpoint;
use endpoint::RemoteEndpoint;
use log::{error, warn};
use mio::{Poll, Token};

use smoltcp::wire::{IpProtocol, Ipv4Packet, Ipv6Packet, TcpPacket, UdpPacket};

use crate::transportation::buffers::{Buffers, TcpBuffers, UdpBuffers};
use std::fmt::{self, Display};
use std::hash::Hash;
use std::io::Error as StdIoError;
use std::net::SocketAddr;

#[derive(PartialEq, Eq, Hash, Debug, Clone, Copy)]
pub(crate) enum TransportProtocol {
    Tcp,
    Udp,
}

#[derive(PartialEq, Eq, Hash, Debug, Clone, Copy)]
pub(crate) enum InternetProtocol {
    Ipv4,
    Ipv6,
}

#[derive(PartialEq, Eq, Hash, Debug, Clone, Copy)]
pub struct TransportationId {
    source: SocketAddr,
    destination: SocketAddr,
    transport_protocol: TransportProtocol,
    internet_protocol: InternetProtocol,
}

impl TransportationId {
    pub(crate) fn new(data: &[u8]) -> Option<TransportationId> {
        Self::parse_ipv4(data)
            .or_else(|| Self::parse_ipv6(data))
            .or_else(|| {
                error!(
                    ">>>> Failed to create transportation id, len={:?}",
                    data.len(),
                );
                None
            })
    }

    fn parse_ipv4(data: &[u8]) -> Option<TransportationId> {
        if let Ok(ip_packet) = Ipv4Packet::new_checked(&data) {
            match ip_packet.next_header() {
                IpProtocol::Tcp => {
                    let payload = ip_packet.payload();
                    let packet = TcpPacket::new_checked(payload).ok()?;
                    let source_ip: [u8; 4] = ip_packet.src_addr().as_bytes().try_into().ok()?;
                    let destination_ip: [u8; 4] = ip_packet.dst_addr().as_bytes().try_into().ok()?;
                    return Some(TransportationId {
                        source: SocketAddr::from((source_ip, packet.src_port())),
                        destination: SocketAddr::from((destination_ip, packet.dst_port())),
                        transport_protocol: TransportProtocol::Tcp,
                        internet_protocol: InternetProtocol::Ipv4,
                    });
                }
                IpProtocol::Udp => {
                    let payload = ip_packet.payload();
                    let packet = UdpPacket::new_checked(payload).ok()?;
                    let source_ip: [u8; 4] = ip_packet.src_addr().as_bytes().try_into().ok()?;
                    let destination_ip: [u8; 4] = ip_packet.dst_addr().as_bytes().try_into().ok()?;
                    return Some(TransportationId {
                        source: SocketAddr::from((source_ip, packet.src_port())),
                        destination: SocketAddr::from((destination_ip, packet.dst_port())),
                        transport_protocol: TransportProtocol::Udp,
                        internet_protocol: InternetProtocol::Ipv4,
                    });
                }
                _ => {
                    warn!(
                        ">>>> Unsupported transport protocol in ipv4 packet, protocol=${:?}",
                        ip_packet.next_header()
                    );
                    return None;
                }
            }
        }
        None
    }

    fn parse_ipv6(data: &[u8]) -> Option<TransportationId> {
        if let Ok(ip_packet) = Ipv6Packet::new_checked(&data) {
            let protocol = ip_packet.next_header();
            match protocol {
                IpProtocol::Tcp => {
                    let payload = ip_packet.payload();
                    let packet = TcpPacket::new_checked(payload).ok()?;
                    let source_ip: [u8; 16] = ip_packet.src_addr().as_bytes().try_into().ok()?;
                    let destination_ip: [u8; 16] = ip_packet.dst_addr().as_bytes().try_into().ok()?;
                    return Some(TransportationId {
                        source: SocketAddr::from((source_ip, packet.src_port())),
                        destination: SocketAddr::from((destination_ip, packet.dst_port())),
                        transport_protocol: TransportProtocol::Tcp,
                        internet_protocol: InternetProtocol::Ipv6,
                    });
                }
                IpProtocol::Udp => {
                    let payload = ip_packet.payload();
                    let packet = UdpPacket::new_checked(payload).ok()?;
                    let source_ip: [u8; 16] = ip_packet.src_addr().as_bytes().try_into().ok()?;
                    let destination_ip: [u8; 16] = ip_packet.dst_addr().as_bytes().try_into().ok()?;
                    return Some(TransportationId {
                        source: SocketAddr::from((source_ip, packet.src_port())),
                        destination: SocketAddr::from((destination_ip, packet.dst_port())),
                        transport_protocol: TransportProtocol::Udp,
                        internet_protocol: InternetProtocol::Ipv6,
                    });
                }
                _ => {
                    warn!(
                        ">>>> Unsupported transport protocol in ipv6 packet, protocol=${:?}",
                        ip_packet.next_header()
                    );
                    return None;
                }
            }
        }

        None
    }
}

impl Display for TransportationId {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(
            formatter,
            "[{:?}][{:?}]{}:{}->{}:{}",
            self.internet_protocol,
            self.transport_protocol,
            self.source.ip(),
            self.source.port(),
            self.destination.ip(),
            self.destination.port()
        )
    }
}

pub(crate) struct Transportation<'buf> {
    trans_id: TransportationId,
    token: Token,
    device_endpoint: DeviceEndpoint<'buf>,
    remote_endpoint: RemoteEndpoint,
    buffers: Buffers,
}

impl<'buf> Transportation<'buf> {
    pub(crate) fn new(trans_id: TransportationId, poll: &mut Poll, token: Token) -> Option<Self> {
        let session = Transportation {
            trans_id,
            token,
            device_endpoint: Self::create_device_endpoint(trans_id)?,
            remote_endpoint: Self::create_remote_endpoint(trans_id, poll, token)?,
            buffers: Self::create_buffer(trans_id),
        };

        Some(session)
    }

    fn create_device_endpoint(trans_id: TransportationId) -> Option<DeviceEndpoint<'buf>> {
        DeviceEndpoint::new(
            trans_id.transport_protocol,
            trans_id.source,
            trans_id.destination,
        )
    }

    fn create_remote_endpoint(trans_id: TransportationId, poll: &mut Poll, token: Token) -> Option<RemoteEndpoint> {
        let mut remote_endpoint = RemoteEndpoint::new(
            trans_id,
            trans_id.transport_protocol,
            trans_id.internet_protocol,
            trans_id.destination,
        )?;

        if let Err(error) = remote_endpoint.register_poll(poll, token) {
            error!(">>>> Transportation {trans_id} failed to register poll for remote endpoint because of error: {error:?}");
            return None;
        }

        Some(remote_endpoint)
    }

    fn create_buffer(trans_id: TransportationId) -> Buffers {
        match trans_id.transport_protocol {
            TransportProtocol::Tcp => Buffers::Tcp(TcpBuffers::new()),
            TransportProtocol::Udp => Buffers::Udp(UdpBuffers::new()),
        }
    }

    pub(crate) fn get_token(&self) -> Token {
        self.token
    }

    pub(crate) fn poll_device_endpoint(&mut self) {
        self.device_endpoint.poll()
    }

    pub(crate) fn close_device_endpoint(&mut self) {
        self.device_endpoint.close();
    }

    pub(crate) fn device_endpoint_can_receive(&self) -> bool {
        self.device_endpoint.can_receive()
    }

    pub(crate) fn device_endpoint_can_send(&self) -> bool {
        self.device_endpoint.can_send()
    }

    pub(crate) fn close_remote_endpoint(&mut self, poll: &mut Poll) {
        self.remote_endpoint.close();
        self.remote_endpoint.deregister_poll(poll).unwrap();
    }

    pub(crate) fn push_rx_to_device(&mut self, rx_data: Vec<u8>) {
        self.device_endpoint.push_rx_to_device(rx_data)
    }

    pub(crate) fn pop_tx_from_device(&mut self) -> Option<Vec<u8>> {
        self.device_endpoint.pop_tx_from_device()
    }

    pub(crate) fn read_from_remote_endpoint(&mut self) -> Result<(Vec<Vec<u8>>, bool), StdIoError> {
        self.remote_endpoint.read()
    }

    pub(crate) fn read_from_device_endpoint(&mut self, data: &mut [u8]) -> Result<usize, NetworkError> {
        self.device_endpoint.receive(data)
    }

    pub(crate) fn push_client_data_to_buffer(&mut self, data: &[u8]) {
        let event = IncomingDataEvent {
            direction: IncomingDirection::FromDevice,
            buffer: data,
        };
        self.buffers.push_data(event)
    }

    pub(crate) fn push_remote_data_to_buffer(&mut self, data: &[u8]) {
        let event = IncomingDataEvent {
            direction: IncomingDirection::FromRemote,
            buffer: data,
        };
        self.buffers.push_data(event)
    }

    pub(crate) fn consume_device_buffer(&mut self) {
        self.buffers.write_data(OutgoingDirection::ToDevice, |b| {
            self.device_endpoint.send(b)
        });
    }

    pub(crate) fn consume_remote_buffer(&mut self) {
        self.buffers.write_data(OutgoingDirection::ToRemote, |b| {
            self.remote_endpoint
                .write(b)
                .map_err(NetworkError::WriteToRemote)
        });
    }
}
