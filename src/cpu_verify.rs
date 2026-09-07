//! Native CPU candidate-chain verification.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use rayon::prelude::*;

use crate::{
    bitslice::{
        BITSLICE_WIDTH, index_to_fast_des_key, netntlmv1_bitslice_batch, with_bitslice_stack,
    },
    cpu::is_exact_des_key_match,
    cpu_schedule::weighted_batch_ranges,
    params::BYTE7_MASK,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuCandidate {
    pub start: u64,
    pub position: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuVerifyProgress {
    pub candidates_done: u64,
    pub candidates_total: u64,
    pub steps_done: u64,
    pub steps_total: u64,
    pub verified_keys: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuVerifyResult {
    pub keys: Vec<u64>,
    pub candidates_completed: u64,
    pub candidates_total: u64,
    pub steps_completed: u64,
    pub steps_total: u64,
    pub stopped_early: bool,
}

#[derive(Clone, Copy, Debug)]
struct ChainState {
    index: u64,
    position: u32,
    target_position: u32,
}

/// Reconstruct candidate chains entirely on the CPU using bitsliced DES.
///
/// A candidate at position `p` is checked at chain positions `0..=p`. Matches
/// compare the complete 64-bit DES ciphertext, then pass through the scalar
/// reference implementation as a final independent guard.
pub fn verify_candidates_with_progress<F>(
    candidates: &[CpuCandidate],
    target: [u8; 8],
    table_index: u32,
    all: bool,
    progress: F,
) -> CpuVerifyResult
where
    F: Fn(CpuVerifyProgress) + Sync,
{
    let candidates_total = candidates.len() as u64;
    let steps_total = candidates.iter().fold(0u64, |total, candidate| {
        total.saturating_add(u64::from(candidate.position) + 1)
    });
    if candidates.is_empty() {
        progress(CpuVerifyProgress {
            candidates_total,
            steps_total,
            ..CpuVerifyProgress::default()
        });
        return CpuVerifyResult {
            keys: Vec::new(),
            candidates_completed: 0,
            candidates_total,
            steps_completed: 0,
            steps_total,
            stopped_early: false,
        };
    }

    let stopped = AtomicBool::new(false);
    let candidates_done = AtomicU64::new(0);
    let steps_done = AtomicU64::new(0);
    let verified_keys = AtomicU64::new(0);
    let recovered = Mutex::new(Vec::<u64>::new());
    let reduction_offset = u64::from(table_index) << 16;
    let target_hash = u64::from_le_bytes(target);

    let threads = rayon::current_num_threads().max(1);
    let ranges = weighted_batch_ranges(candidates.len(), threads, BITSLICE_WIDTH, |index| {
        u64::from(candidates[index].position) + 1
    });

    let publish = |new_steps: u64, new_completed: u64, new_keys: u64| {
        let done = candidates_done.fetch_add(new_completed, Ordering::Relaxed) + new_completed;
        let steps = steps_done.fetch_add(new_steps, Ordering::Relaxed) + new_steps;
        let keys = verified_keys.fetch_add(new_keys, Ordering::Relaxed) + new_keys;
        progress(CpuVerifyProgress {
            candidates_done: done.min(candidates_total),
            candidates_total,
            steps_done: steps.min(steps_total),
            steps_total,
            verified_keys: keys,
        });
    };

    ranges.into_par_iter().for_each(|range| {
        if stopped.load(Ordering::Relaxed) {
            return;
        }
        let chunk = &candidates[range];
        let local_hits = with_bitslice_stack(|| {
            let mut active = chunk
                .iter()
                .map(|candidate| ChainState {
                    index: candidate.start,
                    position: 0,
                    target_position: candidate.position,
                })
                .collect::<Vec<_>>();
            let mut keys = vec![0u64; BITSLICE_WIDTH];
            let mut hashes = vec![0u64; BITSLICE_WIDTH];
            let mut local_hits = Vec::new();
            let mut pending_steps = 0u64;
            let mut pending_completed = 0u64;
            let mut pending_keys = 0u64;
            let mut last_report = std::time::Instant::now();

            while !active.is_empty() && !stopped.load(Ordering::Relaxed) {
                let active_len = active.len();
                let mut read = 0usize;
                let mut write = 0usize;
                while read < active_len {
                    if stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    let n = (active_len - read).min(BITSLICE_WIDTH);
                    for i in 0..n {
                        keys[i] = index_to_fast_des_key(active[read + i].index);
                    }
                    netntlmv1_bitslice_batch(&keys[..n], &mut hashes[..n]);
                    pending_steps = pending_steps.saturating_add(n as u64);

                    for (i, &hash) in hashes[..n].iter().enumerate() {
                        let slot = read + i;
                        let state = active[slot];
                        let exact_hit =
                            hash == target_hash && is_exact_des_key_match(state.index, &target);
                        if exact_hit {
                            local_hits.push(state.index);
                            pending_completed += 1;
                            pending_keys += 1;
                            if !all {
                                stopped.store(true, Ordering::Relaxed);
                                break;
                            }
                            continue;
                        }

                        let next_position = state.position + 1;
                        if next_position > state.target_position {
                            pending_completed += 1;
                        } else {
                            let next_index = hash
                                .wrapping_add(reduction_offset)
                                .wrapping_add(u64::from(state.position))
                                & BYTE7_MASK;
                            active[write] = ChainState {
                                index: next_index,
                                position: next_position,
                                target_position: state.target_position,
                            };
                            write += 1;
                        }
                    }
                    read += n;
                }
                active.truncate(write);

                if last_report.elapsed() >= std::time::Duration::from_millis(250)
                    && (pending_steps > 0 || pending_completed > 0 || pending_keys > 0)
                {
                    publish(pending_steps, pending_completed, pending_keys);
                    pending_steps = 0;
                    pending_completed = 0;
                    pending_keys = 0;
                    last_report = std::time::Instant::now();
                }
            }
            if pending_steps > 0 || pending_completed > 0 || pending_keys > 0 {
                publish(pending_steps, pending_completed, pending_keys);
            }
            local_hits
        });
        if !local_hits.is_empty() {
            recovered
                .lock()
                .expect("recovered-key lock")
                .extend(local_hits);
        }
    });

    let mut keys = recovered.into_inner().expect("recovered-key lock");
    keys.sort_unstable();
    keys.dedup();
    if !all && keys.len() > 1 {
        keys.truncate(1);
    }
    let candidates_completed = candidates_done
        .load(Ordering::Relaxed)
        .min(candidates_total);
    let steps_completed = steps_done.load(Ordering::Relaxed).min(steps_total);
    CpuVerifyResult {
        keys,
        candidates_completed,
        candidates_total,
        steps_completed,
        steps_total,
        stopped_early: stopped.load(Ordering::Relaxed) && candidates_completed < candidates_total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{byte7_hash_to_index, byte7_index_to_plaintext, netntlmv1_hash};
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };

    fn walk(start: u64, position: u32) -> (u64, [u8; 8]) {
        let mut index = start;
        for current in 0..=position {
            let hash = netntlmv1_hash(&byte7_index_to_plaintext(index));
            if current == position {
                return (index, hash);
            }
            index = byte7_hash_to_index(&hash, 0, current);
        }
        unreachable!()
    }

    #[test]
    fn finds_full_64_bit_hit_at_nonzero_position() {
        let (key, target) = walk(0x1234, 19);
        let candidates = [
            CpuCandidate {
                position: 18,
                start: 0x1234,
            },
            CpuCandidate {
                position: 19,
                start: 0x1234,
            },
        ];
        let result = verify_candidates_with_progress(&candidates, target, 0, true, |_| {});
        assert_eq!(result.keys, vec![key]);
        assert_eq!(result.candidates_completed, 2);
        assert_eq!(result.steps_completed, 39);
    }

    #[test]
    fn full_ciphertext_comparison_rejects_shared_prefix() {
        let (_, mut target) = walk(0x4321, 4);
        target[7] ^= 1;
        let result = verify_candidates_with_progress(
            &[CpuCandidate {
                position: 4,
                start: 0x4321,
            }],
            target,
            0,
            true,
            |_| {},
        );
        assert!(result.keys.is_empty());
        assert_eq!(result.steps_completed, 5);
    }

    #[test]
    fn all_deduplicates_hits_and_default_stops() {
        let start = 0x0088_46f7_eaee_8fb1;
        let target = netntlmv1_hash(&byte7_index_to_plaintext(start));
        let candidates = vec![
            CpuCandidate { position: 0, start },
            CpuCandidate { position: 0, start },
            CpuCandidate {
                position: 100_000,
                start: 1,
            },
        ];
        let all = verify_candidates_with_progress(&candidates[..2], target, 0, true, |_| {});
        assert_eq!(all.keys, vec![start]);
        let first = verify_candidates_with_progress(&candidates, target, 0, false, |_| {});
        assert_eq!(first.keys, vec![start]);
        assert!(first.steps_completed < first.steps_total);
    }

    #[test]
    fn cost_balanced_verify_batches_preserve_hits_and_accounting() {
        let known_start = 0x0088_46f7_eaee_8fb1;
        let target = netntlmv1_hash(&byte7_index_to_plaintext(known_start));
        let mut candidates = (0u64..1_537)
            .map(|index| CpuCandidate {
                start: 0x10_0000 + index,
                position: (index % 8) as u32,
            })
            .collect::<Vec<_>>();
        candidates[700] = CpuCandidate {
            start: known_start,
            position: 0,
        };
        let expected_steps = candidates
            .iter()
            .map(|candidate| u64::from(candidate.position) + 1)
            .sum::<u64>();

        let result = verify_candidates_with_progress(&candidates, target, 0, true, |_| {});

        assert_eq!(result.keys, vec![known_start]);
        assert_eq!(result.candidates_completed, candidates.len() as u64);
        assert_eq!(result.steps_completed, expected_steps);
    }

    #[test]
    #[ignore = "large CPU throughput regression; run in release mode"]
    fn large_false_alarm_schedule_completes() {
        let candidates = (0u64..262_144)
            .map(|index| CpuCandidate {
                start: index.wrapping_mul(0x9e37_79b9_7f4a_7c15) & BYTE7_MASK,
                position: (index as u32) & 4_095,
            })
            .collect::<Vec<_>>();
        let expected_steps = candidates
            .iter()
            .map(|candidate| u64::from(candidate.position) + 1)
            .sum::<u64>();
        let maximum_reported = AtomicU64::new(0);
        let started = Instant::now();
        let result = verify_candidates_with_progress(&candidates, [0; 8], 0, true, |progress| {
            maximum_reported.fetch_max(progress.steps_done, Ordering::Relaxed);
        });
        assert!(result.keys.is_empty());
        assert_eq!(result.candidates_completed, candidates.len() as u64);
        assert_eq!(result.steps_completed, expected_steps);
        assert_eq!(maximum_reported.load(Ordering::Relaxed), expected_steps);
        eprintln!(
            "large CPU false-alarm schedule: {} steps in {:.3}s ({:.3}M steps/s)",
            expected_steps,
            started.elapsed().as_secs_f64(),
            expected_steps as f64 / started.elapsed().as_secs_f64() / 1e6
        );
    }
}
