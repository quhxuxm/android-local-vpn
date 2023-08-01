use log::error;
use std::{collections::VecDeque, fs::File, io::Write, sync::Arc};

use crate::{config::PpaassVpnServerConfig, device::SmoltcpDevice, error::ClientEndpointError};

use anyhow::anyhow;
use anyhow::Result;

use smoltcp::{
    iface::{Config, Interface, Routes, SocketHandle},
    phy::PacketMeta,
    socket::udp::UdpMetadata,
    time::Instant,
    wire::HardwareAddress,
};

use crate::transport::TransportId;
use smoltcp::socket::tcp::{Socket as SmoltcpTcpSocket, SocketBuffer as SmoltcpTcpSocketBuffer};
use smoltcp::socket::udp::{
    PacketBuffer as SmoltcpUdpSocketBuffer, PacketMetadata as SmoltcpUdpPacketMetadata, Socket as SmoltcpUdpSocket,
};
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};

use smoltcp::iface::SocketSet;
use tokio::sync::{Mutex, Notify};

use super::{ClientEndpoint, ClientEndpointCtl, ClientEndpointCtlLockGuard};

pub(crate) fn prepare_smoltcp_iface_and_device(transport_id: TransportId) -> Result<(Interface, SmoltcpDevice)> {
    let mut routes = Routes::new();
    let default_gateway_ipv4 = Ipv4Address::new(0, 0, 0, 1);
    routes.add_default_ipv4_route(default_gateway_ipv4).unwrap();
    let mut interface_config = Config::new(HardwareAddress::Ip);
    interface_config.random_seed = rand::random::<u64>();
    let mut vpn_device = SmoltcpDevice::new(transport_id);
    let mut interface = Interface::new(interface_config, &mut vpn_device, Instant::now());
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

pub(crate) fn create_smoltcp_tcp_socket<'a>(
    transport_id: TransportId,
    config: &PpaassVpnServerConfig,
) -> Result<SmoltcpTcpSocket<'a>, ClientEndpointError> {
    let mut socket = SmoltcpTcpSocket::new(
        SmoltcpTcpSocketBuffer::new(vec![0; config.get_smoltcp_tcp_rx_buffer_size()]),
        SmoltcpTcpSocketBuffer::new(vec![0; config.get_smoltcp_tcp_tx_buffer_size()]),
    );
    socket.listen(transport_id.destination)?;
    socket.set_ack_delay(None);
    Ok(socket)
}

pub(crate) fn create_smoltcp_udp_socket<'a>(trans_id: TransportId) -> Result<SmoltcpUdpSocket<'a>> {
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
    socket.bind(trans_id.destination).map_err(|e| {
        error!(">>>> Transport {trans_id} failed to bind smoltcp udp socket");
        anyhow!("{e:?}")
    })?;

    Ok(socket)
}

pub(crate) fn new_tcp(
    transport_id: TransportId,
    client_file_write: Arc<Mutex<File>>,
    config: &'static PpaassVpnServerConfig,
) -> Result<(ClientEndpoint<'_>, Arc<Notify>), ClientEndpointError> {
    let (smoltcp_iface, smoltcp_device) = prepare_smoltcp_iface_and_device(transport_id)?;
    let mut smoltcp_socket_set = SocketSet::new(Vec::with_capacity(1024));
    let smoltcp_tcp_socket = create_smoltcp_tcp_socket(transport_id, config)?;
    let smoltcp_socket_handle = smoltcp_socket_set.add(smoltcp_tcp_socket);
    let recv_buffer_notify = Arc::new(Notify::new());
    let ctl = ClientEndpointCtl::new(
        transport_id,
        Arc::new(Mutex::new(smoltcp_socket_set)),
        Arc::new(Mutex::new(smoltcp_iface)),
        Arc::new(Mutex::new(smoltcp_device)),
    );
    Ok((
        ClientEndpoint::Tcp {
            transport_id,
            smoltcp_socket_handle,
            ctl,
            recv_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(
                config.get_client_endpoint_tcp_recv_buffer_size(),
            ))),
            recv_buffer_notify: Arc::clone(&recv_buffer_notify),
            client_file_write,
            _config: config,
        },
        recv_buffer_notify,
    ))
}

