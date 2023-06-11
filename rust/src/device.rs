use log::debug;
use smoltcp::time::Instant;
use smoltcp::{
    phy::{self, Device, DeviceCapabilities, Medium},
    wire::{Ipv4Packet, PrettyPrinter},
};

use std::collections::VecDeque;

#[derive(Debug)]
pub(crate) struct PpaassVpnDevice {
    rx_queue: VecDeque<Vec<u8>>,
    tx_queue: VecDeque<Vec<u8>>,
}

impl PpaassVpnDevice {
    pub(crate) fn new() -> PpaassVpnDevice {
        PpaassVpnDevice {
            rx_queue: VecDeque::new(),
            tx_queue: VecDeque::new(),
        }
    }

    pub(crate) fn push_rx(&mut self, bytes: Vec<u8>) {
        self.rx_queue.push_back(bytes);
    }

    pub(crate) fn pop_tx(&mut self) -> Option<Vec<u8>> {
        self.tx_queue.pop_front()
    }
}

impl Device for PpaassVpnDevice {
    type RxToken<'a> = RxToken where Self: 'a;
    type TxToken<'a> = TxToken<'a> where Self: 'a;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut default = DeviceCapabilities::default();
        default.max_transmission_unit = 65535;
        default.medium = Medium::Ip;
        default
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.rx_queue.pop_front().map(move |buffer| {
            let rx = RxToken { buffer };
            let tx = TxToken {
                queue: &mut self.tx_queue,
            };
            (rx, tx)
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken {
            queue: &mut self.tx_queue,
        })
    }
}

pub(crate) struct RxToken {
    buffer: Vec<u8>,
}

impl phy::RxToken for RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let result = f(&mut self.buffer);
        debug!(
            ">>>> Ppaass vpn receive token rx from device:{}",
            PrettyPrinter::<Ipv4Packet<&'static [u8]>>::new("", &self.buffer)
        );
        result
    }
}

pub(crate) struct TxToken<'a> {
    queue: &'a mut VecDeque<Vec<u8>>,
}

impl<'a> phy::TxToken for TxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0; len];
        let result = f(&mut buffer);
        debug!(
            "<<<< Ppaass vpn send tx token to device:{}",
            PrettyPrinter::<Ipv4Packet<&'static [u8]>>::new("", &buffer)
        );
        self.queue.push_back(buffer);
        result
    }
}
