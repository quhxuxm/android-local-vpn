use std::{collections::VecDeque, future::Future};

use anyhow::Result;

use bytes::{Bytes, BytesMut};
use log::error;
use smoltcp::{iface::Interface, socket::udp::Socket as SmoltcpUdpSocket};

use smoltcp::{
    iface::{SocketHandle, SocketSet},
    phy::PacketMeta,
    socket::udp::UdpMetadata,
};
use tokio::sync::mpsc::Sender;

use super::{
    ClientOutputPacket, TransportId,
    {poll_smoltcp_and_flush, prepare_smoltcp_iface_and_device},
};
use crate::config::PpaassVpnServerConfig;
use crate::device::SmoltcpDevice;
use crate::error::RemoteEndpointError;
use crate::{error::ClientEndpointError, transport::remote::RemoteUdpEndpoint};
use smoltcp::socket::udp::{
    PacketBuffer as SmoltcpUdpSocketBuffer,
    PacketMetadata as SmoltcpUdpPacketMetadata,
};

type ClientUdpRecvBuf = VecDeque<Bytes>;

pub(crate) struct ClientUdpEndpoint<'buf> {
    transport_id: TransportId,
    smoltcp_socket_handle: SocketHandle,
    smoltcp_socket_set: SocketSet<'buf>,
    smoltcp_iface: Interface,
    smoltcp_device: SmoltcpDevice,
    recv_buffer: ClientUdpRecvBuf,
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

        Ok(Self {
            transport_id,
            smoltcp_socket_handle,
            smoltcp_socket_set,
            smoltcp_iface,
            smoltcp_device,
            recv_buffer: VecDeque::with_capacity(
                config.get_client_endpoint_udp_recv_buffer_size(),
            ),
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
                vec![
                    SmoltcpUdpPacketMetadata::EMPTY;
                    config.get_smoltcp_udp_rx_packet_buffer_number()
                ],
                vec![
                    0;
                    config.get_smoltcp_udp_packet_size()
                        * config.get_smoltcp_udp_rx_packet_buffer_number()
                ],
            ),
            SmoltcpUdpSocketBuffer::new(
                vec![
                    SmoltcpUdpPacketMetadata::EMPTY;
                    config.get_smoltcp_udp_tx_packet_buffer_number()
                ],
                vec![
                    0;
                    config.get_smoltcp_udp_packet_size()
                        * config.get_smoltcp_udp_tx_packet_buffer_number()
                ],
            ),
        );
        socket.bind(trans_id.destination)?;
        Ok(socket)
    }

    pub(crate) async fn consume_recv_buffer<'r, F, Fut>(
        &mut self,
        remote: &'r mut RemoteUdpEndpoint,
        mut consume_fn: F,
    ) -> Result<(), RemoteEndpointError>
    where
        F: FnMut(TransportId, Vec<Bytes>, &'r mut RemoteUdpEndpoint) -> Fut,
        Fut: Future<Output = Result<usize, RemoteEndpointError>>,
    {
        if self.recv_buffer.is_empty() {
            return Ok(());
        }

        let consume_size = consume_fn(
            self.transport_id,
            self.recv_buffer.make_contiguous().to_vec(),
            remote,
        )
        .await?;
        // let mut consume_size = 0;
        // for udp_data in self.recv_buffer.iter() {
        //     consume_fn(self.transport_id, udp_data.to_vec(), remote).await?;
        //     consume_size += 1;
        // }
        self.recv_buffer.drain(..consume_size);
        Ok(())
    }

    pub(crate) async fn send_to_smoltcp(
        &mut self,
        data: &[u8],
    ) -> Result<usize, ClientEndpointError> {
        let smoltcp_socket = self
            .smoltcp_socket_set
            .get_mut::<SmoltcpUdpSocket>(self.smoltcp_socket_handle);
        if smoltcp_socket.can_send() {
            let mut udp_packet_meta = PacketMeta::default();
            udp_packet_meta.id = rand::random::<u32>();

            let udp_meta_data = UdpMetadata {
                endpoint: self.transport_id.source.into(),
                meta: udp_packet_meta,
            };
            smoltcp_socket.send_slice(data, udp_meta_data)?;
            poll_smoltcp_and_flush(
                self.transport_id,
                &mut self.smoltcp_socket_set,
                &mut self.smoltcp_iface,
                &mut self.smoltcp_device,
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
        &mut self,
        client_data: BytesMut,
    ) -> Result<(), ClientEndpointError> {
        self.smoltcp_device.push_rx(client_data);
        if poll_smoltcp_and_flush(
            self.transport_id,
            &mut self.smoltcp_socket_set,
            &mut self.smoltcp_iface,
            &mut self.smoltcp_device,
            &self.client_output_tx,
        )
        .await
        {
            let smoltcp_udp_socket = self
                .smoltcp_socket_set
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
                    Ok((size, _)) => Bytes::from(udp_data[..size].to_vec()),
                    Err(e) => {
                        error!(">>>> Transport {} fail to receive udp data from smoltcp because of error: {e:?}", self.transport_id);
                        return Err(
                            ClientEndpointError::SmoltcpUdpReceiveError(e),
                        );
                    }
                };
                self.recv_buffer.push_back(udp_data);
            }
        }
        Ok(())
    }

    pub(crate) async fn close(&mut self) {
        poll_smoltcp_and_flush(
            self.transport_id,
            &mut self.smoltcp_socket_set,
            &mut self.smoltcp_iface,
            &mut self.smoltcp_device,
            &self.client_output_tx,
        )
        .await;
    }

    pub(crate) async fn destroy(&mut self) {
        self.smoltcp_socket_set.remove(self.smoltcp_socket_handle);
        self.smoltcp_device.destory();
        self.recv_buffer.clear();
    }
}
