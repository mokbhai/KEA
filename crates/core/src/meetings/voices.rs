//! Which participant ("Person N") each speech segment belongs to, by voice.
//!
//! ## Two passes, because the first one has to guess
//!
//! A live label is decided as the segment arrives, from the voices heard so
//! far, so [`VoiceClusters::assign`] is online: nearest centroid under a
//! cosine threshold. That decision rests on very little evidence, so one
//! person can end up split across two labels early in a meeting. The
//! stop-time pass, [`VoiceClusters::recluster`], sees every window of the
//! meeting at once, merges clusters that are the same voice, and says which
//! old label each surviving label came from, so a name the user typed follows
//! the voice rather than the number.
//!
//! ## Nothing here is persisted
//!
//! The embeddings are a voiceprint in all but name. They live in this value,
//! which is dropped with the meeting session, so "voice evidence does not
//! outlive the meeting" holds by ownership rather than by a cleanup step that
//! someone could forget.
//!
//! Pure: no audio device, no model, no database. Embeddings come from the
//! caller and nothing here assumes their dimension, so the same code works for
//! any speaker-embedding model. Every embedding in one `VoiceClusters` must come
//! from the same extractor, or the cosines compare nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use crate::transcript::speakers::speaker_key;

/// Cosine similarity a window needs to its nearest centroid to join it.
///
/// Set below the level at which two windows of one voice usually agree, so a
/// voice heard through a compressed call still reuses its label. A window that
/// falls short opens a new label, which the stop-time pass can undo. A starting
/// point, to be settled on recorded calls.
pub const JOIN_SIM: f32 = 0.55;

/// Cosine similarity at which the stop-time pass merges two clusters.
///
/// Above [`JOIN_SIM`] on purpose: a live split is cheap to undo at stop, but a
/// wrong merge puts two people under one name with no way to recover it.
pub const MERGE_SIM: f32 = 0.6;

/// Hard cap on participants in one meeting.
///
/// Past it, a window joins its nearest cluster whatever the similarity. A long
/// meeting with many quiet voices then keeps the label set bounded, at the cost
/// of merging voices that are genuinely different.
pub const MAX_CLUSTERS: usize = 16;

/// Windows a segment must yield before it can be given a voice.
///
/// Two windows is about two seconds of speech. One window is a single breath,
/// and a voice taken from it is mostly noise. Such a segment keeps its channel
/// label instead.
pub const MIN_WINDOWS: usize = 2;

/// Above this many windows the stop-time pass clusters per-segment means.
///
/// The pairwise similarity table is quadratic: at this size it is about 144 MB
/// of `f32`, which is the most a transient pass should ask for. Segment means
/// keep the cost linear in segments, and the window-level overlap that decides
/// the labels is unchanged.
const MAX_WINDOWS_FOR_WINDOW_LINKAGE: usize = 6_000;

/// Window length the embeddings are computed over.
///
/// 1.5 s is long enough for the embedding model to hear a voice, and short
/// enough that a segment of two or three seconds still yields two windows.
const WINDOW_MS: usize = 1_500;

/// Hop between windows: half a window, so adjacent windows overlap.
const HOP_MS: usize = 750;

/// RMS below which a window is silence and is not embedded.
///
/// Matches `attribution::SPEECH_FLOOR`. Silence embeds to something arbitrary,
/// and a few arbitrary embeddings would drag a real voice's centroid.
const SPEECH_FLOOR: f32 = 0.005;

/// One participant's running centroid.
#[derive(Debug, Clone, Default)]
struct Cluster {
    centroid: Vec<f32>,
    count: usize,
}

impl Cluster {
    /// Folds one window into the running mean.
    fn absorb(&mut self, embedding: &[f32]) {
        debug_assert!(self.count == 0 || self.centroid.len() == embedding.len());
        if self.count == 0 {
            self.centroid = embedding.to_vec();
        } else {
            let n = self.count as f32;
            for (c, e) in self.centroid.iter_mut().zip(embedding) {
                *c = (*c * n + e) / (n + 1.0);
            }
        }
        self.count += 1;
    }
}

/// The outcome of the stop-time pass. See [`VoiceClusters::recluster`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclusterResult {
    /// The key of every segment that had voice evidence, in sequence order.
    /// Segments without evidence are not listed and keep their live key.
    pub segments: Vec<(i32, String)>,
    /// Each old key that still has a segment, mapped to the key now holding
    /// most of its windows. A key that survives maps to itself. A key whose
    /// voice was merged into another maps to the survivor, so a user name on
    /// it can move with the voice. Old keys with no evidence are not listed.
    pub mapping: Vec<(String, String)>,
    /// Keys the pass created, with their default display name. Renumbering
    /// starts at the lowest free index, so a new label reads as the next
    /// person to have spoken rather than a gap in the sequence.
    pub fresh: Vec<(String, String)>,
}

