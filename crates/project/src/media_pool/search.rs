//! The media pool's search index: every item's name, file name, tags, camera metadata,
//! codec, resolution class, kind, label and card tokenised into one sorted token map
//! with postings, so a query is a few ordered lookups (exact and prefix matches) plus a
//! bounded edit-distance scan over tokens of similar length (typos).
//!
//! The index is a derived cache, never saved. It remembers the pool version it describes
//! (an O(1) clone of the item map), so [`SearchIndex::is_current`] is a pointer
//! comparison, and after an edit [`SearchIndex::refresh`] re-indexes only the items the
//! persistent map says changed.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound;
use std::sync::Arc;

use imbl::OrdMap;
use imbl::ordmap::DiffItem;

use super::{ColourLabel, MediaItem, MediaKind, MediaPool};
use crate::id::{BinId, MediaId};

/// Field weights: a name match outranks a tag match outranks a metadata match.
const NAME: u8 = 3;
const TAG: u8 = 2;
const OTHER: u8 = 1;

/// A search result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub id: MediaId,
    pub score: f32,
}

/// Splits text into lower-case tokens at anything that is not a letter or digit, and
/// between letters and digits (`A014_C003` is `a014`, `c003`, and also `a`, `014`,
/// `c`, `003` so either spelling finds it). `H.264` becomes `h264`.
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let joined = join_dotted_codecs(&text.to_lowercase());
    for word in joined
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        out.push(word.to_string());
        let mut run = String::new();
        let mut digit = None;
        let mut parts = Vec::new();
        for c in word.chars() {
            let d = c.is_ascii_digit();
            if digit.is_some_and(|prev| prev != d) {
                parts.push(std::mem::take(&mut run));
            }
            run.push(c);
            digit = Some(d);
        }
        parts.push(run);
        if parts.len() > 1 {
            out.extend(parts);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `h.264` and `h.265` as people type them.
fn join_dotted_codecs(text: &str) -> String {
    text.replace("h.264", "h264").replace("h.265", "h265")
}

/// Friendly names for codecs FFmpeg names tersely.
fn codec_aliases(codec: &str) -> &'static [&'static str] {
    match codec {
        "h264" => &["h264", "avc"],
        "hevc" => &["hevc", "h265"],
        "prores" => &["prores"],
        "av1" => &["av1"],
        "vp9" => &["vp9"],
        "mjpeg" => &["jpeg", "mjpeg"],
        "dnxhd" => &["dnxhd", "dnxhr"],
        "aac" => &["aac"],
        "pcm_s16le" | "pcm_s24le" | "pcm_s32le" | "pcm_f32le" | "pcm_s16be" | "pcm_s24be" => {
            &["pcm", "wav"]
        }
        _ => &[],
    }
}

/// A search as the user states it: free text plus filters (the toolbar's chips).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// Words that must all match (see [`SearchIndex::search`]).
    pub text: String,
    pub kind: Option<MediaKind>,
    pub label: Option<ColourLabel>,
    /// Tags every result carries (ignoring case).
    pub tags: Vec<String>,
    /// `Some(true)`: only offline items; `Some(false)`: only online ones.
    pub offline: Option<bool>,
    /// Only items in this bin (and its bins when `recursive`).
    pub bin: Option<BinId>,
    pub recursive: bool,
}

impl Query {
    /// Reads filters typed into the search field: `type:video`, `label:amber` (or the
    /// label's name), `tag:interview`, `is:offline`, `is:online`. Everything else is
    /// free text. An unknown filter stays in the text, so it simply finds nothing.
    pub fn parse(input: &str, pool: &MediaPool) -> Self {
        let mut query = Self::default();
        let mut text = Vec::new();
        for word in input.split_whitespace() {
            let Some((key, value)) = word.split_once(':') else {
                text.push(word);
                continue;
            };
            let known = match key.to_ascii_lowercase().as_str() {
                "type" | "kind" => MediaKind::from_key(value).map(|k| query.kind = Some(k)),
                "label" => ColourLabel::from_key(value)
                    .or_else(|| {
                        ColourLabel::ALL.into_iter().find(|l| {
                            pool.label_name(*l)
                                .is_some_and(|n| n.eq_ignore_ascii_case(value))
                        })
                    })
                    .map(|l| query.label = Some(l)),
                "tag" if !value.is_empty() => {
                    query.tags.push(value.to_string());
                    Some(())
                }
                "is" if value.eq_ignore_ascii_case("offline") => {
                    query.offline = Some(true);
                    Some(())
                }
                "is" if value.eq_ignore_ascii_case("online") => {
                    query.offline = Some(false);
                    Some(())
                }
                _ => None,
            };
            if known.is_none() {
                text.push(word);
            }
        }
        query.text = text.join(" ");
        query
    }

