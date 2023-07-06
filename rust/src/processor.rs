use crate::util::log_ip_packet;

use super::transportation::Transportation;
use super::transportation::TransportationId;
use log::{debug, error};

use tokio::sync::{oneshot::Receiver, Mutex};

use anyhow::Result;
use std::io::ErrorKind;
use std::os::unix::io::FromRawFd;
use std::{
    collections::hash_map::Entry,
    io::{Read, Write},
    sync::Arc,
};
use std::{collections::HashMap, fs::File};

pub(crate) struct TransportationProcessor<'buf>
where
    'buf: 'static,
{
    client_file_read: Arc<Mutex<File>>,
    client_file_write: Arc<Mutex<File>>,
    transportations: Arc<Mutex<HashMap<TransportationId, Arc<Transportation<'buf>>>>>,
    stop_receiver: Receiver<bool>,
}

impl<'buf> TransportationProcessor<'buf>
where
    'buf: 'static,
{
    pub(crate) fn new(client_file_descriptor: i32, stop_receiver: Receiver<bool>) -> Result<Self> {
        let client_file = unsafe { File::from_raw_fd(client_file_descriptor) };
        let client_file_read = Arc::new(Mutex::new(client_file));
        let client_file_write = client_file_read.clone();
        Ok(TransportationProcessor {
            client_file_read,
            client_file_write,
            transportations: Default::default(),
            stop_receiver,
        })
    }

    pub(crate) async fn run(&mut self) -> Result<()> {
        let mut client_file_read_buffer = [0u8; 65536];
        loop {
            let client_data = match {
                let mut client_file_read = self.client_file_read.lock().await;
                client_file_read.read(&mut client_file_read_buffer)
            } {
                Ok(0) => {
                    break;
                }
                Ok(size) => &client_file_read_buffer[..size],
                Err(e) => {
                    if e.kind() == ErrorKind::WouldBlock {
                        continue;
                    }
                    break;
                }
            };
            if let Some(transportation) = self.get_or_create_transportation(client_data).await {
                let trans_id = transportation.get_trans_id();
                // Push the ip packet to smoltcp
                transportation
                    .push_rx_to_smoltcp_device(client_data.to_vec())
                    .await;
                // Poll smoltcp to make sure the protocol packet will send to client side
                if transportation.poll_local_endpoint().await {
                    while let Some(data_to_client) = transportation.pop_tx_from_smoltcp_device().await {
                        let log = log_ip_packet(&data_to_client);
                        debug!("<<<< Transportation {trans_id} write the tx to device:\n{log}\n",);
                        let mut client_file_write = self.client_file_write.lock().await;
                        client_file_write.write_all(&data_to_client)?;
                    }
                };
                // Read data from smoltcp and store to the local receive buffer
                transportation.receive_from_local_endpoint().await?;
                transportation.consume_local_recv_buf().await;
            }
        }
        Ok(())
    }

    async fn get_or_create_transportation(&mut self, data: &[u8]) -> Option<Arc<Transportation<'buf>>> {
        let trans_id = TransportationId::new(data)?;
        let mut transportations = self.transportations.lock().await;
        match transportations.entry(trans_id) {
            Entry::Occupied(entry) => Some(entry.get().clone()),
            Entry::Vacant(entry) => {
                debug!(">>>> Transportation {trans_id} not exist in repository create a new one.");
                let transportation = Transportation::new(trans_id, Arc::clone(&self.client_file_write))?;
                entry.insert(Arc::clone(&transportation));
                let transportation_for_remote = Arc::clone(&transportation);
                tokio::spawn(async move {
                    // Spawn a task for transportation
                    if let Err(e) = transportation_for_remote.start_remote_endpoint().await {
                        error!(">>>> Transportation {trans_id} fail connect to remote endpoint because of error: {e:?}");
                    };
                });
                Some(transportation)
            }
        }
    }
}