/// Online participant clustering for one meeting.
#[derive(Debug, Clone, Default)]
pub struct VoiceClusters {
    clusters: Vec<Cluster>,
    /// Every window seen, tagged with its segment's sequence number. Kept so
    /// the stop-time pass can cluster the whole meeting, not just the centroids.
    evidence: Vec<(i32, Vec<f32>)>,
}

impl VoiceClusters {
    pub fn new() -> Self {
        Self::default()
    }

    /// Assigns a segment to a participant, and returns that participant's
    /// cluster index. The key is `speaker_key(index)` and the default name is
    /// [`person_display_name`].
    ///
    /// Each window is compared with the centroids as they stood before this
    /// segment, so the segment's own windows cannot vote for a label they
    /// created. The segment takes whichever cluster most of its windows chose.
    /// Unmatched windows vote for a new cluster, and that cluster opens only if
    /// it out-polls every existing one, so a single odd window does not open a
    /// participant. Returns `None` when there are fewer than [`MIN_WINDOWS`]
    /// windows, in which case nothing is recorded.
    pub fn assign(&mut self, sequence: i32, windows: Vec<Vec<f32>>) -> Option<usize> {
        if windows.len() < MIN_WINDOWS {
            return None;
        }

        let mut votes = vec![0usize; self.clusters.len()];
        let mut unmatched = 0usize;
        for window in &windows {
            match self.nearest(window) {
                Some((index, similarity))
                    if similarity >= JOIN_SIM || self.clusters.len() >= MAX_CLUSTERS =>
                {
                    votes[index] += 1;
                }
                _ => unmatched += 1,
            }
        }

        let (best_index, best_votes) =
            votes
                .iter()
                .enumerate()
                .fold((None, 0usize), |best, (index, &count)| {
                    if count > best.1 {
                        (Some(index), count)
                    } else {
                        best
                    }
                });

        // Existing evidence wins a tie: a new participant needs more windows
        // than an established one, not just as many.
        let winner = match best_index {
            Some(index) if best_votes >= unmatched => index,
            _ => {
                self.clusters.push(Cluster::default());
                self.clusters.len() - 1
            }
        };

        for window in windows {
            self.clusters[winner].absorb(&window);
            self.evidence.push((sequence, window));
        }
        Some(winner)
    }

    fn nearest(&self, embedding: &[f32]) -> Option<(usize, f32)> {
        self.clusters
            .iter()
            .enumerate()
            .map(|(index, cluster)| (index, cosine(&cluster.centroid, embedding)))
            .fold(None, |best, (index, similarity)| match best {
                // Ties keep the earlier cluster, so a label is stable.
                Some((_, best_similarity)) if best_similarity >= similarity => best,
                _ => Some((index, similarity)),
            })
    }

    /// Reclusters every window of the meeting, at stop, and maps the result
    /// back onto the labels the segments already carry.
    ///
    /// `existing_keys` is the key each segment carries now, as `(sequence, key)`.
    /// The pass is average-linkage agglomerative clustering over the windows,
    /// stopping at [`MERGE_SIM`]. Each segment then takes the cluster holding
    /// most of its windows.
    ///
    /// Labels are then decided greedily. Each old key keeps its label on the
    /// cluster where it has the most windows, largest overlap first, so a user
    /// name survives on the voice it was typed for. Clusters left over are
    /// numbered by first appearance, skipping any number a kept key holds.
    pub fn recluster(&self, existing_keys: &[(i32, Option<String>)]) -> ReclusterResult {
        self.recluster_with_limit(existing_keys, MAX_WINDOWS_FOR_WINDOW_LINKAGE)
    }

