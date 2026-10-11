//! Stable-channel application updates for the native mobile client.
//!
//! The phone installs its APK straight from the signed GitHub Release, so the
//! desktop update contract is reused as-is: the same Ed25519-signed manifest and
//! the same pinned artifact digests. Only the last step differs — Android hands
//! the verified APK to the system package installer, which is why the staged
//! file path is published to the settings UI instead of calling the updater
//! crate's installer.
//!
//! Only the stable channel is offered. A prerelease build still learns about the
//! stable release it leads up to, but this client never follows rc or preview
//! tags.
//!
//! Checks run on the process-lifetime runtime, never on a GPUI-owned task: the
//! app is a foreground client whose Activity can be recreated at any time, and
//! a check or a download must survive that. A failed automatic check is silent —
//! the phone may simply be offline, and nothing about pairing or the timeline
//! depends on it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use vibex_app_update::{
    AppUpdateConfig, AppUpdateService, CheckReason, UpdateChannel, UpdateSnapshot, UpdateState,
};
use vibex_core::unix_timestamp_ms;

/// How long a successful check is trusted before a running app checks again.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Floor between checks triggered by returning to the foreground, so switching
/// apps never hammers the release feed.
const FOREGROUND_CHECK_FLOOR_MS: i64 = 15 * 60 * 1000;
/// The record that keeps "a newer version is waiting" to one notification per
/// published version.
const NOTIFIED_FILE: &str = "update-notified.json";
const MAX_NOTIFIED_BYTES: u64 = 4 * 1024;

/// A tap on the update notification, delivered from the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateAction;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotifiedUpdate {
    notified_version: String,
}

struct ServiceState {
    service: AppUpdateService,
    data_dir: PathBuf,
    /// The version this installed package reports. It is the platform's own
    /// answer, so a build never compares the manifest against a version the
    /// binary merely believes it has.
    version: String,
    notified_version: Mutex<Option<String>>,
    /// Last check attempt, successful or not, in Unix milliseconds.
    last_check_ms: AtomicI64,
}

fn state() -> &'static Mutex<Option<Arc<ServiceState>>> {
    static STATE: OnceLock<Mutex<Option<Arc<ServiceState>>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

fn current() -> Option<Arc<ServiceState>> {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Whether this platform checks for updates at all.
///
/// The updater lands the APK through Android's package installer, so every
/// other platform (including iOS, whose store owns replacement) shows no update
/// surface and runs no checks.
pub const fn supported() -> bool {
    cfg!(target_os = "android")
}

/// Starts the update service for this process.
///
/// Returns `false` when this build or platform cannot check for updates, which
/// keeps every update surface hidden instead of showing one the user cannot act
/// on.
pub fn init(data_dir: &Path) -> bool {
    if !supported() {
        return false;
    }
    let mut slot = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.is_some() {
        return true;
    }
    // Resolved before the service lock is taken: the platform query is a JNI
    // call, and the cached answer is what every later read returns.
    let version =
        platform::installed_version().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let Some(service) = build_service(data_dir, &version) else {
        return false;
    };
    *slot = Some(Arc::new(ServiceState {
        service,
        data_dir: data_dir.to_path_buf(),
        version,
        notified_version: Mutex::new(read_notified(data_dir)),
        last_check_ms: AtomicI64::new(unix_timestamp_ms()),
    }));
    true
}

fn build_service(data_dir: &Path, version: &str) -> Option<AppUpdateService> {
    let config =
        AppUpdateConfig::for_current_build(version, UpdateChannel::Stable, data_dir).ok()?;
    // A build without the release verification key has no updater. Checking
    // would only report a failure the user cannot act on, so stay silent.
    config.public_key_base64.as_ref()?;
    AppUpdateService::new(config).ok()
}

/// The installed application version, as the platform reports it.
pub fn installed_version() -> String {
    if let Some(state) = current() {
        return state.version.clone();
    }
    platform::installed_version().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

pub fn snapshot() -> Option<UpdateSnapshot> {
    current().map(|state| state.service.snapshot())
}

pub fn subscribe() -> Option<watch::Receiver<UpdateSnapshot>> {
    current().map(|state| state.service.subscribe())
}

pub fn subscribe_actions() -> UnboundedReceiver<UpdateAction> {
    let (sender, receiver) = mpsc::unbounded();
    if let Ok(mut state) = action_state().lock() {
        // A cold start reports the tap from `Activity.onCreate`, before the
        // GPUI app exists; the buffered action is replayed to its first
        // subscriber instead of being lost.
        if state.pending {
            state.pending = false;
            let _ = sender.unbounded_send(UpdateAction);
        }
        state.sender = Some(sender);
    }
    receiver
}

#[derive(Default)]
struct ActionState {
    sender: Option<UnboundedSender<UpdateAction>>,
    pending: bool,
}

fn action_state() -> &'static Mutex<ActionState> {
    static STATE: OnceLock<Mutex<ActionState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(ActionState::default()))
}

