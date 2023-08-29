use log::{error, info};
use std::{collections::VecDeque, sync::Arc};

use crate::{
    config::PpaassVpnServerConfig, device::SmoltcpDevice,
    error::ClientEndpointError, util::ClientOutputPacket,
};

use anyhow::anyhow;
use anyhow::Result;

use smoltcp::{
    iface::{Config, Interface, SocketHandle},
    phy::PacketMeta,
    socket::udp::UdpMetadata,
    time::Instant,
    wire::HardwareAddress,
};

use crate::transport::TransportId;
use smoltcp::socket::tcp::{
    Socket as SmoltcpTcpSocket, SocketBuffer as SmoltcpTcpSocketBuffer,
};
use smoltcp::socket::udp::{
    PacketBuffer as SmoltcpUdpSocketBuffer,
    PacketMetadata as SmoltcpUdpPacketMetadata, Socket as SmoltcpUdpSocket,
};
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};

use smoltcp::iface::SocketSet;
use tokio::sync::{mpsc::Sender, Mutex, Notify, RwLock};

use super::{
    ClientEndpoint, ClientEndpointCtl, ClientEndpointCtlLockGuard,
    ClientTcpRecvBuf, ClientUdpRecvBuf,
};

static DEFAULT_GATEWAY_IPV4_ADDR: Ipv4Address = Ipv4Address::new(0, 0, 0, 1);

pub(crate) fn prepare_smoltcp_iface_and_device(
    transport_id: TransportId,
) -> Result<(Interface, SmoltcpDevice)> {
    let mut interface_config = Config::new(HardwareAddress::Ip);
    interface_config.random_seed = rand::random::<u64>();
    let mut vpn_device = SmoltcpDevice::new(transport_id);
    let mut interface =
        Interface::new(interface_config, &mut vpn_device, Instant::now());
    interface.set_any_ip(true);
    interface.update_ip_addrs(|ip_addrs| {
        if let Err(e) = ip_addrs.push(IpCidr::new(IpAddress::v4(0, 0, 0, 1), 0)) {
            error!(">>>> Transportation {transport_id} fail to add ip address to interface in device endpoint because of error: {e:?}")
        }
    });
    interface
        .routes_mut()
        .add_default_ipv4_route(DEFAULT_GATEWAY_IPV4_ADDR)
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
        SmoltcpTcpSocketBuffer::new(vec![
            0;
            config
                .get_smoltcp_tcp_rx_buffer_size()
        ]),
        SmoltcpTcpSocketBuffer::new(vec![
            0;
            config
                .get_smoltcp_tcp_tx_buffer_size()
        ]),
    );
    socket.listen(transport_id.destination)?;
    socket.set_ack_delay(None);
    Ok(socket)
}

pub(crate) fn create_smoltcp_udp_socket<'a>(
    trans_id: TransportId,
) -> Result<SmoltcpUdpSocket<'a>> {
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
    client_output_tx: Sender<ClientOutputPacket>,
    config: &'static PpaassVpnServerConfig,
) -> Result<ClientEndpoint<'_>, ClientEndpointError> {
    let (smoltcp_iface, smoltcp_device) =
        prepare_smoltcp_iface_and_device(transport_id)?;
    let mut smoltcp_socket_set = SocketSet::new(Vec::with_capacity(1));
    let smoltcp_tcp_socket = create_smoltcp_tcp_socket(transport_id, config)?;
    let smoltcp_socket_handle = smoltcp_socket_set.add(smoltcp_tcp_socket);
    let ctl = ClientEndpointCtl::new(
        Arc::new(Mutex::new(smoltcp_socket_set)),
        Arc::new(Mutex::new(smoltcp_iface)),
        Arc::new(Mutex::new(smoltcp_device)),
    );
    Ok(ClientEndpoint::Tcp {
        transport_id,
        smoltcp_socket_handle,
        ctl,
        recv_buffer: Arc::new((
            RwLock::new(VecDeque::with_capacity(
                config.get_client_endpoint_tcp_recv_buffer_size(),
            )),
            Notify::new(),
        )),
        client_output_tx,
        _config: config,
    })
}

