mod buffers;
mod endpoint;
mod value;

use crate::error::NetworkError;

use endpoint::DeviceEndpoint;
use endpoint::RemoteEndpoint;
use log::{debug, error, trace};

use buffers::Buffer;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{
    io::Ready,
    sync::{
        mpsc::{channel, Sender},
        RwLock,
    },
};

pub(crate) use self::value::InternetProtocol;
pub(crate) use self::value::TransportProtocol;
pub(crate) use self::value::TransportationId;

pub(crate) struct Transportation<'buf>
where
    'buf: 'static,
{
    trans_id: TransportationId,
    device_endpoint: Arc<DeviceEndpoint<'buf>>,
    remote_endpoint: RwLock<Option<Arc<RemoteEndpoint>>>,
    buffer: Arc<Buffer>,
    notify_transfer_to_remote_sender: Sender<Instant>,
}

impl<'buf> Transportation<'buf>
where
    'buf: 'static,
{
    pub(crate) fn new(trans_id: TransportationId) -> Option<Arc<Transportation<'buf>>> {
        let (notify_transfer_to_remote_sender, mut notify_transfer_to_remote_receiver) = channel::<Instant>(1024);
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
            remote_endpoint: RwLock::new(None),
            buffer,
            notify_transfer_to_remote_sender,
        };

        debug!(">>>> Transportation {trans_id} created.");
        let transportation = Arc::new(transportation);
        let transportation_clone = Arc::clone(&transportation);
        tokio::spawn(async move {
            loop {
                match notify_transfer_to_remote_receiver.recv().await {
                    Some(time) => {
                        // trace!(">>>> Transportation {trans_id} receive notify to transfer remote buffer on {time:?}")
                    }
                    None => return,
                };

                if let Some(remote_endpoint) = transportation_clone.remote_endpoint.read().await.as_ref() {
                    transportation_clone
                        .buffer
                        .consume_remote_buffer_with(
                            Arc::clone(remote_endpoint),
                            Self::concrete_write_to_remote_endpoint,
                        )
                        .await;
                }
            }
        });
        Some(transportation)
    }

    pub(crate) async fn init(&self) -> Result<(), NetworkError> {
        let connected_remote_endpoint = Self::create_remote_endpoint(self.trans_id)
            .await
            .ok_or(NetworkError::RemoteEndpointInInvalidState)?;
        let connected_remote_endpoint = Arc::new(connected_remote_endpoint);
        let mut remote_endpoint = self.remote_endpoint.write().await;
        *remote_endpoint = Some(connected_remote_endpoint);
        self.notify_transfer_to_remote().await;
        Ok(())
    }

    pub(crate) async fn notify_transfer_to_remote(&self) {
        if let Err(e) = self
            .notify_transfer_to_remote_sender
            .send(Instant::now())
            .await
        {
            error!(
                ">>>> Transportation {} fail to notify transfer remote buffer because of error: {e:?}",
                self.trans_id
            );
        }
    }

    pub(crate) async fn poll_remote_endpoint(&self) -> Result<Ready, NetworkError> {
        if let Some(remote_endpoint) = self.remote_endpoint.read().await.as_ref() {
            remote_endpoint.poll().await
        } else {
            Err(NetworkError::RemoteEndpointInInvalidState)
        }
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
        if let Some(remote_endpoint) = self.remote_endpoint.read().await.as_ref() {
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

    pub(crate) async fn push_rx_to_device(&self, rx_data: Vec<u8>) {
        self.device_endpoint.push_rx_to_device(rx_data).await
    }

    pub(crate) async fn pop_tx_from_device(&self) -> Option<Vec<u8>> {
        self.device_endpoint.pop_tx_from_device().await
    }

    pub(crate) async fn read_from_remote_endpoint(&self) -> Result<(Vec<Vec<u8>>, bool), NetworkError> {
        if let Some(remote_endpoint) = self.remote_endpoint.read().await.as_ref() {
            remote_endpoint.read().await
        } else {
            Err(NetworkError::RemoteEndpointInInvalidState)
        }
    }

    pub(crate) async fn read_from_device_endpoint(&self, data: &mut [u8]) -> Result<usize, NetworkError> {
        self.device_endpoint.receive(data).await
    }

    pub(crate) async fn store_data_to_remote_buffer(&self, data: &[u8]) {
        self.buffer.push_device_data_to_remote(data).await
    }

    pub(crate) async fn store_data_to_device_buffer(&self, data: &[u8]) {
        self.buffer.push_remote_data_to_device(data).await
    }

    async fn concrete_write_to_device_endpoint(device_endpoint: Arc<DeviceEndpoint<'_>>, data: Vec<u8>) -> Result<usize, NetworkError> {
        device_endpoint.send(&data).await
    }

    /// Transfer the data inside device buffer to device endpoint.
    pub(crate) async fn transfer_device_buffer(&self) {
        self.buffer
            .consume_device_buffer_with(
                self.device_endpoint.clone(),
                Self::concrete_write_to_device_endpoint,
            )
            .await;
    }

    async fn concrete_write_to_remote_endpoint(remote_endpoint: Arc<RemoteEndpoint>, data: Vec<u8>) -> Result<usize, NetworkError> {
        remote_endpoint.write(&data).await
    }

    /// Transfer the data inside remote buffer to remote endpoint
    async fn transfer_remote_buffer(&self) -> Result<(), NetworkError> {
        let trans_id = self.trans_id;
        if let Some(remote_endpoint) = self.remote_endpoint.read().await.as_ref() {
            self.buffer
                .consume_remote_buffer_with(
                    Arc::clone(remote_endpoint),
                    Self::concrete_write_to_remote_endpoint,
                )
                .await;
            Ok(())
        } else {
            error!(">>>> Transportation {trans_id} fail transfer remote buffer data to remote point because of remote endpoint not exist");
            Err(NetworkError::RemoteEndpointInInvalidState)
        }
    }

    pub(crate) fn get_trans_id(&self) -> TransportationId {
        self.trans_id
    }
}
