use crate::error::{AgentError, NetworkError, ServerError};

use super::buffers::{IncomingDataEvent, IncomingDirection, OutgoingDirection, WriteError};
use super::session::Transportation;
use super::session_info::TransportationInfo;
use mio::event::Event;
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token, Waker};
use smoltcp::time::Instant;
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

pub(crate) struct TransportationProcessor<'sockets> {
    device_file_descriptor: i32,
    device_file: File,
    poll: Poll,
    transportations: HashMap<TransportationInfo, Transportation<'sockets>>,
    tokens_to_transportations: HashMap<Token, TransportationInfo>,
    next_token_id: usize,
}

impl<'sockets> TransportationProcessor<'sockets> {
    pub(crate) fn new(device_file_descriptor: i32) -> Result<Self, AgentError> {
        let poll = Poll::new().map_err(NetworkError::FailToInitializePoll)?;
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
        let waker = Waker::new(self.poll.registry(), TOKEN_WAKER).map_err(NetworkError::FailToInitializeWaker)?;
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
            .map_err(NetworkError::FailToRegisterSource)?;

        let mut events = Events::with_capacity(EVENTS_CAPACITY);

        'device_file_poll_loop: loop {
            self.poll
                .poll(&mut events, None)
                .map_err(NetworkError::FailToPollSource)?;
            for event in events.iter() {
                if event.token() == TOKEN_DEVICE {
                    self.handle_device_event(event);
                } else if event.token() == TOKEN_WAKER {
                    break 'device_file_poll_loop;
                } else {
                    self.handle_remote_event(event);
                }
            }
        }
        Ok(())
    }

    fn get_or_create_transportation(&mut self, bytes: &Vec<u8>) -> Option<TransportationInfo> {
        let transportation_info = TransportationInfo::new(bytes)?;
        match self.transportations.entry(transportation_info) {
            Entry::Occupied(_) => Some(transportation_info),
            Entry::Vacant(entry) => {
                let token = Token(self.next_token_id);
                let transportation = Transportation::new(&transportation_info, &mut self.poll, token)?;
                self.tokens_to_transportations
                    .insert(token, transportation_info);
                self.next_token_id += 1;
                entry.insert(transportation);
                Some(transportation_info)
            }
        }
    }

    fn destroy_transportation(&mut self, transportation_info: &TransportationInfo) {
        // push any pending data back to tun device before destroying session.
        self.write_to_smoltcp(transportation_info);
        self.write_to_device(transportation_info);

        if let Some(transportation) = self.transportations.get_mut(transportation_info) {
            let mut smoltcp_socket = transportation
                .smoltcp_socket
                .get(&mut transportation.socketset);
            smoltcp_socket.close();
            let mio_socket = &mut transportation.mio_socket;
            mio_socket.close();
            mio_socket.deregister_poll(&mut self.poll).unwrap();
            self.tokens_to_transportations.remove(&transportation.token);
            self.transportations.remove(transportation_info);
        }
    }

    fn handle_device_event(&mut self, event: &Event) -> Result<(), AgentError> {
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
                    let read_buffer = buffer[..count].to_vec();
                    if let Some(transportation_info) = self.get_or_create_transportation(&read_buffer) {
                        let transportation = self
                            .transportations
                            .get_mut(&transportation_info)
                            .ok_or(ServerError::TransportationNotExist(transportation_info))?;
                        transportation.vpn_device.push_rx(read_buffer);
                        self.write_to_device(&transportation_info);
                        self.read_from_smoltcp(&transportation_info);
                        self.write_to_remote(&transportation_info);
                    }
                }
                Err(_) => {
                    break Ok(());
                }
            }
        }
    }

    fn write_to_device(&mut self, transportation_info: &TransportationInfo) {
        if let Some(transportation) = self.transportations.get_mut(transportation_info) {
            transportation.interface.poll(
                Instant::now(),
                &mut transportation.vpn_device,
                &mut transportation.socketset,
            );

            while let Some(data_to_device) = transportation.vpn_device.pop_tx() {
                self.device_file.write_all(&data_to_device[..]).unwrap();
            }

            log::trace!("finished write to tun");
        }
    }

    fn handle_remote_event(&mut self, event: &Event) {
        if let Some(session_info) = self.tokens_to_transportations.get(&event.token()) {
            let session_info = *session_info;
            if event.is_readable() {
                log::trace!("handle server event read, session={:?}", session_info);

                self.read_from_remote(&session_info);
                self.write_to_smoltcp(&session_info);
                self.write_to_device(&session_info);

                log::trace!("finished server event read, session={:?}", session_info);
            }
            if event.is_writable() {
                log::trace!("handle server event write, session={:?}", session_info);

                self.read_from_smoltcp(&session_info);
                self.write_to_remote(&session_info);

                log::trace!("finished server event write, session={:?}", session_info);
            }
            if event.is_read_closed() || event.is_write_closed() {
                log::trace!("handle server event closed, session={:?}", session_info);

                self.destroy_transportation(&session_info);

                log::trace!("finished server event closed, session={:?}", session_info);
            }
        }
    }

    fn read_from_remote(&mut self, session_info: &TransportationInfo) {
        if let Some(session) = self.transportations.get_mut(session_info) {
            log::trace!("read from server, session={:?}", session_info);

            let is_session_closed = match session.mio_socket.read() {
                Ok((read_seqs, is_closed)) => {
                    for bytes in read_seqs {
                        if !bytes.is_empty() {
                            let event = IncomingDataEvent {
                                direction: IncomingDirection::FromServer,
                                buffer: &bytes[..],
                            };
                            session.buffers.push_data(event);
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
                self.destroy_transportation(session_info);
            }

            log::trace!("finished read from server, session={:?}", session_info);
        }
    }

    fn write_to_remote(&mut self, session_info: &TransportationInfo) {
        if let Some(session) = self.transportations.get_mut(session_info) {
            log::trace!("write to server, session={:?}", session_info);

            session
                .buffers
                .write_data(OutgoingDirection::ToServer, |b| {
                    session.mio_socket.write(b).map_err(WriteError::Stderr)
                });

            log::trace!("finished write to server, session={:?}", session_info);
        }
    }

    fn read_from_smoltcp(&mut self, session_info: &TransportationInfo) {
        if let Some(session) = self.transportations.get_mut(session_info) {
            log::trace!("read from smoltcp, session={:?}", session_info);

            let mut data: [u8; 65535] = [0; 65535];
            loop {
                let mut socket = session.smoltcp_socket.get(&mut session.socketset);
                if !socket.can_receive() {
                    break;
                }
                match socket.receive(&mut data) {
                    Ok(data_len) => {
                        let event = IncomingDataEvent {
                            direction: IncomingDirection::FromClient,
                            buffer: &data[..data_len],
                        };
                        session.buffers.push_data(event);
                    }
                    Err(error) => {
                        log::error!("failed to receive from smoltcp, error={:?}", error);
                        break;
                    }
                }
            }

            log::trace!("finished read from smoltcp, session={:?}", session_info);
        }
    }

    fn write_to_smoltcp(&mut self, session_info: &TransportationInfo) {
        if let Some(session) = self.transportations.get_mut(session_info) {
            log::trace!("write to smoltcp, session={:?}", session_info);

            let mut socket = session.smoltcp_socket.get(&mut session.socketset);
            if socket.can_send() {
                session
                    .buffers
                    .write_data(OutgoingDirection::ToClient, |b| socket.send(b));
            }

            log::trace!("finished write to smoltcp, session={:?}", session_info);
        }
    }
}
