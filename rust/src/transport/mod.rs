mod client;
mod common;
mod value;

use std::{fs::File, io::Write, sync::Arc};

pub(crate) use self::value::ControlProtocol;
use self::{
    client::ClientEndpoint,
    common::{create_smoltcp_tcp_socket, create_smoltcp_udp_socket, prepare_smoltcp_iface_and_device},
};
use anyhow::Result;
use log::{debug, error};

use tokio::sync::{
    mpsc::{channel, Receiver, Sender},
    Mutex,
};

pub(crate) use self::value::TransportId;

#[derive(Debug)]
pub(crate) struct Transport {
    transport_id: TransportId,
    client_file_write: Arc<Mutex<File>>,
    client_data_sender: Sender<Vec<u8>>,
    client_data_receiver: Mutex<Option<Receiver<Vec<u8>>>>,
}

impl Transport {
    pub(crate) fn new(transport_id: TransportId, client_file_write: Arc<Mutex<File>>) -> Self {
        let (client_data_sender, client_data_receiver) = channel::<Vec<u8>>(1024);
        Self {
            transport_id,
            client_file_write,
            client_data_sender,
            client_data_receiver: Mutex::new(Some(client_data_receiver)),
        }
    }

    pub(crate) async fn start(&self) -> Result<()> {
        let transport_id = self.transport_id;
        if let Some(mut client_data_receiver) = self.client_data_receiver.lock().await.take() {
            let (mut client_endpoint, client_endpoint_output_tx) = match self.transport_id.control_protocol {
                ControlProtocol::Tcp => ClientEndpoint::new_tcp(self.transport_id)?,
                ControlProtocol::Udp => ClientEndpoint::new_udp(self.transport_id)?,
            };
            debug!("Transport {transport_id} begin spawn client output task");
            self.spawn_client_output(client_endpoint_output_tx);
            debug!("Transport {transport_id} success spawn client output task");
            loop {
                if let Some(client_data) = client_data_receiver.recv().await {
                    client_endpoint.receive(client_data).await;
                };
            }
        }
        Ok(())
    }

    pub(crate) async fn feed_client_data(&self, data: &[u8]) {
        if let Err(e) = self.client_data_sender.send(data.to_vec()).await {
            error!(
                ">>>> Transport {} fail to feed client data because of error: {e:?}",
                self.transport_id
            );
        }
    }

    fn spawn_client_output(&self, mut client_endpoint_output_tx: Receiver<Vec<u8>>) {
        // Spwn a task for output data to client
        let transport_id = self.transport_id;
        let client_file_write = Arc::clone(&self.client_file_write);
        tokio::spawn(async move {
            while let Some(data) = client_endpoint_output_tx.recv().await {
                let mut client_file_write = client_file_write.lock().await;
                if let Err(e) = client_file_write.write_all(&data) {
                    error!("<<<< Transport {transport_id} fail to write data to client because of error: {e:?}");
                    break;
                };
            }
        });
    }
}
