//! Noticing that `chat.db` has changed, without Messages.app open.
//!
//! macOS keeps the store in WAL mode, so an incoming message lands in
//! `chat.db-wal` long before it is checkpointed into `chat.db` itself. This
//! module watches the directory the database lives in and reports "something
//! touched the store" after a short quiet period, which the app turns into one
//! re-query rather than one per write.
//!
//! Nothing here reads the database, so nothing here can leak message content:
//! it deals in file names and instants only. If the platform watcher cannot be
//! started at all — no notification backend, a path that cannot be watched —
//! the same interface falls back to a two-second timer, and
//! [`Watcher::status`] says which of the two is running.

use std::ffi::OsString;
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::app::WatcherStatus;

/// Quiet period after the last write before the database is re-read.
///
/// A single incoming message is several writes to the WAL; waiting for them to
/// stop turns the burst into one query.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// Longest a continuous run of writes may hold the re-read off.
///
/// Without it, a long sync — the first launch after a restore, say — would keep
/// resetting the debounce and the screen would never move.
pub const MAX_HOLD: Duration = Duration::from_secs(1);

/// How often the fallback timer re-reads when no watcher could be started.
pub const POLL_EVERY: Duration = Duration::from_secs(2);

/// The debounce, split out so it can be tested without a filesystem.
///
/// Every method takes the current instant rather than reading the clock, so a
/// test can drive a burst of writes through it in no time at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Debounce {
    /// When the current burst of writes started.
    first: Option<Instant>,
    /// When the most recent write of the burst arrived.
    last: Option<Instant>,
}

impl Debounce {
    /// Note that the store was written to.
    pub fn record(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    /// Whether a burst is waiting to be read.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.last.is_some()
    }

    /// Whether the burst has gone quiet — or run long enough that waiting for
    /// quiet is no longer reasonable. Consumes the burst when it says yes.
    pub fn ready(&mut self, now: Instant) -> bool {
        let (Some(first), Some(last)) = (self.first, self.last) else {
            return false;
        };
        let quiet = now.saturating_duration_since(last) >= DEBOUNCE;
        let held = now.saturating_duration_since(first) >= MAX_HOLD;
        if quiet || held {
            *self = Self::default();
            return true;
        }
        false
    }
}

/// Live updates for one database file.
///
/// [`Watcher::ready`] is called once per frame and answers "re-read the
/// database now". It is cheap: it drains a channel and compares two instants.
pub struct Watcher {
    status: WatcherStatus,
    /// Dropping the platform watcher stops the notifications, so it is kept
    /// alive here even though nothing calls into it again.
    inner: Option<RecommendedWatcher>,
    events: Option<Receiver<notify::Result<Event>>>,
    /// `chat.db` and its WAL sidecars, as bare file names.
    names: Vec<OsString>,
    debounce: Debounce,
    /// When the fallback timer last fired.
    last_poll: Instant,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("status", &self.status)
            .field("pending", &self.debounce.is_pending())
            .finish_non_exhaustive()
    }
}

impl Default for Watcher {
    fn default() -> Self {
        Self::off()
    }
}

impl Watcher {
    /// A watcher that never reports anything, for an app with no database.
    #[must_use]
    pub fn off() -> Self {
        Self {
            status: WatcherStatus::Off,
            inner: None,
            events: None,
            names: Vec::new(),
            debounce: Debounce::default(),
            last_poll: Instant::now(),
        }
    }

    /// Start watching the directory `path` lives in.
    ///
    /// The *directory* rather than the file: `chat.db-wal` is deleted and
    /// recreated around a checkpoint, and a watch on a file that gets replaced
    /// stops seeing it. Events for anything else in the directory are filtered
    /// out by name.
    ///
    /// Never fails. If the platform watcher will not start, the returned
    /// watcher polls on a timer instead and says so through
    /// [`Watcher::status`].
    #[must_use]
    pub fn start(path: &Path) -> Self {
        let mut watcher = Self::off();
        watcher.names = sidecars(path);
        watcher.status = WatcherStatus::Polling;

        let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) else {
            return watcher;
        };
        let (tx, rx) = channel();
        let Ok(mut inner) = RecommendedWatcher::new(tx, notify::Config::default()) else {
            return watcher;
        };
        if inner.watch(dir, RecursiveMode::NonRecursive).is_err() {
            return watcher;
        }