fn enqueue_action() {
    let Ok(mut state) = action_state().lock() else {
        return;
    };
    if state
        .sender
        .as_ref()
        .is_some_and(|sender| sender.unbounded_send(UpdateAction).is_ok())
    {
        return;
    }
    state.sender = None;
    state.pending = true;
}

/// Keeps checking while the process lives: once at startup, then every
/// [`CHECK_INTERVAL`].
pub fn spawn_checks() {
    let Some(state) = current() else {
        return;
    };
    crate::background_connection::tokio_handle().spawn(async move {
        let mut interval = tokio::time::interval(CHECK_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            run_check(&state, CheckReason::Automatic).await;
        }
    });
}

/// Checks when the app returns to the foreground, at most once per floor.
pub fn check_if_stale() {
    let Some(state) = current() else {
        return;
    };
    let now = unix_timestamp_ms();
    if now.saturating_sub(state.last_check_ms.load(Ordering::Acquire)) < FOREGROUND_CHECK_FLOOR_MS {
        return;
    }
    crate::background_connection::tokio_handle().spawn(async move {
        run_check(&state, CheckReason::Automatic).await;
    });
}

/// Runs a check the user asked for, so a failure is visible.
pub fn check_now() {
    let Some(state) = current() else {
        return;
    };
    crate::background_connection::tokio_handle().spawn(async move {
        run_check(&state, CheckReason::Manual).await;
    });
}

async fn run_check(state: &ServiceState, reason: CheckReason) {
    state
        .last_check_ms
        .store(unix_timestamp_ms(), Ordering::Release);
    match state.service.check(reason).await {
        Ok(snapshot) => {
            if let UpdateState::Available { release } = &snapshot.state {
                notify_available(state, &release.version.to_string());
            }
        }
        Err(_) => {
            // Automatic failures are already recorded on the snapshot, where
            // the settings card ignores them; a manual failure publishes an
            // error state. Neither needs a second surface here.
        }
    }
}

/// Downloads the signed APK, verifies it, and hands it to the system installer.
pub fn download_and_install() {
    let Some(state) = current() else {
        return;
    };
    crate::background_connection::tokio_handle().spawn(async move {
        if let Ok(snapshot) = state.service.download().await {
            install_staged_snapshot(&snapshot);
        }
    });
}

/// Hands an already staged APK to the system installer again.
pub fn install_staged(path: &Path) {
    platform::install_apk(path);
}

fn install_staged_snapshot(snapshot: &UpdateSnapshot) {
    if let UpdateState::Staged { path, .. } = &snapshot.state {
        install_staged(path);
    }
}

/// Opens the signed APK download in the system browser.
///
/// The caller passes [`vibex_app_update::UpdateRelease::download_url`], which
/// already prefers the signed artifact and falls back to the release page.
pub fn open_download_page(url: &str) {
    platform::open_url(url);
}

fn notify_available(state: &ServiceState, version: &str) {
    let mut notified = state
        .notified_version
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if notified.as_deref() == Some(version) {
        return;
    }
    platform::notify_update(version);
    *notified = Some(version.to_string());
    let _ = write_notified(&state.data_dir, version);
}

fn notified_path(data_dir: &Path) -> PathBuf {
    data_dir.join(NOTIFIED_FILE)
}

fn read_notified(data_dir: &Path) -> Option<String> {
    let bytes = std::fs::read(notified_path(data_dir)).ok()?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_NOTIFIED_BYTES {
        return None;
    }
    let record: NotifiedUpdate = serde_json::from_slice(&bytes).ok()?;
    Some(record.notified_version)
}

fn write_notified(data_dir: &Path, version: &str) -> std::io::Result<()> {
    let record = NotifiedUpdate {
        notified_version: version.to_string(),
    };
    let encoded = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(notified_path(data_dir), encoded)
}

#[cfg(target_os = "android")]
mod platform {
    use std::path::Path;

    use jni::objects::JString;

