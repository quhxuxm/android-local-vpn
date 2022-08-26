// This is free and unencumbered software released into the public domain.
//
// Anyone is free to copy, modify, publish, use, compile, sell, or
// distribute this software, either in source code form or as a compiled
// binary, for any purpose, commercial or non-commercial, and by any
// means.
//
// In jurisdictions that recognize copyright laws, the author or authors
// of this software dedicate any and all copyright interest in the
// software to the public domain. We make this dedication for the benefit
// of the public at large and to the detriment of our heirs and
// successors. We intend this dedication to be an overt act of
// relinquishment in perpetuity of all present and future rights to this
// software under copyright law.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
// IN NO EVENT SHALL THE AUTHORS BE LIABLE FOR ANY CLAIM, DAMAGES OR
// OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
// ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
// OTHER DEALINGS IN THE SOFTWARE.
//
// For more information, please refer to <https://unlicense.org>

extern crate jni;

use self::jni::objects::{GlobalRef, JClass, JMethodID, JObject, JValue};
use self::jni::signature::{JavaType, Primitive};
use self::jni::{JNIEnv, JavaVM};
use std::sync::Arc;
use std::sync::Mutex;

lazy_static! {
    pub static ref JNI: Mutex<Option<Jni>> = Mutex::new(None);
}

pub struct Jni {
    java_vm: Arc<JavaVM>,
    object: GlobalRef,
}

impl Jni {
    pub fn init(env: JNIEnv, _: JClass, object: JObject) {
        let mut jni = JNI.lock().unwrap();
        let java_vm = Arc::new(env.get_java_vm().unwrap());
        *jni = Some(Jni {
            java_vm,
            object: env.new_global_ref(object).unwrap(),
        });
    }

    pub fn release() {
        let mut jni = JNI.lock().unwrap();
        *jni = None;
    }

    pub fn new_context(&self) -> Option<JniContext> {
        match self.java_vm.attach_current_thread_permanently() {
            Ok(jni_env) => {
                if let Some(protect_method_id) = Jni::create_protect_jni_method_id(jni_env) {
                    return Some(JniContext {
                        jni_env,
                        object: self.object.as_obj(),
                        protect_method_id,
                    });
                }
            }
            Err(error) => {
                log::error!("failed to attach to current thread, error={:?}", error);
            }
        }
        None
    }

    fn create_protect_jni_method_id(jni_env: JNIEnv) -> Option<JMethodID> {
        if let Ok(class) = jni_env.find_class("android/net/VpnService") {
            log::trace!("found vpn service class");
            if let Ok(method_id) = jni_env.get_method_id(class, "protect", "(I)Z") {
                log::trace!("found protect method id");
                return Some(method_id);
            }
        }
        None
    }
}

pub struct JniContext<'a> {
    jni_env: JNIEnv<'a>,
    object: JObject<'a>,
    protect_method_id: JMethodID<'a>,
}

impl<'a> JniContext<'a> {
    pub fn protect_socket(&self, socket: i32) -> bool {
        if socket <= 0 {
            log::error!("invalid socket, socket={:?}", socket);
            return true;
        }
        let is_successful = JavaType::Primitive(Primitive::Boolean);
        let arguments = [JValue::Int(socket)];
        let result = self.jni_env.call_method_unchecked(
            self.object,
            self.protect_method_id,
            is_successful,
            &arguments[..],
        );
        match result {
            Ok(value) => {
                log::trace!("protected socket, result={:?}", value);
                true
            }
            Err(error_code) => {
                log::error!("failed to protect socket, error={:?}", error_code);
                false
            }
        }
    }
}
