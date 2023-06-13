use crate::error::{AgentError, NetworkError, ServerError};

use super::transportation::Transportation;
use super::transportation::TransportationId;
use log::{debug, error};
use mio::event::Event;
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token, Waker};

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fs::File;
use std::io::ErrorKind;
use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;

const EVENTS_CAPACITY: usize = 1024;

const TOKEN_DEVICE: Token = Token(0);
const TOKEN_WAKER: Token = Token(1);
const TOKEN_START_ID: usize = 2;

pub(crate) struct TransportationProcessor<'buf> {
    device_file_descriptor: i32,
    device_file: File,
    poll: Poll,
    transportations: HashMap<TransportationId, Transportation<'buf>>,
    tokens_to_transportations: HashMap<Token, TransportationId>,
    next_token_id: usize,
}

impl<'buf> TransportationProcessor<'buf> {
    pub(crate) fn new(device_file_descriptor: i32) -> Result<Self, AgentError> {
        let poll = Poll::new().map_err(NetworkError::InitializePoll)?;
        Ok(TransportationProcessor {
            device_file_descriptor,
            device_file: unsafe { File::from_raw_fd(device_file_descriptor) },
            poll,
            transportations: HashMap::new(),
            tokens_to_transportations: HashMap::new(),
            next_token_id: TOKEN_START_ID,
        })
    }

    pub(crate) fn new_stop_waker(&self) -> Result<Waker, AgentError> {
        let waker = Waker::new(self.poll.registry(), TOKEN_WAKER).map_err(NetworkError::InitializeWaker)?;
        Ok(waker)
    }

    pub(crate) fn run(&mut self) -> Result<(), AgentError> {
        let registry = self.poll.registry();
        registry
            .register(
                &mut SourceFd(&self.device_file_descriptor),
                TOKEN_DEVICE,
                Interest::READABLE,
            )
            .map_err(NetworkError::RegisterSource)?;

        let mut events = Events::with_capacity(EVENTS_CAPACITY);

        'device_file_poll_loop: loop {
            self.poll
                .poll(&mut events, None)
                .map_err(NetworkError::PollSource)?;
            for event in events.iter() {
                if event.token() == TOKEN_DEVICE {
                    if let Err(e) = self.handle_device_file_io_event(event) {
                        error!("Fail to handle device envent because of error: {e:?}");
                    };
                } else if event.token() == TOKEN_WAKER {
                    break 'device_file_poll_loop;
                } else {
                    self.handle_remote_io_event(event);
                }
            }
        }
        Ok(())
    }

    fn get_or_create_transportation(&mut self, data: &[u8]) -> Option<TransportationId> {
        let trans_id = TransportationId::new(data)?;
        match self.transportations.entry(trans_id) {
            Entry::Occupied(_) => Some(trans_id),
            Entry::Vacant(entry) => {
                let token = Token(self.next_token_id);
                let transportation = Transportation::new(trans_id, &mut self.poll, token)?;
                self.tokens_to_transportations.insert(token, trans_id);
                self.next_token_id += 1;
                entry.insert(transportation);
                Some(trans_id)
            }
        }
    }

    fn destroy_transportation(&mut self, trans_id: TransportationId) {
        // push any pending data back to tun device before destroying session.
        self.write_to_smoltcp(trans_id);
        self.write_to_device_file(trans_id);

        if let Some(transportation) = self.transportations.get_mut(&trans_id) {
            transportation.close_device_endpoint();
            transportation.close_remote_endpoint(&mut self.poll);
            self.tokens_to_transportations
                .remove(&transportation.get_token());
            self.transportations.remove(&trans_id);
        }
    }

    fn handle_device_file_io_event(&mut self, event: &Event) -> Result<(), AgentError> {
        if !event.is_readable() {
            return Ok(());
        }
        let mut buffer: [u8; 65535] = [0; 65535];
        loop {
            match self.device_file.read(&mut buffer) {
                Ok(0) => {
                    break Ok(());
                }
                Ok(count) => {
                    let buffer = &buffer[..count];
                    if let Some(trans_id) = self.get_or_create_transportation(buffer) {
                        let transportation = self
                            .transportations
                            .get_mut(&trans_id)
                            .ok_or(ServerError::TransportationNotExist(trans_id))?;
                        transportation.push_rx_to_device(buffer.to_vec());
                        self.write_to_device_file(trans_id);
                        self.read_from_smoltcp(trans_id);
                        self.write_to_remote(trans_id);
                    }
                }
                Err(_) => {
                    break Ok(());
                }
            }
        }
    }

    fn write_to_device_file(&mut self, trans_id: TransportationId) {
        if let Some(transportation) = self.transportations.get_mut(&trans_id) {
            transportation.poll_device_endpoint();
            while let Some(data_to_device) = transportation.pop_tx_from_device() {
                self.device_file.write_all(&data_to_device[..]).unwrap();
            }
        }
    }

    fn handle_remote_io_event(&mut self, event: &Event) {
        if let Some(trans_id) = self.tokens_to_transportations.get(&event.token()) {
            let trans_id = *trans_id;
            if event.is_readable() {
                debug!("<<<< Transportation {trans_id} is readable.");
                self.read_from_remote(trans_id);
                self.write_to_smoltcp(trans_id);
                self.write_to_device_file(trans_id);
            }
            if event.is_writable() {
                debug!("<<<< Transportation {trans_id} is writable.");
                self.read_from_smoltcp(trans_id);
                self.write_to_remote(trans_id);
            }
            if event.is_read_closed() || event.is_write_closed() {
                debug!("<<<< Transportation {trans_id} is read/write closed.");
                self.destroy_transportation(trans_id);
            }
        }
    }

    fn read_from_remote(&mut self, trans_id: TransportationId) {
        if let Some(transportation) = self.transportations.get_mut(&trans_id) {
            debug!("<<<< Transportation {trans_id} read from remote.");

            let is_session_closed = match transportation.read_from_remote_endpoint() {
                Ok((read_seqs, is_closed)) => {
                    for bytes in read_seqs {
                        if !bytes.is_empty() {
                            transportation.push_remote_data_to_buffer(&bytes[..])
                        }
                    }
                    is_closed
                }
                Err(error) => {
                    if error.kind() == ErrorKind::WouldBlock {
                        false
                    } else if error.kind() == ErrorKind::ConnectionReset {
                        true
                    } else {
                        log::error!("failed to read from tcp stream, errro={:?}", error);
                        true
                    }
                }
            };
            if is_session_closed {
                self.destroy_transportation(trans_id);
            }

            log::trace!("finished read from server, session={:?}", trans_id);
        }
    }

    fn write_to_remote(&mut self, trans_id: TransportationId) {
        if let Some(transportation) = self.transportations.get_mut(&trans_id) {
            transportation.consume_remote_buffer();
        }
    }

    fn read_from_smoltcp(&mut self, trans_id: TransportationId) {
        if let Some(transportation) = self.transportations.get_mut(&trans_id) {
            let mut data: [u8; 65535] = [0; 65535];
            loop {
                if !transportation.device_endpoint_can_receive() {
                    break;
                }
                match transportation.read_from_device_endpoint(&mut data) {
                    Ok(data_len) => transportation.push_client_data_to_buffer(&data[..data_len]),
                    Err(error) => {
                        break;
                    }
                }
            }
        }
    }

    fn write_to_smoltcp(&mut self, trans_id: TransportationId) {
        if let Some(transportation) = self.transportations.get_mut(&trans_id) {
            log::trace!("write to smoltcp, session={:?}", trans_id);

            if transportation.device_endpoint_can_send() {
                transportation.consume_device_buffer();
            }
        }
    }
}
