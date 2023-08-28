mod client;
mod remote;
mod value;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use anyhow::Result;
use log::{debug, error};

use tokio::sync::mpsc::{channel, Receiver, Sender};

use self::client::ClientEndpoint;
use crate::{
    config::PpaassVpnServerConfig,
    error::{AgentError, ClientEndpointError, RemoteEndpointError},
    util::ClientOutputPacket,
};
use crate::{transport::remote::RemoteEndpoint, util::AgentRsaCryptoFetcher};

pub(crate) use self::value::ControlProtocol;
pub(crate) use self::value::TransportId;
pub(crate) use self::value::Transports;

#[derive(Debug)]
pub(crate) struct Transport {
    transport_id: TransportId,
    client_output_tx: Sender<ClientOutputPacket>,
    client_input_rx: Receiver<Vec<u8>>,
    closed: Arc<AtomicBool>,
}

impl Transport {
    pub(crate) fn new(
        transport_id: TransportId,
        client_output_tx: Sender<ClientOutputPacket>,
    ) -> (Self, Sender<Vec<u8>>) {
        let (client_input_tx, client_input_rx) = channel::<Vec<u8>>(1024);
        (
            Self {
                transport_id,
                client_output_tx,
                client_input_rx,
                closed: Arc::new(AtomicBool::new(false)),
            },
            client_input_tx,
        )
    }

    pub(crate) async fn exec(
        mut self,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
        transports_remove_tx: Sender<TransportId>,
    ) -> Result<(), AgentError> {
        let transport_id = self.transport_id;
        let client_endpoint = match ClientEndpoint::new(
            self.transport_id,
            self.client_output_tx,
            config,
        ) {
            Ok(client_endpoint_result) => client_endpoint_result,
            Err(e) => {
                if let Err(e) = transports_remove_tx.send(transport_id).await {
                    error!(
                            ">>>> Transport {transport_id} fail to send remove message because of error: {e:?}"
                        )
                };
                return Err(e.into());
            }
        };
        debug!(">>>> Transport {transport_id} success create client endpoint.");
        let remote_endpoint = match RemoteEndpoint::new(
            transport_id,
            agent_rsa_crypto_fetcher,
            config,
        )
        .await
        {
            Ok(remote_endpoint_result) => remote_endpoint_result,
            Err(e) => {
                if let Err(e) = transports_remove_tx.send(transport_id).await {
                    error!(">>>> Transport {transport_id} fail to send remove message because of error: {e:?}")
                };
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
            transports_remove_tx.clone(),
            Arc::clone(&self.closed),
        );
        Self::spawn_consume_remote_recv_buf_task(
            transport_id,
            Arc::clone(&client_endpoint),
            Arc::clone(&remote_endpoint),
            transports_remove_tx.clone(),
            Arc::clone(&self.closed),
        );
        Self::spawn_read_remote_task(
            transport_id,
            Arc::clone(&remote_endpoint),
            Arc::clone(&client_endpoint),
            transports_remove_tx.clone(),
            Arc::clone(&self.closed),
        );
        while let Some(client_data) = self.client_input_rx.recv().await {
            if let Err(e) =
                client_endpoint.receive_from_client(client_data).await
            {
                error!(">>>> Transport {transport_id} fail to receive client data from smoltcp because of error: {e:?}");
                Self::close(
                    transport_id,
                    &client_endpoint,
                    &remote_endpoint,
                    transports_remove_tx,
                    &self.closed,
                )
                .await;
                break;
            };
        }
        Ok::<(), AgentError>(())
    }

    /// Spawn a task to read remote data
    fn spawn_read_remote_task<'b>(
        transport_id: TransportId,
        remote_endpoint: Arc<RemoteEndpoint>,
        client_endpoint: Arc<ClientEndpoint<'b>>,
        transports_remove_tx: Sender<TransportId>,
        closed: Arc<AtomicBool>,
    ) where
        'b: 'static,
    {
        tokio::spawn(async move {
            loop {
                if closed.load(Ordering::Relaxed) {
                    // The transport closed already quite the task
                    break;
                }
                match remote_endpoint.read_from_remote().await {
                    Ok(false) => continue,
                    Ok(true) => {
                        Self::close(
                            transport_id,
                            &client_endpoint,
                            &remote_endpoint,
                            transports_remove_tx,
                            &closed,
                        )
                        .await;
                        break;
                    }
                    Err(e) => {
                        debug!(">>>> Transport {transport_id} error happen on remote connection close client & remote endpoint, error: {e:?}");
                        Self::close(
                            transport_id,
                            &client_endpoint,
                            &remote_endpoint,
                            transports_remove_tx,
                            &closed,
                        )
                        .await;
                        break;
                    }
                };
            }
            Ok::<(), RemoteEndpointError>(())
        });
    }

