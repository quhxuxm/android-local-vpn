use std::{collections::VecDeque, future::Future, sync::Arc};

use anyhow::Result;

use log::error;
use smoltcp::socket::udp::Socket as SmoltcpUdpSocket;

use smoltcp::{
    iface::{SocketHandle, SocketSet},
    phy::PacketMeta,
    socket::udp::UdpMetadata,
};
use tokio::sync::{mpsc::Sender, Mutex, RwLock};

use crate::config::PpaassVpnServerConfig;
use crate::error::RemoteEndpointError;
use crate::{error::ClientEndpointError, transport::remote::RemoteUdpEndpoint};
use smoltcp::socket::udp::{
    PacketBuffer as SmoltcpUdpSocketBuffer,
    PacketMetadata as SmoltcpUdpPacketMetadata,
};

use super::{
    ClientEndpointCtl, ClientEndpointCtlLockGuard, ClientEndpointState,
    ClientEndpointUdpState, ClientOutputPacket, TransportId,
    {
        poll_and_transfer_smoltcp_data_to_client,
        prepare_smoltcp_iface_and_device,
    },
};

type ClientUdpRecvBuf = RwLock<VecDeque<Vec<u8>>>;

pub(crate) struct ClientUdpEndpoint<'buf> {
    transport_id: TransportId,
    smoltcp_socket_handle: SocketHandle,
    ctl: ClientEndpointCtl<'buf>,
    recv_buffer: Arc<ClientUdpRecvBuf>,
    client_output_tx: Sender<ClientOutputPacket>,
    _config: &'static PpaassVpnServerConfig,
}

