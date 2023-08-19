use std::{fs::File, io::Write, sync::Arc};

use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::hash_map::Entry,
    io::{ErrorKind, Read},
    os::fd::FromRawFd,
};

use anyhow::Error as AnyhowError;
use anyhow::Result;
use log::{debug, error, info};

use tokio::{
    runtime::{Builder as TokioRuntimeBuilder, Runtime as TokioRuntime},
    sync::mpsc::Sender,
};
use tokio::{sync::mpsc::channel, task::yield_now};

use crate::{config::PpaassVpnServerConfig, transport::Transports, util::ClientOutputPacket};
use crate::{
    transport::{Transport, TransportId},
    util::AgentRsaCryptoFetcher,
};

#[derive(Debug)]
pub(crate) struct PpaassVpnServer {
    config: &'static PpaassVpnServerConfig,
    file_descriptor: i32,
    closed: Arc<AtomicBool>,
    runtime: Option<TokioRuntime>,
    transports: Transports,
}

impl PpaassVpnServer {
    pub(crate) fn new(file_descriptor: i32, config: &'static PpaassVpnServerConfig) -> Self {
        Self {
            config,
            file_descriptor,
            closed: Arc::new(AtomicBool::new(false)),
            runtime: None,
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

    pub(crate) fn start(
        &mut self,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
    ) -> Result<()> {
        debug!("Ppaass vpn server starting ...");
        let runtime = Self::init_async_runtime(self.config)?;
        let file_descriptor = self.file_descriptor;
        let client_file_read = unsafe { File::from_raw_fd(file_descriptor) };
        let mut client_file_write = unsafe { File::from_raw_fd(file_descriptor) };
        let (client_output_tx, mut client_output_rx) = channel::<ClientOutputPacket>(1024);
        let transports = Arc::clone(&self.transports);
        let ppaass_server_config = self.config;
        let closed = Arc::clone(&self.closed);
        runtime.spawn(async move {
            info!("Begin to handle client file data.");
            Self::start_handle_client_data(
                client_file_read,
                closed,
                transports,
                client_output_tx,
                agent_rsa_crypto_fetcher,
                ppaass_server_config,
            )
            .await;
            Ok::<(), AnyhowError>(())
        });
        runtime.spawn(async move {
            while let Some(client_outpu_packet) = client_output_rx.recv().await {
                let transport_id = client_outpu_packet.transport_id;
                if let Err(e) = client_file_write.write_all(&client_outpu_packet.data) {
                    error!("<<<< Transport {transport_id} fail to write client output packet because of error: {e:?}");
                };
            }
        });

        self.runtime = Some(runtime);

        info!("Ppaass vpn server started");
        Ok(())
    }

    async fn start_handle_client_data(
        mut client_file_read: File,
        closed: Arc<AtomicBool>,
        transports: Transports,
        client_output_tx: Sender<ClientOutputPacket>,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
    ) {
        let mut client_file_read_buffer = [0u8; 65536];
        loop {
            if closed.load(Ordering::Relaxed) {
                break;
            }
            let client_data = match client_file_read.read(&mut client_file_read_buffer) {
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
                        entry.remove();
                        info!("Transport {transport_id} removed from vpn server(error quite), current connection number in vpn server: {}", transports_lock.len());
                    };
                }
                Entry::Vacant(entry) => {
                    info!(
                        ">>>> Create new transport {transport_id}, current connection number in vpn server: {connection_number}"
                    );
                    let (transport, client_input_tx) =
                        Transport::new(transport_id, client_output_tx.clone());
                    let transports = Arc::clone(&transports);
                    tokio::spawn(async move {
                        if let Err(e) = transport
                            .exec(agent_rsa_crypto_fetcher, config, Arc::clone(&transports))
                            .await
                        {
                            error!(">>>> Transport {transport_id} fail to start because of error: {e:?}");
                        };
                    });
                    if client_input_tx.send(client_data.to_vec()).await.is_err() {
                        error!("Transport {transport_id} closed already, can not send client data to transport");
                        continue;
                    };
                    entry.insert(client_input_tx);
                }
            }
        }
        error!("****** Server quite because of unknown reason ****** ");
    }

    pub(crate) fn stop(&mut self) -> Result<()> {
        debug!("Stop ppaass vpn server");
        if let Some(runtime) = self.runtime.take() {
            runtime.block_on(async {
                self.closed.swap(true, Ordering::Relaxed);
            });
        };
        Ok(())
    }
}
