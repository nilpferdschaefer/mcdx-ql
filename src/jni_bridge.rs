//! JNI bridge: `com.nilpferdschaefer.mcdxql.McdxQl.compileNative(String) -> String`

use jni::objects::{JClass, JString};
use jni::sys::jstring;
use jni::JNIEnv;

use crate::json_api::compile_json;

/// JNI entrypoint — JSON request in, JSON response out.
#[no_mangle]
pub extern "system" fn Java_com_nilpferdschaefer_mcdxql_McdxQl_compileNative<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    request_json: JString<'local>,
) -> jstring {
    let input: String = match env.get_string(&request_json) {
        Ok(s) => s.into(),
        Err(e) => {
            let msg = format!(
                r#"{{"ok":false,"error":{{"code":"compile_error","message":"jni get_string failed: {e}","expr":""}}}}"#
            );
            return env
                .new_string(msg)
                .expect("failed to allocate error jstring")
                .into_raw();
        }
    };

    let output = compile_json(&input);
    env.new_string(output)
        .expect("failed to allocate result jstring")
        .into_raw()
}
