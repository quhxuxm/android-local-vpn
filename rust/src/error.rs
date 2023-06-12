use std::io::Error as StdIoError;
use thiserror::Error;

use crate::transportation::TransportationId;
#[derive(Error, Debug)]
pub enum AgentError {
    #[error("Network error happen: {0:?}")]
    Network(#[from] NetworkError),
    #[error("Server error happen: {0:?}")]
    Server(#[from] ServerError),
}

#[derive(Error, Debug)]
pub enum NetworkError {
    #[error("Fail to initialize poll because of error: {0:?}")]
    FailToInitializePoll(#[source] StdIoError),
    #[error("Fail to initialize waker because of error: {0:?}")]
    FailToInitializeWaker(#[source] StdIoError),
    #[error("Fail to register source because of error: {0:?}")]
    FailToRegisterSource(#[source] StdIoError),
    #[error("Fail to poll source because of error: {0:?}")]
    FailToPollSource(#[source] StdIoError),
}

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("Stop waker not exist.")]
    StopWakerNotExist,
    #[error("Processor handle not exist.")]
    ProcessorHandleNotExist,
    #[error("Fail to wakeup processor because of error: {0:?}")]
    FailToWakeupProcessor(#[source] StdIoError),
    #[error("Fail to stop processor.")]
    FailToStopProcessor,
    #[error("Transportation not exist: {0:?}")]
    TransportationNotExist(TransportationId),
}

pub enum Parse {}
