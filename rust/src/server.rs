use std::{fs::File, io::Write, sync::Arc};

use std::{
    io::{ErrorKind, Read},
    os::fd::FromRawFd,
};

use anyhow::Result;
use bytes::BytesMut;
use log::{debug, error, info, trace};

use tokio::task::yield_now;
use tokio::{
    runtime::{Builder as TokioRuntimeBuilder, Runtime as TokioRuntime},
    sync::{
        mpsc::{self, UnboundedSender},
        oneshot::{
            channel as OneshotChannel, Receiver as OneshotReceiver,
            Sender as OneshotSender,
        },
    },
};

use crate::util::AgentRsaCryptoFetcher;
use crate::{
    config,
    repository::{TcpTransportsRepoCmd, TcpTransportsRepository},
};
use crate::{
    config::PpaassVpnServerConfig,
    transport::{
        ClientInputIpPacket, ClientInputParser, ClientInputTransportPacket,
        ClientOutputPacket, ControlProtocol, UdpTransport,
    },
};

#[derive(Debug)]
pub(crate) struct PpaassVpnServer {
    config: &'static PpaassVpnServerConfig,
    file_descriptor: i32,
    stop_signal_tx: Option<OneshotSender<bool>>,
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
            stop_signal_tx: Default::default(),
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
            mpsc::unbounded_channel::<ClientOutputPacket>();

        let ppaass_server_config = self.config;
        let (stop_signal_tx, stop_signal_rx) = OneshotChannel();
        self.stop_signal_tx = Some(stop_signal_tx);
        runtime.spawn(async move {
            while let Some(client_output_packet) = client_output_rx.recv().await
            {
                trace!(
                    "<<<< Transport {} write data to client file.",
                    client_output_packet.transport_id
                );
               if let Err(e)= client_file_write.write_all(&client_output_packet.data){
                   error!("<<<< Transport {} fail to write data to client file because of error: {e:?}", client_output_packet.transport_id);
               };
               if let Err(e) = client_file_write.flush(){
                   error!("<<<< Transport {} fail to flush data to client file because of error: {e:?}", client_output_packet.transport_id);
               }
            }
        });
        runtime.spawn(async move {
            info!("Begin to handle client file data.");
            Self::start_handle_client_rx(
                client_file_read,
                stop_signal_rx,
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
        mut stop_signal_rx: OneshotReceiver<bool>,
        client_output_tx: UnboundedSender<ClientOutputPacket>,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        config: &'static PpaassVpnServerConfig,
    ) {
        let tcp_transports_repo = Arc::new(TcpTransportsRepository::new(
            client_output_tx.clone(),
            agent_rsa_crypto_fetcher,
            config,
        ));
        let mut client_rx_buffer = [0u8; config::MTU];
        loop {
            if let Ok(true) = stop_signal_rx.try_recv() {
                info!("Stop ppaass vpn server.");
                return;
            }

            let client_data = match client_file_read.read(&mut client_rx_buffer)
            {
                Ok(0) => {
                    error!("Nothing to read from client file, stop ppaass vpn server.");
                    break;
                }
                Ok(size) => BytesMut::from(&client_rx_buffer[..size]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    yield_now().await;
                    continue;
                }
                Err(e) => {
                    error!("Fail to read client file data because of error, stop ppaass vpn server: {e:?}");
                    break;
                }
            };

            let (transport_id, create_on_not_exist) =
                match ClientInputParser::parse(&client_data) {
                    Ok((
                        transport_id,
                        ClientInputIpPacket::Ipv4(
                            ClientInputTransportPacket::Tcp(tcp_packet),
                        ),
                    )) => (transport_id, tcp_packet.syn()),
                    Ok((
                        transport_id,
                        ClientInputIpPacket::Ipv4(
                            ClientInputTransportPacket::Udp(_),
                        ),
                    )) => (transport_id, true),
                    Ok((
                        transport_id,
                        ClientInputIpPacket::Ipv6(
                            ClientInputTransportPacket::Tcp(tcp_packet),
                        ),
                    )) => (transport_id, tcp_packet.syn()),
                    Ok((
                        transport_id,
                        ClientInputIpPacket::Ipv6(
                            ClientInputTransportPacket::Udp(_),
                        ),
                    )) => (transport_id, true),
                    Err(e) => {
                        error!(">>>> Fail to parse transport id from ip packet because of error: {e:?}");
                        continue;
                    }
                };

            match transport_id.control_protocol {
                ControlProtocol::Tcp => {
                    if let Err(e) = tcp_transports_repo
                        .send_repo_cmd(TcpTransportsRepoCmd::ClientData {
                            transport_id,
                            client_data,
                            create_on_not_exist,
                        })
                        .await
                    {
                        error!(">>>> Transport {transport_id} fail to send repository command because of error: {e:?}")
                    };
                }
                ControlProtocol::Udp => {
                    let udp_transport = UdpTransport::new(
                        transport_id,
                        client_output_tx.clone(),
                    );
                    tokio::spawn(async move {
                        debug!("###### Transport {transport_id} begin to handle udp packet.");
                        if let Err(e) = udp_transport
                            .exec(agent_rsa_crypto_fetcher, config, client_data)
                            .await
                        {
                            error!("###### Transport {transport_id} fail to handle udp packet because of error: {e:?}");
                            return;
                        };
                        debug!("###### Transport {transport_id} complete to handle udp packet.");
                    });
                }
            }
        }
    }

    pub(crate) fn stop(&mut self) -> Result<()> {
        debug!("Stop ppaass vpn server");
        if let Some(stop_signal_tx) = self.stop_signal_tx.take() {
            if let Err(e) = stop_signal_tx.send(true) {
                error!("Fail to stop ppaass vpn server because of error: {e:?}")
            };
        }
        Ok(())
    }
}
