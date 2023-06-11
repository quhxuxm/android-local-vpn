use super::buffers::{Buffers, TcpBuffers, UdpBuffers};
use super::device::PpaassVpnDevice;
use super::mio_socket::{InternetProtocol as MioInternetProtocol, Socket as MioSocket, TransportProtocol as MioTransportProtocol};
use super::session_info::{InternetProtocol, TransportProtocol, TransportationInfo};
use super::smoltcp_socket::{Socket as SmoltcpSocket, TransportProtocol as SmoltcpProtocol};
use mio::{Poll, Token};
use smoltcp::iface::{Config, Interface, Routes, SocketSet};
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};

pub(crate) struct Transportation<'sockets> {
    pub(crate) smoltcp_socket: SmoltcpSocket,
    pub(crate) mio_socket: MioSocket,
    pub(crate) token: Token,
    pub(crate) buffers: Buffers,
    pub(crate) interface: Interface,
    pub(crate) vpn_device: PpaassVpnDevice,
    pub(crate) socketset: SocketSet<'sockets>,
}

impl<'sockets> Transportation<'sockets> {
    pub(crate) fn new<'a>(session_info: &TransportationInfo, poll: &mut Poll, token: Token) -> Option<Transportation<'a>> {
        let (interface, vpn_device) = Self::init();
        let mut socketset = SocketSet::new(vec![]);

        let session = Transportation {
            smoltcp_socket: Self::create_smoltcp_socket(session_info, &mut socketset)?,
            mio_socket: Self::create_mio_socket(session_info, poll, token)?,
            token,
            buffers: Self::create_buffer(session_info),
            interface,
            vpn_device,
            socketset,
        };

        Some(session)
    }

    fn create_smoltcp_socket(session_info: &TransportationInfo, sockets: &mut SocketSet) -> Option<SmoltcpSocket> {
        let transport_protocol = match session_info.transport_protocol {
            TransportProtocol::Tcp => SmoltcpProtocol::Tcp,
            TransportProtocol::Udp => SmoltcpProtocol::Udp,
        };

        SmoltcpSocket::new(
            transport_protocol,
            session_info.source,
            session_info.destination,
            sockets,
        )
    }

    fn create_mio_socket(session_info: &TransportationInfo, poll: &mut Poll, token: Token) -> Option<MioSocket> {
        let transport_protocol = match session_info.transport_protocol {
            TransportProtocol::Tcp => MioTransportProtocol::Tcp,
            TransportProtocol::Udp => MioTransportProtocol::Udp,
        };

        let internet_protocol = match session_info.internet_protocol {
            InternetProtocol::Ipv4 => MioInternetProtocol::Ipv4,
            InternetProtocol::Ipv6 => MioInternetProtocol::Ipv6,
        };

        let mut mio_socket = MioSocket::new(
            transport_protocol,
            internet_protocol,
            session_info.destination,
        )?;

        if let Err(error) = mio_socket.register_poll(poll, token) {
            log::error!("failed to register poll, error={:?}", error);
            return None;
        }

        Some(mio_socket)
    }

    fn init() -> (Interface, PpaassVpnDevice) {
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

    fn create_buffer(session_info: &TransportationInfo) -> Buffers {
        match session_info.transport_protocol {
            TransportProtocol::Tcp => Buffers::Tcp(TcpBuffers::new()),
            TransportProtocol::Udp => Buffers::Udp(UdpBuffers::new()),
        }
    }
}
