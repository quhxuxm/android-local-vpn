use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use log::{debug, error, trace};
use smoltcp::socket::tcp::State;
use tokio::{
    sync::mpsc::{channel, Receiver, Sender},
    time::timeout,
};

use crate::{
    config::PpaassVpnServerConfig,
    error::{ClientEndpointError, RemoteEndpointError, TransportError},
    util::AgentRsaCryptoFetcher,
};

use super::{
    client::ClientTcpEndpoint, remote::RemoteTcpEndpoint, ClientOutputPacket,
    TransportId,
};

#[derive(Debug)]
pub(crate) struct TcpTransport {
    transport_id: TransportId,
    client_output_tx: Sender<ClientOutputPacket>,
    client_input_rx: Receiver<Vec<u8>>,
}

impl TcpTransport {
    /// Create a new tcp transport
    /// * transprot_id: The transport id
    /// * client_output_tx: The sender which send output packe to client.
    pub(crate) fn new(
        transport_id: TransportId,
        client_output_tx: Sender<ClientOutputPacket>,
    ) -> (Self, Sender<Vec<u8>>) {
        let (client_input_tx, client_input_rx) = channel::<Vec<u8>>(65536);
        (
            Self {
                transport_id,
                client_output_tx,
                client_input_rx,
            },
            client_input_tx,
        )
    }

    pub(crate) async fn exec(
        mut self,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
        remove_tcp_transports_tx: Sender<TransportId>,
    ) -> Result<(), TransportError> {
        let transport_id = self.transport_id;
        let client_endpoint = match ClientTcpEndpoint::new(
            self.transport_id,
            self.client_output_tx,
            remove_tcp_transports_tx.clone(),
            config,
        ) {
            Ok(client_endpoint) => client_endpoint,
            Err(e) => {
                error!(">>>> Transport {transport_id} fail to create client endpoint because of error: {e:?}");
                if let Err(e) =
                    remove_tcp_transports_tx.send(self.transport_id).await
                {
                    error!("###### Transport {} fail to send remove transports signal because of error: {e:?}", self.transport_id)
                }
                return Err(e.into());
            }
        };

        let remote_endpoint = match RemoteTcpEndpoint::new(
            transport_id,
            agent_rsa_crypto_fetcher,
            config,
        )
        .await
        {
            Ok(remote_endpoint) => remote_endpoint,
            Err(e) => {
                error!(">>>> Transport {transport_id} error happen when initialize the remote endpoint because of the error: {e:?}");
                client_endpoint.abort().await;
                client_endpoint.destroy().await;
                return Err(e.into());
            }
        };

        let client_endpoint = Arc::new(client_endpoint);
        let remote_endpoint = Arc::new(remote_endpoint);

        Self::spawn_read_remote_task(
            transport_id,
            Arc::clone(&client_endpoint),
            Arc::clone(&remote_endpoint),
        );

        debug!(">>>> Transport {transport_id} initialize success, begin to serve client input data.");
        while let Some(client_data) = self.client_input_rx.recv().await {
            // Push the data into smoltcp stack.
            match timeout(
                Duration::from_secs(10),
                client_endpoint.receive_from_client(client_data),
            )
            .await
            {
                Err(_) => {
                    error!(">>>> Transport {transport_id} receive tcp from client timeout in 10 seconds.");
                    if let Err(e) = Self::flush_client_recv_buf_to_remote(
                        &client_endpoint,
                        &remote_endpoint,
                    )
                    .await
                    {
                        error!(">>>> Transport {transport_id} error happen when flush client receive buffer to remote because of the error: {e:?}");
                    };
                    remote_endpoint.close().await;
                    client_endpoint.abort().await;
                    client_endpoint.destroy().await;
                    return Err(ClientEndpointError::ReceiveTimeout(10).into());
                }
                Ok(Err(e)) => {
                    if let Err(e) = Self::flush_client_recv_buf_to_remote(
                        &client_endpoint,
                        &remote_endpoint,
                    )
                    .await
                    {
                        error!(">>>> Transport {transport_id} error happen when flush client receive buffer to remote because of the error: {e:?}");
                    };
                    remote_endpoint.close().await;
                    client_endpoint.abort().await;
                    client_endpoint.destroy().await;
                    return Err(TransportError::ClientEndpoint(e));
                }
                Ok(Ok(State::Closed)) => {
                    // The tcp connection is closed we should remove the transport from the repository because of no data will come again.
                    debug!(">>>> Transport {transport_id} is TCP protocol in [Closed] state, destroy client endpoint and remove the transport.");
                    // Flush all the client receiver buffer data to remote.
                    if let Err(e) = Self::flush_client_recv_buf_to_remote(
                        &client_endpoint,
                        &remote_endpoint,
                    )
                    .await
                    {
                        error!(">>>> Transport {transport_id} error happen when flush client receive buffer to remote because of the error: {e:?}");
                    };
                    remote_endpoint.close().await;
                    client_endpoint.destroy().await;
                    return Ok(());
                }
                Ok(Ok(state)) => {
                    //Check the tcp connection state because of the ip packet just pass through the smoltcp stack
                    //For other case we just continue, even for tcp CloseWait and LastAck because of the smoltcp stack should handle the tcp packet.
                    debug!("###### Transport {transport_id} client endpoint in {state:?} state, continue to receive client data");
                    if let Err(e) = Self::flush_client_recv_buf_to_remote(
                        &client_endpoint,
                        &remote_endpoint,
                    )
                    .await
                    {
                        error!(">>>> Transport {transport_id} error happen when flush client receive buffer to remote because of the error: {e:?}");
                    };
                    continue;
                }
            };
        }
        Ok(())
    }

