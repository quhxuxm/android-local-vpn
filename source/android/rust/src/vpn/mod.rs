mod buffers;
mod mio_socket;
mod processor;
mod session;
mod session_info;
mod smoltcp_socket;
mod utils;
mod vpn_device;

use mio::Waker;
use processor::Processor;
use std::thread;

#[derive(Debug)]
pub struct Vpn {
    file_descriptor: i32,
    stop_waker: Option<Waker>,
    thread_join_handle: Option<thread::JoinHandle<()>>,
}

impl Vpn {
    pub fn new(file_descriptor: i32) -> Self {
        Self {
            file_descriptor,
            stop_waker: None,
            thread_join_handle: None,
        }
    }

    pub fn start(&mut self) {
        let mut processor = Processor::new(self.file_descriptor);
        self.stop_waker = Some(processor.new_stop_waker());

        self.thread_join_handle = Some(thread::spawn(move || processor.run()));
    }

    pub fn stop(&mut self) {
        self.stop_waker.as_ref().unwrap().wake().unwrap();
        self.thread_join_handle.take().unwrap().join().unwrap();
    }
}
