use smoltcp::wire::{IpProtocol, Ipv4Packet, Ipv6Packet, TcpPacket, UdpPacket};
use tokio::sync::mpsc::Sender;

use std::{collections::HashMap, hash::Hash};

use anyhow::{anyhow, Result};
use std::fmt::{self, Display};
use std::net::SocketAddr;

pub(crate) type Transports = HashMap<TransportId, Sender<Vec<u8>>>;

pub(crate) struct ClientOutputPacket {
    pub transport_id: TransportId,
    pub data: Vec<u8>,
}

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

pub(crate) enum ClientInputTransportPacket<'p> {
    Tcp(TcpPacket<&'p [u8]>),
    Udp(UdpPacket<&'p [u8]>),
}

pub(crate) enum ClientInputIpPacket<'p> {
    Ipv4(ClientInputTransportPacket<'p>),
    Ipv6(ClientInputTransportPacket<'p>),
}

pub(crate) struct ClientInputParser;

impl ClientInputParser {
    pub(crate) fn parse(
        data: &[u8],
    ) -> Result<(TransportId, ClientInputIpPacket<'_>)> {
        Self::parse_ipv4(data).or_else(|_| Self::parse_ipv6(data))
    }

    fn parse_ipv4(
        data: &[u8],
    ) -> Result<(TransportId, ClientInputIpPacket<'_>)> {
        let ip_packet = Ipv4Packet::new_checked(data)?;
        match ip_packet.next_header() {
            IpProtocol::Tcp => {
                let payload = ip_packet.payload();
                let tcp_packet = TcpPacket::new_checked(payload)?;
                let source_ip: [u8; 4] =
                    ip_packet.src_addr().as_bytes().try_into()?;
                let destination_ip: [u8; 4] =
                    ip_packet.dst_addr().as_bytes().try_into()?;
                Ok((
                    TransportId {
                        source: SocketAddr::from((
                            source_ip,
                            tcp_packet.src_port(),
                        )),
                        destination: SocketAddr::from((
                            destination_ip,
                            tcp_packet.dst_port(),
                        )),
                        control_protocol: ControlProtocol::Tcp,
                        internet_protocol: InternetProtocol::Ipv4,
                    },
                    ClientInputIpPacket::Ipv4(ClientInputTransportPacket::Tcp(
                        tcp_packet,
                    )),
                ))
            }
            IpProtocol::Udp => {
                let payload = ip_packet.payload();
                let udp_packet = UdpPacket::new_checked(payload)?;
                let source_ip: [u8; 4] =
                    ip_packet.src_addr().as_bytes().try_into()?;
                let destination_ip: [u8; 4] =
                    ip_packet.dst_addr().as_bytes().try_into()?;
                Ok((
                    TransportId {
                        source: SocketAddr::from((
                            source_ip,
                            udp_packet.src_port(),
                        )),
                        destination: SocketAddr::from((
                            destination_ip,
                            udp_packet.dst_port(),
                        )),
                        control_protocol: ControlProtocol::Udp,
                        internet_protocol: InternetProtocol::Ipv4,
                    },
                    ClientInputIpPacket::Ipv4(ClientInputTransportPacket::Udp(
                        udp_packet,
                    )),
                ))
            }
            _ => Err(anyhow!(
                "Unsupported transport protocol in ipv4 packet, protocol=${:?}",
                ip_packet.next_header()
            )),
        }
    }

    fn parse_ipv6(data: &[u8]) -> Result<(TransportId, ClientInputIpPacket)> {
        let ip_packet = Ipv6Packet::new_checked(data)?;
        let protocol = ip_packet.next_header();
        match protocol {
            IpProtocol::Tcp => {
                let payload = ip_packet.payload();
                let tcp_packet = TcpPacket::new_checked(payload)?;
                let source_ip: [u8; 16] =
                    ip_packet.src_addr().as_bytes().try_into()?;
                let destination_ip: [u8; 16] =
                    ip_packet.dst_addr().as_bytes().try_into()?;
                Ok((
                    TransportId {
                        source: SocketAddr::from((
                            source_ip,
                            tcp_packet.src_port(),
                        )),
                        destination: SocketAddr::from((
                            destination_ip,
                            tcp_packet.dst_port(),
                        )),
                        control_protocol: ControlProtocol::Tcp,
                        internet_protocol: InternetProtocol::Ipv6,
                    },
                    ClientInputIpPacket::Ipv6(ClientInputTransportPacket::Tcp(
                        tcp_packet,
                    )),
                ))
            }
            IpProtocol::Udp => {
                let payload = ip_packet.payload();
                let udp_packet = UdpPacket::new_checked(payload)?;
                let source_ip: [u8; 16] =
                    ip_packet.src_addr().as_bytes().try_into()?;
                let destination_ip: [u8; 16] =
                    ip_packet.dst_addr().as_bytes().try_into()?;
                Ok((
                    TransportId {
                        source: SocketAddr::from((
                            source_ip,
                            udp_packet.src_port(),
                        )),
                        destination: SocketAddr::from((
                            destination_ip,
                            udp_packet.dst_port(),
                        )),
                        control_protocol: ControlProtocol::Udp,
                        internet_protocol: InternetProtocol::Ipv6,
                    },
                    ClientInputIpPacket::Ipv6(ClientInputTransportPacket::Udp(
                        udp_packet,
                    )),
                ))
            }
            _ => Err(anyhow!(
                "Unsupported transport protocol in ipv6 packet, protocol=${:?}",
                ip_packet.next_header()
            )),
        }
    }
}
