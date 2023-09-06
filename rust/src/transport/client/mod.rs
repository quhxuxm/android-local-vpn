mod tcp;
mod udp;

use smoltcp::iface::SocketSet;

use smoltcp::iface::Interface;
use tokio::sync::{mpsc::Sender, Mutex, MutexGuard};

use crate::device::SmoltcpDevice;
use crate::error::ClientEndpointError;

use super::{ClientOutputPacket, TransportId};
use log::error;

use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};
use smoltcp::{iface::Config, time::Instant, wire::HardwareAddress};

pub(crate) use tcp::ClientTcpEndpoint;
pub(crate) use udp::ClientUdpEndpoint;

static DEFAULT_GATEWAY_IPV4_ADDR: Ipv4Address = Ipv4Address::new(0, 0, 0, 1);

pub(crate) struct ClientEndpointCtlLockGuard<'lock, 'buf> {
    pub(crate) smoltcp_socket_set: MutexGuard<'lock, SocketSet<'buf>>,
    pub(crate) smoltcp_iface: MutexGuard<'lock, Interface>,
    pub(crate) smoltcp_device: MutexGuard<'lock, SmoltcpDevice>,
}

pub(crate) struct ClientEndpointCtl<'buf> {
    smoltcp_socket_set: Mutex<SocketSet<'buf>>,
    smoltcp_iface: Mutex<Interface>,
    smoltcp_device: Mutex<SmoltcpDevice>,
}

impl<'buf> ClientEndpointCtl<'buf> {
    fn new(
        smoltcp_socket_set: Mutex<SocketSet<'buf>>,
        smoltcp_iface: Mutex<Interface>,
        smoltcp_device: Mutex<SmoltcpDevice>,
    ) -> Self {
        Self {
            smoltcp_socket_set,
            smoltcp_iface,
            smoltcp_device,
        }
    }
    pub(crate) async fn lock<'lock>(
        &'lock self,
    ) -> ClientEndpointCtlLockGuard<'lock, 'buf> {
        let smoltcp_device = self.smoltcp_device.lock().await;
        let smoltcp_iface = self.smoltcp_iface.lock().await;
        let smoltcp_socket_set = self.smoltcp_socket_set.lock().await;
        ClientEndpointCtlLockGuard {
            smoltcp_socket_set,
            smoltcp_iface,
            smoltcp_device,
        }
    }
}

fn prepare_smoltcp_iface_and_device(
    transport_id: TransportId,
) -> Result<(Interface, SmoltcpDevice), ClientEndpointError> {
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
        .add_default_ipv4_route(DEFAULT_GATEWAY_IPV4_ADDR)?;
    Ok((interface, vpn_device))
}

async fn poll_and_transfer_smoltcp_data_to_client(
    transport_id: TransportId,
    smoltcp_socket_set: &mut SocketSet<'_>,
    smoltcp_iface: &mut Interface,
    smoltcp_device: &mut SmoltcpDevice,

    client_output_tx: &Sender<ClientOutputPacket>,
) -> bool {
    smoltcp_iface.poll(Instant::now(), smoltcp_device, smoltcp_socket_set);

    while let Some(data) = smoltcp_device.pop_tx() {
        if let Err(e) = client_output_tx
            .send(ClientOutputPacket { transport_id, data })
            .await
        {
            error!("<<<< Transport {transport_id} fail to transfer smoltcp data for output because of error: {e:?}");
            break;
        };
    }
    true
}
