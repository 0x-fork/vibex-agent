//! Whether the Android host's process-lived render thread is still running.
//!
//! `gpui-pre-mobile`'s host-driven entry owns the thread the app renders on and
//! a process-global `ALooper` for it. Every `android::host` entry point this
//! crate calls — touch, key, surface, lifecycle — posts a command onto that
//! thread and wakes it with `ALooper_wake`. That wake dereferences the looper
//! without asking whether the thread which created it is still there, and the
//! pointer is only good for as long as that thread runs: `ALooper_prepare` ties
//! the looper to its thread, nothing takes a second reference, and this
//! dependency holds no path that releases one.
//!
//! So when the render thread goes away the looper goes with it, and the next
//! callback wakes freed memory. `android::Looper::wake()` answers that with
//! `LOG_ALWAYS_FATAL("Could not write wake signal to fd -1")` — the abort
//! `logcat` shows as `fd -1`, or as the garbage descriptor a reused allocation
//! leaves in the field. `LOG_ALWAYS_FATAL` aborts, so no `catch_unwind` can
//! stop it, and every later tap repeats it: the app flashes closed on touch
//! until it is force-stopped.
//!
//! The host lives in a dependency pinned by revision, so the wall between it
//! and the Java callbacks is here: remember the render thread's tid when the
//! host entry starts it, and let the callbacks check that it is still running
//! before they hand anything over. That costs one `stat` and keeps a lost host
//! to a logged refusal instead of a dead process.
//!
//! Nothing restarts the render thread — `gpui-pre-mobile` starts it once behind
//! a `OnceLock` — so a host recorded here as gone stays gone until the process
//! is replaced. The refusal is the point: it turns an unexplained crash loop
//! into one `logcat` line naming the thread that died.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// The tid the host renders on, or 0 until the host entry spawns it.
static RENDER_THREAD: AtomicI32 = AtomicI32::new(0);

/// Set once the loss has been reported, so the callbacks that keep arriving
/// after it do not each write a line.
static REPORTED: AtomicBool = AtomicBool::new(false);

/// Every thread in this process, which is how the host's own thread is told
/// apart from the threads that thread spawns behind it.
fn process_threads() -> BTreeSet<i32> {
    let mut threads = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
        return threads;
    };
    for entry in entries.flatten() {
        if let Some(tid) = entry.file_name().to_str().and_then(|tid| tid.parse().ok()) {
            threads.insert(tid);
        }
    }
    threads
}

/// The thread the host started, out of the ones `start` added.
///
/// The host spawns the render thread and that thread spawns the frame clock and
/// the executor behind it, so the lowest new tid is the render thread. A call
/// that starts nothing — an Activity recreation, where the host is already up —
/// adds no tid and answers `None`.
fn started_thread(before: &BTreeSet<i32>, after: &BTreeSet<i32>) -> Option<i32> {
    // `difference` walks ascending, so the first item is the lowest new tid.
    after.difference(before).next().copied()
}

/// Runs the host entry and remembers the render thread it starts.
///
/// Wraps the one call that spawns the host, so the tid is taken from the process
/// itself rather than guessed from a thread name: Android renames a thread when
/// it attaches to the JVM, which is why the render thread shows up in
/// `/proc/self/task` as `Thread-2` and not as the name the host gave it.
///
/// `log` is a mobile target's dependency in this crate, so this and [`reachable`]
/// — the two halves that report — are the Android-only ones; the tid arithmetic
/// above stays reachable from the host build that runs the tests below.
#[cfg(target_os = "android")]
pub fn start_host(start: impl FnOnce()) {
    let before = process_threads();
    start();
    let after = process_threads();
    match started_thread(&before, &after) {
        Some(tid) => {
            RENDER_THREAD.store(tid, Ordering::Release);
            log::info!("android host: render thread is tid {tid}");
        }
        // Expected on a recreation: the host is already up, and its thread is
        // the one recorded here.
        None => log::debug!(
            "android host: host entry started no thread, still tid {}",
            RENDER_THREAD.load(Ordering::Acquire)
        ),
    }
}

/// Whether the host's render thread is still running.
///
/// Fail-open while no tid has been recorded: there is nothing to compare
/// against yet, and the callbacks keep whatever behaviour they had before. A
/// tid can also be reused by a later thread, which reads as alive — the
/// direction that costs a lost host rather than a refused one that works.
fn render_thread_alive() -> bool {
    let tid = RENDER_THREAD.load(Ordering::Acquire);
    tid == 0 || Path::new("/proc/self/task").join(tid.to_string()).exists()
}

/// Whether a Java callback may be handed to the host, reporting the first one
/// that may not.
///
/// Every caller is on the Java UI thread, which is where the host's entry points
/// expect to be called from.
#[cfg(target_os = "android")]
pub fn reachable(callback: &str) -> bool {
    if render_thread_alive() {
        return true;
    }
    if !REPORTED.swap(true, Ordering::AcqRel) {
        log::error!(
            "android host: render thread tid {} is gone; refusing {callback} and every later \
             callback rather than waking the host's dead looper, which aborts the process",
            RENDER_THREAD.load(Ordering::Acquire)
        );
    }
    false
}

#[cfg(test)]
mod tests {
    use super::started_thread;
    use std::collections::BTreeSet;

    fn threads(tids: &[i32]) -> BTreeSet<i32> {
        tids.iter().copied().collect()
    }

    /// The host spawns the render thread first and that thread spawns the frame
    /// clock and the executor behind it, so the render thread is the lowest of
    /// the threads the entry added — not whichever the set happens to yield.
    #[test]
    fn the_render_thread_is_the_lowest_new_tid() {
        let before = threads(&[10, 11]);
        let after = threads(&[10, 11, 20, 21, 22, 23]);
        assert_eq!(started_thread(&before, &after), Some(20));
    }

    /// An Activity recreation calls the same entry, and the host is already up:
    /// nothing new is added, so the recorded thread has to stand.
    #[test]
    fn an_entry_that_starts_nothing_reports_nothing() {
        let before = threads(&[10, 11, 20]);
        assert_eq!(started_thread(&before, &before), None);
    }

    /// Threads that came and went while the host entry ran are not the host's.
    #[test]
    fn threads_that_outlived_the_entry_are_ignored() {
        let before = threads(&[10, 30]);
        let after = threads(&[10, 20, 30, 40]);
        assert_eq!(started_thread(&before, &after), Some(20));
    }

    /// Tests share the process, so this one puts the recorded tid back rather
    /// than deciding liveness for whatever runs next.
    #[cfg(target_os = "android")]
    #[test]
    fn an_unrecorded_host_is_not_refused() {
        use super::RENDER_THREAD;
        use std::sync::atomic::Ordering;

        let previous = RENDER_THREAD.swap(0, Ordering::AcqRel);
        assert!(super::reachable("test"), "no tid means nothing to refuse");
        RENDER_THREAD.store(previous, Ordering::Release);
    }
}
