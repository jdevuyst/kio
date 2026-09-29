use super::Sample;

pub(super) fn sample() -> Option<Sample> {
    let sample = native()?;
    let parallelism = crate::available_parallelism::get().ok()? as u64;
    // Whole-host idle time cannot establish spare capacity inside a smaller CPU domain.
    (sample.cpus == parallelism
        && sample.memory > 0
        && sample.available <= sample.memory
        && sample.busy <= sample.total)
        .then_some(sample)
}

#[cfg(target_os = "linux")]
fn native() -> Option<Sample> {
    let cpu = read_bounded("/proc/stat")?;
    let memory = read_bounded("/proc/meminfo")?;
    let mut sample = linux_sample(&cpu, &memory)?;
    if let Some(pressure) = read_bounded("/proc/pressure/memory") {
        sample.pressure = pressure
            .lines()
            .find(|line| line.starts_with("some "))?
            .split_ascii_whitespace()
            .find_map(|field| field.strip_prefix("avg10="))?
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))?
            >= 1.0;
    }
    // Consume an exposed local cgroup v2 restriction without resolving hidden ancestors.
    if let Some(groups) = read_bounded("/proc/self/cgroup")
        && let Some(group) = groups.lines().find_map(|line| line.strip_prefix("0::/"))
        && !group.split('/').any(|part| part == "..")
    {
        let root = std::path::Path::new("/sys/fs/cgroup").join(group);
        if let Some(limit) = read_bounded(root.join("memory.max"))
            && limit.trim() != "max"
        {
            let limit = limit.trim().parse::<u64>().ok()?;
            let used = read_bounded(root.join("memory.current"))?
                .trim()
                .parse::<u64>()
                .ok()?;
            sample.memory = sample.memory.min(limit);
            sample.available = sample.available.min(limit.saturating_sub(used));
        }
    }
    Some(sample)
}

#[cfg(target_os = "linux")]
fn read_bounded(path: impl AsRef<std::path::Path>) -> Option<String> {
    use std::io::Read;
    let mut bytes = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(65_537)
        .read_to_string(&mut bytes)
        .ok()?;
    (bytes.len() <= 65_536).then_some(bytes)
}

#[cfg(any(target_os = "linux", test))]
fn linux_sample(cpu: &str, memory: &str) -> Option<Sample> {
    let mut fields = cpu.lines().next()?.split_ascii_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    // Guest time is already in user/nice. I/O wait is not usable idle capacity.
    let ticks: Vec<u64> = fields
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if ticks.len() != 8 {
        return None;
    }
    let total = ticks
        .iter()
        .try_fold(0_u64, |sum, value| sum.checked_add(*value))?;
    let memory_value = |key| -> Option<u64> {
        let mut fields = memory
            .lines()
            .find_map(|line| line.strip_prefix(key))?
            .split_ascii_whitespace();
        let value = fields.next()?.parse::<u64>().ok()?.checked_mul(1024)?;
        (fields.next()? == "kB" && fields.next().is_none()).then_some(value)
    };
    Some(Sample {
        total,
        busy: total.checked_sub(ticks[3])?,
        cpus: cpu
            .lines()
            .filter(|line| {
                line.strip_prefix("cpu")
                    .is_some_and(|rest| rest.as_bytes().first().is_some_and(u8::is_ascii_digit))
            })
            .count() as u64,
        available: memory_value("MemAvailable:")?,
        memory: memory_value("MemTotal:")?,
        pressure: false,
    })
}

