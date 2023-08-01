use std::fs::File;

use std::{
    collections::{hash_map::Entry, HashMap},
    io::{ErrorKind, Read},
    os::fd::FromRawFd,
    sync::Arc,
};

use anyhow::Result;
use anyhow::{anyhow, Error as AnyhowError};
use log::{debug, error, info, trace};
use tokio::sync::mpsc::Receiver;
use tokio::task::yield_now;
use tokio::{
    runtime::{Builder as TokioRuntimeBuilder, Runtime as TokioRuntime},
    sync::{
        mpsc::error::TryRecvError,
        mpsc::{channel, Sender},
        Mutex,
    },
    task::JoinHandle,
};

use crate::config::PpaassVpnServerConfig;
use crate::{
    transport::{Transport, TransportId},
    util::AgentRsaCryptoFetcher,
};

#[derive(Debug)]
pub(crate) struct PpaassVpnServer {
    config: &'static PpaassVpnServerConfig,
    file_descriptor: i32,
    stop_sender: Option<Sender<bool>>,
    runtime: Option<TokioRuntime>,
    processor_handle: Option<JoinHandle<Result<()>>>,
    transports: Arc<Mutex<HashMap<TransportId, Sender<Vec<u8>>>>>,
}

impl PpaassVpnServer {
    pub(crate) fn new(file_descriptor: i32, config: &'static PpaassVpnServerConfig) -> Self {
        Self {
            config,
            file_descriptor,
            stop_sender: None,
            runtime: None,
            processor_handle: None,
            transports: Default::default(),
        }
    }

    fn init_async_runtime(config: &PpaassVpnServerConfig) -> Result<TokioRuntime> {
        let mut runtime_builder = TokioRuntimeBuilder::new_multi_thread();
        runtime_builder.worker_threads(config.get_thread_number());
        runtime_builder.enable_all();
        runtime_builder.thread_name("PPAASS");
        let runtime = runtime_builder.build()?;
        Ok(runtime)
    }

    pub(crate) fn start(&mut self, agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher) -> Result<()> {
        debug!("Ppaass vpn server starting ...");
        let runtime = Self::init_async_runtime(self.config)?;
        let file_descriptor = self.file_descriptor;

        let client_file = Arc::new(Mutex::new(unsafe { File::from_raw_fd(file_descriptor) }));
        let (client_file_read, client_file_write) = (Arc::clone(&client_file), client_file);
        let (stop_sender, stop_receiver) = channel::<bool>(1);
        self.stop_sender = Some(stop_sender);
        let transports = Arc::clone(&self.transports);
        let ppaass_server_config = self.config;

        let processor_handle = runtime.spawn(async move {
            info!("Begin to handle client file data.");
            Self::start_handle_client_data(
                client_file_read,
                stop_receiver,
                transports,
                client_file_write,
                agent_rsa_crypto_fetcher,
                ppaass_server_config,
            )
            .await;
            Ok::<(), AnyhowError>(())
        });
        self.runtime = Some(runtime);
        self.processor_handle = Some(processor_handle);
        info!("Ppaass vpn server started");
        Ok(())
    }

    async fn start_handle_client_data(
        client_file_read: Arc<Mutex<File>>,
        mut stop_receiver: Receiver<bool>,
        transports: Arc<Mutex<HashMap<TransportId, Sender<Vec<u8>>>>>,
        client_file_write: Arc<Mutex<File>>,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
    ) {
        let mut client_file_read_buffer = [0u8; 65536];
        loop {
            match stop_receiver.try_recv() {
                Ok(true) => break,
                Ok(false) => error!("Receive a unexpected stop flag."),
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {
                    trace!("No stop flag.")
                }
            }
            let client_data = {
                let mut client_file_read = client_file_read.lock().await;
                match client_file_read.read(&mut client_file_read_buffer) {
                    Ok(0) => {
                        error!("Nothing to read from client file break the loop.");
                        break;
                    }
                    Ok(size) => &client_file_read_buffer[..size],
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        // sleep(Duration::from_millis(50)).await;
                        yield_now().await;
                        continue;
                    }
                    Err(e) => {
                        error!("Fail to read client file data because of error: {e:?}");
                        break;
                    }
                }
            };

            let transport_id = match TransportId::new(client_data) {
                Ok(transport_id) => transport_id,
                Err(e) => {
                    error!(">>>> Fail to parse transport id because of error: {e:?}");
                    continue;
                }
            };

            let mut transports_lock = transports.lock().await;
            let connection_number = transports_lock.len();
            match transports_lock.entry(transport_id) {
                Entry::Occupied(entry) => {
                    debug!(">>>> Found existing transport {transport_id}.");
                    let transport_client_data_sender = entry.get();
                    if transport_client_data_sender
                        .send(client_data.to_vec())
                        .await
                        .is_err()
                    {
                        error!("Transport {transport_id} closed already, can not send client data to transport");

                        let transport_client_data_sender = entry.remove();
                        transport_client_data_sender.closed().await;

                        info!("Transport {transport_id} removed from vpn server(error quite), current connection number in vpn server: {}", transports_lock.len());
                    };
                }
                Entry::Vacant(entry) => {
                    info!(
                        ">>>> Create new transport {transport_id}, current connection number in vpn server: {connection_number}"
                    );
                    let (transport, transport_client_data_sender) =
                        Transport::new(transport_id, Arc::clone(&client_file_write));
                    let transports = Arc::clone(&transports);
                    tokio::spawn(async move {
                        if let Err(e) = transport
                            .exec(agent_rsa_crypto_fetcher, config, Arc::clone(&transports))
                            .await
                        {
                            error!(">>>> Transport {transport_id} fail to start because of error: {e:?}");
                        };
                    });
                    if transport_client_data_sender
                        .send(client_data.to_vec())
                        .await
                        .is_err()
                    {
                        error!("Transport {transport_id} closed already, can not send client data to transport");
                        continue;
                    };
                    entry.insert(transport_client_data_sender);
                }
            }
        }
        error!("****** Server quite because of unknown reason ****** ");
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
