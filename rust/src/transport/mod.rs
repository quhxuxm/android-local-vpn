mod common;
mod value;

use std::{fs::File, sync::Arc};

use tokio::sync::Mutex;

pub(crate) use self::value::ControlProtocol;

pub(crate) use self::value::TransportId;

#[derive(Debug)]
pub(crate) struct Transport {
    transport_id: TransportId,
    client_file_write: Arc<Mutex<File>>,
}

impl Transport {
    pub(crate) fn new(transport_id: TransportId, client_file_write: Arc<Mutex<File>>) -> Self {
        Self {
            transport_id,
            client_file_write,
        }
    }

    pub(crate) async fn start(&self) {}

    pub(crate) async fn feed_client_data(&self, data: &[u8]) {}
}
