//! The Android app's native library. Three functions, all strings: open an engine on a directory,
//! run a command (see `spike_app::bridge`), close it. The Kotlin side is `network.spike.connect.Native`.

use jni::objects::{JClass, JString};
use jni::sys::{jlong, jstring};
use jni::JNIEnv;
use spike_app::bridge::Engine;

fn text(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(String::from).unwrap_or_default()
}

fn answer(env: &mut JNIEnv, json: String) -> jstring {
    env.new_string(json)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Open an engine whose state lives in `dir` (the app's private storage). Returns a handle, or 0.
#[no_mangle]
pub extern "system" fn Java_network_spike_connect_Native_open(
    mut env: JNIEnv,
    _class: JClass,
    dir: JString,
) -> jlong {
    let dir = text(&mut env, &dir);
    match std::panic::catch_unwind(|| Engine::new(dir)) {
        Ok(Ok(engine)) => Box::into_raw(Box::new(engine)) as jlong,
        _ => 0,
    }
}

/// Run one command. Blocking: the app calls it from a background thread. Always returns JSON.
#[no_mangle]
pub extern "system" fn Java_network_spike_connect_Native_call(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    command: JString,
    args: JString,
) -> jstring {
    let (command, args) = (text(&mut env, &command), text(&mut env, &args));
    let json = if handle == 0 {
        r#"{"err":"the engine is not open"}"#.to_string()
    } else {
        // SAFETY: `handle` came from `open` and is not used after `close`.
        let engine = unsafe { &*(handle as *const Engine) };
        engine.call(&command, &args)
    };
    answer(&mut env, json)
}

/// Close an engine opened by `open`.
#[no_mangle]
pub extern "system" fn Java_network_spike_connect_Native_close(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: as above; this is the one place the box is given back.
        drop(unsafe { Box::from_raw(handle as *mut Engine) });
    }
}