pub(crate) fn new_udp(
    transport_id: TransportId,
    client_output_tx: Sender<ClientOutputPacket>,
    config: &'static PpaassVpnServerConfig,
) -> Result<ClientEndpoint<'_>, ClientEndpointError> {
    let (smoltcp_iface, smoltcp_device) =
        prepare_smoltcp_iface_and_device(transport_id)?;
    let mut smoltcp_socket_set = SocketSet::new(Vec::with_capacity(1));
    let smoltcp_udp_socket = create_smoltcp_udp_socket(transport_id)?;
    let smoltcp_socket_handle = smoltcp_socket_set.add(smoltcp_udp_socket);
    let ctl = ClientEndpointCtl::new(
        Arc::new(Mutex::new(smoltcp_socket_set)),
        Arc::new(Mutex::new(smoltcp_iface)),
        Arc::new(Mutex::new(smoltcp_device)),
    );
    Ok(ClientEndpoint::Udp {
        transport_id,
        smoltcp_socket_handle,
        ctl,
        recv_buffer: Arc::new((
            RwLock::new(VecDeque::with_capacity(
                config.get_client_endpoint_udp_recv_buffer_size(),
            )),
            Notify::new(),
        )),
        client_output_tx,
        _config: config,
    })
}

async fn poll_and_transfer_smoltcp_data_to_client(
    transport_id: TransportId,
    smoltcp_socket_set: &mut SocketSet<'_>,
    smoltcp_iface: &mut Interface,
    smoltcp_device: &mut SmoltcpDevice,
    client_output_tx: &Sender<ClientOutputPacket>,
) -> bool {
    if !smoltcp_iface.poll(Instant::now(), smoltcp_device, smoltcp_socket_set) {
        return false;
    }
    while let Some(output) = smoltcp_device.pop_tx() {
        if let Err(e) = client_output_tx
            .send(ClientOutputPacket {
                transport_id,
                data: output,
            })
            .await
        {
            error!("<<<< Transport {transport_id} fail to transfer smoltcp data for output because of error: {e:?}");
            break;
        };
    }
    true
}

pub(crate) async fn abort_client_tcp(
    ctl: &ClientEndpointCtl<'_>,
    smoltcp_socket_handle: SocketHandle,
    transport_id: TransportId,
    client_output_tx: &Sender<ClientOutputPacket>,
) {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;
    let smoltcp_socket =
        smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(smoltcp_socket_handle);
    smoltcp_socket.abort();
    poll_and_transfer_smoltcp_data_to_client(
        transport_id,
        &mut smoltcp_socket_set,
        &mut smoltcp_iface,
        &mut smoltcp_device,
        client_output_tx,
    )
    .await;
}
pub(crate) async fn close_client_tcp(
    ctl: &ClientEndpointCtl<'_>,
    smoltcp_socket_handle: SocketHandle,
    transport_id: TransportId,
    client_output_tx: &Sender<ClientOutputPacket>,
) {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;
    let smoltcp_socket =
        smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(smoltcp_socket_handle);
    smoltcp_socket.close();
    poll_and_transfer_smoltcp_data_to_client(
        transport_id,
        &mut smoltcp_socket_set,
        &mut smoltcp_iface,
        &mut smoltcp_device,
        client_output_tx,
    )
    .await;
}

pub(crate) async fn close_client_udp(
    ctl: &ClientEndpointCtl<'_>,
    transport_id: TransportId,
    client_output_tx: &Sender<ClientOutputPacket>,
) {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;

    poll_and_transfer_smoltcp_data_to_client(
        transport_id,
        &mut smoltcp_socket_set,
        &mut smoltcp_iface,
        &mut smoltcp_device,
        client_output_tx,
    )
    .await;
}

pub(crate) async fn send_to_client_tcp(
    ctl: &ClientEndpointCtl<'_>,
    smoltcp_socket_handle: SocketHandle,
    data: Vec<u8>,
    transport_id: TransportId,
    client_output_tx: &Sender<ClientOutputPacket>,
) -> Result<usize, ClientEndpointError> {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;
    let smoltcp_socket =
        smoltcp_socket_set.get_mut::<SmoltcpTcpSocket>(smoltcp_socket_handle);
    if smoltcp_socket.may_send() {
        let send_result = smoltcp_socket.send_slice(&data)?;
        poll_and_transfer_smoltcp_data_to_client(
            transport_id,
            &mut smoltcp_socket_set,
            &mut smoltcp_iface,
            &mut smoltcp_device,
            client_output_tx,
        )
        .await;
        return Ok(send_result);
    }
    Ok(0)
}

