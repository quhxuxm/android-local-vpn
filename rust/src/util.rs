use anyhow::Result;
use ppaass_common::{PpaassMessagePayloadEncryptionSelector, RsaCrypto, RsaCryptoFetcher, RsaError};
// use smoltcp::wire::{Ipv4Packet, Ipv6Packet, PrettyPrinter};

// pub(crate) fn log_ip_packet(data: &[u8]) -> String {
//     match Ipv4Packet::new_checked(&data) {
//         Ok(ipv4_packet) => PrettyPrinter::print(&ipv4_packet).to_string(),
//         Err(ipv4_err) => match Ipv6Packet::new_checked(&data) {
//             Ok(ipv6_packet) => PrettyPrinter::print(&ipv6_packet).to_string(),
//             Err(ipv6_err) => format!("{ipv4_err:?} || {ipv6_err:?}"),
//         },
//     }
// }

#[derive(Debug)]
pub(crate) struct AgentRsaCryptoFetcher {
    rsa_crypto: RsaCrypto,
}

impl AgentRsaCryptoFetcher {
    pub(crate) fn new(private_key: Vec<u8>, public_key: Vec<u8>) -> Result<Self> {
        let rsa_crypto = RsaCrypto::new(public_key.as_slice(), private_key.as_slice())?;
        Ok(Self { rsa_crypto })
    }
}

impl RsaCryptoFetcher for AgentRsaCryptoFetcher {
    fn fetch(&self, _user_token: impl AsRef<str>) -> Result<Option<&RsaCrypto>, RsaError> {
        let rsa_crypto = &self.rsa_crypto;
        Ok(Some(rsa_crypto))
    }
}

pub(crate) struct AgentPpaassMessagePayloadEncryptionSelector;

impl PpaassMessagePayloadEncryptionSelector for AgentPpaassMessagePayloadEncryptionSelector {}