impl<'buf> ClientUdpEndpoint<'buf>
where
    'buf: 'static,
{
    pub(crate) fn new(
        transport_id: TransportId,
        client_output_tx: Sender<ClientOutputPacket>,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<ClientUdpEndpoint<'_>, ClientEndpointError> {
        let (smoltcp_iface, smoltcp_device) =
            prepare_smoltcp_iface_and_device(transport_id)?;
        let mut smoltcp_socket_set = SocketSet::new(Vec::with_capacity(1));
        let smoltcp_udp_socket =
            Self::create_smoltcp_udp_socket(transport_id, config)?;
        let smoltcp_socket_handle = smoltcp_socket_set.add(smoltcp_udp_socket);
        let ctl = ClientEndpointCtl::new(
            Mutex::new(smoltcp_socket_set),
            Mutex::new(smoltcp_iface),
            Mutex::new(smoltcp_device),
        );
        Ok(Self {
            transport_id,
            smoltcp_socket_handle,
            ctl,
            recv_buffer: Arc::new(RwLock::new(VecDeque::with_capacity(
                config.get_client_endpoint_udp_recv_buffer_size(),
            ))),
            client_output_tx,
            _config: config,
        })
    }

    fn create_smoltcp_udp_socket<'a>(
        trans_id: TransportId,
        config: &PpaassVpnServerConfig,
    ) -> Result<SmoltcpUdpSocket<'a>, ClientEndpointError> {
        let mut socket = SmoltcpUdpSocket::new(
            SmoltcpUdpSocketBuffer::new(
                vec![SmoltcpUdpPacketMetadata::EMPTY; 32],
                vec![0; config.get_smoltcp_tcp_rx_buffer_size() * 32],
            ),
            SmoltcpUdpSocketBuffer::new(
                vec![SmoltcpUdpPacketMetadata::EMPTY; 32],
                vec![0; config.get_smoltcp_tcp_rx_buffer_size() * 32],
            ),
        );
        socket.bind(trans_id.destination)?;
        Ok(socket)
    }

    pub(crate) async fn get_state(&self) -> ClientEndpointState {
        let ClientEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            ..
        } = self.ctl.lock().await;
        let smoltcp_socket = smoltcp_socket_set
            .get_mut::<SmoltcpUdpSocket>(self.smoltcp_socket_handle);
        ClientEndpointState::Udp(if smoltcp_socket.is_open() {
            ClientEndpointUdpState::Open
        } else {
            ClientEndpointUdpState::Closed
        })
    }

    pub(crate) async fn consume_recv_buffer<'r, F, Fut>(
        &self,
        remote: &'r RemoteUdpEndpoint,
        mut consume_fn: F,
    ) -> Result<(), RemoteEndpointError>
    where
        F: FnMut(TransportId, Vec<u8>, &'r RemoteUdpEndpoint) -> Fut,
        Fut: Future<Output = Result<usize, RemoteEndpointError>>,
    {
        if self.recv_buffer.read().await.len() == 0 {
            return Ok(());
        }
        let mut recv_buffer = self.recv_buffer.write().await;
        let mut consume_size = 0;
        for udp_data in recv_buffer.iter() {
            consume_fn(self.transport_id, udp_data.to_vec(), remote).await?;
            consume_size += 1;
        }
        recv_buffer.drain(..consume_size);
        Ok(())
    }

    pub(crate) async fn send_to_smoltcp(
        &self,
        data: Vec<u8>,
    ) -> Result<usize, ClientEndpointError> {
        let ClientEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            mut smoltcp_iface,
            mut smoltcp_device,
        } = self.ctl.lock().await;
        let smoltcp_socket = smoltcp_socket_set
            .get_mut::<SmoltcpUdpSocket>(self.smoltcp_socket_handle);
        if smoltcp_socket.can_send() {
            let mut udp_packet_meta = PacketMeta::default();
            udp_packet_meta.id = rand::random::<u32>();

            let udp_meta_data = UdpMetadata {
                endpoint: self.transport_id.source.into(),
                meta: udp_packet_meta,
            };
            smoltcp_socket.send_slice(&data, udp_meta_data)?;
            poll_and_transfer_smoltcp_data_to_client(
                self.transport_id,
                &mut smoltcp_socket_set,
                &mut smoltcp_iface,
                &mut smoltcp_device,
                &self.client_output_tx,
            )
            .await;
            return Ok(1);
        }
        Ok(0)
    }

    /// The client tcp & udp packet will go through smoltcp stack
    /// and change the client endpoint state
    pub(crate) async fn receive_from_client(
        &self,
        client_data: Vec<u8>,
    ) -> Result<(), ClientEndpointError> {
        let ClientEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            mut smoltcp_iface,
            mut smoltcp_device,
        } = self.ctl.lock().await;
        smoltcp_device.push_rx(client_data);
        if poll_and_transfer_smoltcp_data_to_client(
            self.transport_id,
            &mut smoltcp_socket_set,
            &mut smoltcp_iface,
            &mut smoltcp_device,
            &self.client_output_tx,
        )
        .await
        {
            let smoltcp_udp_socket = smoltcp_socket_set
                .get_mut::<SmoltcpUdpSocket>(self.smoltcp_socket_handle);
            if !smoltcp_udp_socket.is_open() {
                return Ok(());
            }
            while smoltcp_udp_socket.can_recv() {
                let mut udp_data = [0u8; 65535];
                let udp_data = match smoltcp_udp_socket
                    .recv_slice(&mut udp_data)
                {
                    Ok((0, _)) => break,
                    Ok((size, _)) => &udp_data[..size],
                    Err(e) => {
                        error!(">>>> Transport {} fail to receive udp data from smoltcp because of error: {e:?}", self.transport_id);
                        return Err(
                            ClientEndpointError::SmoltcpUdpReceiveError(e),
                        );
                    }
                };
                self.recv_buffer.write().await.push_back(udp_data.to_vec());
            }
        }
        Ok(())
    }

    pub(crate) async fn abort(&self) {
        self.close().await
    }

    pub(crate) async fn close(&self) {
        let ClientEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            mut smoltcp_iface,
            mut smoltcp_device,
        } = self.ctl.lock().await;

        poll_and_transfer_smoltcp_data_to_client(
            self.transport_id,
            &mut smoltcp_socket_set,
            &mut smoltcp_iface,
            &mut smoltcp_device,
            &self.client_output_tx,
        )
        .await;
    }

    pub(crate) async fn destroy(&self) {
        let ClientEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            mut smoltcp_device,
            ..
        } = self.ctl.lock().await;
        smoltcp_socket_set.remove(self.smoltcp_socket_handle);
        smoltcp_device.destory();
        self.recv_buffer.write().await.clear();
    }
}
