//! Hardware-only shader validation.
//!
//! Run on a Vulkan GPU with:
//! `cargo test --release --test gpu_hardware -- --ignored --nocapture`

use ntlmrain::cpu::{byte7_index_to_plaintext, is_exact_des_key_match, precompute_one};
use ntlmrain::gpu::{
    BackendChoice, DispatchMode, FalseAlarmCandidate, GpuContext, GpuOptions, PrecomputeRequest,
    ShaderChoice, WorkgroupChoice,
};
use std::cell::Cell;
use std::time::Instant;

const CHAIN_LEN: u32 = 1_025;

fn decode_block(value: &str) -> [u8; 8] {
    hex::decode(value).unwrap().try_into().unwrap()
}

fn expected_endpoints(target: &[u8; 8]) -> Vec<u64> {
    let mut raw: Vec<u64> = (0..CHAIN_LEN - 1)
        .map(|gid| precompute_one(target, 0, CHAIN_LEN as u64, 0, 1, 0, gid))
        .collect();
    raw.reverse();
    raw
}

fn options(shader: ShaderChoice, workgroup: u32) -> GpuOptions {
    GpuOptions {
        backend: BackendChoice::Vulkan,
        device: "auto".into(),
        shader,
        workgroup: WorkgroupChoice::Fixed(workgroup),
        dispatch: DispatchMode::FixedSteps(64_000_000),
        retune: true,
        tuning_cache: None,
    }
}

#[test]
#[ignore = "requires a Vulkan-capable GPU"]
fn all_shader_families_match_cpu_and_verify_exact_hits() {
    let target = decode_block("727B4E35F947129E");
    let expected = expected_endpoints(&target);
    let key_index = 0x0088_46f7_eaee_8fb1;
    assert_eq!(
        hex::encode(byte7_index_to_plaintext(key_index)),
        "8846f7eaee8fb1"
    );
    assert!(is_exact_des_key_match(key_index, &target));

    for workgroup in [32, 64, 128] {
        for shader in [ShaderChoice::Compact, ShaderChoice::Expanded] {
            let context = GpuContext::create(&options(shader, workgroup)).unwrap();
            let request = PrecomputeRequest {
                target,
                chain_len: CHAIN_LEN,
                table_index: 0,
                checkpoint_steps: 256,
                dispatch: DispatchMode::FixedSteps(64_000_000),
            };
            let actual = context.precompute(&request).unwrap();
            assert_eq!(
                actual, expected,
                "{} WG{} precompute mismatch",
                context.selection.shader, workgroup
            );

            let hits = context
                .check_candidates(
                    &[FalseAlarmCandidate {
                        start: key_index,
                        position: 0,
                    }],
                    target,
                    0,
                    false,
                    is_exact_des_key_match,
                )
                .unwrap();
            assert_eq!(hits, vec![key_index]);

            let rejected = context
                .check_candidates(
                    &[FalseAlarmCandidate {
                        start: key_index,
                        position: 0,
                    }],
                    target,
                    0,
                    false,
                    |_index, _block| false,
                )
                .unwrap();
            assert!(rejected.is_empty());
        }
    }
}

#[test]
#[ignore = "requires a Vulkan-capable GPU"]
fn large_false_alarm_schedule_completes_and_shrinks() {
    let context = GpuContext::create(&options(ShaderChoice::Compact, 64)).unwrap();
    let candidates = (0u64..262_144)
        .map(|index| FalseAlarmCandidate {
            start: index.wrapping_mul(0x9e37_79b9_7f4a_7c15) & 0x00ff_ffff_ffff_ffff,
            position: (index as u32) & 4_095,
        })
        .collect::<Vec<_>>();
    let expected_steps = candidates
        .iter()
        .map(|candidate| u64::from(candidate.position) + 1)
        .sum::<u64>();
    let last_steps = Cell::new(0u64);
    let minimum_active = Cell::new(candidates.len());
    let started = Instant::now();
    let hits = context
        .check_candidates_with_progress(
            &candidates,
            [0; 8],
            0,
            true,
            |_index, _target| false,
            |progress| {
                assert!(progress.completed_steps >= last_steps.get());
                assert!(progress.completed_steps <= progress.total_steps);
                last_steps.set(progress.completed_steps);
                minimum_active.set(minimum_active.get().min(progress.active_candidates));
            },
        )
        .unwrap();
    assert!(hits.is_empty());
    assert_eq!(last_steps.get(), expected_steps);
    assert_eq!(minimum_active.get(), 0);
    eprintln!(
        "large false-alarm schedule: {} steps in {:.3}s ({:.3}G steps/s)",
        expected_steps,
        started.elapsed().as_secs_f64(),
        expected_steps as f64 / started.elapsed().as_secs_f64() / 1e9
    );
}