    pub fn installed_version() -> Option<String> {
        crate::android_bridge::with_activity(|env, activity| {
            let value = env
                .call_method(
                    activity,
                    jni::jni_str!("appVersionName"),
                    jni::jni_sig!(() -> JString),
                    &[],
                )?
                .l()?;
            if value.is_null() {
                return Ok(String::new());
            }
            // Safety: the Java method is declared to return
            // `java.lang.String`, and the local reference stays alive for the
            // attached frame.
            let value: JString<'_> = unsafe { JString::from_raw(env, value.as_raw()) };
            Ok(value.to_string())
        })
        .filter(|version| !version.trim().is_empty())
    }

    pub fn open_url(url: &str) {
        // Only the signed release URL ever reaches this call, but the scheme is
        // re-checked here so no other caller can hand Android a `file:` or
        // `intent:` URL through the same bridge.
        if !url.starts_with("https://") {
            return;
        }
        let _ = crate::android_bridge::with_activity(|env, activity| {
            let url = env.new_string(url)?;
            env.call_method(
                activity,
                jni::jni_str!("openExternalUrl"),
                jni::jni_sig!((url: JString) -> ()),
                &[(&url).into()],
            )?;
            Ok(())
        });
    }

    pub fn install_apk(path: &Path) {
        let Some(path) = path.to_str() else {
            return;
        };
        let _ = crate::android_bridge::with_activity(|env, activity| {
            let path = env.new_string(path)?;
            env.call_method(
                activity,
                jni::jni_str!("installUpdatePackage"),
                jni::jni_sig!((path: JString) -> ()),
                &[(&path).into()],
            )?;
            Ok(())
        });
    }

    pub fn notify_update(version: &str) {
        let title = "Vibex";
        let body = crate::locale::text(
            "A new version is ready to install",
            "新版本已可安装",
            "新版本已可安裝",
        );
        let _ = crate::android_bridge::with_activity(|env, activity| {
            let version = env.new_string(version)?;
            let title = env.new_string(title)?;
            let body = env.new_string(body)?;
            env.call_method(
                activity,
                jni::jni_str!("showUpdateNotification"),
                jni::jni_sig!((version: JString, title: JString, body: JString) -> ()),
                &[(&version).into(), (&title).into(), (&body).into()],
            )?;
            Ok(())
        });
    }
}

#[cfg(not(target_os = "android"))]
mod platform {
    use std::path::Path;

    pub fn installed_version() -> Option<String> {
        None
    }

    pub fn open_url(_: &str) {}

    pub fn install_apk(_: &Path) {}

    pub fn notify_update(_: &str) {}
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiHostActivity_nativeOnUpdateNotificationActivated<
    'caller,
>(
    mut unowned_env: jni::EnvUnowned<'caller>,
    _class: jni::objects::JClass<'caller>,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            enqueue_action();
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>()
}

/// The notification copy for a discovered release, so the platform layer only
/// receives finished strings.
#[cfg(test)]
mod tests {
    use super::*;

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn reset_actions() {
        *action_state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = ActionState::default();
    }

    #[test]
    fn uninitialized_updates_have_no_surface() {
        if supported() {
            return;
        }
        assert!(snapshot().is_none());
        assert!(subscribe().is_none());
    }

    #[test]
    fn a_cold_start_tap_is_replayed_to_the_first_subscriber() {
        let _guard = test_lock();
        reset_actions();

        // The platform reports the tap before the app subscribes.
        enqueue_action();
        let mut actions = subscribe_actions();
        assert_eq!(actions.try_recv(), Ok(UpdateAction));

        // A live subscriber receives later taps directly.
        enqueue_action();
        assert_eq!(actions.try_recv(), Ok(UpdateAction));
        assert!(actions.try_recv().is_err());
    }

    #[test]
    fn only_one_cold_start_tap_is_buffered() {
        let _guard = test_lock();
        reset_actions();

        enqueue_action();
        enqueue_action();
        let mut actions = subscribe_actions();
        assert_eq!(actions.try_recv(), Ok(UpdateAction));
        assert!(actions.try_recv().is_err());
    }

    #[test]
    fn notified_versions_round_trip_and_reject_corruption() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(read_notified(directory.path()), None);

        write_notified(directory.path(), "0.2.0").unwrap();
        assert_eq!(read_notified(directory.path()), Some("0.2.0".to_string()));

        std::fs::write(notified_path(directory.path()), b"not json").unwrap();
        assert_eq!(read_notified(directory.path()), None);

        std::fs::write(notified_path(directory.path()), vec![b'x'; 8192]).unwrap();
        assert_eq!(read_notified(directory.path()), None);
    }
}
