use smoltcp::socket::tcp::{RecvError as TcpRecvError, SendError as TcpSendError};
use smoltcp::socket::udp::{RecvError as UdpRecvError, SendError as UdpSendError};
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
    InitializePoll(#[source] StdIoError),
    #[error("Fail to initialize waker because of error: {0:?}")]
    InitializeWaker(#[source] StdIoError),
    #[error("Fail to register source because of error: {0:?}")]
    RegisterSource(#[source] StdIoError),
    #[error("Fail to poll source because of error: {0:?}")]
    PollSource(#[source] StdIoError),
    #[error("Fail to send tcp data to device because of error: {0:?}")]
    SendTcpDataToDevice(TcpSendError),
    #[error("Fail to send udp data to device because of error: {0:?}")]
    SendUdpDataToDevice(UdpSendError),
    #[error("Fail to receive tcp data from device because of error: {0:?}")]
    ReceiveTcpDataFromDevice(TcpRecvError),
    #[error("Fail to receive udp data from device because of error: {0:?}")]
    ReceiveUdpDataFromDevice(UdpRecvError),
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
