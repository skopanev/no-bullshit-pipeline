//! An update cannot race a new background job. Sequentially consistent
//! registration ensures either the job is visible to the updater, or the job
//! observes the install flag and refuses to start. Guards release on errors.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Gate {
    jobs: AtomicUsize,
    installing: AtomicBool,
}

impl Gate {
    const fn new() -> Self {
        Self {
            jobs: AtomicUsize::new(0),
            installing: AtomicBool::new(false),
        }
    }

    fn work(&self) -> Result<WorkGuard<'_>, String> {
        self.jobs.fetch_add(1, Ordering::SeqCst);
        let guard = WorkGuard(self);
        if self.installing.load(Ordering::SeqCst) {
            return Err("An app update is in progress. Try again after restart.".into());
        }
        Ok(guard)
    }

    fn install(&self) -> Result<InstallGuard<'_>, String> {
        self.installing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "An app update is already in progress.".to_string())?;
        let guard = InstallGuard(self);
        if self.jobs.load(Ordering::SeqCst) != 0 {
            return Err("Finish transcription, imports and pipelines before updating.".into());
        }
        Ok(guard)
    }
}

pub(crate) struct WorkGuard<'a>(&'a Gate);
impl Drop for WorkGuard<'_> {
    fn drop(&mut self) {
        self.0.jobs.fetch_sub(1, Ordering::SeqCst);
    }
}
pub(crate) struct InstallGuard<'a>(&'a Gate);
impl Drop for InstallGuard<'_> {
    fn drop(&mut self) {
        self.0.installing.store(false, Ordering::SeqCst);
    }
}

static GATE: Gate = Gate::new();
pub(crate) fn work() -> Result<WorkGuard<'static>, String> {
    GATE.work()
}
pub(crate) fn install() -> Result<InstallGuard<'static>, String> {
    GATE.install()
}
pub(crate) fn is_installing() -> bool {
    GATE.installing.load(Ordering::SeqCst)
}
pub(crate) fn has_jobs() -> bool {
    GATE.jobs.load(Ordering::SeqCst) != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_job_blocks_install_and_failed_attempt_releases_its_flag() {
        let gate = Gate::new();
        let job = gate.work().unwrap();
        assert!(gate.install().is_err());
        assert!(!gate.installing.load(Ordering::SeqCst));
        drop(job);
        assert!(gate.install().is_ok());
    }
    #[test]
    fn install_blocks_jobs_and_second_installer_cannot_release_first() {
        let gate = Gate::new();
        let update = gate.install().unwrap();
        assert!(gate.work().is_err());
        assert_eq!(gate.jobs.load(Ordering::SeqCst), 0);
        assert!(gate.install().is_err());
        assert!(gate.installing.load(Ordering::SeqCst));
        drop(update);
        assert!(gate.work().is_ok());
    }
    #[test]
    fn simultaneous_job_and_install_never_both_acquire() {
        use std::sync::{Arc, Barrier};
        let gate = Arc::new(Gate::new());
        let barrier = Arc::new(Barrier::new(2));
        let successes = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for install in [false, true] {
                let (gate, barrier, successes) = (gate.clone(), barrier.clone(), successes.clone());
                scope.spawn(move || {
                    for _ in 0..128 {
                        barrier.wait();
                        let install_guard = if install { gate.install().ok() } else { None };
                        let work_guard = if !install { gate.work().ok() } else { None };
                        if install_guard.is_some() || work_guard.is_some() {
                            successes.fetch_add(1, Ordering::SeqCst);
                        }
                        barrier.wait();
                        assert!(successes.load(Ordering::SeqCst) <= 1);
                        barrier.wait();
                        drop((install_guard, work_guard));
                        barrier.wait();
                        if install {
                            successes.store(0, Ordering::SeqCst);
                        }
                        barrier.wait();
                    }
                });
            }
        });
    }
}
