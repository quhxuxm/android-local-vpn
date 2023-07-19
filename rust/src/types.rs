use crate::transport::{Transport, TransportId};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) type TransportationsRepository<'buf> = Arc<Mutex<HashMap<TransportId, Arc<Transport<'buf>>>>>;
