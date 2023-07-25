use smoltcp::wire::{IpProtocol, Ipv4Packet, Ipv6Packet, TcpPacket, UdpPacket};

use std::hash::Hash;

use anyhow::{anyhow, Result};
use std::fmt::{self, Display};
use std::net::SocketAddr;

/// The transport protocol, TCP and UDP
#[derive(PartialEq, Eq, Hash, Debug, Clone, Copy)]
pub(crate) enum ControlProtocol {
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
pub struct TransportId {
    pub(crate) source: SocketAddr,
    pub(crate) destination: SocketAddr,
    pub(crate) control_protocol: ControlProtocol,
    pub(crate) internet_protocol: InternetProtocol,
}

impl Display for TransportId {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(
            formatter,
            "[{:?}][{:?}][{}:{}->{}:{}]",
            self.internet_protocol,
            self.control_protocol,
            self.source.ip(),
            self.source.port(),
            self.destination.ip(),
            self.destination.port()
        )
    }
}

impl TransportId {
    pub(crate) fn new(data: &[u8]) -> Result<TransportId> {
        Self::parse_ipv4(data).or_else(|_| Self::parse_ipv6(data))
    }

    fn parse_ipv4(data: &[u8]) -> Result<TransportId> {
        let ip_packet = Ipv4Packet::new_checked(&data)?;
        match ip_packet.next_header() {
            IpProtocol::Tcp => {
                let payload = ip_packet.payload();
                let packet = TcpPacket::new_checked(payload)?;
                let source_ip: [u8; 4] = ip_packet.src_addr().as_bytes().try_into()?;
                let destination_ip: [u8; 4] = ip_packet.dst_addr().as_bytes().try_into()?;
                Ok(TransportId {
                    source: SocketAddr::from((source_ip, packet.src_port())),
                    destination: SocketAddr::from((destination_ip, packet.dst_port())),
                    control_protocol: ControlProtocol::Tcp,
                    internet_protocol: InternetProtocol::Ipv4,
                })
            }
            IpProtocol::Udp => {
                let payload = ip_packet.payload();
                let packet = UdpPacket::new_checked(payload)?;
                let source_ip: [u8; 4] = ip_packet.src_addr().as_bytes().try_into()?;
                let destination_ip: [u8; 4] = ip_packet.dst_addr().as_bytes().try_into()?;
                Ok(TransportId {
                    source: SocketAddr::from((source_ip, packet.src_port())),
                    destination: SocketAddr::from((destination_ip, packet.dst_port())),
                    control_protocol: ControlProtocol::Udp,
                    internet_protocol: InternetProtocol::Ipv4,
                })
            }
            _ => Err(anyhow!(
                "Unsupported transport protocol in ipv4 packet, protocol=${:?}",
                ip_packet.next_header()
            )),
        }
    }

    fn parse_ipv6(data: &[u8]) -> Result<TransportId> {
        let ip_packet = Ipv6Packet::new_checked(&data)?;
        let protocol = ip_packet.next_header();
        match protocol {
            IpProtocol::Tcp => {
                let payload = ip_packet.payload();
                let packet = TcpPacket::new_checked(payload)?;
                let source_ip: [u8; 16] = ip_packet.src_addr().as_bytes().try_into()?;
                let destination_ip: [u8; 16] = ip_packet.dst_addr().as_bytes().try_into()?;
                Ok(TransportId {
                    source: SocketAddr::from((source_ip, packet.src_port())),
                    destination: SocketAddr::from((destination_ip, packet.dst_port())),
                    control_protocol: ControlProtocol::Tcp,
                    internet_protocol: InternetProtocol::Ipv6,
                })
            }
            IpProtocol::Udp => {
                let payload = ip_packet.payload();
                let packet = UdpPacket::new_checked(payload)?;
                let source_ip: [u8; 16] = ip_packet.src_addr().as_bytes().try_into()?;
                let destination_ip: [u8; 16] = ip_packet.dst_addr().as_bytes().try_into()?;
                Ok(TransportId {
                    source: SocketAddr::from((source_ip, packet.src_port())),
                    destination: SocketAddr::from((destination_ip, packet.dst_port())),
                    control_protocol: ControlProtocol::Udp,
                    internet_protocol: InternetProtocol::Ipv6,
                })
            }
            _ => Err(anyhow!(
                "Unsupported transport protocol in ipv6 packet, protocol=${:?}",
                ip_packet.next_header()
            )),
        }
    }
}
