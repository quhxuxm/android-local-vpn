mod jni_context;

use jni::objects::{GlobalRef, JClass, JMethodID, JObject};
use jni::{JNIEnv, JavaVM};
pub use jni_context::JniContext;
use lazy_static::lazy_static;
use std::sync::Arc;
use std::sync::Mutex;

lazy_static! {
    pub static ref JNI: Mutex<Option<Jni>> = Mutex::new(None);
}

macro_rules! jni {
    () => {
        crate::jni::JNI.lock().unwrap().as_mut().unwrap()
    };
}

pub struct Jni {
    java_vm: Arc<JavaVM>,
    java_vpn_service: GlobalRef,
}

impl Jni {
    pub fn init(env: JNIEnv, _: JClass, java_vpn_service: JObject) {
        let mut jni = JNI.lock().unwrap();
        let java_vm = Arc::new(env.get_java_vm().unwrap());
        let java_vpn_service = env.new_global_ref(java_vpn_service).unwrap();
        *jni = Some(Jni {
            java_vm,
            java_vpn_service,
        });
    }

    pub fn release() {
        let mut jni = JNI.lock().unwrap();
        *jni = None;
    }

    pub fn new_context(&self) -> Option<JniContext> {
        match self.java_vm.attach_current_thread_permanently() {
            Ok(mut jni_env) => match Jni::get_protect_method_id(&mut jni_env) {
                Some(protect_method_id) => {
                    let java_vpn_service = self.java_vpn_service.as_obj();
                    return Some(JniContext {
                        jni_env,
                        java_vpn_service,
                        protect_method_id,
                    });
                }
                None => {
                    log::error!("failed to get protect method id");
                }
            },
            Err(error) => {
                log::error!("failed to attach to current thread, error={:?}", error);
            }
        }
        None
    }

    fn get_protect_method_id(jni_env: &mut JNIEnv) -> Option<JMethodID> {
        match jni_env.find_class("android/net/VpnService") {
            Ok(class) => match jni_env.get_method_id(class, "protect", "(I)Z") {
                Ok(method_id) => {
                    return Some(method_id);
                }
                Err(error) => {
                    log::error!("failed to get protect method id, error={:?}", error);
                }
            },
            Err(error) => {
                log::error!("failed to find vpn service class, error={:?}", error);
            }
        }
        None
    }
}
