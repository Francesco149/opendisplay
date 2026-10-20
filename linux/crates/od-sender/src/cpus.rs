//! How many threads the encoder and the colour conversion use by default.
//!
//! Both split each frame into equal pieces (x264's sliced threads, the
//! conversion's bands) and wait for the slowest one. On a heterogeneous CPU
//! (Apple M1 Pro under Asahi: 6 performance + 2 efficiency cores; Intel
//! hybrid; big.LITTLE) a piece that lands on an efficiency core finishes
//! last, so the default is one thread per performance core there, and every
//! core elsewhere. `--threads` overrides it.

use std::path::Path;
use std::sync::OnceLock;

use tracing::info;

/// What sysfs says about one online core.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Core {
    /// `cpu_capacity` (scheduler capacity, 1024 = the biggest core).
    pub capacity: Option<u32>,
    /// `cpufreq/cpuinfo_max_freq`, kHz.
    pub max_freq: Option<u32>,
}

/// A core counts as a performance core within this share of the fastest.
/// Generous enough for Intel's favoured cores (a few hundred MHz above the
/// other P-cores) and well above efficiency cores (about half to 0.75).
const PERFORMANCE_SHARE: f64 = 0.8;

/// The number of performance cores, or `None` when the cores are all alike
/// (or sysfs says too little to tell).
pub fn performance_cores(cores: &[Core]) -> Option<usize> {
    let by = |f: fn(&Core) -> Option<u32>| -> Option<Vec<u32>> {
        cores
            .iter()
            .map(f)
            .collect::<Option<Vec<u32>>>()
            .filter(|v| !v.is_empty())
    };
    // Capacity is the scheduler's own ranking; fall back to frequency.
    let values = by(|c| c.capacity).or_else(|| by(|c| c.max_freq))?;
    let max = *values.iter().max()?;
    let min = *values.iter().min()?;
    if max == 0 || min as f64 >= max as f64 * PERFORMANCE_SHARE {
        return None;
    }
    Some(
        values
            .iter()
            .filter(|&&v| v as f64 >= max as f64 * PERFORMANCE_SHARE)
            .count(),
    )
}

/// The online cores under `root` (`/sys/devices/system/cpu`).
pub fn read_cores(root: &Path) -> Vec<Core> {
    let read = |p: std::path::PathBuf| -> Option<u32> {
        std::fs::read_to_string(p).ok()?.trim().parse().ok()
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut cores: Vec<(u32, Core)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id: u32 = name.strip_prefix("cpu")?.parse().ok()?;
            let dir = e.path();
            // cpu0 often has no `online` file: it cannot go offline.
            if read(dir.join("online")) == Some(0) {
                return None;
            }
            Some((
                id,
                Core {
                    capacity: read(dir.join("cpu_capacity")),
                    max_freq: read(dir.join("cpufreq/cpuinfo_max_freq")),
                },
            ))
        })
        .collect();
    cores.sort_by_key(|(id, _)| *id);
    cores.into_iter().map(|(_, c)| c).collect()
}

/// Threads for the encoder and the conversion when `--threads` is 0: the
/// performance cores on a heterogeneous CPU, else every core we may run on.
pub fn default_threads() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        let allowed = std::thread::available_parallelism().map_or(4, |n| n.get());
        let cores = read_cores(Path::new("/sys/devices/system/cpu"));
        match performance_cores(&cores) {
            Some(p) => {
                let n = p.min(allowed).max(1);
                info!(
                    "{n} encode threads: one per performance core ({p} of {} cores)",
                    cores.len()
                );
                n
            }
            None => allowed,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(v: &[u32]) -> Vec<Core> {
        v.iter()
            .map(|&c| Core {
                capacity: Some(c),
                max_freq: None,
            })
            .collect()
    }

    fn freq(v: &[u32]) -> Vec<Core> {
        v.iter()
            .map(|&f| Core {
                capacity: None,
                max_freq: Some(f),
            })
            .collect()
    }

    #[test]
    fn m1_pro_under_asahi_has_six_performance_cores() {
        // This machine's sysfs: cpu0-1 Icestorm, cpu2-7 Firestorm.
        let cores: Vec<Core> = [(485, 2_064_000); 2]
            .into_iter()
            .chain([(1024, 3_036_000); 6])
            .map(|(c, f)| Core {
                capacity: Some(c),
                max_freq: Some(f),
            })
            .collect();
        assert_eq!(performance_cores(&cores), Some(6));
    }

    #[test]
    fn homogeneous_or_unknown_means_all_cores() {
        assert_eq!(performance_cores(&cap(&[1024; 8])), None);
        assert_eq!(performance_cores(&freq(&[3_000_000; 16])), None);
        assert_eq!(performance_cores(&[]), None);
        assert_eq!(performance_cores(&[Core::default(); 4]), None);
    }

    #[test]
    fn frequency_is_the_fallback() {
        // Intel hybrid without cpu_capacity: 8 P-cores (two favoured at 5.0
        // GHz, six at 4.7), 8 E-cores at 3.6 GHz.
        let mut f = vec![5_000_000, 5_000_000];
        f.extend([4_700_000; 6]);
        f.extend([3_600_000; 8]);
        assert_eq!(performance_cores(&freq(&f)), Some(8));
    }

    #[test]
    fn favoured_cores_alone_are_not_heterogeneous() {
        // Only a turbo bump on some cores: all count as performance cores.
        let mut f = vec![5_000_000, 5_000_000];
        f.extend([4_700_000; 6]);
        assert_eq!(performance_cores(&freq(&f)), None);
    }

    #[test]
    fn partial_capacity_falls_back_to_frequency() {
        let cores = vec![
            Core {
                capacity: Some(1024),
                max_freq: Some(3_000_000),
            },
            Core {
                capacity: None,
                max_freq: Some(1_500_000),
            },
        ];
        assert_eq!(performance_cores(&cores), Some(1));
    }

    #[test]
    fn reads_a_sysfs_tree_and_skips_offline_cores() {
        let root = std::env::temp_dir().join(format!("od-cpus-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (id, cap, online) in [
            (0, "485", None),
            (1, "485", Some("1")),
            (2, "1024", Some("1")),
            (3, "1024", Some("0")),
        ] {
            let d = root.join(format!("cpu{id}"));
            std::fs::create_dir_all(d.join("cpufreq")).unwrap();
            std::fs::write(d.join("cpu_capacity"), format!("{cap}\n")).unwrap();
            if let Some(o) = online {
                std::fs::write(d.join("online"), o).unwrap();
            }
        }
        // Not cores.
        std::fs::create_dir_all(root.join("cpufreq")).unwrap();
        std::fs::create_dir_all(root.join("cpuidle")).unwrap();
        let cores = read_cores(&root);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            cores.iter().map(|c| c.capacity).collect::<Vec<_>>(),
            vec![Some(485), Some(485), Some(1024)]
        );
        assert_eq!(performance_cores(&cores), Some(1));
    }

    #[test]
    fn reads_this_machines_sysfs_without_failing() {
        // Whatever the host is, reading must not panic, and every core
        // listed is online.
        let cores = read_cores(Path::new("/sys/devices/system/cpu"));
        let _ = performance_cores(&cores);
        assert!(read_cores(Path::new("/nonexistent")).is_empty());
    }
}
