use log::trace;
use smoltcp::time::Instant;
use smoltcp::{
    phy::{self, Device, DeviceCapabilities, Medium},
    wire::{Ipv4Packet, PrettyPrinter},
};

use std::collections::VecDeque;

use crate::transport::TransportId;

#[derive(Debug)]
pub(crate) struct SmoltcpDevice {
    trans_id: TransportId,
    rx_queue: VecDeque<Vec<u8>>,
    tx_queue: VecDeque<Vec<u8>>,
}

impl SmoltcpDevice {
    pub(crate) fn new(trans_id: TransportId) -> SmoltcpDevice {
        SmoltcpDevice {
            trans_id,
            rx_queue: VecDeque::with_capacity(65536),
            tx_queue: VecDeque::with_capacity(65536),
        }
    }

    pub(crate) fn push_rx(&mut self, bytes: Vec<u8>) {
        self.rx_queue.push_back(bytes);
    }

    pub(crate) fn pop_tx(&mut self) -> Option<Vec<u8>> {
        self.tx_queue.pop_front()
    }

    pub(crate) fn destory(&mut self) {
        self.rx_queue.clear();
        self.tx_queue.clear();
    }
}

impl Device for SmoltcpDevice {
    type RxToken<'a> = RxToken where Self: 'a;
    type TxToken<'a> = TxToken<'a> where Self: 'a;

    fn receive(
        &mut self,
        _timestamp: Instant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.rx_queue.pop_front().map(move |buffer| {
            let rx = RxToken {
                trans_id: self.trans_id,
                buffer,
            };
            let tx = TxToken {
                trans_id: self.trans_id,
                queue: &mut self.tx_queue,
            };
            (rx, tx)
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken {
            trans_id: self.trans_id,
            queue: &mut self.tx_queue,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut default = DeviceCapabilities::default();
        default.max_transmission_unit = 65535;
        default.medium = Medium::Ip;
        default
    }
}

pub(crate) struct RxToken {
    trans_id: TransportId,
    buffer: Vec<u8>,
}

impl phy::RxToken for RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let result = f(&mut self.buffer);
        trace!(
            ">>>> Transportation {} vpn receive rx token from device:{}",
            self.trans_id,
            PrettyPrinter::<Ipv4Packet<&'static [u8]>>::new("", &self.buffer)
        );
        result
    }
}

pub(crate) struct TxToken<'a> {
    trans_id: TransportId,
    queue: &'a mut VecDeque<Vec<u8>>,
}

impl<'a> phy::TxToken for TxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0; len];
        let result = f(&mut buffer);
        trace!(
            "<<<< Transportation {} vpn send tx token to device:{}",
            self.trans_id,
            PrettyPrinter::<Ipv4Packet<&'static [u8]>>::new("", &buffer)
        );
        self.queue.push_back(buffer);
        result
    }
}