#[cfg(target_os = "macos")]
fn native() -> Option<Sample> {
    // Bind port acquisition/release together: libc omits the release entry point.
    unsafe extern "C" {
        fn mach_host_self() -> libc::mach_port_t;
        static mut mach_task_self_: libc::mach_port_t;
        fn mach_port_deallocate(
            task: libc::mach_port_t,
            name: libc::mach_port_t,
        ) -> libc::kern_return_t;
    }

    let mut cpu = std::mem::MaybeUninit::<libc::host_cpu_load_info_data_t>::zeroed();
    let mut vm = std::mem::MaybeUninit::<libc::vm_statistics64_data_t>::zeroed();
    let mut cpu_count = libc::HOST_CPU_LOAD_INFO_COUNT;
    let mut vm_count = libc::HOST_VM_INFO64_COUNT;
    // The buffers/counts have the native ABI sizes; read them only after successful calls.
    let (cpu, vm, page, cpus) = unsafe {
        let host = mach_host_self();
        let cpu_status = libc::host_statistics(
            host,
            libc::HOST_CPU_LOAD_INFO,
            cpu.as_mut_ptr().cast(),
            &mut cpu_count,
        );
        let vm_status = libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            vm.as_mut_ptr().cast(),
            &mut vm_count,
        );
        mach_port_deallocate(mach_task_self_, host);
        if cpu_status != libc::KERN_SUCCESS
            || vm_status != libc::KERN_SUCCESS
            || cpu_count != libc::HOST_CPU_LOAD_INFO_COUNT
            || vm_count != libc::HOST_VM_INFO64_COUNT
        {
            return None;
        }
        (
            cpu.assume_init(),
            vm.assume_init(),
            libc::sysconf(libc::_SC_PAGESIZE),
            libc::sysconf(libc::_SC_NPROCESSORS_ONLN),
        )
    };
    let page = u64::try_from(page).ok().filter(|page| *page > 0)?;
    let cpus = u64::try_from(cpus).ok().filter(|cpus| *cpus > 0)?;
    let mut memory = 0_u64;
    let mut length = std::mem::size_of_val(&memory);
    if unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut memory as *mut u64).cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || length != std::mem::size_of_val(&memory)
    {
        return None;
    }
    let total = cpu
        .cpu_ticks
        .iter()
        .map(|tick| u64::from(*tick))
        .sum::<u64>();
    // Free already includes speculative pages; reclaimable pages are not free headroom.
    let available = u64::from(vm.free_count).checked_mul(page)?.min(memory);
    Some(Sample {
        total,
        busy: total.checked_sub(u64::from(cpu.cpu_ticks[libc::CPU_STATE_IDLE as usize]))?,
        available,
        memory,
        cpus,
        pressure: false,
    })
}

#[cfg(windows)]
fn native() -> Option<Sample> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows::Win32::System::Threading::{
        ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetActiveProcessorGroupCount, GetSystemTimes,
    };
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let mut memory = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    let cpus = unsafe {
        if GetActiveProcessorGroupCount() != 1 {
            return None;
        }
        GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()?;
        GlobalMemoryStatusEx(&mut memory).ok()?;
        GetActiveProcessorCount(ALL_PROCESSOR_GROUPS)
    };
    // GetSystemTimes covers only the caller's group above this native API boundary.
    if cpus == 0 || cpus > 64 {
        return None;
    }
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    let total = ticks(kernel).checked_add(ticks(user))?;
    Some(Sample {
        total,
        busy: total.checked_sub(ticks(idle))?,
        available: memory.ullAvailPhys.min(memory.ullAvailPageFile),
        memory: memory.ullTotalPhys.min(memory.ullTotalPageFile),
        cpus: u64::from(cpus),
        pressure: false,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native() -> Option<Sample> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_counter_units_and_guest_overlap_are_explicit() {
        let sample = linux_sample(
            "cpu 10 2 3 40 5 6 7 8 100 100\ncpu0 0\ncpu1 0\n",
            "MemTotal: 100 kB\nMemAvailable: 40 kB\n",
        )
        .unwrap();
        assert_eq!(sample.total, 81);
        assert_eq!(sample.busy, 41);
        assert_eq!(sample.cpus, 2);
        assert_eq!(sample.available, 40 * 1024);
        assert!(linux_sample("cpu 1 2", "").is_none());
        assert!(
            linux_sample(
                "cpu 1 2 3 4 5 6 7 8",
                "MemTotal: 100 bytes\nMemAvailable: 40 kB"
            )
            .is_none()
        );
    }

    #[test]
    fn native_sample_is_valid_or_explicitly_unavailable() {
        let observed = sample();
        eprintln!("native feedback sample: {observed:?}");
        if let Some(sample) = observed {
            assert!(sample.memory > 0 && sample.available <= sample.memory);
            assert!(sample.busy <= sample.total && sample.cpus > 0);
        }
    }
}
