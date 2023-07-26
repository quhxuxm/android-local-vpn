mod client;
mod common;
mod remote;
mod value;

use std::sync::Arc;

use crate::transport::remote::RemoteEndpoint;

use self::client::ClientEndpoint;

use anyhow::Result;
use log::{debug, error};

use crate::values::ClientFileTxPacket;
use tokio::sync::{
    mpsc::{channel, Receiver, Sender},
    Mutex, Notify,
};

pub(crate) use self::value::ControlProtocol;
pub(crate) use self::value::TransportId;

#[derive(Debug)]
pub(crate) struct Transport {
    transport_id: TransportId,
    client_file_tx_sender: Sender<ClientFileTxPacket>,
    client_data_sender: Sender<Vec<u8>>,
    client_data_receiver: Mutex<Option<Receiver<Vec<u8>>>>,
}

impl Transport {
    pub(crate) fn new(transport_id: TransportId, client_file_tx_sender: Sender<ClientFileTxPacket>) -> Self {
        let (client_data_sender, client_data_receiver) = channel::<Vec<u8>>(1024);
        Self {
            transport_id,
            client_file_tx_sender,
            client_data_sender,
            client_data_receiver: Mutex::new(Some(client_data_receiver)),
        }
    }

    pub(crate) async fn start(&self) -> Result<()> {
        let transport_id = self.transport_id;
        if let Some(mut client_data_receiver) = self.client_data_receiver.lock().await.take() {
            let (client_endpoint, client_endpoint_recv_buffer_notify) = match self.transport_id.control_protocol {
                ControlProtocol::Tcp => ClientEndpoint::new_tcp(self.transport_id, self.client_file_tx_sender.clone())?,
                ControlProtocol::Udp => ClientEndpoint::new_udp(self.transport_id, self.client_file_tx_sender.clone())?,
            };
            debug!(">>>> Transport {transport_id} success create client endpoint.");
            let (remote_endpoint, remote_endpoint_recv_buffer_notify) = match self.transport_id.control_protocol {
                ControlProtocol::Tcp => RemoteEndpoint::new_tcp(self.transport_id).await?,
                ControlProtocol::Udp => RemoteEndpoint::new_udp(self.transport_id).await?,
            };
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

    pub(crate) async fn feed_client_data(&self, data: &[u8]) {
        if let Err(e) = self.client_data_sender.send(data.to_vec()).await {
            error!(
                ">>>> Transport {} fail to feed client data because of error: {e:?}",
                self.transport_id
            );
        }
    }

    /// Spawn a task to read remote data
    fn spawn_read_remote_task<'buf>(&self, client_endpoint: Arc<ClientEndpoint<'buf>>, remote_endpoint: Arc<RemoteEndpoint>)
    where
        'buf: 'static,
    {
        let transport_id = self.transport_id;
        tokio::spawn(async move {
            loop {
                let remote_closed = remote_endpoint.read_from_remote().await?;
                if remote_closed {
                    debug!("<<<< Transport {transport_id} remote endpoint closed.");
                    client_endpoint.close().await;
                    break;
                }
            }
            Ok::<(), anyhow::Error>(())
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
            async fn consume_fn(transport_id: TransportId, data: Vec<u8>, remote: Arc<RemoteEndpoint>) -> Result<usize> {
                debug!(
                    ">>>> Transport {transport_id} write data to remote: {}",
                    pretty_hex::pretty_hex(&data)
                );
                remote.write_to_remote(data).await
            }
            loop {
                client_endpoint_recv_buffer_notify.notified().await;
                if let Err(e) = client_endpoint
                    .consume_recv_buffer(Arc::clone(&remote_endpoint), consume_fn)
                    .await
                {
                    error!(">>>> Transport {transport_id} fail to consume client endpoint receive buffer because of error: {e:?}");
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
            async fn consume_fn(transport_id: TransportId, data: Vec<u8>, client: Arc<ClientEndpoint<'_>>) -> Result<usize> {
                debug!(
                    ">>>> Transport {transport_id} write data to smoltcp: {}",
                    pretty_hex::pretty_hex(&data)
                );
                client.send_to_smoltcp(data).await
            }
            loop {
                remote_endpoint_recv_buffer_notify.notified().await;
                if let Err(e) = remote_endpoint
                    .consume_recv_buffer(Arc::clone(&client_endpoint), consume_fn)
                    .await
                {
                    error!(">>>> Transport {transport_id} fail to consume remote endpoint receive buffer because of error: {e:?}");
                };
            }
        });
    }
}
