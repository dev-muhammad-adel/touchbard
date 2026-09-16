//! Linux logind lifecycle integration for the DRM backend.

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::wakefd::WakeFd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SleepEvent {
    PrepareForSleep(bool),
}

/// Watches logind's `PrepareForSleep` signal and owns the sleep delay inhibitor.
pub(crate) struct SleepWatcher {
    events: Receiver<SleepEvent>,
    monitor: Child,
    inhibitor: Option<(Child, ChildStdin)>,
}

impl SleepWatcher {
    pub(crate) fn start(wake: &WakeFd) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut monitor = Command::new("dbus-monitor")
            .args([
                "--system",
                "type='signal',interface='org.freedesktop.login1.Manager',member='PrepareForSleep'",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = monitor.stdout.take().ok_or("dbus-monitor has no stdout")?;
        let (sender, events) = mpsc::channel();
        let wake_fd = wake.raw_fd();
        thread::Builder::new()
            .name("touchbard-logind".into())
            .spawn(move || {
                let mut signal = false;
                for line in BufReader::new(stdout).lines().flatten() {
                    if line.contains("member=PrepareForSleep") {
                        signal = true;
                    } else if signal && line.trim() == "boolean true" {
                        let _ = sender.send(SleepEvent::PrepareForSleep(true));
                        wake_raw_fd(wake_fd);
                        signal = false;
                    } else if signal && line.trim() == "boolean false" {
                        let _ = sender.send(SleepEvent::PrepareForSleep(false));
                        wake_raw_fd(wake_fd);
                        signal = false;
                    }
                }
            })?;

        let mut watcher = Self {
            events,
            monitor,
            inhibitor: None,
        };
        watcher.acquire_inhibitor()?;
        Ok(watcher)
    }

    pub(crate) fn try_recv(&self) -> Option<SleepEvent> {
        self.events.try_recv().ok()
    }

    pub(crate) fn release_inhibitor(&mut self) {
        if let Some((mut child, stdin)) = self.inhibitor.take() {
            drop(stdin);
            let _ = child.wait();
        }
    }

    pub(crate) fn acquire_inhibitor(
        &mut self,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.inhibitor.is_some() {
            return Ok(());
        }
        let mut child = Command::new("systemd-inhibit")
            .args([
                "--what=sleep",
                "--who=touchbard",
                "--mode=delay",
                "--why=Release Touch Bar DRM resources before suspend",
                "/bin/sh",
                "-c",
                "read _ || true",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().ok_or("systemd-inhibit has no stdin")?;
        self.inhibitor = Some((child, stdin));
        Ok(())
    }
}

impl Drop for SleepWatcher {
    fn drop(&mut self) {
        self.release_inhibitor();
        let _ = self.monitor.kill();
        let _ = self.monitor.wait();
    }
}

fn wake_raw_fd(fd: i32) {
    let _ = unsafe { libc::eventfd_write(fd, 1) };
}
