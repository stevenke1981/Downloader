use super::*;

/// Initial equal-size split.
pub fn plan_segments(total: u64, conns: u32) -> Vec<Seg> {
    let n = (total / MIN_SPLIT).clamp(1, conns.max(1) as u64);
    let step = total / n;
    (0..n)
        .map(|i| Seg::new(i * step, if i == n - 1 { total } else { (i + 1) * step }))
        .collect()
}

/// Pick work for a connection: an idle unfinished segment, or else split the busy
/// segment with the most bytes left. Returns the segment index (now marked busy).
pub fn claim(segs: &mut Vec<Seg>, can_split: bool) -> Option<usize> {
    if let Some(i) = segs.iter().position(|s| !s.busy && !s.done()) {
        segs[i].busy = true;
        return Some(i);
    }
    if !can_split {
        return None;
    }
    let (i, rem) = segs
        .iter()
        .enumerate()
        .filter(|(_, s)| s.busy && s.end != u64::MAX)
        .map(|(i, s)| (i, s.remaining()))
        .max_by_key(|x| x.1)?;
    if rem < 2 * MIN_SPLIT {
        return None;
    }
    let mid = segs[i].pos + rem / 2;
    let end = segs[i].end;
    segs[i].end = mid;
    segs.push(Seg {
        start: mid,
        pos: mid,
        end,
        busy: true,
    });
    Some(segs.len() - 1)
}
