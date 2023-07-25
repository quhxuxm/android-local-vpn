use log::error;

use crate::device::SmoltcpDevice;

use anyhow::anyhow;
use anyhow::Result;

use smoltcp::iface::{Config, Interface, Routes};

use crate::transport::TransportId;
use smoltcp::socket::tcp::{Socket as SmoltcpTcpSocket, SocketBuffer as SmoltcpTcpSocketBuffer};
use smoltcp::socket::udp::{PacketBuffer as SmoltcpUdpSocketBuffer, PacketMetadata as SmoltcpUdpPacketMetadata, Socket as SmoltcpUdpSocket};
use smoltcp::wire::{IpAddress, IpCidr, IpEndpoint, Ipv4Address};

pub(crate) fn prepare_smoltcp_iface_and_device(transport_id: TransportId) -> Result<(Interface, SmoltcpDevice)> {
    let mut routes = Routes::new();
    let default_gateway_ipv4 = Ipv4Address::new(0, 0, 0, 1);
    routes.add_default_ipv4_route(default_gateway_ipv4).unwrap();
    let mut interface_config = Config::default();
    interface_config.random_seed = rand::random::<u64>();
    let mut vpn_device = SmoltcpDevice::new(transport_id);
    let mut interface = Interface::new(interface_config, &mut vpn_device);
    interface.set_any_ip(true);
    interface.update_ip_addrs(|ip_addrs| {
        if let Err(e) = ip_addrs.push(IpCidr::new(IpAddress::v4(0, 0, 0, 1), 0)) {
            error!(">>>> Transportation {transport_id} fail to add ip address to interface in device endpoint because of error: {e:?}")
        }
    });
    interface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(0, 0, 0, 1))
        .map_err(|e| {
            error!(">>>> Transportation {transport_id} fail to add default ipv4 route because of error: {e:?}");
            anyhow!("{e:?}")
        })?;

    Ok((interface, vpn_device))
}

pub(crate) fn create_smoltcp_tcp_socket<'a>(transport_id: TransportId, endpoint: IpEndpoint) -> Result<SmoltcpTcpSocket<'a>> {
    let mut socket = SmoltcpTcpSocket::new(
        SmoltcpTcpSocketBuffer::new(vec![0; 1024 * 1024]),
        SmoltcpTcpSocketBuffer::new(vec![0; 1024 * 1024]),
    );

    socket.listen(endpoint).map_err(|e| {
        error!(
            ">>>> Transport {transport_id} failed to listen on smoltcp tcp socket, endpoint=[{}]",
            endpoint
        );
        anyhow!("{e:?}")
    })?;
    socket.set_ack_delay(None);
    Ok(socket)
}

pub(crate) fn create_smoltcp_udp_socket<'a>(trans_id: TransportId, endpoint: IpEndpoint) -> Result<SmoltcpUdpSocket<'a>> {
    let mut socket = SmoltcpUdpSocket::new(
        SmoltcpUdpSocketBuffer::new(
            // vec![UdpPacketMetadata::EMPTY, UdpPacketMetadata::EMPTY],
            vec![SmoltcpUdpPacketMetadata::EMPTY; 1024 * 1024],
            vec![0; 1024 * 1024],
        ),
        SmoltcpUdpSocketBuffer::new(
            // vec![UdpPacketMetadata::EMPTY, UdpPacketMetadata::EMPTY],
            vec![SmoltcpUdpPacketMetadata::EMPTY; 1024 * 1024],
            vec![0; 1024 * 1024],
        ),
    );

    socket.bind(endpoint).map_err(|e| {
        error!(
            ">>>> Transport {trans_id} failed to bind smoltcp udp socket, endpoint=[{}]",
            endpoint
        );
        anyhow!("{e:?}")
    })?;

    Ok(socket)
}
