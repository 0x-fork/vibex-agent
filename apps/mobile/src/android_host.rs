//! The Android host-driven platform entry.
//!
//! `gpui-pre-mobile` offers two Android entry points. The `android-activity`
//! one (`android_main`) builds the process-global `AndroidPlatform` exactly
//! once: a second `android_main` in the same process cannot replace it, so
//! GPUI's `App::new_app` trips its "must construct App on main thread" assert,
//! `android-activity` catches the panic and finishes the Activity. That is
//! reachable whenever the system reclaims the backgrounded Activity while the
//! foreground connection service keeps the process alive — the app flashed
//! closed on every tap until the process was killed.
//!
//! The host entry owns a **process-lived** render thread instead. This module
//! is the whole Rust half of that contract: [`GpuiHostActivity`] hands over the
//! Activity, its surface, input, window insets and lifecycle, and a recreated
//! Activity simply re-attaches its surface to the same GPUI window. The platform
//! keys that window by a host id the Java side names; Vibex names exactly one,
//! because it shows exactly one panel, so "the same GPUI window" survives every
//! Activity instance.
//!
//! [`GpuiHostActivity`]: ../../../android/app/src/main/java/ai/vibex/mobile/GpuiHostActivity.java

use std::path::PathBuf;

use gpui::AppLifecyclePhase;
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jboolean, jfloat, jint, jlong};
use jni::{Env, EnvUnowned};

use crate::android_insets;
use crate::assets::MobileAssets;
use crate::host_liveness;
use crate::platform;

/// Whether a callback may be handed to the host.
///
/// Everything below reaches `gpui_mobile::android::host`, whose entry points
/// post onto the process-lived render thread and wake its `ALooper`. That wake
/// reads a looper which only lives as long as the thread that created it, so
/// once the thread is gone the call aborts the process rather than returning.
/// [`host_liveness`] is the wall against it; see that module for the whole
/// story.
///
/// Call this on the Java UI thread, as every entry point here is, and before
/// doing any work worth skipping — a refused `nativeSurfaceDestroyed` also
/// spares the two-second wait for a render thread that will never answer.
fn host_reachable(callback: &str) -> bool {
    host_liveness::reachable(callback)
}

/// Records the Activity and starts the process-lived render thread.
///
/// Called from `GpuiHostActivity.onCreate` — every time, recreation included.
/// `gpui-pre-mobile` takes its own global reference to the Activity, keeps the
/// JVM, and makes `start_with_assets` a no-op after the first Activity, so a
/// recreated one only replaces the Activity the Java bridges talk to.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeStart<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    activity: JObject<'caller>,
    data_dir: JString<'caller>,
) {
    let mut launch_dir = None;
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            platform::install_diagnostics();
            if let Err(error) = gpui_mobile::android::jni::set_host_activity(env, &activity) {
                log::error!("nativeStart: recording the host Activity failed: {error}");
            }
            initialize_android_tls(env, &activity)?;
            launch_dir = Some(PathBuf::from(data_dir.to_string()));
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();

    let Some(data_dir) = launch_dir else {
        return;
    };
    // The platform keeps one GPUI window per host id and builds it through this
    // callback the first time that host's surface arrives — the launch closure
    // below only registers the assets. Vibex shows a single fullscreen panel, so
    // the Java host names one constant id: a recreated Activity re-attaches to
    // the window it left instead of stacking a second one.
    gpui_mobile::android::host::on_open_window(move |_host, cx| {
        crate::open_root_window(data_dir.clone(), cx);
    });
    // The one call that spawns the render thread, so it is also the one that can
    // say which tid that thread is; every callback below is checked against it.
    host_liveness::start_host(|| {
        gpui_mobile::android::host::start_with_assets(MobileAssets, |_cx| {});
    });
}

/// Points the platform TLS verifier at this Activity's application context.
///
/// The Android trust store is only reachable through JNI, so the verifier has
/// to be initialized before the first connection is attempted.
fn initialize_android_tls(env: &mut Env<'_>, activity: &JObject<'_>) -> jni::errors::Result<()> {
    use jni::signature::RuntimeMethodSignature;

    let signature = RuntimeMethodSignature::from_str("()Landroid/content/Context;")?;
    let context = env
        .call_method(
            activity,
            jni::jni_str!("getApplicationContext"),
            signature.method_signature(),
            &[],
        )?
        .l()?;
    rustls_platform_verifier::android::init_with_env(env, context)
}

