use std::time::Duration;

use bytes::{Bytes, BytesMut};
use log::{debug, error, trace};

use tokio::{sync::mpsc::Sender, time::timeout};

use crate::{
    config::PpaassVpnServerConfig,
    error::{ClientEndpointError, RemoteEndpointError, TransportError},
    util::AgentRsaCryptoFetcher,
};

use super::{
    client::ClientUdpEndpoint, remote::RemoteUdpEndpoint, ClientOutputPacket,
    TransportId,
};

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
        client_data: BytesMut,
    ) -> Result<(), TransportError> {
        let mut client_endpoint = ClientUdpEndpoint::new(
            self.transport_id,
            self.client_output_tx,
            config,
        )?;

        let mut remote_endpoint = match RemoteUdpEndpoint::new(
            self.transport_id,
            agent_rsa_crypto_fetcher,
            config,
        )
        .await
        {
            Ok(remote_endpoint) => remote_endpoint,
            Err(e) => {
                error!(">>>> Transport {} error happen when initialize the remote endpoint because of the error: {e:?}",self.transport_id);
                client_endpoint.close().await;
                client_endpoint.destroy().await;
                return Err(e.into());
            }
        };

        debug!(">>>> Transport {} initialize success, begin to serve client input data.",self.transport_id);
        // Push the data into smoltcp stack.
        match timeout(
            Duration::from_secs(config.get_client_udp_recv_timeout()),
            client_endpoint.receive_from_client(client_data),
        )
        .await
        {
            Err(_) => {
                error!("<<<< Transport {} receive udp from remote timeout in 10 seconds.",self.transport_id);
                if let Err(e) = client_endpoint
                    .consume_recv_buffer(
                        &mut remote_endpoint,
                        Self::consume_client_recv_buf_fn,
                    )
                    .await
                {
                    error!(">>>> Transport {} error happen when flush client receive buffer to remote because of the error: {e:?}",self.transport_id);
                };
                remote_endpoint.close().await;
                client_endpoint.close().await;
                client_endpoint.destroy().await;
                Err(RemoteEndpointError::ReceiveTimeout(10).into())
            }
            Ok(Ok(())) => {
                if let Err(e) = client_endpoint
                    .consume_recv_buffer(
                        &mut remote_endpoint,
                        Self::consume_client_recv_buf_fn,
                    )
                    .await
                {
                    error!(">>>> Transport {} error happen when flush client receive buffer to remote because of the error: {e:?}",self.transport_id);
                    remote_endpoint.close().await;
                    client_endpoint.close().await;
                    client_endpoint.destroy().await;
                    return Err(e.into());
                };
                match timeout(
                    Duration::from_secs(config.get_remote_udp_recv_timeout()),
                    remote_endpoint.read_from_remote(),
                )
                .await
                {
                    Err(_) => {
                        error!("<<<< Transport {} timeout in 10 seconds when receive from remote.",self.transport_id);
                        if let Err(e) = client_endpoint
                            .consume_recv_buffer(
                                &mut remote_endpoint,
                                Self::consume_client_recv_buf_fn,
                            )
                            .await
                        {
                            error!(">>>> Transport {} error happen when flush client receive buffer to remote because of the error: {e:?}",self.transport_id);
                        };
                        remote_endpoint.close().await;
                        client_endpoint.close().await;
                        client_endpoint.destroy().await;
                        Err(RemoteEndpointError::ReceiveTimeout(10).into())
                    }
                    Ok(Ok(_)) => {
                        // Remote endpoint still have data to read
                        if let Err(e) = remote_endpoint
                            .consume_recv_buffer(
                                &mut client_endpoint,
                                Self::consume_remote_recv_buf_fn,
                            )
                            .await
                        {
                            error!("<<<< Transport {} error happen when flush remote receive buffer to client because of the error: {e:?}",self.transport_id);
                        };
                        remote_endpoint.close().await;
                        client_endpoint.close().await;
                        client_endpoint.destroy().await;
                        Ok(())
                    }
                    Ok(Err(e)) => {
                        error!("<<<< Transport {} error happen when read from remote endpoint because of the error: {e:?}",self.transport_id);
                        if let Err(e) = client_endpoint
                            .consume_recv_buffer(
                                &mut remote_endpoint,
                                Self::consume_client_recv_buf_fn,
                            )
                            .await
                        {
                            error!(">>>> Transport {} error happen when flush client receive buffer to remote because of the error: {e:?}",self.transport_id);
                        };
                        remote_endpoint.close().await;
                        client_endpoint.close().await;
                        client_endpoint.destroy().await;
                        Err(e.into())
                    }
                }
            }
            Ok(Err(e)) => {
                if let Err(e) = client_endpoint
                    .consume_recv_buffer(
                        &mut remote_endpoint,
                        Self::consume_client_recv_buf_fn,
                    )
                    .await
                {
                    error!(">>>> Transport {} error happen when flush client receive buffer to remote because of the error: {e:?}",self.transport_id);
                };
                remote_endpoint.close().await;
                client_endpoint.destroy().await;
                Err(TransportError::ClientEndpoint(e))
            }
        }
    }

    /// The concrete function to forward client receive buffer to remote.
    /// * transport_id: The transportation id.
    /// * data: The data going to send to remote.
    /// * remote_endpoint: The remote endpoint.
    async fn consume_client_recv_buf_fn(
        transport_id: TransportId,
        data: Vec<Bytes>,
        remote_endpoint: &mut RemoteUdpEndpoint,
    ) -> Result<usize, RemoteEndpointError> {
        let mut consume_size = 0;
        for data in data.into_iter() {
            trace!(
                ">>>> Transport {transport_id} write udp data to remote: {}",
                pretty_hex::pretty_hex(&data)
            );
            remote_endpoint.write_to_remote(data).await?;
            consume_size += 1;
        }
        Ok(consume_size)
    }

    /// The concrete function to forward remote receive buffer to client.
    /// * transport_id: The transportation id.
    /// * data: The data going to send to remote.
    /// * client: The client endpoint.
    async fn consume_remote_recv_buf_fn<'b>(
        transport_id: TransportId,
        data: Vec<Bytes>,
        client_endpoint: &mut ClientUdpEndpoint<'b>,
    ) -> Result<usize, ClientEndpointError>
    where
        'b: 'static,
    {
        let mut consume_size = 0;
        for data in data.into_iter() {
            trace!(
                ">>>> Transport {transport_id} write udp data to smoltcp: {}",
                pretty_hex::pretty_hex(&data)
            );
            client_endpoint.send_to_smoltcp(&data).await?;
            consume_size += 1;
        }
        Ok(consume_size)
    }
}
