use std::time::Duration;

use anyhow::anyhow;
use log::{debug, error};

use tokio::sync::mpsc::Sender;

use crate::{
    config::PpaassVpnServerConfig,
    error::{AgentError, ClientEndpointError, RemoteEndpointError},
    transport::{
        client::{ClientEndpoint, ClientEndpointState, ClientEndpointUdpState},
        remote::RemoteEndpoint,
    },
    util::AgentRsaCryptoFetcher,
};

use super::{ClientOutputPacket, TransportId};

#[derive(Debug)]
pub(crate) struct UdpTransport {
    transport_id: TransportId,
    client_output_tx: Sender<ClientOutputPacket>,
}

impl UdpTransport {
    pub(crate) fn new(
        transport_id: TransportId,
        client_output_tx: Sender<ClientOutputPacket>,
    ) -> Self {
        Self {
            transport_id,
            client_output_tx,
        }
    }

    pub(crate) async fn exec(
        self,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
        client_data: Vec<u8>,
    ) -> Result<(), AgentError> {
        let transport_id = self.transport_id;
        let client_endpoint = ClientEndpoint::new(
            self.transport_id,
            self.client_output_tx,
            config,
        )?;

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
                return Err(e.into());
            }
        };

        debug!(">>>> Transport {transport_id} initialize success, begin to serve client input data.");
        // Push the data into smoltcp stack.
        match tokio::time::timeout(
            Duration::from_secs(5),
            client_endpoint.receive_from_client(client_data),
        )
        .await
        {
            Err(_) => {
                error!("<<<< Transport {transport_id} receive udp from remote timeout in 10 seconds.");

                if let Err(e) = Self::flush_client_recv_buf_to_remote(
                    &client_endpoint,
                    &remote_endpoint,
                )
                .await
                {
                    error!(">>>> Transport {transport_id} error happen when flush client receive buffer to remote because of the error: {e:?}");
                };
                remote_endpoint.close().await;
                client_endpoint.close().await;
                client_endpoint.destroy().await;
                Err(RemoteEndpointError::ReceiveTimeout(10).into())
            }
            Ok(Ok(())) => {
                //Check the tcp connection state because of the ip packet just pass through the smoltcp stack
                match client_endpoint.get_state().await {
                    ClientEndpointState::Udp(
                        ClientEndpointUdpState::Closed,
                    ) => {
                        // The udp connection is closed, we should remove the transport from the repository because of no data will come again.
                        debug!(
                                ">>>> Transport {transport_id} is UDP protocol in [Closed] state, destory client endpoint and remove the transport."
                            );
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
                        Ok(())
                    }
                    state => {
                        //For other case we just continue, even for tcp CloseWait and LastAck because of the smoltcp stack should handle the tcp packet.
                        debug!("###### Transport {transport_id} client endpoint in {state:?} state, going to read from remote.");
                        if let Err(e) = Self::flush_client_recv_buf_to_remote(
                            &client_endpoint,
                            &remote_endpoint,
                        )
                        .await
                        {
                            error!(">>>> Transport {transport_id} error happen when flush client receive buffer to remote because of the error: {e:?}");
                            remote_endpoint.close().await;
                            client_endpoint.close().await;
                            client_endpoint.destroy().await;
                            return Err(e.into());
                        };
                        match tokio::time::timeout(
                            Duration::from_secs(10),
                            remote_endpoint.read_from_remote(),
                        )
                        .await
                        {
                            Err(_) => {
                                error!("<<<< Transport {transport_id} timeout in 10 seconds when receive from remote.");
                                if let Err(e) =
                                    Self::flush_client_recv_buf_to_remote(
                                        &client_endpoint,
                                        &remote_endpoint,
                                    )
                                    .await
                                {
                                    error!(">>>> Transport {transport_id} error happen when flush client receive buffer to remote because of the error: {e:?}");
                                };
                                remote_endpoint.close().await;
                                client_endpoint.close().await;
                                client_endpoint.destroy().await;
                                Err(RemoteEndpointError::ReceiveTimeout(10)
                                    .into())
                            }
                            Ok(Ok(_)) => {
                                // Remote endpoint still have data to read
                                if let Err(e) =
                                    Self::flush_remote_recv_buf_to_client(
                                        &client_endpoint,
                                        &remote_endpoint,
                                    )
                                    .await
                                {
                                    error!("<<<< Transport {transport_id} error happen when flush remote receive buffer to client because of the error: {e:?}");
                                };
                                remote_endpoint.close().await;
                                client_endpoint.close().await;
                                client_endpoint.destroy().await;
                                Ok(())
                            }
                            Ok(Err(e)) => {
                                error!("<<<< Transport {transport_id} error happen when read from remote endpoint because of the error: {e:?}");
                                if let Err(e) =
                                    Self::flush_client_recv_buf_to_remote(
                                        &client_endpoint,
                                        &remote_endpoint,
                                    )
                                    .await
                                {
                                    error!(">>>> Transport {transport_id} error happen when flush client receive buffer to remote because of the error: {e:?}");
                                };
                                remote_endpoint.close().await;
                                client_endpoint.close().await;
                                client_endpoint.destroy().await;
                                Err(e.into())
                            }
                        }
                    }
                }
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
                Err(AgentError::ClientEndpoint(ClientEndpointError::Other(
                    anyhow!(e),
                )))
            }
        }
    }

    async fn flush_client_recv_buf_to_remote(
        client_endpoint: &ClientEndpoint<'_>,
        remote_endpoint: &RemoteEndpoint,
    ) -> Result<(), RemoteEndpointError> {
        client_endpoint
            .consume_recv_buffer(
                remote_endpoint,
                super::consume_client_recv_buf_fn,
            )
            .await
    }

    async fn flush_remote_recv_buf_to_client(
        client_endpoint: &ClientEndpoint<'_>,
        remote_endpoint: &RemoteEndpoint,
    ) -> Result<(), ClientEndpointError> {
        match client_endpoint.get_state().await {
            ClientEndpointState::Udp(ClientEndpointUdpState::Closed) => Ok(()),
            _ => {
                remote_endpoint
                    .consume_recv_buffer(
                        client_endpoint,
                        super::consume_remote_recv_buf_fn,
                    )
                    .await
            }
        }
    }
}
