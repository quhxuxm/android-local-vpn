mod endpoint;
mod value;

use endpoint::LocalEndpoint;
use endpoint::RemoteEndpoint;
use log::debug;

use std::{
    collections::HashMap,
    fs::File,
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::anyhow;
use anyhow::Result;
use tokio::sync::Mutex;

use crate::util::log_ip_packet;

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
    client_file_write: Arc<Mutex<File>>,
    closed: AtomicBool,
}

impl<'buf> Transportation<'buf>
where
    'buf: 'static,
{
    pub(crate) fn new(trans_id: TransportationId, client_file_write: Arc<Mutex<File>>) -> Option<Arc<Transportation<'buf>>> {
        let transportation = Transportation {
            trans_id,
            local_endpoint: Arc::new(LocalEndpoint::new(
                trans_id,
                trans_id.transport_protocol,
                trans_id.source,
                trans_id.destination,
            )?),
            remote_endpoint: Mutex::new(None),
            client_file_write,
            closed: AtomicBool::new(false),
        };

        debug!(">>>> Transportation {trans_id} created.");
        Some(Arc::new(transportation))
    }

    pub(crate) async fn start_remote_endpoint(&self, transportations: Arc<Mutex<HashMap<TransportationId, Arc<Transportation<'buf>>>>>) -> Result<()> {
        let connected_remote_endpoint = RemoteEndpoint::new(
            self.trans_id,
            self.trans_id.transport_protocol,
            self.trans_id.internet_protocol,
            self.trans_id.destination,
        )
        .await
        .ok_or(anyhow!(
            "Transportation {} fail to start remote endpoint.",
            self.trans_id
        ))?;
        connected_remote_endpoint.start_read_remote(transportations);
        {
            let mut remote_endpoint = self.remote_endpoint.lock().await;
            *remote_endpoint = Some(Arc::new(connected_remote_endpoint));
        }
        loop {
            if self.closed.load(Ordering::Relaxed) {
                debug!(
                    "Transportation {} closed break the consume remote receive buffer loop.",
                    self.trans_id
                );
                break;
            }
            self.consume_remote_recv_buf().await;
        }
        Ok(())
    }

    /// Poll the device endpoint smoltcp to trigger the iface
    pub(crate) async fn poll_local_endpoint(&self) -> bool {
        self.local_endpoint.poll().await
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

    pub(crate) async fn close_local_endpoint(&self) {
        self.consume_remote_recv_buf().await;
        self.local_endpoint.close().await;
        self.closed.swap(true, Ordering::Relaxed);
        debug!(
            ">>>> Transportation {} close device endpoint.",
            self.trans_id
        )
    }

    pub(crate) async fn transfer_local_recv_buf_to_remote(&self) {
        let remote_endpoint = {
            let remote_endpoint = self.remote_endpoint.lock().await;
            if let Some(remote_endpoint) = &*remote_endpoint {
                Arc::clone(remote_endpoint)
            } else {
                return;
            }
        };
        self.local_endpoint
            .consume_local_recv_buf_with(
                self.trans_id,
                remote_endpoint,
                Self::concrete_transfer_to_remote_fn,
            )
            .await
    }

    async fn concrete_transfer_to_remote_fn(trans_id: TransportationId, remote_endpont: Arc<RemoteEndpoint>, data: Vec<u8>) -> Result<usize> {
        debug!(
            ">>>> Transportation {trans_id} begin write data to remote: {}",
            pretty_hex::pretty_hex(&data)
        );
        let remote_write_result = remote_endpont.write_to_remote(&data).await;
        debug!(">>>> Transportation {trans_id} complete to write data to remote",);
        remote_write_result
    }

    pub(crate) async fn consume_remote_recv_buf(&self) {
        let remote_endpoint = {
            let remote_endpoint = self.remote_endpoint.lock().await;
            if let Some(remote_endpoint) = &*remote_endpoint {
                Arc::clone(remote_endpoint)
            } else {
                return;
            }
        };
        remote_endpoint.waiting_for_consume_notify().await;
        remote_endpoint
            .consume_remote_recv_buf_with(
                self.trans_id,
                Arc::clone(&self.client_file_write),
                Arc::clone(&self.local_endpoint),
                Self::consume_remote_recv_buf_fn,
            )
            .await
    }

    async fn consume_remote_recv_buf_fn(
        trans_id: TransportationId,
        client_file_write: Arc<Mutex<File>>,
        local_endpoint: Arc<LocalEndpoint<'_>>,
        data: Vec<u8>,
    ) -> Result<usize> {
        debug!(
            "<<<< Transportation {trans_id} begin write data to client: {}",
            pretty_hex::pretty_hex(&data)
        );
        let local_write_result = local_endpoint.send_to_smoltcp(&data).await;

        if local_endpoint.poll().await {
            while let Some(data_to_client) = local_endpoint.pop_tx_from_device().await {
                let log = log_ip_packet(&data_to_client);
                debug!("<<<< Transportation {trans_id} write the tx to client:\n{log}\n",);
                let mut client_file_write = client_file_write.lock().await;
                client_file_write.write_all(&data_to_client)?;
            }
        }

        debug!("<<<< Transportation {trans_id} complete to write data to client",);
        local_write_result
    }
}
