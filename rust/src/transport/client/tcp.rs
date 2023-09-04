use std::{collections::VecDeque, future::Future, sync::Arc};

use anyhow::Result;

use log::error;
use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::tcp::Socket as SmoltcpTcpSocket;
use tokio::sync::{mpsc::Sender, Mutex, RwLock};

use crate::config::PpaassVpnServerConfig;
use crate::error::RemoteEndpointError;
use crate::{error::ClientEndpointError, transport::remote::RemoteTcpEndpoint};
use smoltcp::socket::tcp::SocketBuffer as SmoltcpTcpSocketBuffer;

use super::{
    ClientEndpointCtl, ClientEndpointCtlLockGuard, ClientEndpointState,
    ClientOutputPacket, TransportId,
    {
        poll_and_transfer_smoltcp_data_to_client,
        prepare_smoltcp_iface_and_device,
    },
};

type ClientTcpRecvBuf = RwLock<VecDeque<u8>>;

pub(crate) struct ClientTcpEndpoint<'buf> {
    transport_id: TransportId,
    ctl: ClientEndpointCtl<'buf>,
    smoltcp_socket_handle: SocketHandle,
    recv_buffer: Arc<ClientTcpRecvBuf>,
    client_output_tx: Sender<ClientOutputPacket>,
    _config: &'static PpaassVpnServerConfig,
}

impl<'buf> ClientTcpEndpoint<'buf>
where
    'buf: 'static,
{
    pub(crate) fn new(
        transport_id: TransportId,
        client_output_tx: Sender<ClientOutputPacket>,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<ClientTcpEndpoint<'_>, ClientEndpointError> {
        let (smoltcp_iface, smoltcp_device) =
            prepare_smoltcp_iface_and_device(transport_id)?;
        let mut smoltcp_socket_set = SocketSet::new(Vec::with_capacity(1));
        let smoltcp_tcp_socket =
            Self::create_smoltcp_tcp_socket(transport_id, config)?;
        let smoltcp_socket_handle = smoltcp_socket_set.add(smoltcp_tcp_socket);
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
                config.get_client_endpoint_tcp_recv_buffer_size(),
            ))),
            client_output_tx,
            _config: config,
        })
    }

    fn create_smoltcp_tcp_socket<'a>(
        transport_id: TransportId,
        config: &PpaassVpnServerConfig,
    ) -> Result<SmoltcpTcpSocket<'a>, ClientEndpointError> {
        let mut socket = SmoltcpTcpSocket::new(
            SmoltcpTcpSocketBuffer::new(vec![
                0;
                config
                    .get_smoltcp_tcp_rx_buffer_size(
                    )
            ]),
            SmoltcpTcpSocketBuffer::new(vec![
                0;
                config
                    .get_smoltcp_tcp_tx_buffer_size(
                    )
            ]),
        );
        socket.listen(transport_id.destination)?;
        socket.set_ack_delay(None);
        Ok(socket)
    }

    pub(crate) async fn get_state(&self) -> ClientEndpointState {
        let ClientEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            ..
        } = self.ctl.lock().await;
        let smoltcp_socket_handle = self.smoltcp_socket_handle;
        let smoltcp_socket = smoltcp_socket_set
            .get_mut::<SmoltcpTcpSocket>(smoltcp_socket_handle);
        ClientEndpointState::Tcp(smoltcp_socket.state())
    }

    pub(crate) async fn consume_recv_buffer<'r, F, Fut>(
        &self,
        remote: &'r RemoteTcpEndpoint,
        mut consume_fn: F,
    ) -> Result<(), RemoteEndpointError>
    where
        F: FnMut(TransportId, Vec<u8>, &'r RemoteTcpEndpoint) -> Fut,
        Fut: Future<Output = Result<usize, RemoteEndpointError>>,
    {
        if self.recv_buffer.read().await.len() == 0 {
            return Ok(());
        }
        let mut recv_buffer = self.recv_buffer.write().await;
        let recv_buffer_data = recv_buffer.make_contiguous().to_vec();
        let consume_size =
            consume_fn(self.transport_id, recv_buffer_data, remote).await?;
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
            .get_mut::<SmoltcpTcpSocket>(self.smoltcp_socket_handle);
        if smoltcp_socket.may_send() {
            let send_result = smoltcp_socket.send_slice(&data)?;
            poll_and_transfer_smoltcp_data_to_client(
                self.transport_id,
                &mut smoltcp_socket_set,
                &mut smoltcp_iface,
                &mut smoltcp_device,
                &self.client_output_tx,
            )
            .await;
            return Ok(send_result);
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
            let smoltcp_tcp_socket = smoltcp_socket_set
                .get_mut::<SmoltcpTcpSocket>(self.smoltcp_socket_handle);
            while smoltcp_tcp_socket.may_recv() {
                let mut tcp_data = [0u8; 65536];
                let tcp_data = match smoltcp_tcp_socket
                    .recv_slice(&mut tcp_data)
                {
                    Ok(0) => break,
                    Ok(size) => &tcp_data[..size],
                    Err(e) => {
                        error!(">>>> Transport {} fail to receive tcp data from smoltcp because of error: {e:?}", self.transport_id);

                        return Err(
                            ClientEndpointError::SmoltcpTcpReceiveError(e),
                        );
                    }
                };
                self.recv_buffer.write().await.extend(tcp_data);
            }
        }
        Ok(())
    }

    pub(crate) async fn abort(&self) {
        let ClientEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            mut smoltcp_iface,
            mut smoltcp_device,
        } = self.ctl.lock().await;
        let smoltcp_socket = smoltcp_socket_set
            .get_mut::<SmoltcpTcpSocket>(self.smoltcp_socket_handle);
        smoltcp_socket.abort();
        poll_and_transfer_smoltcp_data_to_client(
            self.transport_id,
            &mut smoltcp_socket_set,
            &mut smoltcp_iface,
            &mut smoltcp_device,
            &self.client_output_tx,
        )
        .await;
    }
    pub(crate) async fn close(&self) {
        let ClientEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            mut smoltcp_iface,
            mut smoltcp_device,
        } = self.ctl.lock().await;
        let smoltcp_socket = smoltcp_socket_set
            .get_mut::<SmoltcpTcpSocket>(self.smoltcp_socket_handle);
        smoltcp_socket.close();
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