    /// True when nothing narrows the pool.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
            && self.kind.is_none()
            && self.label.is_none()
            && self.tags.is_empty()
            && self.offline.is_none()
            && self.bin.is_none()
    }

    /// Whether `item` passes the filters (not the text).
    pub fn admits(&self, pool: &MediaPool, item: &MediaItem) -> bool {
        self.kind.is_none_or(|k| item.kind == k)
            && self.label.is_none_or(|l| item.label == Some(l))
            && self.tags.iter().all(|t| item.has_tag(t))
            && self
                .offline
                .is_none_or(|off| item.status.is_offline() == off)
            && self.bin.is_none_or(|bin| {
                if self.recursive {
                    pool.is_within(item.bin, bin)
                } else {
                    item.bin == bin
                }
            })
    }
}

/// The index; see the module docs.
#[derive(Clone, Debug, Default)]
pub struct SearchIndex {
    /// Token to the items carrying it, with the best field weight, sorted by id.
    postings: BTreeMap<String, Vec<(MediaId, u8)>>,
    /// Tokens by character count, for the typo scan.
    by_len: HashMap<usize, BTreeSet<String>>,
    /// Each item's tokens, to take them out again when it changes.
    terms: HashMap<MediaId, Vec<(String, u8)>>,
    /// The pool version this index describes.
    items: OrdMap<MediaId, Arc<MediaItem>>,
    label_names: OrdMap<ColourLabel, String>,
}

