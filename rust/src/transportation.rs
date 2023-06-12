use super::buffers::{Buffers, TcpBuffers, UdpBuffers};
use super::device::PpaassVpnDevice;
use super::remote::{InternetProtocol as MioInternetProtocol, RemoteEndpoint, TransportProtocol as MioTransportProtocol};

use super::local::{DeviceEndpoint, TransportProtocol as SmoltcpProtocol};
use log::{error, warn};
use mio::{Poll, Token};
use smoltcp::iface::{Config, Interface, Routes, SocketSet};
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};
use smoltcp::wire::{IpProtocol, Ipv4Packet, Ipv6Packet, TcpPacket, UdpPacket};
use std::fmt;
use std::hash::Hash;
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
    pub(crate) source: SocketAddr,
    pub(crate) destination: SocketAddr,
    pub(crate) transport_protocol: TransportProtocol,
    pub(crate) internet_protocol: InternetProtocol,
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

impl fmt::Display for TransportationId {
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

pub(crate) struct Transportation<'sockets, 'buf> {
    pub(crate) device_endpoint: DeviceEndpoint<'sockets, 'buf>,
    pub(crate) remote_endpoint: RemoteEndpoint,
    pub(crate) token: Token,
    pub(crate) buffers: Buffers,
    pub(crate) interface: Interface,
    pub(crate) device: PpaassVpnDevice,
    pub(crate) socketset: SocketSet<'sockets>,
}

impl<'sockets, 'buf> Transportation<'sockets, 'buf> {
    pub(crate) fn new(trans_id: TransportationId, poll: &mut Poll, token: Token) -> Option<Self> {
        let (interface, device) = Self::prepare_iface_and_device();
        let mut socketset = SocketSet::new(vec![]);

        let session = Transportation {
            device_endpoint: Self::create_device_endpoint(trans_id, &mut socketset)?,
            remote_endpoint: Self::create_remote_endpoint(trans_id, poll, token)?,
            token,
            buffers: Self::create_buffer(trans_id),
            interface,
            device,
            socketset,
        };

        Some(session)
    }

    fn create_device_endpoint<'a, 'b>(trans_id: TransportationId, sockets: &mut SocketSet) -> Option<DeviceEndpoint<'a, 'b>> {
        let transport_protocol = match trans_id.transport_protocol {
            TransportProtocol::Tcp => SmoltcpProtocol::Tcp,
            TransportProtocol::Udp => SmoltcpProtocol::Udp,
        };

        DeviceEndpoint::new(
            transport_protocol,
            trans_id.source,
            trans_id.destination,
            sockets,
        )
    }

    fn create_remote_endpoint(trans_id: TransportationId, poll: &mut Poll, token: Token) -> Option<RemoteEndpoint> {
        let transport_protocol = match trans_id.transport_protocol {
            TransportProtocol::Tcp => MioTransportProtocol::Tcp,
            TransportProtocol::Udp => MioTransportProtocol::Udp,
        };

        let internet_protocol = match trans_id.internet_protocol {
            InternetProtocol::Ipv4 => MioInternetProtocol::Ipv4,
            InternetProtocol::Ipv6 => MioInternetProtocol::Ipv6,
        };

        let mut mio_socket = RemoteEndpoint::new(transport_protocol, internet_protocol, trans_id.destination)?;

        if let Err(error) = mio_socket.register_poll(poll, token) {
            log::error!("failed to register poll, error={:?}", error);
            return None;
        }

        Some(mio_socket)
    }

    fn prepare_iface_and_device() -> (Interface, PpaassVpnDevice) {
        let mut routes = Routes::new();
        let default_gateway_ipv4 = Ipv4Address::new(0, 0, 0, 1);
        routes.add_default_ipv4_route(default_gateway_ipv4).unwrap();
        let mut interface_config = Config::default();
        interface_config.random_seed = rand::random::<u64>();
        let mut vpn_device = PpaassVpnDevice::new();
        let mut interface = Interface::new(interface_config, &mut vpn_device);
        interface.set_any_ip(true);
        interface.update_ip_addrs(|ip_addrs| {
            ip_addrs
                .push(IpCidr::new(IpAddress::v4(0, 0, 0, 1), 0))
                .unwrap();
        });
        interface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(0, 0, 0, 1))
            .unwrap();

        (interface, vpn_device)
    }

    fn create_buffer(trans_id: TransportationId) -> Buffers {
        match trans_id.transport_protocol {
            TransportProtocol::Tcp => Buffers::Tcp(TcpBuffers::new()),
            TransportProtocol::Udp => Buffers::Udp(UdpBuffers::new()),
        }
    }
}
