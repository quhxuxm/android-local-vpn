mod core;

#[macro_use]
mod jni;

#[macro_use]
mod socket_protector;

#[macro_use]
pub mod android {
    use crate::jni::Jni;
    use crate::socket_protector::SocketProtector;

    use crate::core::tun;
    use android_logger::Config;
    use jni::objects::{JClass, JObject};
    use jni::JNIEnv;
    use log::LevelFilter;
    use std::process;

    /// # Safety
    ///
    /// This function should only be used in jni context.
    #[no_mangle]
    pub unsafe extern "C" fn Java_com_ppaass_agent_vpn_LocalVpnService_onCreateNative(
        env: JNIEnv,
        class: JClass,
        java_vpn_service: JObject,
    ) {
        android_logger::init_once(
            Config::default()
                .with_tag("nativeVpn")
                .with_max_level(LevelFilter::Trace),
        );
        log::trace!("onCreateNative");
        set_panic_handler();
        Jni::init(env, class, java_vpn_service);
        SocketProtector::init();
        log::trace!("create, pid={}", process::id());
    }

    /// # Safety
    ///
    /// This function should only be used in jni context.
    #[no_mangle]
    pub unsafe extern "C" fn Java_com_ppaass_agent_vpn_LocalVpnService_onDestroyNative(_: JNIEnv, _: JClass) {
        log::trace!("onDestroyNative");
        log::trace!("destroy, pid={}", process::id());
        SocketProtector::release();
        Jni::release();
        remove_panic_handler();
    }

    /// # Safety
    ///
    /// This function should only be used in jni context.
    #[no_mangle]
    pub unsafe extern "C" fn Java_com_ppaass_agent_vpn_LocalVpnService_onStartVpn(_: JNIEnv, _: JClass, file_descriptor: i32) {
        log::trace!("onStartVpn, pid={}, fd={}", process::id(), file_descriptor);
        socket_protector!().start();
        tun::start(file_descriptor);
    }

    /// # Safety
    ///
    /// This function should only be used in jni context.
    #[no_mangle]
    pub unsafe extern "C" fn Java_com_ppaass_agent_vpn_LocalVpnService_onStopVpn(_: JNIEnv, _: JClass) {
        log::trace!("onStopVpn, pid={}", process::id());
        tun::stop();
        socket_protector!().stop();
    }

    fn set_panic_handler() {
        std::panic::set_hook(Box::new(|panic_info| {
            log::error!("*** PANIC [{:?}]", panic_info);
        }));
    }

    fn remove_panic_handler() {
        let _ = std::panic::take_hook();
    }

    pub fn on_socket_created(socket: i32) {
        socket_protector!().protect_socket(socket);
    }
}
