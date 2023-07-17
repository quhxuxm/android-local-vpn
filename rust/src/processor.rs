use super::transportation::Transportation;
use super::transportation::TransportationId;
use log::{debug, error};

use tokio::sync::{oneshot::Receiver, Mutex};

use super::types::TransportationsRepository;
use anyhow::Result;
use std::fs::File;
use std::io::ErrorKind;
use std::os::unix::io::FromRawFd;
use std::{collections::hash_map::Entry, io::Read, sync::Arc};

pub(crate) struct TransportationProcessor<'buf>
where
    'buf: 'static,
{
    client_file_read: Arc<Mutex<File>>,
    client_file_write: Arc<Mutex<File>>,
    transportations: TransportationsRepository<'buf>,
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
                // Poll smoltcp to make sure the protocol packet will send to client side
                transportation.send_to_smoltcp(client_data).await;
            }
        }
        Ok(())
    }

    async fn get_or_create_transportation(&mut self, data: &[u8]) -> Option<Arc<Transportation<'buf>>> {
        let trans_id = TransportationId::new(data)?;
        let mut transportations = self.transportations.lock().await;

        match transportations.entry(trans_id) {
            Entry::Occupied(entry) => {
                entry.get().send_to_smoltcp(data).await;
                Some(entry.get().clone())
            }
            Entry::Vacant(entry) => {
                debug!(">>>> Transportation {trans_id} not exist in repository create a new one.");
                  let client_file_write = Arc::clone(&self.client_file_write);
                tokio::spawn(async move{

                });
              
                let transportation = Transportation::new(trans_id, client_file_write).await?;
                transportation.send_to_smoltcp(data).await;
                let transportation = Arc::new(transportation);
                entry.insert(Arc::clone(&transportation));
                if let Err(e) = transportation.start().await {
                    error!(">>>> Transportation {trans_id} fail to start because of error: {e:?}")
                };
                Some(transportation)
            }
        }
    }
}
