mod client;
mod remote;
mod value;

use std::{collections::HashMap, fs::File, sync::Arc};

use anyhow::Result;
use log::{debug, error, info};
use tokio::sync::{
    mpsc::{channel as mpsc_channel, Receiver as MpscReceiver, Sender as MpscSender},
    Mutex, Notify, RwLock,
};

use crate::{
    config::PpaassVpnServerConfig,
    error::{AgentError, ClientEndpointError, RemoteEndpointError},
};
use crate::{transport::remote::RemoteEndpoint, util::AgentRsaCryptoFetcher};

use self::client::ClientEndpoint;
pub(crate) use self::value::ControlProtocol;
pub(crate) use self::value::TransportId;

#[derive(Debug)]
pub(crate) struct Transport {
    transport_id: TransportId,
    client_file_write: Arc<Mutex<File>>,
    client_data_receiver: MpscReceiver<Vec<u8>>,
    closed: Arc<RwLock<bool>>,
}

impl Transport {
    pub(crate) fn new(transport_id: TransportId, client_file_write: Arc<Mutex<File>>) -> (Self, MpscSender<Vec<u8>>) {
        let (client_data_sender, client_data_receiver) = mpsc_channel::<Vec<u8>>(1024);
        (
            Self {
                transport_id,
                client_file_write,
                client_data_receiver,
                closed: Arc::new(RwLock::new(false)),
            },
            client_data_sender,
        )
    }

    pub(crate) async fn exec(
        mut self,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
        transports: Arc<Mutex<HashMap<TransportId, MpscSender<Vec<u8>>>>>,
    ) -> Result<(), AgentError> {
        let transport_id = self.transport_id;
        let (client_endpoint, client_endpoint_recv_buffer_notify) = match ClientEndpoint::new(
            self.transport_id,
            Arc::clone(&self.client_file_write),
            config,
        ) {
            Ok(client_endpoint_result) => client_endpoint_result,
            Err(e) => {
                let mut transports = transports.lock().await;
                transports.remove(&transport_id);
                info!(">>>> Transport {transport_id} removed from vpn server(fail to create client endpoint), current connection number in vpn server: {}", transports.len());
                return Err(e.into());
            }
        };
        debug!(">>>> Transport {transport_id} success create client endpoint.");
        let (remote_endpoint, remote_endpoint_recv_buffer_notify) = match RemoteEndpoint::new(
            transport_id,
            agent_rsa_crypto_fetcher,
            config,
        )
        .await
        {
            Ok(remote_endpoint_result) => remote_endpoint_result,
            Err(e) => {
                let mut transports = transports.lock().await;
                transports.remove(&transport_id);
                info!(">>>> Transport {transport_id} removed from vpn server(fail to create remote endpoint), current connection number in vpn server: {}", transports.len());
                return Err(e.into());
            }
        };
        debug!(">>>> Transport {transport_id} success create remote endpoint.");
        let client_endpoint = Arc::new(client_endpoint);
        let remote_endpoint = Arc::new(remote_endpoint);

        Self::spawn_consume_client_recv_buf_task(
            transport_id,
            Arc::clone(&remote_endpoint),
            Arc::clone(&client_endpoint),
            client_endpoint_recv_buffer_notify,
            Arc::clone(&transports),
            Arc::clone(&self.closed),
        );
        Self::spawn_consume_remote_recv_buf_task(
            transport_id,
            Arc::clone(&client_endpoint),
            Arc::clone(&remote_endpoint),
            remote_endpoint_recv_buffer_notify,
            Arc::clone(&transports),
            Arc::clone(&self.closed),
        );
        Self::spawn_read_remote_task(
            transport_id,
            Arc::clone(&remote_endpoint),
            Arc::clone(&client_endpoint),
            Arc::clone(&transports),
            Arc::clone(&self.closed),
        );
        while let Some(client_data) = self.client_data_receiver.recv().await {
            client_endpoint.receive_from_client(client_data).await;
        }
        let mut transports = transports.lock().await;
        transports.remove(&transport_id);
        info!(">>>> Transport {transport_id} removed from vpn server(normal quite), current connection number in vpn server: {}", transports.len());
        Ok::<(), AgentError>(())
    }

    /// Spawn a task to read remote data
    fn spawn_read_remote_task<'b>(
        transport_id: TransportId,
        remote_endpoint: Arc<RemoteEndpoint>,
        client_endpoint: Arc<ClientEndpoint<'b>>,
        transports: Arc<Mutex<HashMap<TransportId, MpscSender<Vec<u8>>>>>,
        closed: Arc<RwLock<bool>>,
    ) where
        'b: 'static,
    {
        tokio::spawn(async move {
            loop {
                {
                    let closed = closed.read().await;
                    if *closed {
                        break;
                    }
                }
                match remote_endpoint.read_from_remote().await {
                    Ok(false) => continue,
                    Ok(true) => {
                        debug!(">>>> Transport {transport_id} mark client & remote endpoint closed.");
                        {
                            let mut transports = transports.lock().await;
                            transports.remove(&transport_id);
                        }
                        {
                            let mut closed = closed.write().await;
                            *closed = true;
                        }
                        remote_endpoint.close().await;
                        client_endpoint.close().await;
                        break;
                    }
                    Err(e) => {
                        debug!(">>>> Transport {transport_id} error happen on remote connection close client & remote endpoint, error: {e:?}");
                        {
                            let mut transports = transports.lock().await;
                            transports.remove(&transport_id);
                        }
                        {
                            let mut closed = closed.write().await;
                            *closed = true;
                        }
                        remote_endpoint.close().await;
                        client_endpoint.close().await;
                        break;
                    }
                };
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
        transports: Arc<Mutex<HashMap<TransportId, MpscSender<Vec<u8>>>>>,
        closed: Arc<RwLock<bool>>,
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
                {
                    let closed = closed.read().await;
                    if *closed {
                        break;
                    }
                }
                client_endpoint_recv_buffer_notify.notified().await;
                match client_endpoint
                    .consume_recv_buffer(Arc::clone(&remote_endpoint), consume_fn)
                    .await
                {
                    Ok(()) => {
                        let closed = closed.read().await;
                        if *closed {
                            break;
                        }
                    }
                    Err(e) => {
                        error!(">>>> Transport {transport_id} fail to consume client endpoint receive buffer because of error: {e:?}");

                        {
                            let mut transports = transports.lock().await;
                            transports.remove(&transport_id);
                        }
                        {
                            let mut closed = closed.write().await;
                            *closed = true;
                        }
                        remote_endpoint.close().await;
                        client_endpoint.close().await;
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
        transports: Arc<Mutex<HashMap<TransportId, MpscSender<Vec<u8>>>>>,
        closed: Arc<RwLock<bool>>,
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
                {
                    let closed = closed.read().await;
                    if *closed {
                        break;
                    }
                }
                remote_endpoint_recv_buffer_notify.notified().await;
                match remote_endpoint
                    .consume_recv_buffer(Arc::clone(&client_endpoint), consume_fn)
                    .await
                {
                    Ok(()) => {
                        let closed = closed.read().await;
                        if *closed {
                            break;
                        }
                    }
                    Err(e) => {
                        error!(">>>> Transport {transport_id} fail to consume remote endpoint receive buffer because of error: {e:?}");
                        remote_endpoint.close().await;
                        client_endpoint.close().await;
                        {
                            let mut transports = transports.lock().await;
                            transports.remove(&transport_id);
                        }
                        {
                            let mut closed = closed.write().await;
                            *closed = true;
                        }
                    }
                };
            }
        });
    }
}