pub(crate) fn new_udp(
    transport_id: TransportId,
    client_file_write: Arc<Mutex<File>>,
    config: &'static PpaassVpnServerConfig,
) -> Result<(ClientEndpoint<'_>, Arc<Notify>), ClientEndpointError> {
    let (smoltcp_iface, smoltcp_device) = prepare_smoltcp_iface_and_device(transport_id)?;
    let mut smoltcp_socket_set = SocketSet::new(Vec::with_capacity(1024));
    let smoltcp_udp_socket = create_smoltcp_udp_socket(transport_id)?;
    let smoltcp_socket_handle = smoltcp_socket_set.add(smoltcp_udp_socket);
    let recv_buffer_notify = Arc::new(Notify::new());
    let ctl = ClientEndpointCtl::new(
        transport_id,
        Arc::new(Mutex::new(smoltcp_socket_set)),
        Arc::new(Mutex::new(smoltcp_iface)),
        Arc::new(Mutex::new(smoltcp_device)),
    );
    Ok((
        ClientEndpoint::Udp {
            transport_id,
            smoltcp_socket_handle,
            ctl,
            recv_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(
                config.get_client_endpoint_udp_recv_buffer_size(),
            ))),
            recv_buffer_notify: Arc::clone(&recv_buffer_notify),
            client_file_write,
            _config: config,
        },
        recv_buffer_notify,
    ))
}

pub(crate) async fn close_client_tcp(
    ctl: &ClientEndpointCtl<'_>,
    smoltcp_socket_handle: SocketHandle,
    transport_id: TransportId,
    client_file_write: Arc<Mutex<File>>,
) {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;
    let smoltcp_socket = smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(smoltcp_socket_handle);
    smoltcp_socket.close();
    if smoltcp_iface.poll(
        Instant::now(),
        &mut *smoltcp_device,
        &mut smoltcp_socket_set,
    ) {
        while let Some(output) = smoltcp_device.pop_tx() {
            let mut client_file_write = client_file_write.lock().await;
            if let Err(e) = client_file_write.write_all(&output) {
                error!("<<<< Transport {transport_id} fail to transfer smoltcp tcp data for output because of error: {e:?}");
                break;
            };
        }
    }
}

pub(crate) async fn close_client_udp(
    ctl: &ClientEndpointCtl<'_>,
    transport_id: TransportId,
    client_file_write: Arc<Mutex<File>>,
) {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;

    if smoltcp_iface.poll(
        Instant::now(),
        &mut *smoltcp_device,
        &mut smoltcp_socket_set,
    ) {
        while let Some(output) = smoltcp_device.pop_tx() {
            let mut client_file_write = client_file_write.lock().await;
            if let Err(e) = client_file_write.write_all(&output) {
                error!("<<<< Transport {transport_id} fail to transfer smoltcp udp data for output because of error: {e:?}");
                break;
            };
        }
    }
}

pub(crate) async fn send_to_client_tcp(
    ctl: &ClientEndpointCtl<'_>,
    smoltcp_socket_handle: SocketHandle,
    data: Vec<u8>,
    transport_id: TransportId,
    client_file_write: Arc<Mutex<File>>,
) -> Result<usize, ClientEndpointError> {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;
    let smoltcp_socket = smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(smoltcp_socket_handle);
    if smoltcp_socket.may_send() {
        let send_result = smoltcp_socket.send_slice(&data).map_err(|e| {
            error!("<<<< Transport {transport_id} fail to transfer remote tcp recv buffer data to smoltcp because of error: {e:?}");
            anyhow!("{e:?}")
        })?;
        if smoltcp_iface.poll(
            Instant::now(),
            &mut *smoltcp_device,
            &mut smoltcp_socket_set,
        ) {
            while let Some(output) = smoltcp_device.pop_tx() {
                let mut client_file_write = client_file_write.lock().await;
                if let Err(e) = client_file_write.write_all(&output) {
                    error!("<<<< Transport {transport_id} fail to transfer smoltcp tcp data for outupt because of error: {e:?}");
                    break;
                };
            }
        }
        return Ok(send_result);
    }
    Ok(0)
}

pub(crate) async fn send_to_client_udp(
    ctl: &ClientEndpointCtl<'_>,
    smoltcp_socket_handle: SocketHandle,
    transport_id: TransportId,
    data: Vec<u8>,
    client_file_write: Arc<Mutex<File>>,
) -> Result<usize, ClientEndpointError> {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;
    let smoltcp_socket = smoltcp_socket_set.get_mut::<SmoltcpUdpSocket>(smoltcp_socket_handle);
    if smoltcp_socket.can_send() {
        let mut udp_packet_meta = PacketMeta::default();
        udp_packet_meta.id = rand::random::<u32>();

        let udp_meta_data = UdpMetadata {
            endpoint: transport_id.source.into(),
            meta: udp_packet_meta,
        };
        smoltcp_socket
            .send_slice(&data, udp_meta_data)
            .map_err(|e| {
                error!("<<<< Transport {transport_id} fail to transfer remote udp recv buffer data to smoltcp with endpoint [{:?}] because of error: {e:?}", smoltcp_socket.endpoint());
                anyhow!("{e:?}")
            })?;
        if smoltcp_iface.poll(
            Instant::now(),
            &mut *smoltcp_device,
            &mut smoltcp_socket_set,
        ) {
            while let Some(output) = smoltcp_device.pop_tx() {
                let mut client_file_write = client_file_write.lock().await;
                if let Err(e) = client_file_write.write_all(&output) {
                    error!("<<<< Transport {transport_id} fail to transfer smoltcp tcp data for output because of error: {e:?}");
                    break;
                };
            }
        }
        return Ok(1);
    }
    Ok(0)
}

