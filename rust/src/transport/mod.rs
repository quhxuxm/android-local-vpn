mod client;
mod remote;
mod value;

use std::sync::Arc;

use anyhow::{anyhow, Result};
use log::{debug, error, trace};

use smoltcp::socket::tcp::State;
use tokio::sync::mpsc::{channel, Receiver, Sender};

use self::client::{ClientEndpoint, ClientEndpointUdpState};
pub(crate) use self::value::ClientOutputPacket;
pub(crate) use self::value::ControlProtocol;
pub(crate) use self::value::TransportId;
pub(crate) use self::value::Transports;
use crate::{
    config::PpaassVpnServerConfig,
    error::{AgentError, ClientEndpointError, RemoteEndpointError},
};
use crate::{transport::remote::RemoteEndpoint, util::AgentRsaCryptoFetcher};
use client::ClientEndpointState;

#[derive(Debug)]
pub(crate) struct Transport {
    transport_id: TransportId,
    client_output_tx: Sender<ClientOutputPacket>,
    client_input_rx: Receiver<Vec<u8>>,
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
            },
            client_input_tx,
        )
    }

    pub(crate) async fn exec(
        mut self,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
        remove_transports_tx: Sender<TransportId>,
    ) -> Result<(), AgentError> {
        let transport_id = self.transport_id;
        let client_endpoint = match ClientEndpoint::new(
            self.transport_id,
            self.client_output_tx,
            config,
        ) {
            Ok(client_endpoint) => client_endpoint,
            Err(e) => {
                if let Err(e) = remove_transports_tx.send(transport_id).await {
                    error!("###### Transport {transport_id} fail to send remove transports signal because of error: {e:?}")
                };
                return Err(e.into());
            }
        };

        let remote_endpoint = match RemoteEndpoint::new(
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
                if let Err(e) = remove_transports_tx.send(transport_id).await {
                    error!("###### Transport {transport_id} fail to send remove transports signal because of error: {e:?}")
                };
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
            match client_endpoint.receive_from_client(client_data).await {
                Ok(()) => {
                    //Check the tcp connection state because of the ip packet just pass through the smoltcp stack
                    match client_endpoint.get_state().await {
                        ClientEndpointState::Tcp(State::Closed) => {
                            // The tcp connection is closed we should remove the transport from the repository because of no data will come again.
                            debug!(
                                ">>>> Transport {transport_id} is TCP protocol in [Closed] state, destroy client endpoint and remove the transport."
                            );
                            // Flush all the client receiver buffer data to remote.
                            if let Err(e) =
                                Self::flush_client_recv_buf_to_remote(
                                    &client_endpoint,
                                    &remote_endpoint,
                                )
                                .await
                            {
                                error!(">>>> Transport {transport_id} error happen when flush client receive buffer to remote because of the error: {e:?}");
                            };
                            client_endpoint.destroy().await;
                            if let Err(e) =
                                remove_transports_tx.send(transport_id).await
                            {
                                error!("###### Transport {transport_id} fail to send remove transports signal because of error: {e:?}")
                            };
                            return Ok(());
                        }
                        ClientEndpointState::Udp(
                            ClientEndpointUdpState::Closed,
                        ) => {
                            // The udp connection is closed, we should remove the transport from the repository because of no data will come again.
                            debug!(
                                ">>>> Transport {transport_id} is UDP protocol in [Closed] state, destory client endpoint and remove the transport."
                            );
                            // Flush all the client receiver buffer data to remote.
                            if let Err(e) =
                                Self::flush_client_recv_buf_to_remote(
                                    &client_endpoint,
                                    &remote_endpoint,
                                )
                                .await
                            {
                                error!(">>>> Transport {transport_id} error happen when flush client receive buffer to remote because of the error: {e:?}");
                            };
                            client_endpoint.destroy().await;
                            if let Err(e) =
                                remove_transports_tx.send(transport_id).await
                            {
                                error!("###### Transport {transport_id} fail to send remove transports signal because of error: {e:?}")
                            };
                            return Ok(());
                        }
                        state => {
                            //For other case we just continue, even for tcp CloseWait and LastAck because of the smoltcp stack should handle the tcp packet.
                            debug!("###### Transport {transport_id} client endpoint in {state:?} state, continue to receive client data");
                            if let Err(e) =
                                Self::flush_client_recv_buf_to_remote(
                                    &client_endpoint,
                                    &remote_endpoint,
                                )
                                .await
                            {
                                error!(">>>> Transport {transport_id} error happen when flush client receive buffer to remote because of the error: {e:?}");
                            };
                            continue;
                        }
                    }
                }
                Err(e) => {
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
                    if let Err(e) =
                        remove_transports_tx.send(transport_id).await
                    {
                        error!("###### Transport {transport_id} fail to send remove transports signal because of error: {e:?}")
                    };
                    return Err(AgentError::ClientEndpoint(
                        ClientEndpointError::Other(anyhow!(e)),
                    ));
                }
            };
        }
        //Flush all the data to remote endpoint.
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
        Ok(())
    }

    /// Spawn a task to read remote data
    fn spawn_read_remote_task<'b>(
        transport_id: TransportId,
        client_endpoint: Arc<ClientEndpoint<'b>>,
        remote_endpoint: Arc<RemoteEndpoint>,
    ) where
        'b: 'static,
    {
        tokio::spawn(async move {
            loop {
                match remote_endpoint.read_from_remote().await {
                    Ok(exhausted) => {
                        // Remote endpoint still have data to read
                        if let Err(e) = Self::flush_remote_recv_buf_to_client(
                            &client_endpoint,
                            &remote_endpoint,
                        )
                        .await
                        {
                            error!("<<<< Transport {transport_id} error happen when flush remote receive buffer to client because of the error: {e:?}");
                            return;
                        };
                        if exhausted {
                            // Remote date exhausted
                            client_endpoint.close().await;
                            return;
                        }
                        debug!("<<<< Transport {transport_id} keep reading remote data.");
                        continue;
                    }
                    Err(e) => {
                        error!("<<<< Transport {transport_id} error happen when read from remote endpoint because of the error: {e:?}");
                        client_endpoint.close().await;
                        return;
                    }
                }
            }
        });
    }

    async fn flush_client_recv_buf_to_remote(
        client_endpoint: &ClientEndpoint<'_>,
        remote_endpoint: &RemoteEndpoint,
    ) -> Result<(), RemoteEndpointError> {
        client_endpoint
            .consume_recv_buffer(
                remote_endpoint,
                Self::consume_client_recv_buf_fn,
            )
            .await
    }

    async fn flush_remote_recv_buf_to_client(
        client_endpoint: &ClientEndpoint<'_>,
        remote_endpoint: &RemoteEndpoint,
    ) -> Result<(), ClientEndpointError> {
        match client_endpoint.get_state().await {
            ClientEndpointState::Tcp(State::CloseWait)
            | ClientEndpointState::Tcp(State::Closed)
            | ClientEndpointState::Tcp(State::FinWait1)
            | ClientEndpointState::Tcp(State::FinWait2)
            | ClientEndpointState::Tcp(State::TimeWait)
            | ClientEndpointState::Tcp(State::LastAck) => Ok(()),
            ClientEndpointState::Udp(ClientEndpointUdpState::Closed) => Ok(()),
            _ => {
                remote_endpoint
                    .consume_recv_buffer(
                        client_endpoint,
                        Self::consume_remote_recv_buf_fn,
                    )
                    .await
            }
        }
    }

    /// The concrete function to forward client receive buffer to remote.
    /// * transport_id: The transportation id.
    /// * data: The data going to send to remote.
    /// * remote_endpoint: The remote endpoint.
    async fn consume_client_recv_buf_fn(
        transport_id: TransportId,
        data: Vec<u8>,
        remote_endpoint: &RemoteEndpoint,
    ) -> Result<usize, RemoteEndpointError> {
        trace!(
            ">>>> Transport {transport_id} write data to remote: {}",
            pretty_hex::pretty_hex(&data)
        );
        remote_endpoint.write_to_remote(data).await
    }

    /// The concrete function to forward remote receive buffer to client.
    /// * transport_id: The transportation id.
    /// * data: The data going to send to remote.
    /// * client: The client endpoint.
    async fn consume_remote_recv_buf_fn(
        transport_id: TransportId,
        data: Vec<u8>,
        client_endpoint: &ClientEndpoint<'_>,
    ) -> Result<usize, ClientEndpointError> {
        trace!(
            ">>>> Transport {transport_id} write data to smoltcp: {}",
            pretty_hex::pretty_hex(&data)
        );
        client_endpoint.send_to_smoltcp(data).await
    }
}
