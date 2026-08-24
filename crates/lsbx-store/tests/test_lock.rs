// See crates/lsbx-kernel/tests/test_kernel.rs for why this allow is scoped
// to test files: clippy::unwrap_used/expect_used are restriction-group
// lints that fire on any code text, including tests/*.rs, even though every
// fn in this file only compiles under `cargo test`. src/**/*.rs remains
// unwrap/expect/panic-free under the same workspace lints with no allow
// needed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lsbx_kernel::error::LsbxError;
use lsbx_store::lock::LockSentinel;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn try_acquire_fails_while_held_and_succeeds_after_drop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.lock");

    let guard = LockSentinel::try_acquire(&path).unwrap();

    match LockSentinel::try_acquire(&path) {
        Err(LsbxError::LockContention(_)) => {}
        other => panic!("expected LockContention, got {:?}", other.map(|_| ())),
    }

    drop(guard);

    // Succeeds immediately after the first guard is dropped.
    let _guard2 = LockSentinel::try_acquire(&path).unwrap();
}

#[test]
fn acquire_blocks_until_released() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.lock");

    let guard = LockSentinel::try_acquire(&path).unwrap();

    let path2 = path.clone();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (acquired_tx, acquired_rx) = mpsc::channel::<()>();
    let handle = thread::spawn(move || {
        // Blocks until the main thread drops `guard`.
        let _g = LockSentinel::acquire(&path2).unwrap();
        acquired_tx.send(()).unwrap();
        // Hold until told to let go, so the assertion below (that the
        // acquire genuinely didn't return early) is unambiguous.
        release_rx.recv().unwrap();
    });

    // Give the spawned thread a chance to actually block inside
    // `acquire()` before we release — if `acquire` returned early despite
    // contention, `acquired_rx.try_recv()` below would already see it.
    thread::sleep(Duration::from_millis(50));
    assert!(acquired_rx.try_recv().is_err(), "acquire() returned before the lock was released");

    drop(guard);
    acquired_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    release_tx.send(()).unwrap();
    handle.join().unwrap();
}

#[test]
fn dropping_guard_never_unlinks_lock_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.lock");

    let guard = LockSentinel::try_acquire(&path).unwrap();
    assert!(path.exists());
    drop(guard);
    assert!(path.exists(), "LockGuard must never unlink its sentinel file on drop");
}

/// The scenario named explicitly in the unit contract's Verification
/// section: a second thread actively unlinks-and-recreates the lock path
/// WHILE the main thread is genuinely blocked inside `acquire()`, proving
/// the fstat/stat `(dev, ino)` comparison catches the race rather than
/// silently handing back a false success.
///
/// This is a rigorous concurrency test, not a sequential approximation of
/// one: it synchronizes via channels so the unlink-and-recreate genuinely
/// happens while the contending caller is blocked on `flock(LOCK_EX)`
/// inside `acquire()`, not before it gets there or after it already
/// returned.
#[test]
fn race_unlink_recreate_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("race.lock");

    // Thread A: acquire first, signal it's held, then wait to be told to
    // unlink-and-recreate the path (simulating an external process racing
    // the sentinel file itself) before finally releasing.
    let path_a = path.clone();
    let (a_holding_tx, a_holding_rx) = mpsc::channel::<()>();
    let (do_race_tx, do_race_rx) = mpsc::channel::<()>();
    let (raced_tx, raced_rx) = mpsc::channel::<()>();
    let handle_a = thread::spawn(move || {
        let guard = LockSentinel::try_acquire(&path_a).unwrap();
        a_holding_tx.send(()).unwrap();

        // Wait until the main thread's `acquire()` call is confirmed to be
        // blocked on this held lock before we race the file out from under
        // it.
        do_race_rx.recv().unwrap();

        // Unlink and recreate the path at the same location while the
        // held flock is still live. `guard`'s fd still refers to the old
        // (now-unlinked) inode; the new file at `path_a` is a distinct
        // inode with no lock held on it yet.
        std::fs::remove_file(&path_a).unwrap();
        std::fs::File::create(&path_a).unwrap();
        raced_tx.send(()).unwrap();

        // Give the main thread's blocked `flock` a moment to actually
        // succeed against the newly-created (unlocked) inode before we
        // drop our guard (which would also release a lock, but on the
        // now-unlinked old inode — irrelevant to the new file).
        thread::sleep(Duration::from_millis(100));
        drop(guard);
    });

    // Wait for thread A to actually hold the lock.
    a_holding_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    // Main thread: call the blocking `acquire()`. Because thread A holds
    // an exclusive flock on the current inode at `path`, this call must
    // genuinely block inside `flock(LOCK_EX)` — it cannot return before
    // thread A's unlink-and-recreate happens.
    let path_main = path.clone();
    let (main_acquired_tx, main_acquired_rx) = mpsc::channel::<()>();
    let handle_main = thread::spawn(move || {
        let _guard = LockSentinel::acquire(&path_main).unwrap();
        main_acquired_tx.send(()).unwrap();
    });

    // Confirm the main acquire call is genuinely still blocked (nothing
    // to acquire yet, since thread A holds the only inode currently at
    // `path`) before triggering the race.
    thread::sleep(Duration::from_millis(50));
    assert!(
        main_acquired_rx.try_recv().is_err(),
        "acquire() returned before the unlink-and-recreate race even happened"
    );

    // Now tell thread A to unlink-and-recreate the path while the main
    // thread's `acquire()` is blocked.
    do_race_tx.send(()).unwrap();
    raced_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    // The main thread's blocked `flock` call will now succeed against the
    // newly-created (unlocked) inode at `path` — a false-success flock,
    // exactly the race this primitive must detect via fstat/stat. Once
    // `LockSentinel::acquire` notices `(dev, ino)` mismatch, it must
    // reopen and retry rather than returning the bogus guard. Since
    // thread A drops its own guard shortly after recreating the file, the
    // retry should eventually succeed on the recreated file's current
    // (by-then-unlocked) inode.
    main_acquired_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("acquire() must detect the race, retry, and eventually succeed rather than hang or silently return a stale guard");

    handle_a.join().unwrap();
    handle_main.join().unwrap();
}
