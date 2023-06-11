use crate::{
    error::{AgentError, ServerError},
    processor::TransportationProcessor,
};
use log::{debug, error};
use mio::Waker;
use std::thread;

#[derive(Debug)]
pub struct PpaassVpnServer {
    file_descriptor: i32,
    stop_waker: Option<Waker>,
    processor_handle: Option<thread::JoinHandle<()>>,
}

impl PpaassVpnServer {
    pub fn new(file_descriptor: i32) -> Self {
        Self {
            file_descriptor,
            stop_waker: None,
            processor_handle: None,
        }
    }

    pub fn start(&mut self) -> Result<(), AgentError> {
        debug!("Ppaass vpn server starting");
        let mut processor = TransportationProcessor::new(self.file_descriptor)?;
        self.stop_waker = Some(processor.new_stop_waker()?);
        self.processor_handle = Some(thread::spawn(move || {
            debug!("Ppaass vpn server processor thread started.");
            processor.run();
            debug!("Ppaass vpn server processor thread complete.");
        }));
        debug!("Ppaass vpn server started");
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), AgentError> {
        debug!("Stop ppaass vpn server");
        let stop_waker = self
            .stop_waker
            .as_ref()
            .ok_or(ServerError::StopWakerNotExist)?;
        stop_waker
            .wake()
            .map_err(ServerError::FailToWakeupProcessor)?;
        let processor_handle = self
            .processor_handle
            .take()
            .ok_or(ServerError::ProcessorHandleNotExist)?;
        processor_handle.join().map_err(|e| {
            error!("Fail to stop processor because of error: {e:?}");
            ServerError::FailToStopProcessor
        })?;
        Ok(())
    }
}
