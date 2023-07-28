mod device;
mod util;
mod values;

mod server;
mod transport;

use crate::{server::PpaassVpnServer, util::AgentRsaCryptoFetcher};
use android_logger::Config;
use anyhow::{anyhow, Result};
use jni::{
    objects::{GlobalRef, JByteArray, JValue},
    JNIEnv, JavaVM,
};
use jni::{
    objects::{JClass, JObject},
    sys::jint,
};
use log::{debug, error, trace, LevelFilter};

use std::{cell::OnceCell, process};

pub(crate) static mut JAVA_VPN_SERVICE_OBJ: OnceCell<GlobalRef> = OnceCell::new();
pub(crate) static mut JAVA_VPN_JVM: OnceCell<JavaVM> = OnceCell::new();
pub(crate) static mut VPN_SERVER: OnceCell<PpaassVpnServer> = OnceCell::new();

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[no_mangle]
pub unsafe extern "C" fn Java_com_ppaass_agent_vpn_LocalVpnService_onStopVpn(_jni_env: JNIEnv, _class: JClass) {
    JAVA_VPN_JVM.take();
    JAVA_VPN_SERVICE_OBJ.take();
    VPN_SERVER
        .get_mut()
        .expect("Fail to get ppaass vpn server.")
        .stop()
        .unwrap();
}

/// # Safety
///
/// This function should not be called before the horsemen are ready.
#[no_mangle]
pub unsafe extern "C" fn Java_com_ppaass_agent_vpn_LocalVpnService_onStartVpn(
    jni_env: JNIEnv<'static>,
    _class: JClass<'static>,
    vpn_tun_device_fd: jint,
    vpn_service: JObject<'static>,
    agent_private_key: JByteArray,
    proxy_public_key: JByteArray,
) {
    android_logger::init_once(
        Config::default()
            .with_tag("PPAASS-VPN-RUST")
            .with_max_level(LevelFilter::Error),
    );
    std::panic::set_hook(Box::new(|panic_info| {
        error!("*** PANIC [{:?}]", panic_info);
    }));
    let agent_private_key = jni_env
        .convert_byte_array(agent_private_key)
        .expect("Fail to read agent private key bytes");
    debug!(
        "Agent private key: {}",
        pretty_hex::pretty_hex(&agent_private_key)
    );
    let proxy_public_key = jni_env
        .convert_byte_array(proxy_public_key)
        .expect("Fail to read proxy public key bytes");
    debug!(
        "Proxy public key: {}",
        pretty_hex::pretty_hex(&proxy_public_key)
    );
    let agent_crypto_fetcher = AgentRsaCryptoFetcher::new(agent_private_key, proxy_public_key).expect("Fail to create agent rsa crypto fetcher");
    let java_vm = jni_env
        .get_java_vm()
        .expect("Fail to get jvm from jni enviorment.");
    JAVA_VPN_JVM
        .set(java_vm)
        .expect("Fail to save jvm to global reference.");
    JAVA_VPN_SERVICE_OBJ
        .set(
            jni_env
                .new_global_ref(vpn_service)
                .expect("Fail to generate java vpn service object globale reference"),
        )
        .expect("Fail to save java vpn service object to global reference");
    debug!(
        "onStartVpn, pid={}, fd={}",
        process::id(),
        vpn_tun_device_fd
    );
    let vpn_server = PpaassVpnServer::new(vpn_tun_device_fd, agent_crypto_fetcher);
    VPN_SERVER
        .set(vpn_server)
        .expect("Fail to generate ppaass vpn server.");
    VPN_SERVER
        .get_mut()
        .expect("Fail to get ppaass vpn server.")
        .start()
        .unwrap();
}

pub(crate) fn protect_socket(socket_fd: i32) -> Result<(), anyhow::Error> {
    trace!("Begin to protect outbound socket: {socket_fd}");
    let socket_fd_jni_arg = JValue::Int(socket_fd);
    let java_vpn_service_obj = unsafe { JAVA_VPN_SERVICE_OBJ.get_mut() }
        .expect("Fail to get java vpn service object from global ref")
        .as_obj();

    let java_vm = unsafe { JAVA_VPN_JVM.get_mut() }.expect("Fail to get jvm from global");

    let mut jni_env = java_vm
        .attach_current_thread_permanently()
        .expect("Fail to attach jni env to current thread");
    let protect_result = jni_env.call_method(
        java_vpn_service_obj,
        "protect",
        "(I)Z",
        &[socket_fd_jni_arg],
    )?;
    let protect_success = protect_result.z()?;
    if !protect_success {
        error!("Fail to convert protect socket result because of return false");
        return Err(anyhow!(
            "Fail to convert protect socket result because of return false"
        ));
    }
    Ok(())
}
