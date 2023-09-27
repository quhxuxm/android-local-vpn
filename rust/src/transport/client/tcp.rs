use std::{collections::VecDeque, sync::Arc};

use anyhow::Result;

use bytes::{Bytes, BytesMut};
use log::error;
use smoltcp::{iface::Interface, socket::tcp::Socket as SmoltcpTcpSocket};
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp::State,
};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    Mutex, MutexGuard,
};

use crate::{config, repository::TcpTransportsRepoCmd};
use crate::{config::PpaassVpnServerConfig, device::SmoltcpDevice};
use crate::{error::ClientEndpointError, transport::remote::RemoteTcpEndpoint};
use smoltcp::socket::tcp::SocketBuffer as SmoltcpTcpSocketBuffer;

use super::{
    ClientOutputPacket, TransportId,
    {poll_smoltcp_and_flush, prepare_smoltcp_iface_and_device},
};

struct ClientTcpEndpointCtlLockGuard<'lock, 'buf> {
    smoltcp_socket_set: MutexGuard<'lock, SocketSet<'buf>>,
    smoltcp_iface: MutexGuard<'lock, Interface>,
    smoltcp_device: MutexGuard<'lock, SmoltcpDevice>,
}

struct ClientTcpEndpointCtl<'buf> {
    smoltcp_socket_set: Mutex<SocketSet<'buf>>,
    smoltcp_iface: Mutex<Interface>,
    smoltcp_device: Mutex<SmoltcpDevice>,
}

impl<'buf> ClientTcpEndpointCtl<'buf> {
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
    async fn lock<'lock>(
        &'lock self,
    ) -> ClientTcpEndpointCtlLockGuard<'lock, 'buf> {
        let smoltcp_device = self.smoltcp_device.lock().await;
        let smoltcp_iface = self.smoltcp_iface.lock().await;
        let smoltcp_socket_set = self.smoltcp_socket_set.lock().await;
        ClientTcpEndpointCtlLockGuard {
            smoltcp_socket_set,
            smoltcp_iface,
            smoltcp_device,
        }
    }
}

pub(crate) enum ClientTcpEndpointRecvBufCmd {
    DumpToRemote(Arc<RemoteTcpEndpoint>),
    Extend(Vec<u8>),
    Clear,
}

pub(crate) struct ClientTcpEndpoint<'buf> {
    transport_id: TransportId,
    ctl: ClientTcpEndpointCtl<'buf>,
    smoltcp_socket_handle: SocketHandle,
    recv_buf_cmd_tx: UnboundedSender<ClientTcpEndpointRecvBufCmd>,
    client_output_tx: UnboundedSender<ClientOutputPacket>,
    repo_cmd_tx: UnboundedSender<TcpTransportsRepoCmd>,
    _config: &'static PpaassVpnServerConfig,
}

