use crate::util::log_ip_packet;

use super::transportation::Transportation;
use super::transportation::TransportationId;
use log::{debug, error};

use tokio::sync::{oneshot::Receiver, Mutex};

use super::types::TransportationsRepository;
use anyhow::Result;
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};
use std::fs::File;
use std::io::ErrorKind;
use std::os::unix::io::FromRawFd;
use std::{
    collections::hash_map::Entry,
    io::{Read, Write},
    sync::Arc,
};

const TOKEN_DEVICE: Token = Token(0);

pub(crate) struct TransportationProcessor<'buf>
where
    'buf: 'static,
{
    client_file: Arc<Mutex<File>>,
    transportations: TransportationsRepository<'buf>,
    stop_receiver: Receiver<bool>,
    client_file_descriptor: i32,
    poll: Poll,
}

impl<'buf> TransportationProcessor<'buf>
where
    'buf: 'static,
{
    pub(crate) fn new(client_file_descriptor: i32, stop_receiver: Receiver<bool>) -> Result<Self> {
        let client_file = Arc::new(Mutex::new(unsafe {
            File::from_raw_fd(client_file_descriptor)
        }));

        Ok(TransportationProcessor {
            client_file,
            transportations: Default::default(),
            stop_receiver,
            poll: Poll::new()?,
            client_file_descriptor,
        })
    }

    pub(crate) async fn run(&mut self) -> Result<()> {
        let registry = self.poll.registry();
        registry.register(
            &mut SourceFd(&self.client_file_descriptor),
            TOKEN_DEVICE,
            Interest::READABLE,
        )?;
        let mut events = Events::with_capacity(1024);

        'device_file_io_loop: loop {
            self.poll.poll(&mut events, None)?;
            for event in events.iter() {
                if event.token() == TOKEN_DEVICE {
                    let mut client_file_read_buffer = [0u8; 65536];
                    let client_data = match {
                        let mut client_file = self.client_file.lock().await;
                        client_file.read(&mut client_file_read_buffer)
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
                        transportation.transfer_local_recv_buf_to_remote().await;
                    }
                }
            }
        }
    }

    async fn get_or_create_transportation(&mut self, data: &[u8]) -> Option<Arc<Transportation<'buf>>> {
        let trans_id = TransportationId::new(data)?;
        let transportations_for_remote = Arc::clone(&self.transportations);
        let mut transportations = self.transportations.lock().await;

        match transportations.entry(trans_id) {
            Entry::Occupied(entry) => Some(entry.get().clone()),
            Entry::Vacant(entry) => {
                debug!(">>>> Transportation {trans_id} not exist in repository create a new one.");
                let transportation = Transportation::new(trans_id, Arc::clone(&self.client_file_write))?;
                let transportation_for_remote = Arc::clone(&transportation);
                tokio::spawn(async move {
                    // Spawn a task for transportation
                    if let Err(e) = transportation_for_remote
                        .start_remote_endpoint(Arc::clone(&transportations_for_remote))
                        .await
                    {
                        error!(">>>> Transportation {trans_id} fail connect to remote endpoint because of error: {e:?}");
                    };
                });
                entry.insert(Arc::clone(&transportation));
                Some(transportation)
            }
        }
    }
}
