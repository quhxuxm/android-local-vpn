use jni::objects::{JMethodID, JObject, JValue};
use jni::signature::{Primitive, ReturnType};
use jni::JNIEnv;

pub struct JniContext<'a, 'b> {
    pub(super) jni_env: JNIEnv<'a>,
    pub(super) java_vpn_service: &'b JObject<'a>,
    pub(super) protect_method_id: JMethodID,
}

impl<'a, 'b> JniContext<'a, 'b> {
    pub fn protect_socket(&mut self, socket: i32) -> bool {
        if socket <= 0 {
            log::error!("invalid socket, socket={:?}", socket);
            return false;
        }
        let return_type = ReturnType::Primitive(Primitive::Boolean);
        let arguments = [JValue::Int(socket).as_jni()];
        let result = unsafe {
            self.jni_env.call_method_unchecked(
                self.java_vpn_service,
                self.protect_method_id,
                return_type,
                &arguments[..],
            )
        };
        match result {
            Ok(value) => {
                log::trace!("protected socket, result={:?}", value);
                value.z().unwrap()
            }
            Err(error_code) => {
                log::error!("failed to protect socket, error={:?}", error_code);
                false
            }
        }
    }
}