pub(crate) async fn send_to_client_udp(
    ctl: &ClientEndpointCtl<'_>,
    smoltcp_socket_handle: SocketHandle,
    transport_id: TransportId,
    data: Vec<u8>,
    client_output_tx: &Sender<ClientOutputPacket>,
) -> Result<usize, ClientEndpointError> {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;
    let smoltcp_socket =
        smoltcp_socket_set.get_mut::<SmoltcpUdpSocket>(smoltcp_socket_handle);
    if smoltcp_socket.can_send() {
        let mut udp_packet_meta = PacketMeta::default();
        udp_packet_meta.id = rand::random::<u32>();

        let udp_meta_data = UdpMetadata {
            endpoint: transport_id.source.into(),
            meta: udp_packet_meta,
        };
        smoltcp_socket.send_slice(&data, udp_meta_data)?;
        poll_and_transfer_smoltcp_data_to_client(
            transport_id,
            &mut smoltcp_socket_set,
            &mut smoltcp_iface,
            &mut smoltcp_device,
            client_output_tx,
        )
        .await;
        return Ok(1);
    }
    Ok(0)
}

pub(crate) async fn recv_from_client_tcp(
    ctl: &ClientEndpointCtl<'_>,
    client_data: Vec<u8>,
    smoltcp_socket_handle: SocketHandle,
    transport_id: TransportId,
    client_output_tx: &Sender<ClientOutputPacket>,
    recv_buffer: &ClientTcpRecvBuf,
) -> Result<(), ClientEndpointError> {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;

    smoltcp_device.push_rx(client_data);
    if poll_and_transfer_smoltcp_data_to_client(
        transport_id,
        &mut smoltcp_socket_set,
        &mut smoltcp_iface,
        &mut smoltcp_device,
        client_output_tx,
    )
    .await
    {
        let smoltcp_tcp_socket = smoltcp_socket_set
            .get_mut::<SmoltcpTcpSocket>(smoltcp_socket_handle);
        info!(
            ">>>> Transport {transport_id} client endpoint state: {}",
            smoltcp_tcp_socket.state()
        );
        while smoltcp_tcp_socket.may_recv() {
            let mut tcp_data = [0u8; 65536];
            let tcp_data = match smoltcp_tcp_socket.recv_slice(&mut tcp_data) {
                Ok(0) => break,
                Ok(size) => &tcp_data[..size],
                Err(e) => {
                    error!(
                        ">>>> Transport {transport_id} fail to receive tcp data from smoltcp because of error: {e:?}"
                    );
                    recv_buffer.1.notify_waiters();
                    return Err(ClientEndpointError::SmoltcpTcpReceiveError(e));
                }
            };
            recv_buffer.0.write().await.extend(tcp_data);
        }
        recv_buffer.1.notify_waiters();
    }
    Ok(())
}

pub(crate) async fn recv_from_client_udp(
    ctl: &ClientEndpointCtl<'_>,
    client_data: Vec<u8>,
    smoltcp_socket_handle: SocketHandle,
    transport_id: TransportId,
    client_output_tx: &Sender<ClientOutputPacket>,
    recv_buffer: &ClientUdpRecvBuf,
) -> Result<(), ClientEndpointError> {
    let ClientEndpointCtlLockGuard {
        mut smoltcp_socket_set,
        mut smoltcp_iface,
        mut smoltcp_device,
    } = ctl.lock().await;
    smoltcp_device.push_rx(client_data);
    if poll_and_transfer_smoltcp_data_to_client(
        transport_id,
        &mut smoltcp_socket_set,
        &mut smoltcp_iface,
        &mut smoltcp_device,
        client_output_tx,
    )
    .await
    {
        let smoltcp_udp_socket = smoltcp_socket_set
            .get_mut::<SmoltcpUdpSocket>(smoltcp_socket_handle);
        if !smoltcp_udp_socket.is_open() {
            return Ok(());
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
                    recv_buffer.1.notify_waiters();
                    return Err(ClientEndpointError::SmoltcpUdpReceiveError(e));
                }
            };
            recv_buffer.0.write().await.push_back(udp_data.to_vec());
        }

        recv_buffer.1.notify_waiters();
    }
    Ok(())
}
