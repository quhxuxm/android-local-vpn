use std::io::Write;
use std::{
    collections::{hash_map::Entry, HashMap},
    fs::File,
    io::{ErrorKind, Read},
    os::fd::FromRawFd,
    sync::Arc,
};

use crate::transport::{Transport, TransportId};
use log::{debug, error, info};

use tokio::{
    runtime::{Builder as TokioRuntimeBuilder, Runtime as TokioRuntime},
    sync::{
        mpsc::error::TryRecvError,
        mpsc::{channel, Sender},
        Mutex,
    },
    task::JoinHandle,
};

use crate::values::ClientFileTxPacket;
use anyhow::Result;
use anyhow::{anyhow, Error as AnyhowError};
use tokio::sync::mpsc::Receiver;

#[derive(Debug)]
pub(crate) struct PpaassVpnServer {
    file_descriptor: i32,
    stop_sender: Option<Sender<bool>>,
    runtime: Option<TokioRuntime>,
    processor_handle: Option<JoinHandle<Result<()>>>,
    transports: Arc<Mutex<HashMap<TransportId, Arc<Transport>>>>,
}

impl PpaassVpnServer {
    pub(crate) fn new(file_descriptor: i32) -> Self {
        Self {
            file_descriptor,
            stop_sender: None,
            runtime: None,
            processor_handle: None,
            transports: Default::default(),
        }
    }

    fn init_runtime() -> Result<TokioRuntime> {
        let mut runtime_builder = TokioRuntimeBuilder::new_multi_thread();
        runtime_builder.worker_threads(128);
        runtime_builder.enable_all();
        runtime_builder.thread_name("PPAASS");
        let runtime = runtime_builder.build()?;
        Ok(runtime)
    }

    pub(crate) fn start(&mut self) -> Result<()> {
        debug!("Ppaass vpn server starting ...");
        let runtime = Self::init_runtime()?;
        let file_descriptor = self.file_descriptor;
        let client_file = unsafe { File::from_raw_fd(file_descriptor) };
        let client_file_read = Arc::new(Mutex::new(client_file));
        let client_file_write = client_file_read.clone();
        let (stop_sender, stop_receiver) = channel::<bool>(1);

        self.stop_sender = Some(stop_sender);
        let transports = Arc::clone(&self.transports);
        let processor_handle = runtime.spawn(async move {
            let (client_file_tx_sender, mut client_file_tx_receiver) = channel::<ClientFileTxPacket>(1024);
            info!("Spawn client file write task.");
            tokio::spawn(async move {
                while let Some(ClientFileTxPacket { transport_id, data }) = client_file_tx_receiver.recv().await {
                    let mut client_file_write = client_file_write.lock().await;
                    if let Err(e) = client_file_write.write_all(&data) {
                        error!("<<<< Transport {transport_id} fail to write data to client because of error: {e:?}");
                    };
                }
            });
            info!("Begin to handle client file data.");
            Self::start_handle_client_data(
                client_file_read,
                stop_receiver,
                transports,
                client_file_tx_sender,
            )
            .await;
            Ok::<(), AnyhowError>(())
        });
        self.runtime = Some(runtime);
        self.processor_handle = Some(processor_handle);
        debug!("Ppaass vpn server started");
        Ok(())
    }

    async fn start_handle_client_data(
        client_file_read: Arc<Mutex<File>>,
        mut stop_receiver: Receiver<bool>,
        transports: Arc<Mutex<HashMap<TransportId, Arc<Transport>>>>,
        client_file_tx_sender: Sender<ClientFileTxPacket>,
    ) {
        let mut client_file_read_buffer = [0u8; 65536];
        loop {
            match stop_receiver.try_recv() {
                Ok(true) => break,
                Err(TryRecvError::Disconnected) => break,
                _ => {}
            }
            let client_data = match {
                let mut client_file_read = client_file_read.lock().await;
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

            let transport_id = match TransportId::new(client_data) {
                Ok(transport_id) => transport_id,
                Err(e) => {
                    error!(">>>> Fail to parse transport id because of error: {e:?}");
                    continue;
                }
            };
            let mut transports = transports.lock().await;
            match transports.entry(transport_id) {
                Entry::Occupied(entry) => {
                    debug!(">>>> Found existing transport {transport_id}.");
                    entry.get().feed_client_data(client_data).await;
                }
                Entry::Vacant(entry) => {
                    debug!(">>>> Create new transport {transport_id}.");
                    let transport = entry.insert(Arc::new(Transport::new(
                        transport_id,
                        client_file_tx_sender.clone(),
                    )));
                    let transport_clone = Arc::clone(transport);
                    tokio::spawn(async move {
                        if let Err(e) = transport_clone.start().await {
                            error!(">>>> Transport {transport_id} fail to start because of error: {e:?}");
                        };
                    });
                    transport.feed_client_data(client_data).await;
                }
            }
        }
    }

    pub(crate) fn stop(&mut self) -> Result<()> {
        debug!("Stop ppaass vpn server");
        if let Some(runtime) = self.runtime.take() {
            if let Some(stop_sender) = self.stop_sender.take() {
                runtime.spawn(async move {
                    if let Err(e) = stop_sender.send(true).await {
                        error!("Fail to send stop command to ppaass server because of error: {e:?}")
                    };
                });
            }
        }
        let processor_handle = self
            .processor_handle
            .take()
            .ok_or(anyhow!("Fail to take processor handler."))?;
        processor_handle.abort();
        Ok(())
    }
}
