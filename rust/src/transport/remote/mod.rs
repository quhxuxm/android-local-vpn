mod tcp;
mod udp;

use futures_util::stream::{SplitSink, SplitStream};
use tokio::net::TcpStream;

use crate::util::AgentRsaCryptoFetcher;

use super::TransportId;
use ppaass_common::{proxy::PpaassProxyConnection, PpaassAgentMessage};
pub(crate) use tcp::RemoteTcpEndpoint;
pub(crate) use udp::RemoteUdpEndpoint;

type ProxyConnectionWrite = SplitSink<
    PpaassProxyConnection<
        'static,
        TcpStream,
        AgentRsaCryptoFetcher,
        TransportId,
    >,
    PpaassAgentMessage,
>;

type ProxyConnectionRead = SplitStream<
    PpaassProxyConnection<
        'static,
        TcpStream,
        AgentRsaCryptoFetcher,
        TransportId,
    >,
>;
