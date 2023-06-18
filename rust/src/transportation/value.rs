use log::{error, warn};

use smoltcp::wire::{IpProtocol, Ipv4Packet, Ipv6Packet, TcpPacket, UdpPacket};

use std::hash::Hash;

use std::fmt::{self, Display};
use std::net::SocketAddr;

/// The transport protocol, TCP and UDP
#[derive(PartialEq, Eq, Hash, Debug, Clone, Copy)]
pub(crate) enum TransportProtocol {
    Tcp,
    Udp,
}

/// The internet protocol for IPv4 and IPV6
#[derive(PartialEq, Eq, Hash, Debug, Clone, Copy)]
pub(crate) enum InternetProtocol {
    Ipv4,
    Ipv6,
}

/// The transport id.
#[derive(PartialEq, Eq, Hash, Debug, Clone, Copy)]
pub struct TransportationId {
    pub(crate) source: SocketAddr,
    pub(crate) destination: SocketAddr,
    pub(crate) transport_protocol: TransportProtocol,
    pub(crate) internet_protocol: InternetProtocol,
}

impl Display for TransportationId {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(
            formatter,
            "[{:?}][{:?}][{}:{}->{}:{}]",
            self.internet_protocol,
            self.transport_protocol,
            self.source.ip(),
            self.source.port(),
            self.destination.ip(),
            self.destination.port()
        )
    }
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
