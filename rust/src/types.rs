use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::transportation::{Transportation, TransportationId};

pub(crate) type TransportationsRepository<'buf>=Arc<Mutex<HashMap<TransportationId, Arc<Transportation<'buf>>>>>;