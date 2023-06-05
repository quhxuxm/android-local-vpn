mod vpn;

pub mod tun {

    use lazy_static::lazy_static;

    use crate::core::vpn::Vpn;
    use std::process;
    use std::sync::Mutex;

    lazy_static! {
        static ref VPN: Mutex<Option<Vpn>> = Mutex::new(None);
    }

    macro_rules! vpn {
        () => {
            VPN.lock().unwrap().as_mut().unwrap()
        };
    }

    pub fn start(file_descriptor: i32) {
        log::trace!("start, pid={}, fd={}", process::id(), file_descriptor);
        update_vpn(file_descriptor);
        vpn!().start();
        log::trace!("started, pid={}, fd={}", process::id(), file_descriptor);
    }

    pub fn stop() {
        log::trace!("stop, pid={}", process::id());
        vpn!().stop();
        log::trace!("stopped, pid={}", process::id());
    }

    fn update_vpn(file_descriptor: i32) {
        let mut vpn = VPN.lock().unwrap();
        *vpn = Some(Vpn::new(file_descriptor));
    }
}
