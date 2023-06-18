use smoltcp::socket::tcp::{RecvError as TcpRecvError, SendError as TcpSendError};
use smoltcp::socket::udp::{RecvError as UdpRecvError, SendError as UdpSendError};
use std::{error::Error, io::Error as StdIoError};
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
    #[error("Fail to write data to remote because of error: {0:?}")]
    WriteToRemoteEndpoint(#[source] StdIoError),
    #[error("Fail to read data from remote because of error: {0:?}")]
    ReadFromRemoteEndpoint(#[source] StdIoError),
    #[error("Fail to write data to device file because of error: {0:?}")]
    WriteToDeviceFile(#[source] StdIoError),
    #[error("Fail to write data to device.")]
    WriteToDeviceEndpoint,
    #[error("Fail to read data from device.")]
    ReadFromDeviceEndpoint,
    #[error("Fail to poll remote endpoint because of error: {0:?}")]
    PollRemoteEndpoint(#[source] StdIoError),
    #[error("Fail to poll device endpoint because of error: {0:?}")]
    PollDeviceEndpoint(#[source] StdIoError),
    #[error("Fail to create device endpoint")]
    DeviceEndpointCreation,
    #[error("Fail to create remote endpoint")]
    RemoteEndpointCreation,
    #[error("Would block")]
    WouldBlock,
    #[error("Fail to close remote endpoint")]
    RemoteEndpointClosed,
    #[error("Fail to close device endpoint")]
    DeviceEndpointClose,
    #[error("Remote endpoint in invalid state")]
    RemoteEndpointInInvalidState,
}

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("Fail to initialize server because of error: {0:?}")]
    Initialize(Box<dyn Error + Send>),
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
