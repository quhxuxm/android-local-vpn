mod config;
mod device;
mod error;
mod server;
mod transport;
mod util;
mod values;

use crate::{config::PpaassVpnServerConfig, server::PpaassVpnServer, util::AgentRsaCryptoFetcher};
use android_logger::Config as AndroidLoggerConfig;

use jni::{
    objects::{GlobalRef, JByteArray},
    JNIEnv, JavaVM,
};
use jni::{
    objects::{JClass, JObject},
    sys::jint,
};
use log::{error, info, LevelFilter};

use std::{cell::OnceCell, process};

pub(crate) static mut JAVA_VPN_SERVICE_OBJ: OnceCell<GlobalRef> = OnceCell::new();
pub(crate) static mut JAVA_VPN_JVM: OnceCell<JavaVM> = OnceCell::new();
pub(crate) static mut VPN_SERVER: OnceCell<PpaassVpnServer> = OnceCell::new();
pub(crate) static mut AGENT_RSA_CRYPTO_FETCHER: OnceCell<AgentRsaCryptoFetcher> = OnceCell::new();
pub(crate) static mut VPN_SERVER_CONFIG: OnceCell<PpaassVpnServerConfig> = OnceCell::new();

/// Stop the vpn server, the method will stop the vpn server thread first,
/// then destory the Java VPN Service object, finally destory the JVM object.
///
/// # Safety
///
/// This function should not be called before the horsemen are ready.
///
#[no_mangle]
pub unsafe extern "C" fn Java_com_ppaass_agent_vpn_LocalVpnService_onStopVpn(_jni_env: JNIEnv, _class: JClass) {
    info!("Begin to stop VPN ...");
    info!("Stopping vpn server ...");
    let ppaass_vpn_server = VPN_SERVER.get_mut().expect("Fail to get vpn server object");
    if let Err(e) = ppaass_vpn_server.stop() {
        error!("Fail to stop vpn server because of error: {e:?}")
    };
    info!("Success to stop vpn server ...");
    info!("Destorying java vpn service object ...");
    JAVA_VPN_SERVICE_OBJ.take();
    info!("Success to destory java vpn service object ...");
    info!("Destorying jvm object ...");
    JAVA_VPN_JVM.take();
    info!("Success to destory jvm object ...");
    info!("Success to stop VPN.");
}

/// Start the vpn server
///
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
    let ppaass_vpn_server_config = PpaassVpnServerConfig::new();
    let android_logger_config = AndroidLoggerConfig::default()
        .with_max_level(LevelFilter::Error)
        .with_tag("PPAASS-VPN-RUST");
    android_logger::init_once(android_logger_config);
    std::panic::set_hook(Box::new(|panic_info| {
        error!("*** Panic happen on ppaass vpn server:\n{:?}", panic_info);
    }));
    let agent_private_key = jni_env
        .convert_byte_array(agent_private_key)
        .expect("Fail to read agent private key bytes from java object");
    let proxy_public_key = jni_env
        .convert_byte_array(proxy_public_key)
        .expect("Fail to read proxy public key bytes from java object");

    let agent_crypto_fetcher = match AgentRsaCryptoFetcher::new(agent_private_key, proxy_public_key) {
        Ok(agent_crypto_fetcher) => agent_crypto_fetcher,
        Err(e) => {
            error!("Fail to generate agent rsa crypto fetcher because of error: {e:?}");
            return;
        }
    };
    AGENT_RSA_CRYPTO_FETCHER
        .set(agent_crypto_fetcher)
        .expect("Fail to set agent rsa crypto fetcher to global");
    let java_vm = jni_env
        .get_java_vm()
        .expect("Fail to get jvm from jni enviorment.");
    JAVA_VPN_JVM
        .set(java_vm)
        .expect("Fail to set jvm to global.");
    let java_vpn_service_obj_ref = jni_env
        .new_global_ref(vpn_service)
        .expect("Fail to generate java vpn service object reference");
    JAVA_VPN_SERVICE_OBJ
        .set(java_vpn_service_obj_ref)
        .expect("Fail to set java vpn service object to global.");
    VPN_SERVER_CONFIG
        .set(ppaass_vpn_server_config)
        .expect("Fail to set ppaass vpn server config to global.");
    info!(
        "Beging to start ppaass vpn server, java process id: [{}], vpn tun device fd: [{}] ...",
        process::id(),
        vpn_tun_device_fd
    );
    info!("Begin to create ppaass vpn server ...");
    let ppaass_vpn_server = PpaassVpnServer::new(
        vpn_tun_device_fd,
        VPN_SERVER_CONFIG
            .get()
            .expect("Fail to get ppaass vpn server config from global"),
    );
    info!("Success to create ppaass vpn server ...");
    VPN_SERVER
        .set(ppaass_vpn_server)
        .expect("Fail to set ppaass vpn server to global.");
    info!("Begin to start ppaass vpn server ...");
    if let Err(e) = VPN_SERVER
        .get_mut()
        .expect("Fail to get ppaass vpn server from global.")
        .start(
            AGENT_RSA_CRYPTO_FETCHER
                .get()
                .expect("Fail to get agent rsa crypto fetcher from global"),
        )
    {
        error!("Fail to start ppaass vpn server because of error: {e:?}");
        return;
    }
    info!("Success to start ppaass vpn server.");
}
