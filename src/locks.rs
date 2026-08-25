//! Cooperate with Claude Code's own advisory locks while mutating its files.
//!
//! Claude Code guards its OAuth token refresh and its `~/.claude.json` writes
//! with the npm `proper-lockfile` protocol: the lock artifact is a directory,
//! `mkdir` atomicity is the mutex, live holders touch the directory's mtime
//! every 5s, and a lock is only considered stale past its staleness window
//! (60s for the credential locks, 10s for the config lock).
//!
//! The refresh path takes two locks in order — the primary
//! `<config-home>/.oauth_refresh.lock`, then the legacy `~/.claude.lock` —
//! so we take the same pair in the same order. Holding them while swapping
//! credentials closes the one real race with a running Claude Code: a token
//! refresh reading credentials, refreshing over the network, and saving the
//! old account's token right over our swap.

use anyhow::{Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::paths;

const CREDENTIALS_STALENESS: Duration = Duration::from_secs(60);
const CONFIG_STALENESS: Duration = Duration::from_secs(10);
const TOUCH_INTERVAL: Duration = Duration::from_secs(3);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(9);

pub struct CredentialsLock {
    _primary: DirectoryLock,
    _legacy: DirectoryLock,
}

impl CredentialsLock {
    pub fn acquire() -> Result<CredentialsLock> {
        let primary = DirectoryLock::acquire(oauth_refresh_lock_dir(), CREDENTIALS_STALENESS)?;
        let legacy = DirectoryLock::acquire(legacy_credentials_lock_dir(), CREDENTIALS_STALENESS)?;
        Ok(CredentialsLock {
            _primary: primary,
            _legacy: legacy,
        })
    }
}

pub struct ConfigLock {
    _lock: DirectoryLock,
}

impl ConfigLock {
    pub fn acquire() -> Result<ConfigLock> {
        let lock = DirectoryLock::acquire(config_lock_dir(), CONFIG_STALENESS)?;
        Ok(ConfigLock { _lock: lock })
    }
}

fn oauth_refresh_lock_dir() -> PathBuf {
    paths::claude_config_home().join(".oauth_refresh.lock")
}

fn legacy_credentials_lock_dir() -> PathBuf {
    let home = paths::claude_config_home();
    home.with_file_name(format!(
        "{}.lock",
        home.file_name().unwrap().to_string_lossy()
    ))
}

fn config_lock_dir() -> PathBuf {
    let config = paths::global_config_path();
    config.with_file_name(format!(
        "{}.lock",
        config.file_name().unwrap().to_string_lossy()
    ))
}

struct DirectoryLock {
    path: PathBuf,
    stop_touching: Sender<()>,
    toucher: Option<JoinHandle<()>>,
}

impl DirectoryLock {
    fn acquire(path: PathBuf, staleness: Duration) -> Result<DirectoryLock> {
        claim(&path, staleness)?;
        let (stop_touching, stop_signal) = mpsc::channel();
        let toucher = spawn_toucher(path.clone(), stop_signal);
        Ok(DirectoryLock {
            path,
            stop_touching,
            toucher: Some(toucher),
        })
    }
}

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        let _ = self.stop_touching.send(());
        if let Some(toucher) = self.toucher.take() {
            let _ = toucher.join();
        }
        if let Err(error) = fs::remove_dir(&self.path) {
            eprintln!(
                "warning: failed to release lock {}: {error}",
                self.path.display()
            );
        }
    }
}

fn claim(path: &Path, staleness: Duration) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let start = Instant::now();
    loop {
        if fs::create_dir(path).is_ok() {
            return Ok(());
        }
        if start.elapsed() > DEFAULT_TIMEOUT {
            bail!(
                "could not acquire {} — Claude Code appears to be refreshing \
                 credentials, retry in a few seconds",
                path.display()
            );
        }
        match fs::metadata(path) {
            Ok(metadata) => {
                if held_longer_than(&metadata, staleness) {
                    take_over_stale_lock(path);
                } else {
                    thread::sleep(contention_backoff());
                }
            }
            Err(_) => continue,
        }
    }
}

fn held_longer_than(metadata: &fs::Metadata, staleness: Duration) -> bool {
    match metadata.modified().map(|it| it.elapsed()) {
        Ok(Ok(age)) => age > staleness,
        _ => false,
    }
}

fn take_over_stale_lock(path: &Path) {
    if fs::remove_dir(path).is_err() {
        thread::sleep(Duration::from_millis(50));
    }
}

fn contention_backoff() -> Duration {
    let jitter = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|it| it.subsec_millis() % 250)
        .unwrap_or(125);
    Duration::from_millis(250 + jitter as u64)
}

fn spawn_toucher(path: PathBuf, stop: mpsc::Receiver<()>) -> JoinHandle<()> {
    thread::spawn(move || {
        while stop.recv_timeout(TOUCH_INTERVAL) == Err(RecvTimeoutError::Timeout) {
            if touch(&path).is_err() {
                return;
            }
        }
    })
}

fn touch(path: &Path) -> std::io::Result<()> {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    let result = unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), std::ptr::null(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
