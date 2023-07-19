use std::{
    collections::{hash_map::Entry, HashMap},
    fs::File,
    io::{ErrorKind, Read},
    os::fd::FromRawFd,
    sync::Arc,
};

use crate::transport::{Transport, TransportId};
use log::{debug, error};

use tokio::{
    runtime::{Builder as TokioRuntimeBuilder, Runtime as TokioRuntime},
    sync::{
        mpsc::error::TryRecvError,
        oneshot::{channel, Sender},
        Mutex,
    },
    task::JoinHandle,
};

use anyhow::Result;
use anyhow::{anyhow, Error as AnyhowError};

#[derive(Debug)]
pub struct PpaassVpnServer<'buf> {
    file_descriptor: i32,
    stop_sender: Option<Sender<bool>>,
    _runtime: Option<TokioRuntime>,
    processor_handle: Option<JoinHandle<Result<()>>>,
    transports: Arc<Mutex<HashMap<TransportId, Arc<Transport>>>>,
}

impl PpaassVpnServer<'_> {
    pub fn new(file_descriptor: i32) -> Self {
        Self {
            file_descriptor,
            stop_sender: None,
            _runtime: None,
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

    pub fn start(&mut self) -> Result<()> {
        debug!("Ppaass vpn server starting ...");
        let runtime = Self::init_runtime()?;
        let file_descriptor = self.file_descriptor;
        let client_file = unsafe { File::from_raw_fd(file_descriptor) };
        let client_file_read = Arc::new(Mutex::new(client_file));
        let client_file_write = client_file_read.clone();
        let (stop_sender, stop_receiver) = channel::<bool>();
        self._runtime = Some(runtime);
        self.stop_sender = Some(stop_sender);
        let processor_handle = runtime.spawn(async move {
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
                let mut transports = self.transports.lock().await;
                match transports.entry(transport_id) {
                    Entry::Occupied(entry) => {
                        entry.get().feed_client_data(client_data).await;
                    }
                    Entry::Vacant(entry) => {
                        let transport = entry.insert(Arc::new(Transport::new(transport_id, client_file_write)));
                        tokio::spawn(transport.start());
                    }
                }
            }
            Ok::<(), AnyhowError>(())
        });
        self.processor_handle = Some(processor_handle);
        debug!("Ppaass vpn server started");
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        debug!("Stop ppaass vpn server");
        self._runtime.take();
        let stop_sender = self.stop_sender.take().unwrap();
        stop_sender
            .send(true)
            .map_err(|_| anyhow!("Fail to send stop request to ppaass vpn server."))?;
        let processor_handle = self
            .processor_handle
            .take()
            .ok_or(anyhow!("Fail to take processor handler."))?;
        processor_handle.abort();
        Ok(())
    }
}
