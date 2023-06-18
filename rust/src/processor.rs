use crate::{
    error::{AgentError, NetworkError, ServerError},
    util::log_ip_packet,
};

use super::transportation::Transportation;
use super::transportation::TransportationId;
use log::{debug, error};

use smoltcp::wire::{Ipv4Packet, Ipv6Packet, PrettyPrinter};
use tokio::{
    io::Ready,
    sync::{oneshot::Receiver, Mutex},
};

use std::{
    collections::hash_map::Entry,
    io::{Read, Write},
    sync::Arc,
};
use std::{collections::HashMap, fs::File};

use std::io::ErrorKind;

use std::os::unix::io::FromRawFd;

pub(crate) struct TransportationProcessor<'buf>
where
    'buf: 'static,
{
    device_file_read: Arc<Mutex<File>>,
    device_file_write: Arc<Mutex<File>>,
    transportations: Arc<Mutex<HashMap<TransportationId, Arc<Transportation<'buf>>>>>,
    stop_receiver: Receiver<bool>,
}

impl<'buf> TransportationProcessor<'buf>
where
    'buf: 'static,
{
    pub(crate) fn new(device_file_descriptor: i32, stop_receiver: Receiver<bool>) -> Result<Self, AgentError> {
        // let poll = Poll::new().map_err(NetworkError::InitializePoll)?;
        let device_file = unsafe { File::from_raw_fd(device_file_descriptor) };
        let device_file_read = Arc::new(Mutex::new(device_file));
        let device_file_write = device_file_read.clone();
        Ok(TransportationProcessor {
            device_file_read,
            device_file_write,
            transportations: Default::default(),
            stop_receiver,
        })
    }

    pub(crate) async fn run(&mut self) -> Result<(), AgentError> {
        let mut device_file_read_buffer: [u8; 65536] = [0; 65536];
        loop {
            let device_data = match {
                let mut device_file_read = self.device_file_read.lock().await;
                device_file_read.read(&mut device_file_read_buffer)
            } {
                Ok(0) => {
                    break;
                }
                Ok(size) => &device_file_read_buffer[..size],
                Err(e) => {
                    if e.kind() == ErrorKind::WouldBlock {
                        continue;
                    }
                    break;
                }
            };
            if let Some(trans_id) = self.get_or_create_transportation(device_data).await {
                let transportation = {
                    let transportations = self.transportations.lock().await;
                    Arc::clone(
                        transportations
                            .get(&trans_id)
                            .ok_or(ServerError::TransportationNotExist(trans_id))?,
                    )
                };
                transportation.push_rx_to_device(device_data.to_vec()).await;
                Self::write_to_device_file(
                    trans_id,
                    Arc::clone(&transportation),
                    self.device_file_write.clone(),
                )
                .await?;
                Self::read_from_device_endpoint(trans_id, Arc::clone(&transportation)).await;
                Self::write_to_remote_endpoint(trans_id, transportation).await;
            }
        }
        Ok(())
    }

    async fn start_remote_io_loop(
        trans_id: TransportationId,
        transportation: Arc<Transportation<'_>>,
        device_file_write: Arc<Mutex<File>>,
        transportations: Arc<Mutex<HashMap<TransportationId, Arc<Transportation<'_>>>>>,
    ) {
        if let Err(e) = transportation.connect_remote().await {
            error!(">>>> Transportation {trans_id} fail connect to remote endpoint because of error: {e:?}");
            if let Err(destory_error) = Self::destroy_transportation(trans_id, transportation, device_file_write, transportations).await {
                error!(">>>> Transportation {trans_id} fail to destory a unconnected remote endpoint because of error: {destory_error:?}");
            };
            return;
        };
        loop {
            // Poll remote endpoint and forward remote data to device endpoint.
            let remote_io_ready = match transportation.poll_remote_endpoint().await {
                Ok(remote_io_ready) => remote_io_ready,
                Err(e) => {
                    error!("<<<< Transportation {trans_id} fail to poll remote endpoint because of error: {e:?}");
                    break;
                }
            };
            match Self::handle_remote_io(
                trans_id,
                Arc::clone(&transportation),
                remote_io_ready,
                Arc::clone(&device_file_write),
                Arc::clone(&transportations),
            )
            .await
            {
                Ok(continue_loop) => {
                    if !continue_loop {
                        break;
                    }
                }
                Err(e) => {
                    error!("<<<< Fail to handle remote io because of error: {e:?}");
                    break;
                }
            }
        }
    }
    async fn get_or_create_transportation(&mut self, data: &[u8]) -> Option<TransportationId> {
        let trans_id = TransportationId::new(data)?;
        let transportations_owned = self.transportations.clone();
        let mut transportations = self.transportations.lock().await;
        match transportations.entry(trans_id) {
            Entry::Occupied(_) => Some(trans_id),
            Entry::Vacant(entry) => {
                debug!(">>>> Transportation {trans_id} not exist in repository create a new one.");
                let transportation = Arc::new(Transportation::new(trans_id)?);
                entry.insert(Arc::clone(&transportation));
                let device_file_write = Arc::clone(&self.device_file_write);
                tokio::spawn(Self::start_remote_io_loop(
                    trans_id,
                    transportation,
                    device_file_write,
                    transportations_owned,
                ));
                Some(trans_id)
            }
        }
    }

    async fn destroy_transportation(
        trans_id: TransportationId,
        transportation: Arc<Transportation<'_>>,
        device_file_write: Arc<Mutex<File>>,
        transportation_repository: Arc<Mutex<HashMap<TransportationId, Arc<Transportation<'_>>>>>,
    ) -> Result<(), NetworkError> {
        // Push any pending data back to device before destroying transportation.
        Self::write_to_device_endpoint(trans_id, transportation.clone()).await;
        if let Err(e) = Self::write_to_device_file(trans_id, transportation.clone(), device_file_write).await {
            error!("<<<< Transportation {trans_id} fail to write pending data in smoltcp to device when destory because of error: {e:?}");
            return Err(e);
        };
        transportation.close_device_endpoint().await;
        transportation.close_remote_endpoint().await?;
        let mut transportation_repository = transportation_repository.lock().await;
        transportation_repository.remove(&trans_id);
        Ok(())
    }

    async fn write_to_device_file(
        trans_id: TransportationId,
        transportation: Arc<Transportation<'_>>,
        device_file_write: Arc<Mutex<File>>,
    ) -> Result<(), NetworkError> {
        if transportation.poll_device_endpoint().await {
            while let Some(data_to_device) = transportation.pop_tx_from_device().await {
                let log = log_ip_packet(&data_to_device);
                debug!("<<<< Transportation {trans_id} write the tx to device:\n{log}\n",);
                let mut device_file_write = device_file_write.lock().await;
                device_file_write
                    .write_all(&data_to_device)
                    .map_err(NetworkError::WriteToDevice)?;
            }
        };
        Ok(())
    }

    /// Handle the remote io event, if return false means stop the loop, if return true means continue the loop
    async fn handle_remote_io(
        trans_id: TransportationId,
        transportation: Arc<Transportation<'_>>,
        ready: Ready,
        device_file_write: Arc<Mutex<File>>,
        transportations: Arc<Mutex<HashMap<TransportationId, Arc<Transportation<'_>>>>>,
    ) -> Result<bool, NetworkError> {
        if ready.is_readable() {
            Self::read_from_remote_endpoint(
                trans_id,
                transportation.clone(),
                device_file_write.clone(),
                transportations.clone(),
            )
            .await?;
            Self::write_to_device_endpoint(trans_id, transportation.clone()).await;
            Self::write_to_device_file(trans_id, transportation.clone(), device_file_write.clone()).await?;
        }
        if ready.is_writable() {
            Self::read_from_device_endpoint(trans_id, transportation.clone()).await;
            Self::write_to_remote_endpoint(trans_id, transportation.clone()).await;
        }
        if ready.is_read_closed() || ready.is_write_closed() {
            Self::destroy_transportation(trans_id, transportation, device_file_write, transportations).await?;
            return Ok(false);
        }
        Ok(true)
    }

    async fn read_from_remote_endpoint(
        trans_id: TransportationId,
        transportation: Arc<Transportation<'_>>,
        device_file_write: Arc<Mutex<File>>,
        transportation_repository: Arc<Mutex<HashMap<TransportationId, Arc<Transportation<'_>>>>>,
    ) -> Result<(), NetworkError> {
        let is_transportation_closed = match transportation.read_from_remote_endpoint().await {
            Ok((remote_data, is_closed)) => {
                for data in remote_data {
                    debug!(
                        "<<<< Transportation {trans_id} read data from remote endpoint: {}",
                        pretty_hex::pretty_hex(&data)
                    );
                    if !data.is_empty() {
                        transportation.push_data_to_remote_buffer(&data).await
                    }
                }
                is_closed
            }
            Err(error) => match error {
                NetworkError::WouldBlock => false,
                NetworkError::Closed => true,
                other => {
                    error!(
                        "<<<< Transportation {trans_id} failed to read from remote endpoint, errro={:?}",
                        other
                    );
                    true
                }
            },
        };
        if is_transportation_closed {
            Self::destroy_transportation(
                trans_id,
                transportation,
                device_file_write,
                transportation_repository,
            )
            .await?;
        }
        debug!("<<<< Transportation {trans_id} finished read from remote endpoint.",);
        Ok(())
    }

    async fn write_to_remote_endpoint(trans_id: TransportationId, transportation: Arc<Transportation<'_>>) {
        if let Err(e) = transportation.transfer_remote_buffer().await {
            error!(">>>> Transportation {trans_id} fail to transfer remote buffer to remote endpoint because of error: {e:?}");
        };
    }

    async fn read_from_device_endpoint(trans_id: TransportationId, transportation: Arc<Transportation<'_>>) {
        let mut data: [u8; 65535] = [0; 65535];

        while transportation.device_endpoint_can_receive().await {
            match transportation.read_from_device_endpoint(&mut data).await {
                Ok(data_len) => {
                    transportation
                        .push_data_to_device_buffer(&data[..data_len])
                        .await;
                }
                Err(error) => {
                    error!(">>>> Transportation {trans_id} fail to read device data because of error: {error:?}");
                    break;
                }
            }
        }
    }

    async fn write_to_device_endpoint(trans_id: TransportationId, transportation: Arc<Transportation<'_>>) {
        if transportation.device_endpoint_can_send().await {
            transportation.transfer_device_buffer().await;
        }
    }
}
