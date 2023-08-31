use std::{fs::File, io::Write, sync::Arc};

use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    io::{ErrorKind, Read},
    os::fd::FromRawFd,
};

use anyhow::Result;
use log::{debug, error, info, trace};

use tokio::{
    runtime::{Builder as TokioRuntimeBuilder, Runtime as TokioRuntime},
    sync::{mpsc::Sender, Mutex},
};
use tokio::{sync::mpsc::channel, task::yield_now};

use crate::{
    config::PpaassVpnServerConfig,
    transport::{ClientOutputPacket, Transports},
};
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
}

impl PpaassVpnServer {
    pub(crate) fn new(
        file_descriptor: i32,
        config: &'static PpaassVpnServerConfig,
    ) -> Self {
        Self {
            config,
            file_descriptor,
            closed: Arc::new(AtomicBool::new(false)),
            runtime: None,
        }
    }

    fn init_async_runtime(
        config: &PpaassVpnServerConfig,
    ) -> Result<TokioRuntime> {
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
        let mut client_file_write =
            unsafe { File::from_raw_fd(file_descriptor) };
        let (client_output_tx, mut client_output_rx) =
            channel::<ClientOutputPacket>(1024);

        let ppaass_server_config = self.config;
        let closed = Arc::clone(&self.closed);

        runtime.spawn(async move {
            while let Some(client_output_packet) = client_output_rx.recv().await
            {
                trace!(
                    "<<<< Transport {} write data to client file.",
                    client_output_packet.transport_id
                );
               if let Err(e)= client_file_write.write_all(&client_output_packet.data){
                error!("<<<< Transport {} fail to write data to client file because of error: {e:?}", client_output_packet.transport_id)
               };
            }
        });
        runtime.spawn(async move {
            info!("Begin to handle client file data.");
            Self::start_handle_client_rx(
                client_file_read,
                closed,
                client_output_tx,
                agent_rsa_crypto_fetcher,
                ppaass_server_config,
            )
            .await;
        });

        self.runtime = Some(runtime);

        info!("Ppaass vpn server started");
        Ok(())
    }

    async fn start_handle_client_rx(
        mut client_file_read: File,
        closed: Arc<AtomicBool>,
        client_output_tx: Sender<ClientOutputPacket>,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
    ) {
        let transports = Arc::new(Mutex::new(Transports::default()));
        let (remove_transports_tx, mut remove_transports_rx) =
            channel::<TransportId>(1024);
        let mut client_rx_buffer = [0u8; 65536];
        {
            let transports = Arc::clone(&transports);
            tokio::spawn(async move {
                while let Some(transport_id) = remove_transports_rx.recv().await
                {
                    let mut transports = transports.lock().await;
                    transports.remove(&transport_id);
                    info!("###### Remove transport {transport_id}, current transport number: {}", transports.len())
                }
            });
        }
        loop {
            if closed.load(Ordering::Relaxed) {
                info!("Close vpn server, going to quite.");
                break;
            }
            let client_data = match client_file_read.read(&mut client_rx_buffer)
            {
                Ok(0) => {
                    error!("Nothing to read from client file break the loop.");
                    break;
                }
                Ok(size) => &client_rx_buffer[..size],
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    // sleep(Duration::from_millis(50)).await;
                    yield_now().await;
                    continue;
                }
                Err(e) => {
                    error!(
                        "Fail to read client file data because of error: {e:?}"
                    );
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
            let client_input_tx= transports.entry(transport_id).or_insert_with(||{
                    let (transport, client_input_tx) =
                        Transport::new(transport_id, client_output_tx.clone());
                    let remove_transports_tx=remove_transports_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = transport
                            .exec(agent_rsa_crypto_fetcher, config, remove_transports_tx)
                            .await
                        {
                            error!(">>>> Transport {transport_id} fail to start because of error: {e:?}");
                        };
                    });
                    client_input_tx
            });
            if let Err(e) = client_input_tx.send(client_data.to_vec()).await {
                error!("Transport {transport_id} client input receiver closed already, can not send client data to transport, error: {e:?}");
                transports.remove(&transport_id);
            };
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
