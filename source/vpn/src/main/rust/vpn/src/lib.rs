mod vpn;

#[macro_use]
extern crate lazy_static;

#[allow(non_snake_case)]
pub mod android {
    extern crate android_logger;
    extern crate jni;
    extern crate log;

    use self::jni::objects::JClass;
    use self::jni::JNIEnv;

    use crate::vpn::vpn::Vpn;
    use android_logger::Config;
    use std::process;
    use std::sync::Mutex;

    lazy_static! {
        static ref VPN: Mutex<Option<Vpn>> = Mutex::new(None);
    }

    macro_rules! vpn {
        () => {{
            VPN.lock().unwrap().as_mut().unwrap()
        }};
    }

    #[no_mangle]
    pub unsafe extern "C" fn Java_com_github_jonforshort_androidlocalvpn_vpn_LocalVpnService_onCreateNative(
        _: JNIEnv,
        _: JClass,
    ) {
        android_logger::init_once(
            Config::default()
                .with_tag("nativeVpn")
                .with_min_level(log::Level::Trace),
        );
        log::trace!("onCreateNative");
    }

    #[no_mangle]
    pub unsafe extern "C" fn Java_com_github_jonforshort_androidlocalvpn_vpn_LocalVpnService_onDestroyNative(
        _: JNIEnv,
        _: JClass,
    ) {
        log::trace!("onDestroyNative");
    }

    #[no_mangle]
    pub unsafe extern "C" fn Java_com_github_jonforshort_androidlocalvpn_vpn_LocalVpnService_onStartVpn(
        _: JNIEnv,
        _: JClass,
        file_descriptor: i32,
    ) {
        log::trace!("onStartVpn, pid={}, fd={}", process::id(), file_descriptor);
        update_vpn(file_descriptor);
        vpn!().start();
    }

    #[no_mangle]
    pub unsafe extern "C" fn Java_com_github_jonforshort_androidlocalvpn_vpn_LocalVpnService_onStopVpn(
        _: JNIEnv,
        _: JClass,
    ) {
        log::trace!("onStopVpn, pid={}", process::id());
        vpn!().stop();
    }

    fn update_vpn(file_descriptor: i32) {
        let mut vpn = VPN.lock().unwrap();
        *vpn = Some(Vpn::new(file_descriptor));
    }
}
