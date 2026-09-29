//! Best-effort aggregate feedback; all record access is under the claim-store lock.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

mod host;

// Give a newly admitted producer time to become visible before another increase.
const SAMPLE_MS: u64 = 1_000;
const GROW_MS: u64 = 2_000;
const STALE_MS: u64 = 10_000;
const RECORD_BYTES: u64 = 512;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Sample {
    busy: u64,
    total: u64,
    available: u64,
    memory: u64,
    cpus: u64,
    pressure: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Feedback {
    target: usize,
    sampled_ms: u64,
    adjusted_ms: u64,
    sample: Sample,
}

impl Feedback {
    fn initial(now: u64, sample: Sample) -> Self {
        Self {
            target: 1,
            sampled_ms: now,
            adjusted_ms: now,
            sample,
        }
    }

    fn advance(self, now: u64, sample: Sample, ceiling: usize, backlog: bool) -> Self {
        let mut next = Self {
            sampled_ms: now,
            sample,
            target: self.target.min(ceiling).max(1),
            ..self
        };
        let Some(elapsed) = now
            .checked_sub(self.sampled_ms)
            .filter(|ms| *ms <= STALE_MS)
        else {
            return Self::initial(now, sample);
        };
        let comparable = sample.memory > 0
            && sample.available <= sample.memory
            && sample.memory == self.sample.memory
            && sample.cpus == self.sample.cpus;
        let deltas = sample
            .total
            .checked_sub(self.sample.total)
            .zip(sample.busy.checked_sub(self.sample.busy));
        let Some((total, busy)) = deltas.filter(|(total, busy)| *total > 0 && busy <= total) else {
            return Self::initial(now, sample);
        };
        if !comparable || elapsed < SAMPLE_MS {
            return Self::initial(now, sample);
        }
        // Distinct watermarks avoid oscillation at a single memory threshold.
        if sample.pressure || sample.available <= sample.memory / 10 {
            next.target = next.target.div_ceil(2);
            next.adjusted_ms = now;
        } else if backlog
            && sample.available >= sample.memory / 5
            && u128::from(busy) * 5 <= u128::from(total) * 4
            && now
                .checked_sub(self.adjusted_ms)
                .is_some_and(|ms| ms >= GROW_MS)
        {
            next.target = next.target.saturating_add(1).min(ceiling).max(1);
            next.adjusted_ms = now;
        }
        next
    }

    fn read(path: &Path) -> Option<Self> {
        if !fs::symlink_metadata(path).ok()?.file_type().is_file() {
            return None;
        }
        let mut bytes = String::new();
        File::open(path)
            .ok()?
            .take(RECORD_BYTES + 1)
            .read_to_string(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > RECORD_BYTES {
            return None;
        }
        let fields: Vec<u64> = bytes
            .split_ascii_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        let [
            1,
            target,
            sampled_ms,
            adjusted_ms,
            busy,
            total,
            available,
            memory,
            cpus,
            pressure,
        ] = fields[..]
        else {
            return None;
        };
        let unavailable = target == 1
            && busy == 0
            && total == 0
            && available == 0
            && memory == 0
            && cpus == 0
            && pressure == 0;
        if target == 0
            || (target != 1 && adjusted_ms > sampled_ms)
            || pressure > 1
            || busy > total
            || available > memory
            || (!unavailable && (cpus == 0 || memory == 0))
        {
            return None;
        }
        Some(Self {
            target: usize::try_from(target).ok()?,
            sampled_ms,
            adjusted_ms,
            sample: Sample {
                busy,
                total,
                available,
                memory,
                cpus,
                pressure: pressure != 0,
            },
        })
    }

    fn write(self, path: &Path) -> std::io::Result<()> {
        if let Ok(metadata) = fs::symlink_metadata(path)
            && !metadata.file_type().is_file()
        {
            return Err(std::io::Error::other(
                "feedback record is not a regular file",
            ));
        }
        // A partial write is a reset, never durable learned capacity. No fsync is needed.
        writeln!(
            File::create(path)?,
            "1 {} {} {} {} {} {} {} {} {}",
            self.target,
            self.sampled_ms,
            self.adjusted_ms,
            self.sample.busy,
            self.sample.total,
            self.sample.available,
            self.sample.memory,
            self.sample.cpus,
            u8::from(self.sample.pressure)
        )
    }
}

pub(super) fn target(path: &Path, ceiling: usize, idle: bool, backlog: bool) -> usize {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|time| u64::try_from(time.as_millis()).ok());
    target_with(path, ceiling, idle, backlog, now, host::sample)
}

fn target_with(
    path: &Path,
    ceiling: usize,
    idle: bool,
    backlog: bool,
    now: Option<u64>,
    probe: impl FnOnce() -> Option<Sample>,
) -> usize {
    let Some(now) = now else {
        let _ = fs::remove_file(path);
        return 1;
    };
    let old = Feedback::read(path).filter(|old| old.sampled_ms <= now && old.adjusted_ms <= now);
    if idle {
        // Reset learning without turning fast, isolated commands into native probes.
        let reset = old.map_or_else(
            || Feedback::initial(now, Sample::default()),
            |old| Feedback {
                target: 1,
                adjusted_ms: now,
                ..old
            },
        );
        let _ = reset.write(path);
        return 1;
    }
    if let Some(old) = old
        && now
            .checked_sub(old.sampled_ms)
            .is_some_and(|ms| ms < SAMPLE_MS)
    {
        if old.write(path).is_err() {
            return 1;
        }
        return old.target.min(ceiling).max(1);
    }
    // Persist the unavailable result before probing, so failures share the cadence
    // and an unwritable record cannot retain a high target or trigger repeated probes.
    if Feedback::initial(now, Sample::default())
        .write(path)
        .is_err()
    {
        return 1;
    }
    let Some(sample) = probe() else {
        return 1;
    };
    let next = old.map_or_else(
        || Feedback::initial(now, sample),
        |old| old.advance(now, sample, ceiling, backlog),
    );
    if next.write(path).is_err() {
        return 1;
    }
    next.target
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kio-feedback-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn sample(second: u64) -> Sample {
        Sample {
            busy: second * 100,
            total: second * 800,
            available: 80,
            memory: 100,
            cpus: 8,
            pressure: false,
        }
    }

    #[test]
    fn sustained_backlog_grows_and_pressure_only_lowers_future_target() {
        let mut state = Feedback::initial(0, sample(0));
        for second in 1..=6 {
            state = state.advance(second * 1000, sample(second), 8, true);
        }
        assert_eq!(state.target, 4);
        let pressured = Sample {
            pressure: true,
            ..sample(7)
        };
        state = state.advance(7000, pressured, 8, true);
        assert_eq!(state.target, 2);
        state = state.advance(
            8000,
            Sample {
                available: 5,
                ..sample(8)
            },
            8,
            true,
        );
        assert_eq!(state.target, 1);
    }

    #[test]
    fn no_backlog_busy_cpu_and_low_memory_do_not_grow() {
        for (backlog, available, busy) in [(false, 80, 100), (true, 80, 790), (true, 15, 100)] {
            let mut state = Feedback::initial(0, sample(0));
            for second in 1..=10 {
                state = state.advance(
                    second * 1000,
                    Sample {
                        available,
                        busy: second * busy,
                        ..sample(second)
                    },
                    8,
                    backlog,
                );
            }
            assert_eq!(state.target, 1);
        }
    }

    #[test]
    fn ceilings_bad_clocks_and_incomparable_counters_reset_or_clamp() {
        let state = Feedback {
            target: 5,
            ..Feedback::initial(5000, sample(5))
        };
        assert_eq!(state.advance(6000, sample(6), 1, true).target, 1);
        for (now, input) in [
            (4000, sample(6)),
            (16000, sample(16)),
            (6000, sample(0)),
            (
                6000,
                Sample {
                    cpus: 4,
                    ..sample(6)
                },
            ),
            (
                6000,
                Sample {
                    available: 101,
                    ..sample(6)
                },
            ),
        ] {
            assert_eq!(state.advance(now, input, 8, true).target, 1);
        }
    }

    #[test]
    fn shared_record_throttles_successive_heads_and_resets_idle_or_torn_state() {
        let root = root("record");
        let path = root.join("feedback");
        Feedback::initial(0, sample(0)).write(&path).unwrap();
        assert_eq!(
            target_with(&path, 8, true, true, Some(0), || panic!(
                "idle reset does not probe"
            )),
            1
        );
        for second in 1..=6 {
            assert_eq!(
                target_with(&path, 8, false, true, Some(second * 1000), || Some(sample(
                    second
                ))),
                1 + second as usize / 2
            );
            assert_eq!(
                target_with(&path, 8, false, true, Some(second * 1000), || panic!(
                    "a second head must not sample again"
                )),
                1 + second as usize / 2
            );
        }
        assert_eq!(
            target_with(&path, 1, false, true, Some(6001), || panic!(
                "ceiling clamps without probing"
            )),
            1
        );
        assert_eq!(
            target_with(&path, 8, true, true, Some(6001), || panic!(
                "idle reset preserves sample cadence"
            )),
            1
        );
        assert_eq!(
            target_with(&path, 8, false, true, Some(6002), || panic!(
                "idle reset retained a fresh sample"
            )),
            1
        );
        assert_eq!(Feedback::read(&path).unwrap().adjusted_ms, 6001);
        for bytes in [
            "1 8",
            "2 8 0 0 0 0 0 1 1 0",
            "1 0 0 0 0 0 0 1 1 0",
            "1 4 0 1 0 0 80 100 8 0",
        ] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(
                target_with(&path, 8, false, true, Some(7000), || Some(sample(7))),
                1
            );
        }
        fs::write(&path, "8 ".repeat(1000)).unwrap();
        assert!(Feedback::read(&path).is_none());
        assert_eq!(target_with(&path, 8, false, true, Some(8000), || None), 1);
        assert_eq!(Feedback::read(&path).unwrap().target, 1);
        assert_eq!(
            target_with(&path, 8, false, true, Some(8001), || panic!(
                "unavailable samples are throttled"
            )),
            1
        );
        assert_eq!(
            target_with(&root, 8, false, true, Some(9000), || panic!(
                "unwritable state is checked before probing"
            )),
            1
        );
        let cold = root.join("cold");
        assert_eq!(
            target_with(&cold, 8, true, false, Some(0), || panic!(
                "an idle first command needs no sample"
            )),
            1
        );
        assert_eq!(
            target_with(&cold, 8, false, true, Some(1), || panic!(
                "cold unavailable state is throttled"
            )),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unwritable_record_rejects_even_a_fresh_high_target() {
        #[cfg(unix)]
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let root = root("readonly");
        let path = root.join("feedback");
        Feedback {
            target: 4,
            ..Feedback::initial(0, sample(0))
        }
        .write(&path)
        .unwrap();
        let original = fs::metadata(&path).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();
        let target = target_with(&path, 8, false, true, Some(1), || {
            panic!("fresh state does not need a probe")
        });
        fs::set_permissions(&path, original).unwrap();
        assert_eq!(target, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_store_lock_serializes_one_feedback_step_and_preserves_holders() {
        use crate::resource_admission::{ClaimKind, ClaimStore, SchedulerResource, StoreDecision};
        use std::num::NonZeroUsize;
        let root = root("store");
        let store = ClaimStore::open(root.clone(), SchedulerResource::Compiler).unwrap();
        let path = store.compiler_feedback_path();
        Feedback::initial(0, sample(0)).write(&path).unwrap();
        let (holder, _) = store
            .create_pending(
                NonZeroUsize::new(8).unwrap(),
                ClaimKind::Adaptive,
                |_, _| StoreDecision::Admit,
            )
            .unwrap();
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let store = store.clone();
                let path = path.clone();
                std::thread::spawn(move || {
                    store
                        .create_pending(
                            NonZeroUsize::new(8).unwrap(),
                            ClaimKind::Adaptive,
                            |_, _| {
                                assert_eq!(
                                    target_with(&path, 8, false, true, Some(2000), || Some(
                                        sample(2)
                                    )),
                                    2
                                );
                                StoreDecision::Wait
                            },
                        )
                        .unwrap()
                        .0
                })
            })
            .collect();
        let waiters: Vec<_> = handles
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(Feedback::read(&path).unwrap().target, 2);
        store
            .reconsider(&waiters[0], |snapshot, _| {
                assert_eq!(snapshot.active_total(), 1);
                assert_eq!(
                    target_with(&path, 8, false, true, Some(3000), || Some(Sample {
                        pressure: true,
                        ..sample(3)
                    })),
                    1
                );
                assert_eq!(snapshot.active_total(), 1);
                StoreDecision::Wait
            })
            .unwrap();
        drop((holder, waiters));
        fs::remove_dir_all(root).unwrap();
    }
}
