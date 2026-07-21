pub(super) fn merge_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable_by_key(|&(start, end)| (start, end));
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if let Some((previous_start, previous_end)) = merged.last_mut()
            && start <= *previous_end
        {
            *previous_start = (*previous_start).min(start);
            *previous_end = (*previous_end).max(end);
        } else {
            merged.push((start, end));
        }
    }

    merged
}
