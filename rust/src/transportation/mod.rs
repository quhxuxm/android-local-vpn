mod buffers;
mod endpoint;
mod value;

use crate::error::NetworkError;

use endpoint::DeviceEndpoint;
use endpoint::RemoteEndpoint;
use log::{debug, error};

use buffers::Buffer;

use std::sync::Arc;

use tokio::io::Ready;

pub(crate) use self::value::InternetProtocol;
pub(crate) use self::value::TransportProtocol;
pub(crate) use self::value::TransportationId;

pub(crate) struct Transportation<'buf> {
    trans_id: TransportationId,
    device_endpoint: Arc<DeviceEndpoint<'buf>>,
    remote_endpoint: Arc<RemoteEndpoint>,
    buffer: Arc<Buffer>,
}

impl<'buf> Transportation<'buf> {
    pub(crate) async fn new(trans_id: TransportationId) -> Option<Transportation<'buf>> {
        let remote_endpoint = Self::create_remote_endpoint(trans_id).await?;
        let remote_endpoint = Arc::new(remote_endpoint);
        let buffer = match trans_id.transport_protocol {
            TransportProtocol::Tcp => Arc::new(Buffer::new_tcp_buffer()),
            TransportProtocol::Udp => Arc::new(Buffer::new_udp_buffer()),
        };
        let transportation = Transportation {
            trans_id,
            device_endpoint: Arc::new(DeviceEndpoint::new(
                trans_id,
                trans_id.transport_protocol,
                trans_id.source,
                trans_id.destination,
            )?),
            remote_endpoint,
            buffer,
        };

        debug!(">>>> Transportation {trans_id} created.");
        Some(transportation)
    }

    pub(crate) async fn poll_remote_endpoint(&self) -> Result<Ready, NetworkError> {
        self.remote_endpoint.poll().await
    }
    async fn create_remote_endpoint(trans_id: TransportationId) -> Option<RemoteEndpoint> {
        let remote_endpoint = RemoteEndpoint::new(
            trans_id,
            trans_id.transport_protocol,
            trans_id.internet_protocol,
            trans_id.destination,
        )
        .await?;
        Some(remote_endpoint)
    }

    /// Poll the device endpoint smoltcp to trigger the iface
    pub(crate) async fn poll_device_endpoint(&self) -> bool {
        self.device_endpoint.poll().await
    }

    pub(crate) async fn close_device_endpoint(&self) {
        self.device_endpoint.close().await;
        debug!(
            ">>>> Transportation {} close device endpoint.",
            self.trans_id
        )
    }

    pub(crate) async fn device_endpoint_can_receive(&self) -> bool {
        self.device_endpoint.can_receive().await
    }

    pub(crate) async fn device_endpoint_can_send(&self) -> bool {
        self.device_endpoint.can_send().await
    }

    pub(crate) async fn close_remote_endpoint(&self) -> Result<(), NetworkError> {
        debug!(
            ">>>> Transportation {} close remote endpoint.",
            self.trans_id
        );
        if let Err(e) = self.remote_endpoint.close().await {
            error!(
                "<<<< Transportation {} fail to close remote endpoint because of error: {e:?}",
                self.trans_id
            );
        };
        Ok(())
    }

    pub(crate) async fn push_rx_to_device(&self, rx_data: Vec<u8>) {
        self.device_endpoint.push_rx_to_device(rx_data).await
    }

    pub(crate) async fn pop_tx_from_device(&self) -> Option<Vec<u8>> {
        self.device_endpoint.pop_tx_from_device().await
    }

    pub(crate) async fn read_from_remote_endpoint(&self) -> Result<(Vec<Vec<u8>>, bool), NetworkError> {
        self.remote_endpoint.read().await
    }

    pub(crate) async fn read_from_device_endpoint(&self, data: &mut [u8]) -> Result<usize, NetworkError> {
        self.device_endpoint.receive(data).await
    }

    pub(crate) async fn push_data_to_device_buffer(&self, data: &[u8]) {
        self.buffer.push_device_data_to_remote(data).await
    }

    pub(crate) async fn push_data_to_remote_buffer(&self, data: &[u8]) {
        self.buffer.push_remote_data_to_device(data).await
    }

    async fn concrete_write_to_device_endpoint(
        trans_id: TransportationId,
        device_endpoint: Arc<DeviceEndpoint<'_>>,
        data: Vec<u8>,
    ) -> Result<usize, NetworkError> {
        debug!(
            "<<<< Transportation {} going to write data in device buffer to device endpoint: {}",
            trans_id,
            pretty_hex::pretty_hex(&data)
        );
        device_endpoint.send(&data).await
    }

    /// Transfer the data inside device buffer to device endpoint.
    pub(crate) async fn transfer_device_buffer(&self) {
        debug!(
            ">>>> Transportation {} going to transfer the data in device buffer to device endpoint.",
            self.trans_id
        );
        self.buffer
            .consume_device_buffer_with(
                self.trans_id,
                self.device_endpoint.clone(),
                Self::concrete_write_to_device_endpoint,
            )
            .await;
    }

    async fn concrete_write_to_remote_endpoint(trans_id: TransportationId, remote_endpoint: Arc<RemoteEndpoint>, data: Vec<u8>) -> Result<usize, NetworkError> {
        debug!(
            ">>>> Transportation {} going to write data in remote buffer to remote point: {}",
            trans_id,
            pretty_hex::pretty_hex(&data)
        );
        remote_endpoint.write(&data).await
    }

    /// Transfer the data inside remote buffer to remote endpoint
    pub(crate) async fn transfer_remote_buffer(&self) {
        debug!(
            ">>>> Transportation {} going to transfer the data in remote buffer to remote endpoint.",
            self.trans_id
        );
        let trans_id = self.trans_id;

        self.buffer
            .consume_remote_buffer_with(
                trans_id,
                self.remote_endpoint.clone(),
                Self::concrete_write_to_remote_endpoint,
            )
            .await;
    }
}