    /// The concrete function to forward client receive buffer to remote.
    /// * transport_id: The transportation id.
    /// * data: The data going to send to remote.
    /// * remote_endpoint: The remote endpoint.
    async fn consume_client_recv_buf_fn(
        transport_id: TransportId,
        data: Vec<u8>,
        remote_endpoint: Arc<RemoteEndpoint>,
    ) -> Result<usize, RemoteEndpointError> {
        debug!(
            ">>>> Transport {transport_id} write data to remote: {}",
            pretty_hex::pretty_hex(&data)
        );
        remote_endpoint.write_to_remote(data).await
    }

    /// Spawn a task to consume the client endpoint receive data buffer
    fn spawn_consume_client_recv_buf_task<'b>(
        transport_id: TransportId,
        remote_endpoint: Arc<RemoteEndpoint>,
        client_endpoint: Arc<ClientEndpoint<'b>>,
        transports_remove_tx: Sender<TransportId>,
        closed: Arc<AtomicBool>,
    ) where
        'b: 'static,
    {
        // Spawn a task for output data to client
        tokio::spawn(async move {
            loop {
                if closed.load(Ordering::Relaxed) {
                    // The transport closed already quite the task
                    break;
                }
                client_endpoint.awaiting_recv_buf().await;
                if let Err(e) = client_endpoint
                    .consume_recv_buffer(
                        Arc::clone(&remote_endpoint),
                        Self::consume_client_recv_buf_fn,
                    )
                    .await
                {
                    error!(">>>> Transport {transport_id} fail to consume client endpoint receive buffer because of error: {e:?}");
                    Self::close(
                        transport_id,
                        &client_endpoint,
                        &remote_endpoint,
                        transports_remove_tx,
                        &closed,
                    )
                    .await;
                    break;
                };
            }
        });
    }

    /// The concrete function to forward remote receive buffer to client.
    /// * transport_id: The transportation id.
    /// * data: The data going to send to remote.
    /// * client: The client endpoint.
    async fn consume_remote_recv_buf_fn(
        transport_id: TransportId,
        data: Vec<u8>,
        client_endpoint: Arc<ClientEndpoint<'_>>,
    ) -> Result<usize, ClientEndpointError> {
        debug!(
            ">>>> Transport {transport_id} write data to smoltcp: {}",
            pretty_hex::pretty_hex(&data)
        );
        client_endpoint.send_to_smoltcp(data).await
    }

    /// Spawn a task to consume the remote endpoint receive data buffer
    fn spawn_consume_remote_recv_buf_task<'b>(
        transport_id: TransportId,
        client_endpoint: Arc<ClientEndpoint<'b>>,
        remote_endpoint: Arc<RemoteEndpoint>,
        transports_remove_tx: Sender<TransportId>,
        closed: Arc<AtomicBool>,
    ) where
        'b: 'static,
    {
        // Spawn a task for output data to client
        tokio::spawn(async move {
            loop {
                if closed.load(Ordering::Relaxed) {
                    // The transport closed already quite the task
                    break;
                }
                remote_endpoint.awaiting_recv_buf().await;
                if let Err(e) = remote_endpoint
                    .consume_recv_buffer(
                        Arc::clone(&client_endpoint),
                        Self::consume_remote_recv_buf_fn,
                    )
                    .await
                {
                    error!(">>>> Transport {transport_id} fail to consume remote endpoint receive buffer because of error: {e:?}");
                    Self::close(
                        transport_id,
                        &client_endpoint,
                        &remote_endpoint,
                        transports_remove_tx.clone(),
                        &closed,
                    )
                    .await;
                    break;
                };
            }
        });
    }

    async fn close<'b>(
        transport_id: TransportId,
        client_endpoint: &ClientEndpoint<'b>,
        remote_endpoint: &RemoteEndpoint,
        transports_remove_tx: Sender<TransportId>,
        closed: &AtomicBool,
    ) where
        'b: 'static,
    {
        if let Err(e) = transports_remove_tx.send(transport_id).await {
            error!(
                ">>>> Transport {transport_id} fail to send remove message because of error: {e:?}"
            )
        };
        closed.swap(true, Ordering::Relaxed);
        remote_endpoint.close().await;
        client_endpoint.close().await;
    }
}
