mod buffers;
mod endpoint;
mod value;

use endpoint::LocalEndpoint;
use endpoint::RemoteEndpoint;
use log::{debug, error};

use buffers::Buffer;

use std::{future::Future, sync::Arc};

use anyhow::anyhow;
use anyhow::Result;
use tokio::sync::Mutex;

pub(crate) use self::value::InternetProtocol;
pub(crate) use self::value::TransportProtocol;
pub(crate) use self::value::TransportationId;

pub(crate) struct Transportation<'buf>
where
    'buf: 'static,
{
    trans_id: TransportationId,
    local_endpoint: Arc<LocalEndpoint<'buf>>,
    remote_endpoint: Mutex<Option<Arc<RemoteEndpoint>>>,
    buffer: Arc<Buffer>,
}

impl<'buf> Transportation<'buf>
where
    'buf: 'static,
{
    pub(crate) fn new(trans_id: TransportationId) -> Option<Arc<Transportation<'buf>>> {
        let buffer = match trans_id.transport_protocol {
            TransportProtocol::Tcp => Arc::new(Buffer::new_tcp_buffer()),
            TransportProtocol::Udp => Arc::new(Buffer::new_udp_buffer()),
        };
        let transportation = Transportation {
            trans_id,
            local_endpoint: Arc::new(LocalEndpoint::new(
                trans_id,
                trans_id.transport_protocol,
                trans_id.source,
                trans_id.destination,
            )?),
            remote_endpoint: Mutex::new(None),
            buffer,
        };

        debug!(">>>> Transportation {trans_id} created.");
        Some(Arc::new(transportation))
    }

    pub(crate) async fn start_remote_endpoint(&self) -> Result<()> {
        let connected_remote_endpoint = RemoteEndpoint::new(
            self.trans_id,
            self.trans_id.transport_protocol,
            self.trans_id.internet_protocol,
            self.trans_id.destination,
        )
        .await
        .ok_or(anyhow!("Fail to start remote endpoint."))?;
        let connected_remote_endpoint = Arc::new(connected_remote_endpoint);
        let mut remote_endpoint = self.remote_endpoint.lock().await;
        connected_remote_endpoint.start();
        *remote_endpoint = Some(connected_remote_endpoint);
        Ok(())
    }

    /// Poll the device endpoint smoltcp to trigger the iface
    pub(crate) async fn poll_local_endpoint(&self) -> bool {
        self.local_endpoint.poll().await
    }

    pub(crate) async fn close_local_endpoint(&self) {
        self.local_endpoint.close().await;
        debug!(
            ">>>> Transportation {} close device endpoint.",
            self.trans_id
        )
    }

    pub(crate) async fn local_endpoint_can_receive(&self) -> bool {
        self.local_endpoint.can_receive_from_smoltcp().await
    }

    pub(crate) async fn local_endpoint_can_send(&self) -> bool {
        self.local_endpoint.can_send_to_smoltcp().await
    }

    pub(crate) async fn close_remote_endpoint(&self) -> Result<()> {
        if let Some(remote_endpoint) = self.remote_endpoint.lock().await.as_ref() {
            debug!(
                ">>>> Transportation {} close remote endpoint.",
                self.trans_id
            );
            if let Err(e) = remote_endpoint.close().await {
                error!(
                    "<<<< Transportation {} fail to close remote endpoint because of error: {e:?}",
                    self.trans_id
                );
            };
        }

        Ok(())
    }

    pub(crate) async fn push_rx_to_smoltcp_device(&self, rx_data: Vec<u8>) {
        self.local_endpoint.push_rx_to_device(rx_data).await
    }

    pub(crate) async fn pop_tx_from_smoltcp_device(&self) -> Option<Vec<u8>> {
        self.local_endpoint.pop_tx_from_device().await
    }

    pub(crate) async fn receive_from_local_endpoint(&self) -> Result<()> {
        self.local_endpoint.receive_from_smoltcp().await
    }

    pub(crate) fn get_trans_id(&self) -> TransportationId {
        self.trans_id
    }

    pub(crate) async fn consume_local_recv_buf(&self) {
        let remote_endpoint = self.remote_endpoint.lock().await;
        if let Some(remote_endpoint) = &*remote_endpoint {
            self.local_endpoint
                .consume_local_recv_buf_with(Self::consume_local_recv_buf_fn)
                .await
        }
    }

    async fn consume_local_recv_buf_fn(data: &[u8]) -> Result<usize> {
        todo!()
    }

    pub(crate) async fn write_to_remote(remote_endpoint: Arc<RemoteEndpoint>, data: &[u8]) {
        remote_endpoint.write_to_remote(data).await;
    }
}
