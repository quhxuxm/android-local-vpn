mod client;
mod remote;
mod value;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Error as AnyhowError;
use anyhow::Result;
use log::{debug, error};
use tokio::sync::{
    mpsc::{channel, Receiver, Sender},
    Mutex, Notify,
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
    client_file_tx_sender: Sender<ClientFileTxPacket>,
    client_data_sender: Sender<Vec<u8>>,
    client_data_receiver: Mutex<Option<Receiver<Vec<u8>>>>,
    transports: Arc<Mutex<HashMap<TransportId, Arc<Transport>>>>,
    closed: Arc<Mutex<bool>>,
}

impl Transport {
    pub(crate) fn new(
        transport_id: TransportId,
        client_file_tx_sender: Sender<ClientFileTxPacket>,
        transports: Arc<Mutex<HashMap<TransportId, Arc<Transport>>>>,
    ) -> Self {
        let (client_data_sender, client_data_receiver) = channel::<Vec<u8>>(1024);
        Self {
            transport_id,
            client_file_tx_sender,
            client_data_sender,
            client_data_receiver: Mutex::new(Some(client_data_receiver)),
            transports,
            closed: Arc::new(Mutex::new(false)),
        }
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
    ) -> Result<(), AgentError> {
        let transport_id = self.transport_id;
        if let Some(mut client_data_receiver) = self.client_data_receiver.lock().await.take() {
            let (client_endpoint, client_endpoint_recv_buffer_notify) = ClientEndpoint::new(
                self.transport_id,
                self.client_file_tx_sender.clone(),
                config,
            )?;
            debug!(">>>> Transport {transport_id} success create client endpoint.");
            let (remote_endpoint, remote_endpoint_recv_buffer_notify) =
                RemoteEndpoint::new(transport_id, agent_rsa_crypto_fetcher, config).await?;
            debug!(">>>> Transport {transport_id} success create remote endpoint.");
            let remote_endpoint = Arc::new(remote_endpoint);
            let client_endpoint = Arc::new(client_endpoint);
            self.spawn_client_endpoint_recv_buffer_consume_task(
                Arc::clone(&client_endpoint),
                client_endpoint_recv_buffer_notify,
                Arc::clone(&remote_endpoint),
            );
            self.spawn_read_remote_task(Arc::clone(&client_endpoint), Arc::clone(&remote_endpoint));
            self.spawn_remote_endpoint_recv_buffer_consume_task(
                Arc::clone(&client_endpoint),
                remote_endpoint_recv_buffer_notify,
                Arc::clone(&remote_endpoint),
            );
            loop {
                if let Some(client_data) = client_data_receiver.recv().await {
                    client_endpoint.receive_from_client(client_data).await;
                };
            }
        }
        Ok(())
    }

    pub(crate) async fn is_closed(&self) -> bool {
        let closed = self.closed.lock().await;
        *closed
    }

    pub(crate) async fn feed_client_data(&self, data: &[u8]) {
        if let Err(e) = self.client_data_sender.send(data.to_vec()).await {
            error!(
                ">>>> Transport {} fail to feed client data because of error: {e:?}",
                self.transport_id
            );
        }
    }

    pub(crate) async fn close(&self) {
        self.client_data_sender.closed().await;
        let mut transports = self.transports.lock().await;
        transports.remove(&self.transport_id);
        let mut closed = self.closed.lock().await;
        *closed = true;
    }

    /// Spawn a task to read remote data
    fn spawn_read_remote_task<'buf, 'r>(
        self: &Arc<Self>,
        client_endpoint: Arc<ClientEndpoint<'buf>>,
        remote_endpoint: Arc<RemoteEndpoint>,
    ) where
        'buf: 'static,
        'r: 'static,
    {
        let transport_id = self.transport_id;
        let transport_self = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let remote_closed = remote_endpoint.read_from_remote().await?;
                if remote_closed {
                    client_endpoint.close().await;
                    remote_endpoint.close().await;
                    transport_self.close().await;
                    debug!(">>>> Transport {transport_id} mark client & remote endpoint closed.");
                    break;
                }
            }
            Ok::<(), AnyhowError>(())
        });
    }

    /// Spawn a task to consume the client endpoint receive data buffer
    fn spawn_client_endpoint_recv_buffer_consume_task<'buf>(
        &self,
        client_endpoint: Arc<ClientEndpoint<'buf>>,
        client_endpoint_recv_buffer_notify: Arc<Notify>,
        remote_endpoint: Arc<RemoteEndpoint>,
    ) where
        'buf: 'static,
    {
        // Spawn a task for output data to client
        let transport_id = self.transport_id;
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
                    Ok(true) => {
                        break;
                    }
                    Err(e) => {
                        error!(">>>> Transport {transport_id} fail to consume client endpoint receive buffer because of error: {e:?}");
                        break;
                    }
                    _ => {}
                };
            }
        });
    }

    /// Spawn a task to consume the remote endpoint receive data buffer
    fn spawn_remote_endpoint_recv_buffer_consume_task<'buf>(
        &self,
        client_endpoint: Arc<ClientEndpoint<'buf>>,
        remote_endpoint_recv_buffer_notify: Arc<Notify>,
        remote_endpoint: Arc<RemoteEndpoint>,
    ) where
        'buf: 'static,
    {
        // Spawn a task for output data to client
        let transport_id = self.transport_id;
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
                    Ok(true) => {
                        break;
                    }
                    Err(e) => {
                        error!(">>>> Transport {transport_id} fail to consume remote endpoint receive buffer because of error: {e:?}");
                        break;
                    }
                    _ => {}
                };
            }
        });
    }
}