impl<'buf> ClientTcpEndpoint<'buf>
where
    'buf: 'static,
{
    pub(crate) fn new(
        transport_id: TransportId,
        client_output_tx: UnboundedSender<ClientOutputPacket>,
        repo_cmd_tx: UnboundedSender<TcpTransportsRepoCmd>,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<ClientTcpEndpoint<'_>, ClientEndpointError> {
        let (smoltcp_iface, smoltcp_device) =
            prepare_smoltcp_iface_and_device(transport_id)?;
        let mut smoltcp_socket_set = SocketSet::new(Vec::with_capacity(1));

        let smoltcp_tcp_socket =
            Self::create_smoltcp_tcp_socket(transport_id, config)?;

        let smoltcp_socket_handle = smoltcp_socket_set.add(smoltcp_tcp_socket);
        let ctl = ClientTcpEndpointCtl::new(
            Mutex::new(smoltcp_socket_set),
            Mutex::new(smoltcp_iface),
            Mutex::new(smoltcp_device),
        );
        let (recv_buf_cmd_tx, recv_buf_cmd_rx) =
            mpsc::unbounded_channel::<ClientTcpEndpointRecvBufCmd>();
        tokio::spawn(Self::handle_recv_buf_cmd(
            transport_id,
            config,
            recv_buf_cmd_rx,
        ));
        Ok(Self {
            transport_id,
            smoltcp_socket_handle,
            ctl,
            repo_cmd_tx,
            recv_buf_cmd_tx,
            client_output_tx,
            _config: config,
        })
    }

    async fn handle_recv_buf_cmd(
        transport_id: TransportId,
        config: &'static PpaassVpnServerConfig,
        mut recv_buf_cmd_rx: UnboundedReceiver<ClientTcpEndpointRecvBufCmd>,
    ) {
        let mut recv_buffer = VecDeque::with_capacity(
            config.get_client_endpoint_tcp_recv_buffer_size(),
        );
        while let Some(cmd) = recv_buf_cmd_rx.recv().await {
            match cmd {
                ClientTcpEndpointRecvBufCmd::DumpToRemote(remote) => {
                    if recv_buffer.is_empty() {
                        continue;
                    }
                    let recv_buffer_data =
                        Bytes::from(recv_buffer.make_contiguous().to_vec());
                    let consume_size = match remote
                        .write_to_remote(recv_buffer_data)
                        .await
                    {
                        Ok(consume_size) => consume_size,
                        Err(e) => {
                            error!(">>>> Transport {transport_id} fail to dump client endpoint receive buffer to remote endpoint because of error: {e:?}");
                            continue;
                        }
                    };
                    recv_buffer.drain(..consume_size);
                }
                ClientTcpEndpointRecvBufCmd::Extend(data) => {
                    recv_buffer.extend(&data);
                }
                ClientTcpEndpointRecvBufCmd::Clear => recv_buffer.clear(),
            }
        }
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

    pub(crate) async fn consume_recv_buffer(
        &self,
        remote: Arc<RemoteTcpEndpoint>,
    ) -> Result<(), ClientEndpointError> {
        self.recv_buf_cmd_tx
            .send(ClientTcpEndpointRecvBufCmd::DumpToRemote(remote))?;
        Ok(())
    }

    pub(crate) async fn send_to_smoltcp(
        &self,
        data: &[u8],
    ) -> Result<usize, ClientEndpointError> {
        let ClientTcpEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            mut smoltcp_iface,
            mut smoltcp_device,
        } = self.ctl.lock().await;
        let smoltcp_socket = smoltcp_socket_set
            .get_mut::<SmoltcpTcpSocket>(self.smoltcp_socket_handle);
        if smoltcp_socket.may_send() {
            let send_result = smoltcp_socket.send_slice(data)?;
            poll_smoltcp_and_flush(
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
        client_data: BytesMut,
    ) -> Result<State, ClientEndpointError> {
        let ClientTcpEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            mut smoltcp_iface,
            mut smoltcp_device,
        } = self.ctl.lock().await;

        smoltcp_device.push_rx(client_data);
        if poll_smoltcp_and_flush(
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
                let mut tcp_data = [0u8; config::MTU];
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
                self.recv_buf_cmd_tx.send(
                    ClientTcpEndpointRecvBufCmd::Extend(tcp_data.to_vec()),
                )?;
            }
            return Ok(smoltcp_tcp_socket.state());
        }
        let smoltcp_tcp_socket = smoltcp_socket_set
            .get::<SmoltcpTcpSocket>(self.smoltcp_socket_handle);
        Ok(smoltcp_tcp_socket.state())
    }

    pub(crate) async fn abort(&self) {
        let ClientTcpEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            mut smoltcp_iface,
            mut smoltcp_device,
        } = self.ctl.lock().await;
        let smoltcp_socket = smoltcp_socket_set
            .get_mut::<SmoltcpTcpSocket>(self.smoltcp_socket_handle);
        smoltcp_socket.abort();
        poll_smoltcp_and_flush(
            self.transport_id,
            &mut smoltcp_socket_set,
            &mut smoltcp_iface,
            &mut smoltcp_device,
            &self.client_output_tx,
        )
        .await;
    }
    pub(crate) async fn close(&self) {
        let ClientTcpEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            mut smoltcp_iface,
            mut smoltcp_device,
        } = self.ctl.lock().await;
        let smoltcp_socket = smoltcp_socket_set
            .get_mut::<SmoltcpTcpSocket>(self.smoltcp_socket_handle);
        smoltcp_socket.close();
        poll_smoltcp_and_flush(
            self.transport_id,
            &mut smoltcp_socket_set,
            &mut smoltcp_iface,
            &mut smoltcp_device,
            &self.client_output_tx,
        )
        .await;
    }

    pub(crate) async fn destroy(&self) {
        let ClientTcpEndpointCtlLockGuard {
            mut smoltcp_socket_set,
            mut smoltcp_device,
            ..
        } = self.ctl.lock().await;
        smoltcp_socket_set.remove(self.smoltcp_socket_handle);
        smoltcp_device.destory();
        if let Err(e) = self
            .recv_buf_cmd_tx
            .send(ClientTcpEndpointRecvBufCmd::Clear)
        {
            error!("###### Transport {} fail to send clear recv buffer command because of error: {e:?}", self.transport_id)
        };
        if let Err(e) = self
            .repo_cmd_tx
            .send(TcpTransportsRepoCmd::Remove(self.transport_id))
        {
            error!("###### Transport {} fail to send remove transports signal because of error: {e:?}", self.transport_id)
        }
        self.recv_buf_cmd_tx.closed().await;
    }
}