/// Every token of `item` with its best field weight, sorted by token.
fn item_terms(item: &MediaItem, pool: &MediaPool) -> Vec<(String, u8)> {
    let mut out: Vec<(String, u8)> = Vec::new();
    let mut add = |text: &str, weight: u8| {
        out.extend(tokens(text).into_iter().map(|t| (t, weight)));
    };
    add(&item.name, NAME);
    if let Some(name) = item.path.file_name() {
        add(&name.to_string_lossy(), NAME);
    }
    for tag in &item.tags {
        add(tag, TAG);
    }
    let m = &item.metadata;
    for text in [
        &m.camera_make,
        &m.camera_model,
        &m.lens,
        &m.reel,
        &m.timecode,
        &m.created,
    ]
    .into_iter()
    .flatten()
    {
        add(text, OTHER);
    }
    let f = &item.facts;
    if let Some(codec) = &f.video_codec {
        add(codec, OTHER);
        for alias in codec_aliases(codec) {
            add(alias, OTHER);
        }
    }
    for text in [&f.video_profile, &f.hdr].into_iter().flatten() {
        add(text, OTHER);
    }
    if let Some(codec) = &f.audio_codec {
        for alias in codec_aliases(codec) {
            add(alias, OTHER);
        }
    }
    let long = f.width.max(f.height);
    if long >= 3800 {
        add("4k uhd", OTHER);
    } else if long >= 1900 {
        add("hd 1080p", OTHER);
    } else if long >= 1200 {
        add("720p", OTHER);
    }
    add(
        match item.kind {
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
            MediaKind::Still => "still photo picture",
            MediaKind::Sequence => "sequence",
        },
        OTHER,
    );
    if let Some(label) = item.label {
        add(label.key(), OTHER);
        if let Some(name) = pool.label_name(label) {
            add(name, OTHER);
        }
    }
    if let Some(card) = &item.card {
        add(card, OTHER);
    }
    if item.status.is_offline() {
        add("offline", OTHER);
    }
    // Best weight first within each token, then keep one entry per token.
    out.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

impl SearchIndex {
    /// Indexes every item of `pool`. 10,000 items take tens of milliseconds, so the app
    /// builds it once per project (off the UI thread if it likes; the index is `Send`)
    /// and then follows edits with [`SearchIndex::refresh`].
    pub fn build(pool: &MediaPool) -> Self {
        let mut entries: HashMap<String, Vec<(MediaId, u8)>> = HashMap::new();
        let mut terms = HashMap::with_capacity(pool.len());
        for item in pool.items() {
            let item_terms = item_terms(item, pool);
            for (token, weight) in &item_terms {
                // Items come in id order, so each list stays sorted.
                entries
                    .entry(token.clone())
                    .or_default()
                    .push((item.id, *weight));
            }
            terms.insert(item.id, item_terms);
        }
        let mut by_len: HashMap<usize, BTreeSet<String>> = HashMap::new();
        for token in entries.keys() {
            by_len
                .entry(token.chars().count())
                .or_default()
                .insert(token.clone());
        }
        Self {
            postings: entries.into_iter().collect(),
            by_len,
            terms,
            items: pool.items.clone(),
            label_names: pool.label_names.clone(),
        }
    }

    /// True when `pool` is the version this index was built from, or one whose items
    /// and label names were not edited since.
    pub fn is_current(&self, pool: &MediaPool) -> bool {
        self.items.ptr_eq(&pool.items) && self.labels_current(pool)
    }

    fn labels_current(&self, pool: &MediaPool) -> bool {
        self.label_names.ptr_eq(&pool.label_names) || self.label_names == pool.label_names
    }

    /// Brings the index up to date with `pool` and returns whether anything changed.
    /// Only the items that differ are re-indexed: the item maps share every untouched
    /// node, so finding them skips the shared parts, and labelling one clip of ten
    /// thousand costs microseconds. Renaming a colour label rebuilds the index.
    pub fn refresh(&mut self, pool: &MediaPool) -> bool {
        if !self.labels_current(pool) {
            *self = Self::build(pool);
            return true;
        }
        if self.items.ptr_eq(&pool.items) {
            return false;
        }
        let changed: Vec<MediaId> = self
            .items
            .diff(&pool.items)
            .map(|d| match d {
                DiffItem::Add(id, _) | DiffItem::Remove(id, _) => *id,
                DiffItem::Update { new: (id, _), .. } => *id,
            })
            .collect();
        for id in changed {
            self.remove(id);
            if let Some(item) = pool.item(id) {
                self.insert(item, pool);
            }
        }
        self.items = pool.items.clone();
        true
    }

    fn remove(&mut self, id: MediaId) {
        let Some(terms) = self.terms.remove(&id) else {
            return;
        };
        for (token, _) in terms {
            let Some(list) = self.postings.get_mut(&token) else {
                continue;
            };
            if let Ok(i) = list.binary_search_by_key(&id, |p| p.0) {
                list.remove(i);
            }
            if list.is_empty() {
                self.postings.remove(&token);
                if let Some(set) = self.by_len.get_mut(&token.chars().count()) {
                    set.remove(&token);
                }
            }
        }
    }

    fn insert(&mut self, item: &MediaItem, pool: &MediaPool) {
        let terms = item_terms(item, pool);
        for (token, weight) in &terms {
            let list = match self.postings.get_mut(token) {
                Some(list) => list,
                None => {
                    self.by_len
                        .entry(token.chars().count())
                        .or_default()
                        .insert(token.clone());
                    self.postings.entry(token.clone()).or_default()
                }
            };
            match list.binary_search_by_key(&item.id, |p| p.0) {
                Ok(i) => list[i].1 = *weight,
                Err(i) => list.insert(i, (item.id, *weight)),
            }
        }
        self.terms.insert(item.id, terms);
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn token_count(&self) -> usize {
        self.postings.len()
    }

    /// Items matching every word of `query`, best first. A word matches a token exactly
    /// (score 1), as its start (0.8, words of two letters or more), or within one typo
    /// (words of four to seven letters) or two (eight or more, no digits), 0.6 less 0.1
    /// per typo; times the field weight, summed over the words. An empty query returns
    /// nothing.
    pub fn search(&self, query: &str) -> Vec<Hit> {
        let lowered = join_dotted_codecs(&query.to_lowercase());
        let words: Vec<&str> = lowered
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut total: Option<HashMap<MediaId, f32>> = None;
        for word in words {
            let scores = self.word_scores(word);
            total = Some(match total {
                None => scores,
                Some(prev) => prev
                    .into_iter()
                    .filter_map(|(id, s)| scores.get(&id).map(|t| (id, s + t)))
                    .collect(),
            });
            if total.as_ref().is_some_and(HashMap::is_empty) {
                return Vec::new();
            }
        }
        let mut hits: Vec<Hit> = total
            .unwrap_or_default()
            .into_iter()
            .map(|(id, score)| Hit { id, score })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
        hits
    }

    /// The ids `query` finds in `pool`: text hits best first, or with no text every item
    /// by id; filters applied either way. Call [`SearchIndex::refresh`] first.
    pub fn query(&self, pool: &MediaPool, query: &Query) -> Vec<MediaId> {
        let admits = |id: MediaId| pool.item(id).is_some_and(|i| query.admits(pool, i));
        if query.text.trim().is_empty() {
            pool.items()
                .filter(|i| query.admits(pool, i))
                .map(|i| i.id)
                .collect()
        } else {
            self.search(&query.text)
                .into_iter()
                .map(|h| h.id)
                .filter(|id| admits(*id))
                .collect()
        }
    }

    fn word_scores(&self, word: &str) -> HashMap<MediaId, f32> {
        let mut scores: HashMap<MediaId, f32> = HashMap::new();
        let mut credit = |list: &[(MediaId, u8)], quality: f32| {
            for &(id, weight) in list {
                let s = quality * f32::from(weight);
                let e = scores.entry(id).or_insert(0.0);
                if s > *e {
                    *e = s;
                }
            }
        };
        let len = word.chars().count();
        if len >= 2 {
            let from = (Bound::Included(word), Bound::Unbounded);
            for (token, list) in self
                .postings
                .range::<str, _>(from)
                .take_while(|(t, _)| t.starts_with(word))
            {
                credit(list, if token == word { 1.0 } else { 0.8 });
            }
        } else if let Some(list) = self.postings.get(word) {
            credit(list, 1.0);
        }
        // Numbers and codes (`h264`, `c003`, `2026`) match exactly or as a prefix: one
        // wrong digit names a different thing, not a typo.
        let max_edits = match len {
            _ if word.chars().any(|c| c.is_ascii_digit()) => 0,
            0..=3 => 0,
            4..=7 => 1,
            _ => 2,
        };
        if max_edits > 0 {
            let word_chars: Vec<char> = word.chars().collect();
            for l in len.saturating_sub(max_edits)..=len + max_edits {
                let Some(candidates) = self.by_len.get(&l) else {
                    continue;
                };
                for token in candidates {
                    if token.starts_with(word) {
                        continue;
                    }
                    if let Some(d) = bounded_distance(&word_chars, token, max_edits)
                        && let Some(list) = self.postings.get(token)
                    {
                        credit(list, 0.6 - 0.1 * d as f32);
                    }
                }
            }
        }
        scores
    }
}

/// Levenshtein distance between `a` and `b` when it is at most `max`.
fn bounded_distance(a: &[char], b: &str, max: usize) -> Option<usize> {
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut row = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        row[0] = i + 1;
        let mut best = row[0];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            row[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(row[j] + 1);
            best = best.min(row[j + 1]);
        }
        if best > max {
            return None;
        }
        std::mem::swap(&mut prev, &mut row);
    }
    (prev[b.len()] <= max).then_some(prev[b.len()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_pool::{Facts, MediaStatus, Metadata, OfflineReason};

    fn pool() -> MediaPool {
        let mut pool = MediaPool::new("Harbour film");
        let root = pool.root();
        let broll = pool.add_bin(root, "B-roll").unwrap();
        let mut add = |name: &str, codec: &str, width: u32, tags: &[&str], camera: Option<&str>| {
            let mut item = MediaItem::new(name, MediaKind::Video, format!("/card/{name}.mov"));
            item.facts = Facts {
                video_codec: Some(codec.into()),
                width,
                height: width * 9 / 16,
                ..Facts::default()
            };
            item.tags = tags.iter().map(|t| t.to_string()).collect();
            item.metadata = Metadata {
                camera_model: camera.map(str::to_string),
                ..Metadata::default()
            };
            pool.add_item(broll, item).unwrap()
        };
        add(
            "Harbour at dusk",
            "prores",
            3840,
            &["Golden hour", "Wide"],
            Some("ILME-FX6V"),
        );
        add(
            "Harbourmaster interview",
            "hevc",
            3840,
            &["Interview"],
            Some("ILME-FX3"),
        );
        add("Gulls take off", "h264", 3840, &["Slow motion"], None);
        add("A014_C003", "h264", 1920, &[], None);
        let id = add("Rope detail", "prores", 3840, &["B-roll"], None);
        pool.set_label(&[id], Some(ColourLabel::Amber)).unwrap();
        pool
    }

    fn names(pool: &MediaPool, ids: impl IntoIterator<Item = MediaId>) -> Vec<String> {
        ids.into_iter()
            .map(|id| pool.item(id).unwrap().name.clone())
            .collect()
    }

    fn hits(pool: &MediaPool, index: &SearchIndex, q: &str) -> Vec<String> {
        names(pool, index.search(q).into_iter().map(|h| h.id))
    }

    #[test]
    fn tokenises_names_codes_and_codecs() {
        assert_eq!(
            tokens("A014_C003"),
            ["003", "014", "a", "a014", "c", "c003"]
        );
        assert_eq!(tokens("H.264 clip"), ["264", "clip", "h", "h264"]);
        assert_eq!(tokens("Été 2"), ["2", "été"]);
    }

    #[test]
    fn prefix_exact_fuzzy_and_fields() {
        let pool = pool();
        let index = SearchIndex::build(&pool);
        assert_eq!(index.len(), 5);
        // Prefix: both harbour clips.
        assert_eq!(
            hits(&pool, &index, "harb"),
            ["Harbour at dusk", "Harbourmaster interview"]
        );
        // Every word must match.
        assert_eq!(hits(&pool, &index, "harbour golden"), ["Harbour at dusk"]);
        // One typo, and two in a long word.
        assert_eq!(hits(&pool, &index, "intervew"), ["Harbourmaster interview"]);
        assert_eq!(
            hits(&pool, &index, "harbormastr"),
            ["Harbourmaster interview"]
        );
        // Codec aliases and dotted spellings.
        assert_eq!(
            hits(&pool, &index, "H.264"),
            ["Gulls take off", "A014_C003"]
        );
        assert_eq!(hits(&pool, &index, "h265"), ["Harbourmaster interview"]);
        // Camera metadata, resolution, label and reel-style codes.
        assert_eq!(hits(&pool, &index, "fx3"), ["Harbourmaster interview"]);
        assert_eq!(index.search("4k").len(), 4);
        assert_eq!(hits(&pool, &index, "amber"), ["Rope detail"]);
        assert_eq!(hits(&pool, &index, "c003"), ["A014_C003"]);
        assert!(index.search("").is_empty());
        assert!(index.search("zebra").is_empty());
    }

    #[test]
    fn name_outranks_tag_outranks_metadata() {
        let mut pool = MediaPool::new("");
        let root = pool.root();
        let mut by_meta = MediaItem::new("Clip one", MediaKind::Video, "/a.mov");
        by_meta.metadata.lens = Some("Sigma 18-35 Art".into());
        let by_meta = pool.add_item(root, by_meta).unwrap();
        let mut by_tag = MediaItem::new("Clip two", MediaKind::Video, "/b.mov");
        by_tag.tags = vec!["Art".into()];
        let by_tag = pool.add_item(root, by_tag).unwrap();
        let by_name = pool
            .add_item(
                root,
                MediaItem::new("Art gallery", MediaKind::Video, "/c.mov"),
            )
            .unwrap();
        let index = SearchIndex::build(&pool);
        let ids: Vec<MediaId> = index.search("art").into_iter().map(|h| h.id).collect();
        assert_eq!(ids, [by_name, by_tag, by_meta]);
    }

    #[test]
    fn queries_filter_and_the_index_follows_edits() {
        let mut pool = pool();
        let mut index = SearchIndex::build(&pool);
        assert!(index.is_current(&pool));
        assert!(!index.refresh(&pool));

        let rope = pool.items().find(|i| i.name == "Rope detail").unwrap().id;
        pool.set_label_name(ColourLabel::Amber, "Selects");
        assert!(!index.is_current(&pool));
        assert!(index.refresh(&pool));
        assert_eq!(
            names(&pool, index.query(&pool, &Query::parse("selects", &pool))),
            ["Rope detail"]
        );

        let q = Query::parse("label:selects", &pool);
        assert_eq!(q.label, Some(ColourLabel::Amber));
        assert!(q.text.is_empty());
        assert_eq!(names(&pool, index.query(&pool, &q)), ["Rope detail"]);

        let q = Query::parse("type:video tag:interview harb", &pool);
        assert_eq!(q.kind, Some(MediaKind::Video));
        assert_eq!(q.text, "harb");
        assert_eq!(
            names(&pool, index.query(&pool, &q)),
            ["Harbourmaster interview"]
        );
        assert_eq!(Query::parse("foo:bar", &pool).text, "foo:bar");

        pool.set_status(
            &[rope],
            MediaStatus::Offline {
                reason: OfflineReason::Missing,
            },
        )
        .unwrap();
        index.refresh(&pool);
        assert_eq!(
            names(
                &pool,
                index.query(&pool, &Query::parse("is:offline", &pool))
            ),
            ["Rope detail"]
        );
        assert_eq!(
            index.query(&pool, &Query::parse("is:online", &pool)).len(),
            4
        );
        assert_eq!(hits(&pool, &index, "offline"), ["Rope detail"]);

        let q = Query {
            bin: Some(pool.root()),
            recursive: false,
            ..Query::default()
        };
        assert!(index.query(&pool, &q).is_empty());
        let q = Query {
            recursive: true,
            ..q
        };
        assert_eq!(index.query(&pool, &q).len(), 5);
        assert!(Query::default().is_empty());
    }

    #[test]
    fn refresh_follows_edits_like_a_rebuild() {
        let mut pool = pool();
        let mut index = SearchIndex::build(&pool);
        let root = pool.root();
        let gulls = pool
            .items()
            .find(|i| i.name == "Gulls take off")
            .unwrap()
            .id;
        let dusk = pool
            .items()
            .find(|i| i.name == "Harbour at dusk")
            .unwrap()
            .id;
        pool.add_tag(&[gulls], "Harbour mouth").unwrap();
        pool.update_item(dusk, |i| {
            i.name = "Pier at night".into();
            i.path = "/card/pier.mov".into();
        })
        .unwrap();
        let new = pool
            .add_item(
                root,
                MediaItem::new("Harbour crane", MediaKind::Still, "/card/crane.jpg"),
            )
            .unwrap();
        let rope = pool.items().find(|i| i.name == "Rope detail").unwrap().id;
        pool.remove_item(rope).unwrap();
        assert!(index.refresh(&pool));
        assert!(index.is_current(&pool));
        let fresh = SearchIndex::build(&pool);
        assert_eq!(index.len(), fresh.len());
        assert_eq!(index.token_count(), fresh.token_count());
        for q in [
            "harb", "harbour", "pier", "dusk", "amber", "detail", "photo", "intervew",
        ] {
            assert_eq!(index.search(q), fresh.search(q), "{q}");
        }
        assert_eq!(
            hits(&pool, &index, "harbour"),
            ["Harbour crane", "Harbourmaster interview", "Gulls take off"]
        );
        assert!(index.search("dusk").is_empty());
        assert!(index.search("rope").is_empty());
        assert!(pool.contains(new));
        assert!(!index.refresh(&pool));
    }

    #[test]
    fn distance_is_bounded() {
        let a: Vec<char> = "harbour".chars().collect();
        assert_eq!(bounded_distance(&a, "harbor", 1), Some(1));
        assert_eq!(bounded_distance(&a, "harbour", 1), Some(0));
        assert_eq!(bounded_distance(&a, "arbor", 1), None);
    }
}
