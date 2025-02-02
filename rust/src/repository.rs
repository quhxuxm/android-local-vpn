use std::collections::{hash_map::Entry, HashMap};

use crate::{
    config::PpaassVpnServerConfig,
    error::{TcpTransportRepositoryError, TransportError},
    transport::{ClientOutputPacket, TcpTransport, TransportId},
    util::AgentRsaCryptoFetcher,
};
use bytes::BytesMut;

use log::{debug, error};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

pub(crate) enum TcpTransportsRepoCmd {
    ClientData {
        transport_id: TransportId,
        client_data: BytesMut,
        create_on_not_exist: bool,
    },
    Remove(TransportId),
}
pub(crate) struct TcpTransportsRepository {
    repo_cmd_tx: UnboundedSender<TcpTransportsRepoCmd>,
}

impl TcpTransportsRepository {
    pub(crate) fn new(
        client_output_tx: mpsc::UnboundedSender<ClientOutputPacket>,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        vpn_server_config: &'static PpaassVpnServerConfig,
    ) -> Self {
        let (repo_cmd_tx, repo_cmd_rx) =
            mpsc::unbounded_channel::<TcpTransportsRepoCmd>();
        {
            let repo_cmd_tx = repo_cmd_tx.clone();
            tokio::spawn(async move {
                if let Err(e) = Self::handle_cmd(
                    repo_cmd_tx,
                    repo_cmd_rx,
                    client_output_tx,
                    agent_rsa_crypto_fetcher,
                    vpn_server_config,
                )
                .await
                {
                    error!("#### Transport repository fail to handle command because of error: {e:?}");
                };
            });
        }
        Self { repo_cmd_tx }
    }

    pub(crate) async fn send_repo_cmd(
        &self,
        cmd: TcpTransportsRepoCmd,
    ) -> Result<(), TransportError> {
        self.repo_cmd_tx
            .send(cmd)
            .map_err(|e| TcpTransportRepositoryError::CommandTxError(e).into())
    }

    async fn handle_cmd(
        repo_cmd_tx: UnboundedSender<TcpTransportsRepoCmd>,
        mut repo_cmd_rx: UnboundedReceiver<TcpTransportsRepoCmd>,
        client_output_tx: mpsc::UnboundedSender<ClientOutputPacket>,
        agent_rsa_crypto_fetcher: &'static AgentRsaCryptoFetcher,
        vpn_server_config: &'static PpaassVpnServerConfig,
    ) -> Result<(), TcpTransportRepositoryError> {
        let mut concrete_repository: HashMap<
            TransportId,
            UnboundedSender<BytesMut>,
        > = Default::default();
        while let Some(cmd) = repo_cmd_rx.recv().await {
            match cmd {
                TcpTransportsRepoCmd::ClientData {
                    transport_id,
                    client_data,
                    create_on_not_exist,
                } => {
                    if let Entry::Occupied(entry) =
                        concrete_repository.entry(transport_id)
                    {
                        entry.get().send(client_data)?;
                        continue;
                    }
                    if !create_on_not_exist {
                        error!("#### Transport {transport_id} incoming client input packet is not a valid handshake.");
                        continue;
                    }

                    let (transport, client_input_tx) = TcpTransport::new(
                        transport_id,
                        client_output_tx.clone(),
                    );
                    concrete_repository.insert(transport_id, client_input_tx);
                    let repo_cmd_tx = repo_cmd_tx.clone();
                    tokio::spawn(async move {
                        debug!("###### Transport {transport_id} begin to handle tcp packet.");
                        if let Err(e) = transport
                            .exec(
                                agent_rsa_crypto_fetcher,
                                vpn_server_config,
                                repo_cmd_tx,
                            )
                            .await
                        {
                            error!("###### Transport {transport_id} fail to handle tcp packet because of error: {e:?}");
                            return;
                        };
                        debug!("###### Transport {transport_id} complete to handle tcp packet.");
                    });
                }
                TcpTransportsRepoCmd::Remove(transport_id) => {
                    concrete_repository.remove(&transport_id);
                }
            }
        }
        Ok(())
    }
}
