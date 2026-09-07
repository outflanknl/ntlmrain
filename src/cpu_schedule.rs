//! Shared cost-aware batching for native CPU work.

use std::ops::Range;

const BATCHES_PER_WORKER: usize = 16;

/// Split a contiguous item range into aligned batches with approximately equal work.
///
/// `weight_at` supplies the expected DES steps for an item. Batch boundaries are
/// aligned so the bitslice engines retain full lanes, except for the final tail.
pub(crate) fn weighted_batch_ranges<F>(
    item_count: usize,
    workers: usize,
    alignment: usize,
    mut weight_at: F,
) -> Vec<Range<usize>>
where
    F: FnMut(usize) -> u64,
{
    if item_count == 0 {
        return Vec::new();
    }

    let alignment = alignment.max(1);
    let block_count = item_count.div_ceil(alignment);
    let batch_count = block_count.min(workers.max(1).saturating_mul(BATCHES_PER_WORKER));
    if batch_count == 1 {
        return std::iter::once(0..item_count).collect();
    }

    let block_weights = (0..block_count)
        .map(|block| {
            let start = block * alignment;
            let end = (start + alignment).min(item_count);
            (start..end).fold(0u64, |total, item| total.saturating_add(weight_at(item)))
        })
        .collect::<Vec<_>>();
    let total_weight = block_weights
        .iter()
        .copied()
        .fold(0u64, u64::saturating_add);

    if total_weight == 0 {
        return (0..batch_count)
            .map(|batch| {
                let start_block = batch * block_count / batch_count;
                let end_block = (batch + 1) * block_count / batch_count;
                block_range(start_block, end_block, alignment, item_count)
            })
            .collect();
    }

    let mut ranges = Vec::with_capacity(batch_count);
    let mut start_block = 0usize;
    let mut remaining_weight = total_weight;

    while ranges.len() + 1 < batch_count {
        let batches_left = batch_count - ranges.len();
        let target_weight = remaining_weight.div_ceil(batches_left as u64);
        let last_end_block = block_count - (batches_left - 1);
        let mut end_block = start_block;
        let mut batch_weight = 0u64;

        while end_block < last_end_block
            && (end_block == start_block || batch_weight < target_weight)
        {
            batch_weight = batch_weight.saturating_add(block_weights[end_block]);
            end_block += 1;
        }

        ranges.push(block_range(start_block, end_block, alignment, item_count));
        start_block = end_block;
        remaining_weight = remaining_weight.saturating_sub(batch_weight);
    }

    ranges.push(block_range(start_block, block_count, alignment, item_count));
    ranges
}

fn block_range(
    start_block: usize,
    end_block: usize,
    alignment: usize,
    item_count: usize,
) -> Range<usize> {
    start_block * alignment..(end_block * alignment).min(item_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_complete_aligned_coverage(
        ranges: &[Range<usize>],
        item_count: usize,
        alignment: usize,
    ) {
        assert!(!ranges.is_empty());
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges.last().unwrap().end, item_count);
        for (index, range) in ranges.iter().enumerate() {
            assert!(range.start < range.end);
            assert_eq!(range.start % alignment, 0);
            if index + 1 < ranges.len() {
                assert_eq!(range.end % alignment, 0);
                assert_eq!(range.end, ranges[index + 1].start);
            }
        }
    }

    #[test]
    fn weighted_batches_cover_every_item_and_keep_lane_alignment() {
        let ranges = weighted_batch_ranges(65_537, 4, 512, |item| item as u64);
        assert_eq!(ranges.len(), 64);
        assert_complete_aligned_coverage(&ranges, 65_537, 512);
    }

    #[test]
    fn weighted_batches_reduce_the_triangular_precompute_tail() {
        let item_count = 65_536usize;
        let workers = 4usize;
        let ranges = weighted_batch_ranges(item_count, workers, 512, |item| item as u64);
        let maximum_balanced = ranges
            .iter()
            .map(|range| range.clone().map(|item| item as u64).sum::<u64>())
            .max()
            .unwrap();

        let legacy_chunk = item_count.div_ceil(workers).next_multiple_of(512);
        let maximum_legacy = (0..item_count)
            .step_by(legacy_chunk)
            .map(|start| {
                (start..(start + legacy_chunk).min(item_count))
                    .map(|item| item as u64)
                    .sum::<u64>()
            })
            .max()
            .unwrap();

        assert!(maximum_balanced * 8 < maximum_legacy);
    }

    #[test]
    fn zero_weight_work_is_split_evenly_without_empty_batches() {
        let ranges = weighted_batch_ranges(2_049, 2, 512, |_| 0);
        assert_complete_aligned_coverage(&ranges, 2_049, 512);
        assert_eq!(ranges.len(), 5);
    }
}