async fn recv_to_vpn_device_and_poll(
    smoltcp_device: &mut SmoltcpDevice,
    smoltcp_iface: &mut Interface,
    smoltcp_socket_set: &mut SocketSet<'_>,
    client_data: Vec<u8>,
) -> bool {
    smoltcp_device.push_rx(client_data);
    smoltcp_iface.poll(Instant::now(), smoltcp_device, smoltcp_socket_set)
}

pub(crate) async fn recv_from_client_tcp(
    ctl: &ClientEndpointCtl<'_>,
    client_data: Vec<u8>,
    smoltcp_socket_handle: SocketHandle,
    transport_id: TransportId,
    client_file_write: Arc<Mutex<File>>,
    recv_buffer: &Mutex<VecDeque<u8>>,
    recv_buffer_notify: &Notify,
) {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;

    if recv_to_vpn_device_and_poll(
        &mut smoltcp_device,
        &mut smoltcp_iface,
        &mut smoltcp_socket_set,
        client_data,
    )
    .await
    {
        let smoltcp_tcp_socket = smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(smoltcp_socket_handle);
        while let Some(output) = smoltcp_device.pop_tx() {
            let mut client_file_write = client_file_write.lock().await;
            if let Err(e) = client_file_write.write_all(&output) {
                error!("<<<< Transport {transport_id} fail to transfer smoltcp tcp data for output because of error: {e:?}");
                break;
            };
        }
        while smoltcp_tcp_socket.may_recv() {
            let mut tcp_data = [0u8; 65536];
            let tcp_data = match smoltcp_tcp_socket.recv_slice(&mut tcp_data) {
                Ok(0) => break,
                Ok(size) => &tcp_data[..size],
                Err(e) => {
                    error!(
                        ">>>> Transport {transport_id} fail to receive tcp data from smoltcp because of error: {e:?}"
                    );
                    break;
                }
            };
            recv_buffer.lock().await.extend(tcp_data);
        }
        if !recv_buffer.lock().await.is_empty() {
            recv_buffer_notify.notify_waiters();
        }
    }
}

pub(crate) async fn recv_from_client_udp(
    ctl: &ClientEndpointCtl<'_>,
    client_data: Vec<u8>,
    smoltcp_socket_handle: SocketHandle,
    transport_id: TransportId,
    client_file_write: Arc<Mutex<File>>,
    recv_buffer: &Mutex<VecDeque<Vec<u8>>>,
    recv_buffer_notify: &Notify,
) {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;
    if recv_to_vpn_device_and_poll(
        &mut smoltcp_device,
        &mut smoltcp_iface,
        &mut smoltcp_socket_set,
        client_data,
    )
    .await
    {
        let smoltcp_udp_socket = smoltcp_socket_set.get_mut::<SmoltcpUdpSocket>(smoltcp_socket_handle);
        while let Some(output) = smoltcp_device.pop_tx() {
            let mut client_file_write = client_file_write.lock().await;
            if let Err(e) = client_file_write.write_all(&output) {
                error!("<<<< Transport {transport_id} fail to transfer smoltcp udp data for output because of error: {e:?}");
                break;
            };
        }
        while smoltcp_udp_socket.can_recv() {
            let mut udp_data = [0u8; 65535];
            let udp_data = match smoltcp_udp_socket.recv_slice(&mut udp_data) {
                Ok((0, _)) => break,
                Ok((size, _)) => &udp_data[..size],
                Err(e) => {
                    error!(
                        ">>>> Transport {transport_id} fail to receive udp data from smoltcp because of error: {e:?}"
                    );
                    break;
                }
            };
            recv_buffer.lock().await.push_back(udp_data.to_vec());
        }
        if !recv_buffer.lock().await.is_empty() {
            recv_buffer_notify.notify_waiters();
        }
    }
}