        watcher.inner = Some(inner);
        watcher.events = Some(rx);
        watcher.status = WatcherStatus::Watching;
        watcher
    }

    /// Whether this is watching, polling, or off.
    #[must_use]
    pub const fn status(&self) -> WatcherStatus {
        self.status
    }

    /// Whether the database should be re-read now.
    ///
    /// Call once per frame. While watching, this is true a debounce after the
    /// last write to the store; while polling, once every [`POLL_EVERY`].
    pub fn ready(&mut self) -> bool {
        self.ready_at(Instant::now())
    }

    /// [`Watcher::ready`] against a caller-supplied clock, for tests.
    pub fn ready_at(&mut self, now: Instant) -> bool {
        match self.status {
            WatcherStatus::Off => false,
            WatcherStatus::Polling => {
                if now.saturating_duration_since(self.last_poll) < POLL_EVERY {
                    return false;
                }
                self.last_poll = now;
                true
            }
            WatcherStatus::Watching => {
                self.drain(now);
                if self.status == WatcherStatus::Polling {
                    // The backend died while draining; the timer takes over
                    // from here, starting on the next call.
                    self.last_poll = now;
                    return true;
                }
                self.debounce.ready(now)
            }
        }
    }

    /// Take whatever the platform watcher has queued up.
    ///
    /// A backend error is not fatal: the events channel is dropped and the
    /// timer takes over, which is exactly what an unstartable watcher does.
    fn drain(&mut self, now: Instant) {
        let Some(events) = self.events.as_ref() else {
            self.status = WatcherStatus::Polling;
            return;
        };
        loop {
            match events.try_recv() {
                Ok(Ok(event)) => {
                    if event.paths.iter().any(|path| self.is_ours(path)) {
                        self.debounce.record(now);
                    }
                }
                // A backend that reports an error keeps running; a single bad
                // event is not a reason to stop believing it.
                Ok(Err(_)) => {}
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.inner = None;
                    self.events = None;
                    self.status = WatcherStatus::Polling;
                    return;
                }
            }
        }
    }

    /// Whether an event path is the database or one of its sidecars.
    fn is_ours(&self, path: &Path) -> bool {
        path.file_name()
            .is_some_and(|name| self.names.iter().any(|ours| ours == name))
    }
}

/// `chat.db` and the two files WAL mode keeps beside it.
fn sidecars(path: &Path) -> Vec<OsString> {
    let Some(name) = path.file_name() else {
        return Vec::new();
    };
    ["", "-wal", "-shm"]
        .into_iter()
        .map(|suffix| {
            let mut owned = name.to_os_string();
            owned.push(suffix);
            owned
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_of_writes_becomes_one_read_after_the_quiet_period() {
        let start = Instant::now();
        let mut debounce = Debounce::default();
        assert!(!debounce.is_pending());
        assert!(!debounce.ready(start));

        debounce.record(start);
        debounce.record(start + Duration::from_millis(100));
        assert!(debounce.is_pending());
        assert!(!debounce.ready(start + Duration::from_millis(200)));
        assert!(debounce.ready(start + Duration::from_millis(400)));
        // Consumed: the same burst does not fire twice.
        assert!(!debounce.ready(start + Duration::from_secs(5)));
        assert!(!debounce.is_pending());
    }

    #[test]
    fn a_continuous_run_of_writes_still_fires() {
        let start = Instant::now();
        let mut debounce = Debounce::default();
        let mut at = start;
        // A write every 100ms never leaves a 300ms gap, so only the ceiling
        // gets the screen moving.
        for _ in 0..20 {
            debounce.record(at);
            if debounce.ready(at) {
                assert!(at.saturating_duration_since(start) >= MAX_HOLD);
                return;
            }
            at += Duration::from_millis(100);
        }
        panic!("a continuous run of writes never fired");
    }

    #[test]
    fn the_sidecars_of_a_database_are_its_wal_and_shm() {
        let names = sidecars(Path::new("/tmp/msgs/chat.db"));
        let names: Vec<String> = names
            .iter()
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["chat.db", "chat.db-wal", "chat.db-shm"]);
    }

    #[test]
    fn a_watcher_that_is_off_never_asks_for_a_read() {
        let mut watcher = Watcher::off();
        assert_eq!(watcher.status(), WatcherStatus::Off);
        assert!(!watcher.ready());
    }

    #[test]
    fn an_unwatchable_path_falls_back_to_the_timer() {
        // No parent directory to watch, so the platform watcher cannot start.
        let mut watcher = Watcher::start(Path::new("chat.db"));
        assert_eq!(watcher.status(), WatcherStatus::Polling);

        let start = Instant::now();
        watcher.last_poll = start;
        assert!(!watcher.ready_at(start + Duration::from_millis(500)));
        assert!(watcher.ready_at(start + POLL_EVERY));
        assert!(!watcher.ready_at(start + POLL_EVERY));
    }

    #[test]
    fn a_real_directory_is_watched_and_only_its_own_files_count() {
        let dir = std::env::temp_dir().join(format!("msgs-watch-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        let db = dir.join("chat.db");
        std::fs::write(&db, b"").expect("scratch database");

        let watcher = Watcher::start(&db);
        assert_eq!(watcher.status(), WatcherStatus::Watching);
        assert!(watcher.is_ours(&db));
        assert!(watcher.is_ours(&dir.join("chat.db-wal")));
        assert!(!watcher.is_ours(&dir.join("something-else.db")));

        drop(watcher);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The acceptance case, in miniature: something writes the WAL beside the
    /// database and the watcher asks for a read without anyone polling it.
    #[test]
    fn a_write_to_the_wal_reaches_the_watcher() {
        let dir = std::env::temp_dir().join(format!("msgs-watch-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        let db = dir.join("chat.db");
        std::fs::write(&db, b"").expect("scratch database");

        let mut watcher = Watcher::start(&db);
        assert_eq!(watcher.status(), WatcherStatus::Watching);
        // The backend needs a moment to arm before a write can be seen.
        std::thread::sleep(Duration::from_millis(250));
        std::fs::write(dir.join("chat.db-wal"), b"something landed").expect("write the sidecar");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut fired = false;
        while Instant::now() < deadline {
            if watcher.ready() {
                fired = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        drop(watcher);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(fired, "a write to the WAL must reach the watcher");
    }
}
