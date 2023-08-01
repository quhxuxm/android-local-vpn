mod client;
mod remote;
mod value;

use std::sync::Arc;

use anyhow::Result;
use log::{debug, error};
use tokio::sync::{
    broadcast::{
        channel as broadcast_channel, error::TryRecvError, Receiver as BroadcastReceiver, Sender as BroadcastSender,
    },
    mpsc::{channel as mpsc_channel, Receiver as MpscReceiver, Sender as MpscSender},
    Notify,
};

use crate::{
    config::PpaassVpnServerConfig,
    error::{AgentError, ClientEndpointError, RemoteEndpointError},
    values::ClientFileTxPacket,
};
use crate::{transport::remote::RemoteEndpoint, util::AgentRsaCryptoFetcher};

use self::client::ClientEndpoint;
pub(crate) use self::value::ControlProtocol;
pub(crate) use self::value::TransportId;

#[derive(Debug)]
pub(crate) struct Transport {
    transport_id: TransportId,
    client_file_tx_sender: MpscSender<ClientFileTxPacket>,
    client_data_receiver: MpscReceiver<Vec<u8>>,
    client_data_sender: MpscSender<Vec<u8>>,
}

impl Transport {
    pub(crate) fn new(
        transport_id: TransportId,
        client_file_tx_sender: MpscSender<ClientFileTxPacket>,
    ) -> (Self, MpscSender<Vec<u8>>) {
        let (client_data_sender, client_data_receiver) = mpsc_channel::<Vec<u8>>(1024);

        (
            Self {
                transport_id,
                client_file_tx_sender,
                client_data_receiver,
                client_data_sender: client_data_sender.clone(),
            },
            client_data_sender,
        )
    }

    pub(crate) async fn exec(
        mut self,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<(), AgentError> {
        let transport_id = self.transport_id;
        let (close_notify_sender, close_notify_receiver) = broadcast_channel::<bool>(1);
        let (client_endpoint, client_endpoint_recv_buffer_notify) = ClientEndpoint::new(
            self.transport_id,
            self.client_file_tx_sender.clone(),
            config,
        )?;
        debug!(">>>> Transport {transport_id} success create client endpoint.");
        let (remote_endpoint, remote_endpoint_recv_buffer_notify) =
            RemoteEndpoint::new(transport_id, agent_rsa_crypto_fetcher, config).await?;
        debug!(">>>> Transport {transport_id} success create remote endpoint.");
        let client_endpoint = Arc::new(client_endpoint);
        let remote_endpoint = Arc::new(remote_endpoint);

        Self::spawn_consume_client_recv_buf_task(
            transport_id,
            Arc::clone(&remote_endpoint),
            Arc::clone(&client_endpoint),
            client_endpoint_recv_buffer_notify,
            close_notify_sender.subscribe(),
        );
        Self::spawn_consume_remote_recv_buf_task(
            transport_id,
            Arc::clone(&client_endpoint),
            Arc::clone(&remote_endpoint),
            remote_endpoint_recv_buffer_notify,
            close_notify_receiver,
        );
        Self::spawn_read_remote_task(
            transport_id,
            Arc::clone(&remote_endpoint),
            Arc::clone(&client_endpoint),
            self.client_data_sender,
            close_notify_sender,
        );
        loop {
            if let Some(client_data) = self.client_data_receiver.recv().await {
                client_endpoint.receive_from_client(client_data).await;
            };
        }
    }

    /// Spawn a task to read remote data
    fn spawn_read_remote_task<'b>(
        transport_id: TransportId,
        remote_endpoint: Arc<RemoteEndpoint>,
        client_endpoint: Arc<ClientEndpoint<'b>>,
        client_data_sender: MpscSender<Vec<u8>>,
        close_notify_sender: BroadcastSender<bool>,
    ) where
        'b: 'static,
    {
        tokio::spawn(async move {
            loop {
                let remote_closed = remote_endpoint.read_from_remote().await?;
                if remote_closed {
                    debug!(">>>> Transport {transport_id} mark client & remote endpoint closed.");
                    client_data_sender.closed().await;
                    remote_endpoint.close().await;
                    client_endpoint.close().await;
                    if let Err(e) = close_notify_sender.send(true) {
                        error!(">>>> Transport {transport_id} fail to send close notify because of error: {e:?}");
                    };
                    break;
                }
            }
            Ok::<(), RemoteEndpointError>(())
        });
    }

    /// Spawn a task to consume the client endpoint receive data buffer
    fn spawn_consume_client_recv_buf_task<'b>(
        transport_id: TransportId,
        remote_endpoint: Arc<RemoteEndpoint>,
        client_endpoint: Arc<ClientEndpoint<'b>>,
        client_endpoint_recv_buffer_notify: Arc<Notify>,
        mut close_notify_receiver: BroadcastReceiver<bool>,
    ) where
        'b: 'static,
    {
        // Spawn a task for output data to client
        tokio::spawn(async move {
            async fn consume_fn(
                transport_id: TransportId,
                data: Vec<u8>,
                remote: Arc<RemoteEndpoint>,
            ) -> Result<usize, RemoteEndpointError> {
                debug!(
                    ">>>> Transport {transport_id} write data to remote: {}",
                    pretty_hex::pretty_hex(&data)
                );
                remote.write_to_remote(data).await
            }
            loop {
                client_endpoint_recv_buffer_notify.notified().await;
                match client_endpoint
                    .consume_recv_buffer(Arc::clone(&remote_endpoint), consume_fn)
                    .await
                {
                    Ok(()) => match close_notify_receiver.try_recv() {
                        Err(TryRecvError::Empty) => {
                            continue;
                        }
                        _ => {
                            break;
                        }
                    },
                    Err(e) => {
                        error!(">>>> Transport {transport_id} fail to consume client endpoint receive buffer because of error: {e:?}");
                        break;
                    }
                };
            }
        });
    }

    /// Spawn a task to consume the remote endpoint receive data buffer
    fn spawn_consume_remote_recv_buf_task<'b>(
        transport_id: TransportId,
        client_endpoint: Arc<ClientEndpoint<'b>>,
        remote_endpoint: Arc<RemoteEndpoint>,
        remote_endpoint_recv_buffer_notify: Arc<Notify>,
        mut close_notify_receiver: BroadcastReceiver<bool>,
    ) where
        'b: 'static,
    {
        // Spawn a task for output data to client
        tokio::spawn(async move {
            async fn consume_fn(
                transport_id: TransportId,
                data: Vec<u8>,
                client: Arc<ClientEndpoint<'_>>,
            ) -> Result<usize, ClientEndpointError> {
                debug!(
                    ">>>> Transport {transport_id} write data to smoltcp: {}",
                    pretty_hex::pretty_hex(&data)
                );
                client.send_to_smoltcp(data).await
            }
            loop {
                remote_endpoint_recv_buffer_notify.notified().await;
                match remote_endpoint
                    .consume_recv_buffer(Arc::clone(&client_endpoint), consume_fn)
                    .await
                {
                    Ok(()) => match close_notify_receiver.try_recv() {
                        Err(TryRecvError::Empty) => {
                            continue;
                        }
                        _ => {
                            break;
                        }
                    },
                    Err(e) => {
                        error!(">>>> Transport {transport_id} fail to consume remote endpoint receive buffer because of error: {e:?}");
                        break;
                    }
                };
            }
        });
    }
}
