use crate::transport::TransportId;
use derive_more::Constructor;

#[derive(Constructor)]
pub(crate) struct ClientFileTxPacket {
    pub transport_id: TransportId,
    pub data: Vec<u8>,
}
