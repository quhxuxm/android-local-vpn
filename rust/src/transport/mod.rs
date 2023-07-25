mod client;
mod common;
mod remote;
mod value;

use std::{fs::File, io::Write, sync::Arc};

use crate::transport::remote::RemoteEndpoint;

use self::client::ClientEndpoint;

use anyhow::Result;
use log::{debug, error};

use tokio::sync::{
    mpsc::{channel, Receiver, Sender},
    Mutex, Notify,
};

pub(crate) use self::value::ControlProtocol;
pub(crate) use self::value::TransportId;

#[derive(Debug)]
pub(crate) struct Transport {
    transport_id: TransportId,
    client_file_write: Arc<Mutex<File>>,
    client_data_sender: Sender<Vec<u8>>,
    client_data_receiver: Mutex<Option<Receiver<Vec<u8>>>>,
}

impl Transport {
    pub(crate) fn new(transport_id: TransportId, client_file_write: Arc<Mutex<File>>) -> Self {
        let (client_data_sender, client_data_receiver) = channel::<Vec<u8>>(1024);
        Self {
            transport_id,
            client_file_write,
            client_data_sender,
            client_data_receiver: Mutex::new(Some(client_data_receiver)),
        }
    }

    pub(crate) async fn start(&self) -> Result<()> {
        let transport_id = self.transport_id;
        if let Some(mut client_data_receiver) = self.client_data_receiver.lock().await.take() {
            let (client_endpoint, client_endpoint_output_tx, client_endpoint_recv_buffer_notify) = match self.transport_id.control_protocol {
                ControlProtocol::Tcp => ClientEndpoint::new_tcp(self.transport_id)?,
                ControlProtocol::Udp => ClientEndpoint::new_udp(self.transport_id)?,
            };
            debug!("Transport {transport_id} success create client endpoint");
            let (remote_endpoint, remote_endpoint_recv_buffer_notify) = match self.transport_id.control_protocol {
                ControlProtocol::Tcp => RemoteEndpoint::new_tcp(self.transport_id).await?,
                ControlProtocol::Udp => RemoteEndpoint::new_udp(self.transport_id).await?,
            };
            let remote_endpoint = Arc::new(remote_endpoint);
            debug!("Transport {transport_id} success create remote endpoint");
            self.spawn_client_output_task(client_endpoint_output_tx);
            debug!("Transport {transport_id} spawn client output task");
            let client_endpoint = Arc::new(client_endpoint);
            {
                let client_endpoint = Arc::clone(&client_endpoint);
                let remote_endpoint = Arc::clone(&remote_endpoint);
                self.spawn_client_endpoint_recv_buffer_consume_task(
                    client_endpoint,
                    client_endpoint_recv_buffer_notify,
                    remote_endpoint,
                );
            }
            debug!("Transport {transport_id} spawn consume client receive buffer task");
            {
                let client_endpoint = Arc::clone(&client_endpoint);
                let remote_endpoint = Arc::clone(&remote_endpoint);
                self.spawn_read_remote_task(client_endpoint, remote_endpoint)
            }
            debug!("Transport {transport_id} spawn read remote task");
            {
                let client_endpoint = Arc::clone(&client_endpoint);
                let remote_endpoint = Arc::clone(&remote_endpoint);
                self.spawn_remote_endpoint_recv_buffer_consume_task(
                    client_endpoint,
                    remote_endpoint_recv_buffer_notify,
                    remote_endpoint,
                );
            }
            debug!("Transport {transport_id} spawn consume remote receive buffer task");
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

    fn spawn_client_output_task(&self, mut client_endpoint_output_tx: Receiver<Vec<u8>>) {
        // Spwn a task for output data to client
        let transport_id = self.transport_id;
        let client_file_write = Arc::clone(&self.client_file_write);
        tokio::spawn(async move {
            while let Some(data) = client_endpoint_output_tx.recv().await {
                let mut client_file_write = client_file_write.lock().await;
                if let Err(e) = client_file_write.write_all(&data) {
                    error!("<<<< Transport {transport_id} fail to write data to client because of error: {e:?}");
                    break;
                };
            }
        });
    }

    fn spawn_client_endpoint_recv_buffer_consume_task<'buf>(
        &self,
        client_endpoint: Arc<ClientEndpoint<'buf>>,
        client_endpoint_recv_buffer_notify: Arc<Notify>,
        remote_endpoint: Arc<RemoteEndpoint>,
    ) where
        'buf: 'static,
    {
        // Spwn a task for output data to client
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

    fn spawn_remote_endpoint_recv_buffer_consume_task<'buf>(
        &self,
        client_endpoint: Arc<ClientEndpoint<'buf>>,
        remote_endpoint_recv_buffer_notify: Arc<Notify>,
        remote_endpoint: Arc<RemoteEndpoint>,
    ) where
        'buf: 'static,
    {
        // Spwn a task for output data to client
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
