use crate::{
    error::{AgentError, ServerError},
    processor::TransportationProcessor,
};
use log::{debug, error};

use tokio::{
    runtime::{Builder as TokioRuntimeBuilder, Runtime as TokioRuntime},
    sync::oneshot::{channel, Sender},
    task::JoinHandle,
};

#[derive(Debug)]
pub struct PpaassVpnServer {
    file_descriptor: i32,
    stop_sender: Option<Sender<bool>>,
    _runtime: Option<TokioRuntime>,
    processor_handle: Option<JoinHandle<Result<(), AgentError>>>,
}

impl PpaassVpnServer {
    pub fn new(file_descriptor: i32) -> Self {
        Self {
            file_descriptor,
            stop_sender: None,
            _runtime: None,
            processor_handle: None,
        }
    }

    fn init_runtime() -> Result<TokioRuntime, AgentError> {
        let mut runtime_builder = TokioRuntimeBuilder::new_multi_thread();
        runtime_builder.worker_threads(128);
        runtime_builder.enable_all();
        runtime_builder.thread_name("PPAASS");
        let runtime = runtime_builder
            .build()
            .map_err(|e| ServerError::Initialize(Box::new(e)))?;
        Ok(runtime)
    }

    pub fn start(&mut self) -> Result<(), AgentError> {
        debug!("Ppaass vpn server starting");
        let runtime = Self::init_runtime()?;
        let file_descriptor = self.file_descriptor;
        let (stop_sender, stop_receiver) = channel::<bool>();
        let processor_handle = runtime.spawn(async move {
            let mut processor = TransportationProcessor::new(file_descriptor, stop_receiver)?;
            debug!("Ppaass vpn server processor thread started.");
            if let Err(e) = processor.run().await {
                error!("Error happen when process transportation: {e:?}")
            };
            debug!("Ppaass vpn server processor thread complete.");
            Ok::<(), AgentError>(())
        });
        self._runtime = Some(runtime);
        self.stop_sender = Some(stop_sender);
        self.processor_handle = Some(processor_handle);
        debug!("Ppaass vpn server started");
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), AgentError> {
        debug!("Stop ppaass vpn server");
        self._runtime.take();
        let stop_sender = self.stop_sender.take().unwrap();
        stop_sender.send(true).map_err(|_| {
            error!("Fail to stop ppaass vpn server");
            ServerError::FailToStopProcessor
        })?;
        let processor_handle = self
            .processor_handle
            .take()
            .ok_or(ServerError::ProcessorHandleNotExist)?;
        processor_handle.abort();
        Ok(())
    }
}
