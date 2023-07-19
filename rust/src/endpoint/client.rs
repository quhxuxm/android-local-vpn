use crate::types::TransportationsRepository;
use crate::util::log_ip_packet;
use crate::{device::SmoltcpDevice, transport::TransportId};
use crate::{protect_socket, transport::ControlProtocol};
use log::{debug, error, trace};

use std::os::fd::AsRawFd;
use std::{collections::VecDeque, sync::Arc};
use std::{fs::File, io::Write, sync::atomic::AtomicBool};

use anyhow::{anyhow, Result};
use pretty_hex::pretty_hex;
use smoltcp::iface::Config;
use smoltcp::socket::tcp::{Socket as SmoltcpTcpSocket, SocketBuffer as SmoltcpTcpSocketBuffer};
use smoltcp::socket::udp::{PacketBuffer as SmoltcpUdpSocketBuffer, PacketMetadata as SmoltcpUdpPacketMetadata, Socket as SmoltcpUdpSocket};
use smoltcp::time::Instant;
use smoltcp::wire::IpEndpoint;
use smoltcp::wire::{IpAddress, IpCidr};
use smoltcp::{
    iface::{Interface, Routes, SocketHandle, SocketSet},
    wire::Ipv4Address,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use tokio::sync::{Mutex, Notify};
pub(crate) enum ClientEndpoint<'buf> {
    Tcp {
        trans_id: TransportId,
        smoltcp_socket_handle: SocketHandle,
        smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
        smoltcp_interface: Arc<Mutex<Interface>>,
        smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
        smoltcp_recv_buf: Arc<Mutex<VecDeque<u8>>>,
        smoltcp_recv_buf_notify: Arc<Notify>,
        client_file_write: Arc<Mutex<File>>,
    },
    Udp {
        trans_id: TransportId,
        smoltcp_socket_handle: SocketHandle,
        smoltcp_socket_set: Arc<Mutex<SocketSet<'buf>>>,
        smoltcp_interface: Arc<Mutex<Interface>>,
        smoltcp_device: Arc<Mutex<SmoltcpDevice>>,
        smoltcp_recv_buf: Arc<Mutex<VecDeque<u8>>>,
        smoltcp_recv_buf_notify: Arc<Notify>,
        client_file_write: Arc<Mutex<File>>,
    },
}

impl<'buf> ClientEndpoint<'buf> {
    pub(crate) async fn new(trans_id: TransportId) -> Result<ClientEndpoint<'buf>> {
        let (smoltcp_interface, smoltcp_device) = prepare_smoltcp_iface_and_device(trans_id)?;
        let smoltcp_socket_set = SocketSet::new(vec![]);
        let remote_ip_endpoint = IpEndpoint::from(trans_id.destination);

        match trans_id.control_protocol {
            ControlProtocol::Tcp => {
                let smoltcp_socket = create_smoltcp_tcp_socket(trans_id, remote_ip_endpoint)?;
            }
            ControlProtocol::Udp => {}
        }

        todo!()
    }
}

pub(crate) fn prepare_smoltcp_iface_and_device(trans_id: TransportId) -> Result<(Interface, SmoltcpDevice)> {
    let mut routes = Routes::new();
    let default_gateway_ipv4 = Ipv4Address::new(0, 0, 0, 1);
    routes.add_default_ipv4_route(default_gateway_ipv4).unwrap();
    let mut interface_config = Config::default();
    interface_config.random_seed = rand::random::<u64>();
    let mut vpn_device = SmoltcpDevice::new(trans_id);
    let mut interface = Interface::new(interface_config, &mut vpn_device);
    interface.set_any_ip(true);
    interface.update_ip_addrs(|ip_addrs| {
        if let Err(e) = ip_addrs.push(IpCidr::new(IpAddress::v4(0, 0, 0, 1), 0)) {
            error!(">>>> Transportation {trans_id} fail to add ip address to interface in device endpoint because of error: {e:?}")
        }
    });
    interface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(0, 0, 0, 1))
        .map_err(|e| {
            error!(">>>> Transportation {trans_id} fail to add default ipv4 route because of error: {e:?}");
            anyhow!("{e:?}")
        })?;

    Ok((interface, vpn_device))
}

pub(crate) fn create_smoltcp_tcp_socket<'a>(trans_id: TransportId, endpoint: IpEndpoint) -> Result<SmoltcpTcpSocket<'a>> {
    let mut socket = SmoltcpTcpSocket::new(
        SmoltcpTcpSocketBuffer::new(vec![0; 1024 * 1024]),
        SmoltcpTcpSocketBuffer::new(vec![0; 1024 * 1024]),
    );
    socket.listen(endpoint).map_err(|e| anyhow!("{e:?}"))?;
    socket.set_ack_delay(None);
    Ok(socket)
}

pub(crate) fn create_smoltcp_udp_socket<'a>(trans_id: TransportId, endpoint: IpEndpoint) -> Option<SmoltcpUdpSocket<'a>> {
    let mut socket = SmoltcpUdpSocket::new(
        SmoltcpUdpSocketBuffer::new(
            vec![SmoltcpUdpPacketMetadata::EMPTY; 1024 * 1024],
            vec![0; 1024 * 1024],
        ),
        SmoltcpUdpSocketBuffer::new(
            vec![SmoltcpUdpPacketMetadata::EMPTY; 1024 * 1024],
            vec![0; 1024 * 1024],
        ),
    );

    if socket.bind(endpoint).is_err() {
        error!(
            ">>>> Transportation {trans_id} failed to bind smoltcp udp socket, endpoint=[{}]",
            endpoint
        );
        return None;
    }

    Some(socket)
}
