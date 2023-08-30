use anyhow::Result;
use jni::objects::JValue;
use log::trace;
use ppaass_common::{
    PpaassMessagePayloadEncryptionSelector, RsaCrypto, RsaCryptoFetcher,
    RsaError,
};

use crate::{error::RemoteEndpointError, transport::TransportId};
use crate::{JAVA_VPN_JVM, JAVA_VPN_SERVICE_OBJ};

pub(crate) fn protect_socket(
    transport_id: TransportId,
    socket_fd: i32,
) -> Result<(), RemoteEndpointError> {
    trace!("Begin to protect outbound socket: {socket_fd}");
    let socket_fd_jni_arg = JValue::Int(socket_fd);
    let java_vpn_service_obj = unsafe {
        JAVA_VPN_SERVICE_OBJ.get_mut().ok_or(
            RemoteEndpointError::ProtectRemoteSocket {
                transport_id,
                socket_fd,
                message: "Can not get vpn service java object".to_string(),
            },
        )?
    }
    .as_obj();
    let java_vm = unsafe {
        JAVA_VPN_JVM.get_mut().ok_or(
            RemoteEndpointError::ProtectRemoteSocket {
                transport_id,
                socket_fd,
                message: "Can not get JVM instance.".to_string(),
            },
        )?
    };
    let mut jni_env =
        java_vm.attach_current_thread_permanently().map_err(|e| {
            RemoteEndpointError::ProtectRemoteSocket {
                transport_id,
                socket_fd,
                message: format!(
                "Can not attach current java thread because of error: {e:?}"
            ),
            }
        })?;
    let protect_result = jni_env
        .call_method(
            java_vpn_service_obj,
            "protect",
            "(I)Z",
            &[socket_fd_jni_arg],
        )
        .map_err(|e| RemoteEndpointError::ProtectRemoteSocket {
            transport_id,
            socket_fd,
            message: format!("Fail to invoke protect socket java method because of error: {e:?}"),
        })?;
    let protect_success =
        protect_result
            .z()
            .map_err(|e| RemoteEndpointError::ProtectRemoteSocket {
                transport_id,
                socket_fd,
                message: format!(
                    "Fail to get protect socket java method result because of error: {e:?}"
                ),
            })?;
    if !protect_success {
        return Err(RemoteEndpointError::ProtectRemoteSocket {
            transport_id,
            socket_fd,
            message: "Protect socket java method return false".to_string(),
        });
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct AgentRsaCryptoFetcher {
    rsa_crypto: RsaCrypto,
}

impl AgentRsaCryptoFetcher {
    pub(crate) fn new(
        private_key: Vec<u8>,
        public_key: Vec<u8>,
    ) -> Result<Self> {
        let rsa_crypto =
            RsaCrypto::new(public_key.as_slice(), private_key.as_slice())?;
        Ok(Self { rsa_crypto })
    }
}

impl RsaCryptoFetcher for AgentRsaCryptoFetcher {
    fn fetch(
        &self,
        _user_token: impl AsRef<str>,
    ) -> Result<Option<&RsaCrypto>, RsaError> {
        let rsa_crypto = &self.rsa_crypto;
        Ok(Some(rsa_crypto))
    }
}

pub(crate) struct AgentPpaassMessagePayloadEncryptionSelector;

impl PpaassMessagePayloadEncryptionSelector
    for AgentPpaassMessagePayloadEncryptionSelector
{
}