/// Hands one Activity's surface to the render thread.
///
/// `host` is the constant id the Java host names; the platform re-attaches that
/// host's existing window when a recreated Activity presents a new surface.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeSurfaceCreated<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    host: jlong,
    surface: JObject<'caller>,
    scale: jfloat,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            if !host_reachable("nativeSurfaceCreated") {
                return Ok(());
            }
            // SAFETY: `surface` is the `android.view.Surface` the callback was
            // handed, and the NDK conversion takes its own reference to the
            // matching `ANativeWindow`.
            let window = unsafe {
                ndk::native_window::NativeWindow::from_surface(
                    env.get_raw().cast(),
                    surface.as_raw().cast(),
                )
            };
            match window {
                Some(window) => {
                    gpui_mobile::android::host::surface_created(host.max(0) as u64, window, scale)
                }
                None => log::warn!("nativeSurfaceCreated: the Surface has no native window"),
            }
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Takes the surface back once Android tears it down.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeSurfaceDestroyed<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    host: jlong,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            if !host_reachable("nativeSurfaceDestroyed") {
                return Ok(());
            }
            gpui_mobile::android::host::surface_destroyed(host.max(0) as u64);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Forwards `onResume` / `onPause`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeOnAppLifecycle<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    host: jlong,
    foreground: jboolean,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            let host = host.max(0) as u64;
            // The app's own lifecycle state keeps following the Activity even
            // when the host cannot be told: the haptic gate reads it, and it
            // does not depend on the render thread.
            if foreground {
                if host_reachable("nativeOnAppLifecycle") {
                    gpui_mobile::android::host::resumed(host);
                }
                platform::notify_lifecycle(AppLifecyclePhase::Active);
            } else {
                platform::notify_lifecycle(AppLifecyclePhase::Background);
                if host_reachable("nativeOnAppLifecycle") {
                    gpui_mobile::android::host::paused(host);
                }
            }
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Reports system-bar/cutout edge distances in physical pixels.
///
/// Java and GPUI resize asynchronously, so this must not be a content rect
/// calculated using the Java view's dimensions.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeInsets<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    left: jint,
    top: jint,
    right: jint,
    bottom: jint,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            android_insets::report(left, top, right, bottom);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Reports the software keyboard's visibility and its height in logical pixels.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeKeyboardState<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    visible: jboolean,
    height: jfloat,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            platform::set_keyboard_visible(visible);
            gpui_mobile::set_keyboard_height(if visible { height.max(0.0) } else { 0.0 });
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Forwards one InputConnection update from the IME proxy.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeIme<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    session: jlong,
    kind: jint,
    text: JString<'caller>,
    start: jint,
    end: jint,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            gpui_mobile::android::host::ime_event(
                session as u64,
                kind,
                text.to_string(),
                start.max(0) as usize,
                end.max(0) as usize,
            );
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Forwards one pointer of a `MotionEvent`.
///
/// The Java host already collapsed `ACTION_POINTER_DOWN`/`ACTION_POINTER_UP`
/// onto the affected pointer, so a one-pointer slice is exactly the fan-out
/// [`gpui_mobile::android::host::motion_event`] documents.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeTouch<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    action: jint,
    pointer_id: jint,
    x: jfloat,
    y: jfloat,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            if !host_reachable("nativeTouch") {
                return Ok(());
            }
            // GPUI wants physical pixels relative to the surface, which is
            // what `MotionEvent` reports for a full-window view.
            gpui_mobile::android::host::motion_event(
                action as u32,
                0,
                &[gpui_mobile::android::host::Pointer {
                    id: pointer_id,
                    x,
                    y,
                }],
            );
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}

/// Forwards a hardware key. `action` is 0 for down and 1 for up.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeKey<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    key_code: jint,
    action: jint,
    meta_state: jint,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            if !host_reachable("nativeKey") {
                return Ok(());
            }
            gpui_mobile::android::host::key(key_code, action, meta_state);
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>();
}