    fn recluster_with_limit(
        &self,
        existing_keys: &[(i32, Option<String>)],
        window_limit: usize,
    ) -> ReclusterResult {
        let mut by_segment: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
        for (window, (sequence, _)) in self.evidence.iter().enumerate() {
            by_segment.entry(*sequence).or_default().push(window);
        }
        let segments: Vec<(i32, Vec<usize>)> = by_segment.into_iter().collect();

        let window_label = label_windows(&self.evidence, &segments, window_limit);

        // A segment takes the label most of its windows agree on. Ties go to
        // the lower label, which keeps the result deterministic.
        let segment_label: Vec<usize> = segments
            .iter()
            .map(|(_, windows)| {
                let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
                for &window in windows {
                    *counts.entry(window_label[window]).or_default() += 1;
                }
                counts
                    .into_iter()
                    .fold(
                        None,
                        |best: Option<(usize, usize)>, (label, count)| match best {
                            Some((_, best_count)) if best_count >= count => best,
                            _ => Some((label, count)),
                        },
                    )
                    .map(|(label, _)| label)
                    .unwrap_or(0)
            })
            .collect();

        let mut old_key: BTreeMap<i32, Option<&str>> = BTreeMap::new();
        for (sequence, key) in existing_keys {
            old_key.entry(*sequence).or_insert(key.as_deref());
        }

        // Overlap in windows between each old key and each new label.
        let mut overlap: BTreeMap<(String, usize), usize> = BTreeMap::new();
        for (segment, (sequence, windows)) in segments.iter().enumerate() {
            if let Some(Some(key)) = old_key.get(sequence) {
                *overlap
                    .entry(((*key).to_string(), segment_label[segment]))
                    .or_default() += windows.len();
            }
        }

        // Greedy by overlap, largest first. The key tie-break keeps the choice
        // stable across runs.
        let mut pairs: Vec<(usize, String, usize)> = overlap
            .iter()
            .map(|((key, label), &count)| (count, key.clone(), *label))
            .collect();
        pairs.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
        });

        let mut label_key: BTreeMap<usize, String> = BTreeMap::new();
        let mut kept: BTreeSet<String> = BTreeSet::new();
        for (_, key, label) in pairs {
            if label_key.contains_key(&label) || kept.contains(&key) {
                continue;
            }
            kept.insert(key.clone());
            label_key.insert(label, key);
        }

        // Leftover labels take the lowest numbers no kept key holds, in the
        // order a reader meets them.
        let mut fresh = Vec::new();
        let mut next = 0u32;
        for &label in &segment_label {
            if label_key.contains_key(&label) {
                continue;
            }
            let index = loop {
                let candidate = next;
                next += 1;
                if !kept.contains(&speaker_key(candidate)) {
                    break candidate;
                }
            };
            let key = speaker_key(index);
            fresh.push((key.clone(), person_display_name(index)));
            label_key.insert(label, key);
        }

        // Each old key follows the label holding most of its windows. For a
        // kept key that is its own label; for an absorbed key it is the survivor.
        let mut best_for_key: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for ((key, label), &count) in &overlap {
            let entry = best_for_key.entry(key.clone()).or_insert((0, *label));
            if count > entry.0 {
                *entry = (count, *label);
            }
        }
        let mapping = best_for_key
            .into_iter()
            .map(|(key, (_, label))| (key, label_key[&label].clone()))
            .collect();

        let segments = segments
            .iter()
            .enumerate()
            .map(|(segment, (sequence, _))| (*sequence, label_key[&segment_label[segment]].clone()))
            .collect();

        ReclusterResult {
            segments,
            mapping,
            fresh,
        }
    }
}

/// The default name for a participant, shown until someone names them.
///
/// "Person N" rather than "Speaker N", because the notes prompt uses a bare
/// "Speaker" to mean "could not be attributed". "Speaker 3" next to it would
/// blur exactly the distinction the prompt relies on.
pub fn person_display_name(index: u32) -> String {
    format!("Person {}", index + 1)
}