    /// Spawn a task to read remote data
    fn spawn_read_remote_task<'b>(
        transport_id: TransportId,
        client_endpoint: Arc<ClientTcpEndpoint<'b>>,
        remote_endpoint: Arc<RemoteTcpEndpoint>,
    ) where
        'b: 'static,
    {
        tokio::spawn(async move {
            loop {
                match timeout(
                    Duration::from_secs(10),
                    remote_endpoint.read_from_remote(),
                )
                .await
                {
                    Err(_) => {
                        error!("<<<< Transport {transport_id} receive tcp from remote timeout in 10 seconds.");
                        if let Err(e) = Self::flush_remote_recv_buf_to_client(
                            &client_endpoint,
                            &remote_endpoint,
                        )
                        .await
                        {
                            error!("<<<< Transport {transport_id} error happen when flush remote receive buffer to client because of the error: {e:?}");
                        };
                        client_endpoint.close().await;
                        client_endpoint.destroy().await;
                        return;
                    }
                    Ok(Ok(exhausted)) => {
                        // Remote endpoint still have data to read
                        if let Err(e) = Self::flush_remote_recv_buf_to_client(
                            &client_endpoint,
                            &remote_endpoint,
                        )
                        .await
                        {
                            client_endpoint.close().await;
                            client_endpoint.destroy().await;
                            error!("<<<< Transport {transport_id} error happen when flush remote receive buffer to client because of the error: {e:?}");
                            return;
                        };
                        if exhausted {
                            // Remote date exhausted and recv buffer also flushed, close the client endpoint.
                            client_endpoint.close().await;
                            client_endpoint.destroy().await;
                            return;
                        }
                        debug!("<<<< Transport {transport_id} keep reading remote data.");
                        continue;
                    }
                    Ok(Err(e)) => {
                        error!("<<<< Transport {transport_id} error happen when read from remote endpoint because of the error: {e:?}");
                        if let Err(e) = Self::flush_remote_recv_buf_to_client(
                            &client_endpoint,
                            &remote_endpoint,
                        )
                        .await
                        {
                            error!("<<<< Transport {transport_id} error happen when flush remote receive buffer to client because of the error: {e:?}");
                        };
                        client_endpoint.close().await;
                        client_endpoint.destroy().await;
                        return;
                    }
                }
            }
        });
    }

    /// The concrete function to forward client receive buffer to remote.
    /// * transport_id: The transportation id.
    /// * data: The data going to send to remote.
    /// * remote_endpoint: The remote endpoint.
    async fn consume_client_recv_buf_fn(
        transport_id: TransportId,
        data: Bytes,
        remote_endpoint: &RemoteTcpEndpoint,
    ) -> Result<usize, RemoteEndpointError> {
        trace!(
            ">>>> Transport {transport_id} write data to remote: {}",
            pretty_hex::pretty_hex(&data)
        );
        remote_endpoint.write_to_remote(data).await
    }

    async fn flush_client_recv_buf_to_remote<'b>(
        client_endpoint: &ClientTcpEndpoint<'b>,
        remote_endpoint: &RemoteTcpEndpoint,
    ) -> Result<(), RemoteEndpointError>
    where
        'b: 'static,
    {
        client_endpoint
            .consume_recv_buffer(
                remote_endpoint,
                Self::consume_client_recv_buf_fn,
            )
            .await
    }

    /// The concrete function to forward remote receive buffer to client.
    /// * transport_id: The transportation id.
    /// * data: The data going to send to remote.
    /// * client: The client endpoint.
    async fn consume_remote_recv_buf_fn<'b>(
        transport_id: TransportId,
        data: Bytes,
        client_endpoint: &ClientTcpEndpoint<'b>,
    ) -> Result<usize, ClientEndpointError>
    where
        'b: 'static,
    {
        trace!(
            ">>>> Transport {transport_id} write data to smoltcp: {}",
            pretty_hex::pretty_hex(&data)
        );
        client_endpoint.send_to_smoltcp(&data).await
    }

    async fn flush_remote_recv_buf_to_client<'b>(
        client_endpoint: &ClientTcpEndpoint<'b>,
        remote_endpoint: &RemoteTcpEndpoint,
    ) -> Result<(), ClientEndpointError>
    where
        'b: 'static,
    {
        remote_endpoint
            .consume_recv_buffer(
                client_endpoint,
                Self::consume_remote_recv_buf_fn,
            )
            .await
    }
}
