//! Work that must not block the UI thread. It runs elsewhere and comes back as an `AppEvent::Apply`.

use std::sync::mpsc;
use std::thread::JoinHandle;

use tokio::sync::mpsc::UnboundedSender;

use super::{App, AppEvent};
use crate::secrets::vault::SaveJob;

pub type Apply = Box<dyn FnOnce(&mut App) + Send>;

impl App {
    /// Runs `work` on the blocking pool and hands its result to `done` on the UI thread.
    pub fn spawn_job<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(&mut App, T) + Send + 'static) {
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let out = work();
            let _ = tx.send(AppEvent::Apply(Box::new(move |app| done(app, out))));
        });
    }

    /// Writes the vault in the background; `done` is the toast shown once it is on disk.
    pub fn save_vault(&mut self, done: Option<&'static str>) {
        match self.vault.save_job() {
            Ok(job) => self.vault_writer.submit(job, done),
            Err(e) => self.error(format!("Could not save the vault: {e:#}")),
        }
    }
}

type VaultJob = (SaveJob, Option<&'static str>);

/// Writes vault snapshots one after the other (each one is a scrypt run of ~0.3 s).
pub struct VaultWriter {
    jobs: Option<mpsc::Sender<VaultJob>>,
    thread: Option<JoinHandle<()>>,
}

impl VaultWriter {
    pub fn start(tx: UnboundedSender<AppEvent>) -> Self {
        let (jobs, rx) = mpsc::channel::<VaultJob>();
        let thread = std::thread::Builder::new()
            .name("vault-writer".into())
            .spawn(move || {
                for (job, done) in rx {
                    let result = job.run();
                    let apply: Apply = Box::new(move |app| match result {
                        Ok(saved) => {
                            app.vault.apply_saved(saved);
                            if let Some(msg) = done {
                                app.info(msg);
                            }
                        }
                        Err(e) => app.error(format!("Could not save the vault: {e:#}")),
                    });
                    let _ = tx.send(AppEvent::Apply(apply));
                }
            })
            .expect("could not start the vault writer thread");
        VaultWriter { jobs: Some(jobs), thread: Some(thread) }
    }

    fn submit(&self, job: SaveJob, done: Option<&'static str>) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send((job, done));
        }
    }

    /// Waits for the saves still queued; called before quitting.
    pub fn finish(&mut self) {
        self.jobs = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