/// The windows of `samples` to embed, as sample ranges.
///
/// Only whole windows are returned, so the tail shorter than one window is
/// dropped. A segment under 1.5 s therefore yields nothing and keeps its
/// channel label, which is the intended outcome for too little speech. Windows
/// whose RMS is under the speech floor are skipped.
pub fn embedding_windows(samples: &[f32], rate_hz: u32) -> Vec<Range<usize>> {
    if rate_hz == 0 {
        return Vec::new();
    }
    let window = rate_hz as usize * WINDOW_MS / 1000;
    let hop = (rate_hz as usize * HOP_MS / 1000).max(1);
    if window == 0 || samples.len() < window {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut start = 0;
    while start + window <= samples.len() {
        let range = start..start + window;
        if rms(&samples[range.clone()]) >= SPEECH_FLOOR {
            ranges.push(range);
        }
        start += hop;
    }
    ranges
}

/// The cluster label of every window.
///
/// Clusters the windows themselves while the meeting is small enough.
/// Otherwise it clusters each segment's mean and gives every window its
/// segment's label. Segment means are weighted by window count, so a long
/// segment still counts for what it is.
fn label_windows(
    evidence: &[(i32, Vec<f32>)],
    segments: &[(i32, Vec<usize>)],
    window_limit: usize,
) -> Vec<usize> {
    if evidence.len() <= window_limit {
        let vectors: Vec<Vec<f32>> = evidence.iter().map(|(_, e)| e.clone()).collect();
        return average_linkage(&vectors, &vec![1; vectors.len()]);
    }

    let mut segment_of = vec![0usize; evidence.len()];
    let mut means = Vec::with_capacity(segments.len());
    let mut sizes = Vec::with_capacity(segments.len());
    for (segment, (_, windows)) in segments.iter().enumerate() {
        let dim = evidence[windows[0]].1.len();
        let mut mean = vec![0.0f32; dim];
        for &window in windows {
            segment_of[window] = segment;
            for (m, e) in mean.iter_mut().zip(&evidence[window].1) {
                *m += e;
            }
        }
        for m in &mut mean {
            *m /= windows.len() as f32;
        }
        means.push(mean);
        sizes.push(windows.len());
    }

    let labels = average_linkage(&means, &sizes);
    segment_of.iter().map(|&segment| labels[segment]).collect()
}

/// Weighted average-linkage agglomerative clustering, stopping at [`MERGE_SIM`].
///
/// Returns a compact label for every input, numbered in first-appearance order.
/// The similarity between two clusters is the size-weighted mean of their
/// member similarities, which the Lance–Williams update keeps current without
/// re-reading the members. The nearest-neighbour cache means only the rows
/// touched by a merge are rescanned, so the pass is close to quadratic rather
/// than cubic.
fn average_linkage(vectors: &[Vec<f32>], sizes: &[usize]) -> Vec<usize> {
    let n = vectors.len();
    if n == 0 {
        return Vec::new();
    }

    let unit: Vec<Vec<f32>> = vectors.iter().map(|v| normalized(v)).collect();
    let mut sim = vec![0.0f32; n * n];
    for i in 0..n {
        for j in (i + 1)..n {
            let s = dot(&unit[i], &unit[j]);
            sim[i * n + j] = s;
            sim[j * n + i] = s;
        }
    }

    let mut size: Vec<f32> = sizes.iter().map(|&s| s as f32).collect();
    let mut active = vec![true; n];
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut best: Vec<(f32, usize)> = (0..n)
        .map(|i| nearest_active(&sim, &active, n, i))
        .collect();

    loop {
        let mut pick: Option<usize> = None;
        for i in 0..n {
            if !active[i] || best[i].0 < MERGE_SIM {
                continue;
            }
            match pick {
                Some(p) if best[p].0 >= best[i].0 => {}
                _ => pick = Some(i),
            }
        }
        let Some(i) = pick else { break };
        let j = best[i].1;

        let (si, sj) = (size[i], size[j]);
        for k in 0..n {
            if !active[k] || k == i || k == j {
                continue;
            }
            let merged = (si * sim[i * n + k] + sj * sim[j * n + k]) / (si + sj);
            sim[i * n + k] = merged;
            sim[k * n + i] = merged;
        }
        size[i] = si + sj;
        active[j] = false;
        let moved = std::mem::take(&mut members[j]);
        members[i].extend(moved);

        best[i] = nearest_active(&sim, &active, n, i);
        for k in 0..n {
            if !active[k] || k == i {
                continue;
            }
            if best[k].1 == i || best[k].1 == j {
                best[k] = nearest_active(&sim, &active, n, k);
            } else if sim[k * n + i] > best[k].0 {
                best[k] = (sim[k * n + i], i);
            }
        }
    }

    let mut root = vec![0usize; n];
    for i in 0..n {
        if active[i] {
            for &m in &members[i] {
                root[m] = i;
            }
        }
    }
    let mut label_of_root = vec![usize::MAX; n];
    let mut next = 0usize;
    (0..n)
        .map(|item| {
            let r = root[item];
            if label_of_root[r] == usize::MAX {
                label_of_root[r] = next;
                next += 1;
            }
            label_of_root[r]
        })
        .collect()
}

fn nearest_active(sim: &[f32], active: &[bool], n: usize, i: usize) -> (f32, usize) {
    let mut best = (f32::NEG_INFINITY, usize::MAX);
    for k in 0..n {
        if active[k] && k != i && sim[i * n + k] > best.0 {
            best = (sim[i * n + k], k);
        }
    }
    best
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalized(v: &[f32]) -> Vec<f32> {
    let norm = dot(v, v).sqrt();
    if norm == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

/// Cosine similarity, or 0 for mismatched or zero vectors. A mismatch then
/// opens a new cluster instead of comparing unlike spaces.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let na = dot(a, a).sqrt();
    let nb = dot(b, b).sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot(a, b) / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIM: usize = 32;

    /// A unit vector along one axis: a voice with no overlap with any other.
    fn axis(i: usize) -> Vec<f32> {
        let mut v = vec![0.0; DIM];
        v[i] = 1.0;
        v
    }

    /// Mostly `base`, with a little of `other` mixed in. Cosine to `base`
    /// stays high, so it reads as the same voice.
    fn jitter(base: &[f32], other: &[f32]) -> Vec<f32> {
        base.iter()
            .zip(other)
            .map(|(b, o)| b * 0.95 + o * 0.05)
            .collect()
    }

    fn state(evidence: &[(i32, Vec<f32>)]) -> VoiceClusters {
        VoiceClusters {
            clusters: Vec::new(),
            evidence: evidence.to_vec(),
        }
    }

    fn key(sequence: i32, key: &str) -> (i32, Option<String>) {
        (sequence, Some(key.to_string()))
    }

    #[test]
    fn the_same_voice_later_reuses_its_cluster() {
        let mut clusters = VoiceClusters::new();
        let a = axis(0);
        let wobble = jitter(&a, &axis(1));
        assert_eq!(clusters.assign(1, vec![a.clone(), a.clone()]), Some(0));
        assert_eq!(clusters.assign(2, vec![wobble.clone(), wobble]), Some(0));
        assert_eq!(clusters.clusters.len(), 1);
        assert_eq!(speaker_key(0), "spk0");
    }

    #[test]
    fn a_new_voice_opens_a_new_cluster_with_the_next_label() {
        let mut clusters = VoiceClusters::new();
        clusters.assign(1, vec![axis(0), axis(0)]);
        let index = clusters.assign(2, vec![axis(1), axis(1)]).unwrap();
        assert_eq!(index, 1);
        assert_eq!(speaker_key(index as u32), "spk1");
        assert_eq!(person_display_name(index as u32), "Person 2");
    }

    #[test]
    fn a_segment_with_too_few_windows_gets_no_voice_and_is_not_recorded() {
        let mut clusters = VoiceClusters::new();
        assert_eq!(clusters.assign(1, vec![axis(0)]), None);
        assert_eq!(clusters.assign(2, Vec::new()), None);
        assert!(clusters.clusters.is_empty());
        assert!(clusters.evidence.is_empty());
    }

    #[test]
    fn one_odd_window_does_not_open_a_participant() {
        let mut clusters = VoiceClusters::new();
        clusters.assign(1, vec![axis(0), axis(0)]);
        // Two windows of a known voice and one stranger: the known voice wins.
        let index = clusters.assign(2, vec![axis(0), axis(1), axis(0)]);
        assert_eq!(index, Some(0));
        assert_eq!(clusters.clusters.len(), 1);
    }

    #[test]
    fn a_segment_of_mostly_new_windows_opens_a_cluster() {
        let mut clusters = VoiceClusters::new();
        clusters.assign(1, vec![axis(0), axis(0)]);
        let index = clusters.assign(2, vec![axis(1), axis(1), axis(0)]);
        assert_eq!(index, Some(1));
        assert_eq!(clusters.clusters.len(), 2);
    }

    #[test]
    fn the_cluster_cap_holds_and_extra_voices_join_the_nearest() {
        let mut clusters = VoiceClusters::new();
        for i in 0..(MAX_CLUSTERS + 4) {
            let index = clusters.assign(i as i32, vec![axis(i), axis(i)]).unwrap();
            assert!(index < MAX_CLUSTERS, "voice {i} got label {index}");
        }
        assert_eq!(clusters.clusters.len(), MAX_CLUSTERS);
    }

    #[test]
    fn a_split_voice_is_merged_and_keeps_the_surviving_key() {
        // Two labels that really are one voice. Segment 1 kept spk0, segment 2
        // kept spk1; the pass has to join them and say spk1 now means spk0.
        let near = jitter(&axis(0), &axis(1));
        let state = state(&[(1, axis(0)), (1, axis(0)), (2, near.clone()), (2, near)]);
        let result = state.recluster(&[key(1, "spk0"), key(2, "spk1")]);

        assert_eq!(
            result.segments,
            vec![(1, "spk0".to_string()), (2, "spk0".to_string())]
        );
        assert_eq!(
            result.mapping,
            vec![
                ("spk0".to_string(), "spk0".to_string()),
                ("spk1".to_string(), "spk0".to_string()),
            ]
        );
        assert!(result.fresh.is_empty());
    }

    #[test]
    fn the_per_segment_fallback_merges_the_same_way() {
        let near = jitter(&axis(0), &axis(1));
        let state = state(&[(1, axis(0)), (1, axis(0)), (2, near.clone()), (2, near)]);
        // A window limit of zero forces the above-6,000 path: segment means.
        let result = state.recluster_with_limit(&[key(1, "spk0"), key(2, "spk1")], 0);
        assert_eq!(
            result.segments,
            vec![(1, "spk0".to_string()), (2, "spk0".to_string())]
        );
    }

    #[test]
    fn distinct_voices_stay_distinct_after_the_pass() {
        let state = state(&[(1, axis(0)), (1, axis(0)), (2, axis(1)), (2, axis(1))]);
        let result = state.recluster(&[key(1, "spk0"), key(2, "spk1")]);
        assert_eq!(
            result.segments,
            vec![(1, "spk0".to_string()), (2, "spk1".to_string())]
        );
        assert_eq!(
            result.mapping,
            vec![
                ("spk0".to_string(), "spk0".to_string()),
                ("spk1".to_string(), "spk1".to_string()),
            ]
        );
    }

    #[test]
    fn a_kept_key_survives_renumbering_of_an_earlier_unmapped_cluster() {
        // Segment 1 has no old key and comes first. Segment 2 holds spk0, which
        // is kept, so the unmapped voice must skip 0 and become spk1.
        let state = state(&[(1, axis(0)), (1, axis(0)), (2, axis(1)), (2, axis(1))]);
        let result = state.recluster(&[(1, None), key(2, "spk0")]);
        assert_eq!(
            result.segments,
            vec![(1, "spk1".to_string()), (2, "spk0".to_string())]
        );
        assert_eq!(
            result.fresh,
            vec![("spk1".to_string(), "Person 2".to_string())]
        );
    }

    #[test]
    fn an_unmapped_first_voice_takes_the_lowest_free_number() {
        let state = state(&[(1, axis(0)), (1, axis(0)), (2, axis(1)), (2, axis(1))]);
        let result = state.recluster(&[(1, None), key(2, "spk1")]);
        assert_eq!(
            result.segments,
            vec![(1, "spk0".to_string()), (2, "spk1".to_string())]
        );
        assert_eq!(
            result.fresh,
            vec![("spk0".to_string(), "Person 1".to_string())]
        );
    }

    #[test]
    fn an_empty_meeting_reclusters_to_nothing() {
        let result = VoiceClusters::new().recluster(&[]);
        assert!(result.segments.is_empty());
        assert!(result.mapping.is_empty());
        assert!(result.fresh.is_empty());
    }

    #[test]
    fn embedding_windows_are_one_and_a_half_seconds_every_three_quarters() {
        // 4.5 s at 16 kHz, all speech. Full windows start at 0, 0.75 s, 1.5 s,
        // 2.25 s and 3.0 s; the last one ends exactly at the buffer's end.
        let samples = vec![0.1f32; 72_000];
        let ranges = embedding_windows(&samples, 16_000);
        let starts: Vec<usize> = ranges.iter().map(|r| r.start).collect();
        assert_eq!(starts, vec![0, 12_000, 24_000, 36_000, 48_000]);
        assert!(ranges.iter().all(|r| r.len() == 24_000));
    }

    #[test]
    fn embedding_windows_skip_silent_windows() {
        // First 1.5 s silent, the rest speech. The window at 0 is silent and
        // is skipped; the one at 0.75 s straddles the boundary and is not.
        let mut samples = vec![0.0f32; 24_000];
        samples.extend(vec![0.1f32; 24_000]);
        let ranges = embedding_windows(&samples, 16_000);
        let starts: Vec<usize> = ranges.iter().map(|r| r.start).collect();
        assert_eq!(starts, vec![12_000, 24_000]);
    }

    #[test]
    fn a_segment_shorter_than_one_window_yields_nothing() {
        let samples = vec![0.1f32; 16_000];
        assert!(embedding_windows(&samples, 16_000).is_empty());
        assert!(embedding_windows(&samples, 0).is_empty());
    }
}
