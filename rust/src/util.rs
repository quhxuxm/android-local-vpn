use smoltcp::wire::{Ipv4Packet, Ipv6Packet, PrettyPrinter};

pub(crate) fn log_ip_packet(data: &[u8]) -> String {
    match Ipv4Packet::new_checked(&data) {
        Ok(ipv4_packet) => PrettyPrinter::print(&ipv4_packet).to_string(),
        Err(ipv4_err) => match Ipv6Packet::new_checked(&data) {
            Ok(ipv6_packet) => PrettyPrinter::print(&ipv6_packet).to_string(),
            Err(ipv6_err) => format!("{ipv4_err:?} || {ipv6_err:?}"),
        },
    }
}
