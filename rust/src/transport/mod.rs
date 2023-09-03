mod client;
mod remote;
mod tcp;
mod udp;
mod value;

use log::trace;

use crate::error::{ClientEndpointError, RemoteEndpointError};

pub(crate) use self::tcp::TcpTransport;
pub(crate) use self::udp::UdpTransport;
pub(crate) use self::value::ClientInputIpPacket;
pub(crate) use self::value::ClientInputParser;
pub(crate) use self::value::ClientInputTransportPacket;
pub(crate) use self::value::ClientOutputPacket;
pub(crate) use self::value::ControlProtocol;
pub(crate) use self::value::TransportId;
pub(crate) use self::value::Transports;
use self::{client::ClientEndpoint, remote::RemoteEndpoint};

/// The concrete function to forward client receive buffer to remote.
/// * transport_id: The transportation id.
/// * data: The data going to send to remote.
/// * remote_endpoint: The remote endpoint.
async fn consume_client_recv_buf_fn(
    transport_id: TransportId,
    data: Vec<u8>,
    remote_endpoint: &RemoteEndpoint,
) -> Result<usize, RemoteEndpointError> {
    trace!(
        ">>>> Transport {transport_id} write data to remote: {}",
        pretty_hex::pretty_hex(&data)
    );
    remote_endpoint.write_to_remote(data).await
}

/// The concrete function to forward remote receive buffer to client.
/// * transport_id: The transportation id.
/// * data: The data going to send to remote.
/// * client: The client endpoint.
async fn consume_remote_recv_buf_fn(
    transport_id: TransportId,
    data: Vec<u8>,
    client_endpoint: &ClientEndpoint<'_>,
) -> Result<usize, ClientEndpointError> {
    trace!(
        ">>>> Transport {transport_id} write data to smoltcp: {}",
        pretty_hex::pretty_hex(&data)
    );
    client_endpoint.send_to_smoltcp(data).await
}
