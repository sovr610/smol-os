// SPDX-License-Identifier: GPL-3.0-or-later
//! The machine's load in the history (D-092): every minute (or five, or
//! never: `[center] sample`), also while the command center is closed, so
//! its charts cover the days the journal keeps. The thread sleeps in the
//! kernel between samples; one sample costs about a millisecond.

use std::sync::Arc;
use std::time::Duration;

use termos_core::config::SampleEvery;
use termos_core::journal::{Event, Journal};
use termos_hw::{Sample, Source};

/// The journal record for a sample.
pub fn event(s: &Sample) -> Event {
    Event::System {
        cpu: s.cpu.total,
        memory_used: s.memory.used,
        memory_total: s.memory.total,
        rx_per_sec: s.net.rx_per_sec,
        tx_per_sec: s.net.tx_per_sec,
    }
}

/// Starts recording, unless `every` is off. The thread ends with the
/// process.
pub fn start(journal: Arc<Journal>, every: SampleEvery) {
    let Some(interval) = every.interval() else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("center-sampler".into())
        .spawn(move || {
            let mut source = match termos_hw::Sampler::new() {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("the command center cannot sample this machine: {e}");
                    return;
                }
            };
            // CPU use is measured between two readings: the first one
            // only starts the clock.
            let _ = source.sample();
            run(&mut source, &journal, interval, || {
                std::thread::sleep(interval);
                true
            });
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start the command center's sampler: {e}");
    }
}

/// Samples after each `wait` that returns `true`.
fn run(
    source: &mut dyn Source,
    journal: &Journal,
    _interval: Duration,
    mut wait: impl FnMut() -> bool,
) {
    while wait() {
        journal.record(event(&source.sample()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_hw::{CpuSample, Inventory, MemorySample, NetSample};

    struct Fixture(f32);

    impl Source for Fixture {
        fn inventory(&mut self) -> Inventory {
            Inventory {
                os: "fixture".into(),
                kernel: None,
                arch: "x86_64".into(),
                host: None,
                cpu: termos_hw::CpuInfo {
                    model: "fixture".into(),
                    cores: None,
                    threads: 1,
                },
                memory_total: 0,
                swap_total: 0,
                gpus: Vec::new(),
            }
        }
        fn sample(&mut self) -> Sample {
            self.0 += 10.0;
            Sample {
                cpu: CpuSample {
                    total: self.0,
                    ..CpuSample::default()
                },
                memory: MemorySample {
                    used: 4 << 30,
                    total: 16 << 30,
                    ..MemorySample::default()
                },
                net: NetSample {
                    rx_per_sec: 1000,
                    tx_per_sec: 50,
                },
                ..Sample::default()
            }
        }
    }

    #[test]
    fn each_tick_records_one_sample() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = Journal::new(dir.path(), 7);
        let mut ticks = 3;
        run(&mut Fixture(0.0), &journal, Duration::ZERO, || {
            ticks -= 1;
            ticks >= 0
        });
        let cpus: Vec<f32> = journal
            .read(0)
            .iter()
            .filter_map(|r| match r.event {
                Event::System { cpu, .. } => Some(cpu),
                _ => None,
            })
            .collect();
        assert_eq!(cpus, [10.0, 20.0, 30.0]);
        assert!(SampleEvery::Off.interval().is_none());
    }
}
