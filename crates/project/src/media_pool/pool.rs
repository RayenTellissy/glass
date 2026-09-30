//! The pool itself: nested bins, media items keyed by [`MediaId`], colour labels and
//! tags. Both collections are `imbl` maps of `Arc` nodes, so cloning a pool is O(1) and
//! an edit copies only the path to the nodes it changes: labelling one clip of ten
//! thousand leaves the other 9,999 items the same allocations in the old and new pool
//! (undo keeps both for the cost of one item).
//!
//! The pool is standalone until the project root embeds it (Phase 5 schema work). It
//! hands out its own ids from its own counter, and [`MediaPool::insert_item`] accepts an
//! id chosen by the caller, so the project's node counter can take over later without
//! changing this API.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use imbl::OrdMap;
use serde::{Deserialize, Serialize};

use super::{Facts, Metadata};
use crate::id::{BinId, MediaId};

/// The colour labels, in menu order. The UI shows each in the clip colour for its role
/// (amber as `clip/title`, blue as `clip/video` and so on) and always with its name,
/// which the user can change ([`MediaPool::set_label_name`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColourLabel {
    Amber,
    Blue,
    Green,
    Red,
    Violet,
}

impl ColourLabel {
    pub const ALL: [ColourLabel; 5] = [
        ColourLabel::Amber,
        ColourLabel::Blue,
        ColourLabel::Green,
        ColourLabel::Red,
        ColourLabel::Violet,
    ];

    /// The stable name used in search (`label:amber`) and files.
    pub fn key(self) -> &'static str {
        match self {
            ColourLabel::Amber => "amber",
            ColourLabel::Blue => "blue",
            ColourLabel::Green => "green",
            ColourLabel::Red => "red",
            ColourLabel::Violet => "violet",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|l| l.key().eq_ignore_ascii_case(key))
    }
}

/// What a media item holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Video,
    Audio,
    Still,
    Sequence,
}

impl MediaKind {
    /// The stable name used in search (`type:video`).
    pub fn key(self) -> &'static str {
        match self {
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
            MediaKind::Still => "still",
            MediaKind::Sequence => "sequence",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        [
            MediaKind::Video,
            MediaKind::Audio,
            MediaKind::Still,
            MediaKind::Sequence,
        ]
        .into_iter()
        .find(|k| k.key().eq_ignore_ascii_case(key))
    }
}

/// Why a file cannot be used right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineReason {
    /// The file is not at its path any more (moved, renamed or deleted).
    Missing,
    /// The drive or card it was on is not connected.
    VolumeNotConnected,
    /// The file is there but cannot be read (permissions, damaged, changed content).
    Unreadable,
}

/// Whether a media item's file can be used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum MediaStatus {
    #[default]
    Online,
    Offline {
        reason: OfflineReason,
    },
}

impl MediaStatus {
    pub fn is_offline(&self) -> bool {
        matches!(self, MediaStatus::Offline { .. })
    }
}

/// One source file (or spanned recording, or image sequence) in the pool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaItem {
    /// Set by the pool when the item is added.
    pub id: MediaId,
    /// Set by the pool; change it with [`MediaPool::move_items`].
    pub bin: BinId,
    pub name: String,
    pub kind: MediaKind,
    /// The first (or only) file; for a sequence, its first frame.
    pub path: PathBuf,
    /// Further files of a recording the camera split, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spanned: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<PathBuf>,
    /// The content hash (hex) the analysis cache and relinking key on.
    #[serde(default)]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub size_bytes: u64,
    /// The camera card format the item came from (`XDCAM`, `DJI`).
    #[serde(default)]
    pub card: Option<String>,
    /// When it was imported, seconds since the Unix epoch.
    #[serde(default)]
    pub added_unix: u64,
    #[serde(default)]
    pub facts: Facts,
    #[serde(default)]
    pub metadata: Metadata,
    #[serde(default)]
    pub label: Option<ColourLabel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub status: MediaStatus,
}

impl MediaItem {
    /// A new, online item with no label or tags (the pool assigns `id` and `bin`).
    pub fn new(name: impl Into<String>, kind: MediaKind, path: impl Into<PathBuf>) -> Self {
        Self {
            id: MediaId(0),
            bin: BinId(0),
            name: name.into(),
            kind,
            path: path.into(),
            spanned: Vec::new(),
            sidecars: Vec::new(),
            content_hash: None,
            size_bytes: 0,
            card: None,
            added_unix: 0,
            facts: Facts::default(),
            metadata: Metadata::default(),
            label: None,
            tags: Vec::new(),
            status: MediaStatus::Online,
        }
    }

    /// Every media file of the item, in order.
    pub fn files(&self) -> impl Iterator<Item = &PathBuf> {
        std::iter::once(&self.path).chain(&self.spanned)
    }

    /// Tags compare ignoring case.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    }
}

/// A folder in the pool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bin {
    pub id: BinId,
    pub name: String,
    /// `None` for the root bin (the project).
    pub parent: Option<BinId>,
}

/// Errors from pool edits. A failed edit changes nothing.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PoolError {
    #[error("no bin {0}")]
    NoSuchBin(BinId),
    #[error("no media item {0}")]
    NoSuchItem(MediaId),
    #[error("media item {0} is already in the pool")]
    DuplicateItem(MediaId),
    #[error("the root bin cannot be removed or moved")]
    RootBin,
    #[error("a bin cannot move into itself or one of its own bins")]
    BinCycle,
}

/// Orders for the item list and grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    #[default]
    Name,
    /// Import date, newest last.
    Added,
    Duration,
    Kind,
    Size,
}

/// The media pool: bins, items and label names. See the module docs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PoolFile", into = "PoolFile")]
pub struct MediaPool {
    pub(super) bins: OrdMap<BinId, Arc<Bin>>,
    pub(super) items: OrdMap<MediaId, Arc<MediaItem>>,
    /// Names the user gave the colour labels (`Selects`, `Interviews`).
    pub(super) label_names: OrdMap<ColourLabel, String>,
    /// The next id [`MediaPool::add_bin`] and [`MediaPool::add_item`] hand out.
    next_id: u64,
}

const ROOT: BinId = BinId(0);

impl Default for MediaPool {
    fn default() -> Self {
        Self::new("")
    }
}

impl MediaPool {
    /// An empty pool whose root bin is named after the project.
    pub fn new(root_name: impl Into<String>) -> Self {
        let root = Bin {
            id: ROOT,
            name: root_name.into(),
            parent: None,
        };
        Self {
            bins: OrdMap::unit(ROOT, Arc::new(root)),
            items: OrdMap::new(),
            label_names: OrdMap::new(),
            next_id: 1,
        }
    }

    fn alloc(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// True when `other` is this very version (nothing edited in between).
    pub fn ptr_eq(&self, other: &MediaPool) -> bool {
        self.next_id == other.next_id
            && self.bins.ptr_eq(&other.bins)
            && self.items.ptr_eq(&other.items)
            && self.label_names.ptr_eq(&other.label_names)
    }

    // Bins.

    pub fn root(&self) -> BinId {
        ROOT
    }

    /// Every bin, the root first, then by id.
    pub fn bins(&self) -> impl Iterator<Item = &Arc<Bin>> {
        self.bins.values()
    }

    pub fn bin_count(&self) -> usize {
        self.bins.len()
    }

    pub fn bin(&self, id: BinId) -> Option<&Arc<Bin>> {
        self.bins.get(&id)
    }

    fn check_bin(&self, id: BinId) -> Result<(), PoolError> {
        self.bins
            .contains_key(&id)
            .then_some(())
            .ok_or(PoolError::NoSuchBin(id))
    }

    /// Child bins of `parent`, in natural name order.
    pub fn children(&self, parent: BinId) -> Vec<&Arc<Bin>> {
        let mut children: Vec<&Arc<Bin>> = self
            .bins
            .values()
            .filter(|b| b.parent == Some(parent))
            .collect();
        children.sort_by(|a, b| natural_cmp(&a.name, &b.name).then(a.id.cmp(&b.id)));
        children
    }

    pub fn add_bin(&mut self, parent: BinId, name: impl Into<String>) -> Result<BinId, PoolError> {
        self.check_bin(parent)?;
        let id = BinId(self.alloc());
        let bin = Bin {
            id,
            name: name.into(),
            parent: Some(parent),
        };
        self.bins.insert(id, Arc::new(bin));
        Ok(id)
    }

    /// The bin at `path` under `parent`, creating the missing ones (folder import).
    pub fn bin_path(&mut self, parent: BinId, path: &[String]) -> Result<BinId, PoolError> {
        self.check_bin(parent)?;
        let mut at = parent;
        for name in path {
            let existing = self
                .bins
                .values()
                .find(|b| b.parent == Some(at) && b.name == *name)
                .map(|b| b.id);
            at = match existing {
                Some(id) => id,
                None => self.add_bin(at, name.clone())?,
            };
        }
        Ok(at)
    }

    pub fn rename_bin(&mut self, id: BinId, name: impl Into<String>) -> Result<(), PoolError> {
        let bin = self.bins.get_mut(&id).ok_or(PoolError::NoSuchBin(id))?;
        Arc::make_mut(bin).name = name.into();
        Ok(())
    }

    /// True when `bin` is `ancestor` or inside it.
    pub fn is_within(&self, bin: BinId, ancestor: BinId) -> bool {
        let mut at = Some(bin);
        // Depth is bounded by the bin count, which also guards a damaged file's cycle.
        for _ in 0..=self.bins.len() {
            match at {
                Some(id) if id == ancestor => return true,
                Some(id) => at = self.bins.get(&id).and_then(|b| b.parent),
                None => return false,
            }
        }
        false
    }

    pub fn move_bin(&mut self, id: BinId, parent: BinId) -> Result<(), PoolError> {
        if id == ROOT {
            return Err(PoolError::RootBin);
        }
        self.check_bin(id)?;
        self.check_bin(parent)?;
        if self.is_within(parent, id) {
            return Err(PoolError::BinCycle);
        }
        let bin = self.bins.get_mut(&id).ok_or(PoolError::NoSuchBin(id))?;
        Arc::make_mut(bin).parent = Some(parent);
        Ok(())
    }

    /// Removes a bin; its items and bins move to its parent, so no media is lost.
    pub fn remove_bin(&mut self, id: BinId) -> Result<(), PoolError> {
        if id == ROOT {
            return Err(PoolError::RootBin);
        }
        let parent = self
            .bin(id)
            .ok_or(PoolError::NoSuchBin(id))?
            .parent
            .unwrap_or(ROOT);
        let child_bins: Vec<BinId> = self
            .bins
            .values()
            .filter(|b| b.parent == Some(id))
            .map(|b| b.id)
            .collect();
        for child in child_bins {
            if let Some(bin) = self.bins.get_mut(&child) {
                Arc::make_mut(bin).parent = Some(parent);
            }
        }
        let items: Vec<MediaId> = self
            .items
            .values()
            .filter(|i| i.bin == id)
            .map(|i| i.id)
            .collect();
        for item in items {
            if let Some(item) = self.items.get_mut(&item) {
                Arc::make_mut(item).bin = parent;
            }
        }
        self.bins.remove(&id);
        Ok(())
    }

    /// The bin names from the root to `id` (breadcrumbs).
    pub fn bin_names(&self, id: BinId) -> Vec<&str> {
        let mut names = Vec::new();
        let mut at = self.bin(id);
        while let Some(bin) = at {
            names.push(bin.name.as_str());
            if names.len() > self.bins.len() {
                break;
            }
            at = bin.parent.and_then(|p| self.bin(p));
        }
        names.reverse();
        names
    }

    // Items.

    /// Every item, by id (import order).
    pub fn items(&self) -> impl Iterator<Item = &Arc<MediaItem>> {
        self.items.values()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn item(&self, id: MediaId) -> Option<&Arc<MediaItem>> {
        self.items.get(&id)
    }

    pub fn contains(&self, id: MediaId) -> bool {
        self.items.contains_key(&id)
    }

    /// Adds `item` to `bin` under a new id and returns it.
    pub fn add_item(&mut self, bin: BinId, item: MediaItem) -> Result<MediaId, PoolError> {
        self.check_bin(bin)?;
        let id = MediaId(self.alloc());
        self.put(id, bin, item);
        Ok(id)
    }

    /// Adds `item` to `bin` under an id the caller chose (the project's node counter, or
    /// a relinked item keeping its identity). Later ids from the pool skip past it.
    pub fn insert_item(
        &mut self,
        id: MediaId,
        bin: BinId,
        item: MediaItem,
    ) -> Result<(), PoolError> {
        self.check_bin(bin)?;
        if self.items.contains_key(&id) {
            return Err(PoolError::DuplicateItem(id));
        }
        self.next_id = self.next_id.max(id.0 + 1);
        self.put(id, bin, item);
        Ok(())
    }

    fn put(&mut self, id: MediaId, bin: BinId, mut item: MediaItem) {
        item.id = id;
        item.bin = bin;
        self.items.insert(id, Arc::new(item));
    }

    /// Changes one item in place (name, facts, metadata, status...). Its `id` and `bin`
    /// stay as they were; move items with [`MediaPool::move_items`].
    pub fn update_item(
        &mut self,
        id: MediaId,
        f: impl FnOnce(&mut MediaItem),
    ) -> Result<(), PoolError> {
        let item = Arc::make_mut(self.items.get_mut(&id).ok_or(PoolError::NoSuchItem(id))?);
        let bin = item.bin;
        f(item);
        item.id = id;
        item.bin = bin;
        Ok(())
    }

    /// Removes an item from the pool (the caller checks the timeline no longer uses it;
    /// to keep edits, take it offline instead).
    pub fn remove_item(&mut self, id: MediaId) -> Result<Arc<MediaItem>, PoolError> {
        self.items.remove(&id).ok_or(PoolError::NoSuchItem(id))
    }

    /// Items directly in `bin`, or in it and all its bins, by id.
    pub fn items_in(&self, bin: BinId, recursive: bool) -> Vec<&Arc<MediaItem>> {
        if recursive && bin == ROOT {
            return self.items.values().collect();
        }
        self.items
            .values()
            .filter(|item| {
                if recursive {
                    self.is_within(item.bin, bin)
                } else {
                    item.bin == bin
                }
            })
            .collect()
    }

    /// Item counts per bin, counting only the items directly in each.
    pub fn counts(&self) -> BTreeMap<BinId, usize> {
        let mut counts: BTreeMap<BinId, usize> = self.bins.keys().map(|b| (*b, 0)).collect();
        for item in self.items.values() {
            *counts.entry(item.bin).or_default() += 1;
        }
        counts
    }

    fn check_items(&self, ids: &[MediaId]) -> Result<(), PoolError> {
        match ids.iter().find(|id| !self.items.contains_key(*id)) {
            Some(id) => Err(PoolError::NoSuchItem(*id)),
            None => Ok(()),
        }
    }

    /// Applies `f` to each item; checks every id first so a failed edit changes nothing.
    /// Items `f` leaves unchanged keep their allocation.
    fn each_mut(
        &mut self,
        ids: &[MediaId],
        mut f: impl FnMut(&MediaItem) -> Option<MediaItem>,
    ) -> Result<(), PoolError> {
        self.check_items(ids)?;
        for id in ids {
            if let Some(slot) = self.items.get_mut(id)
                && let Some(changed) = f(slot)
            {
                *slot = Arc::new(changed);
            }
        }
        Ok(())
    }

    pub fn move_items(&mut self, ids: &[MediaId], bin: BinId) -> Result<(), PoolError> {
        self.check_bin(bin)?;
        self.each_mut(ids, |item| {
            (item.bin != bin).then(|| MediaItem {
                bin,
                ..item.clone()
            })
        })
    }

    pub fn set_label(
        &mut self,
        ids: &[MediaId],
        label: Option<ColourLabel>,
    ) -> Result<(), PoolError> {
        self.each_mut(ids, |item| {
            (item.label != label).then(|| MediaItem {
                label,
                ..item.clone()
            })
        })
    }

    pub fn set_status(&mut self, ids: &[MediaId], status: MediaStatus) -> Result<(), PoolError> {
        self.each_mut(ids, |item| {
            (item.status != status).then(|| MediaItem {
                status,
                ..item.clone()
            })
        })
    }

    /// Adds `tag` to each item that does not have it. Tags compare ignoring case, and a
    /// tag already in use keeps its first spelling.
    pub fn add_tag(&mut self, ids: &[MediaId], tag: &str) -> Result<(), PoolError> {
        let tag = tag.trim();
        self.check_items(ids)?;
        if tag.is_empty() {
            return Ok(());
        }
        let spelling = self
            .items
            .values()
            .flat_map(|i| &i.tags)
            .find(|t| t.eq_ignore_ascii_case(tag))
            .cloned()
            .unwrap_or_else(|| tag.to_string());
        self.each_mut(ids, |item| {
            (!item.has_tag(&spelling)).then(|| {
                let mut item = item.clone();
                item.tags.push(spelling.clone());
                item
            })
        })
    }

    pub fn remove_tag(&mut self, ids: &[MediaId], tag: &str) -> Result<(), PoolError> {
        self.each_mut(ids, |item| {
            item.has_tag(tag).then(|| {
                let mut item = item.clone();
                item.tags.retain(|t| !t.eq_ignore_ascii_case(tag));
                item
            })
        })
    }

    /// Every tag in use with how many items carry it, in natural order.
    pub fn tags(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, (String, usize)> = BTreeMap::new();
        for tag in self.items.values().flat_map(|i| &i.tags) {
            let entry = counts
                .entry(tag.to_lowercase())
                .or_insert_with(|| (tag.clone(), 0));
            entry.1 += 1;
        }
        let mut tags: Vec<(String, usize)> = counts.into_values().collect();
        tags.sort_by(|a, b| natural_cmp(&a.0, &b.0));
        tags
    }

    /// The name shown for a label, when the user gave it one.
    pub fn label_name(&self, label: ColourLabel) -> Option<&str> {
        self.label_names.get(&label).map(String::as_str)
    }

    /// Names a label; an empty name clears it.
    pub fn set_label_name(&mut self, label: ColourLabel, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.label_names.remove(&label);
        } else {
            self.label_names.insert(label, name.to_string());
        }
    }

    pub fn find_by_hash(&self, hash_hex: &str) -> Option<&Arc<MediaItem>> {
        self.items
            .values()
            .find(|i| i.content_hash.as_deref() == Some(hash_hex))
    }

    pub fn find_by_path(&self, path: &Path) -> Option<&Arc<MediaItem>> {
        self.items.values().find(|i| i.files().any(|f| f == path))
    }

    /// Items that are offline.
    pub fn offline(&self) -> impl Iterator<Item = &Arc<MediaItem>> {
        self.items.values().filter(|i| i.status.is_offline())
    }

    /// Sorts `items` for the list or grid; ties fall back to the name, then the id.
    pub fn sort(items: &mut [&Arc<MediaItem>], by: SortBy, descending: bool) {
        items.sort_by(|a, b| {
            let primary = match by {
                SortBy::Name => Ordering::Equal,
                SortBy::Added => a.added_unix.cmp(&b.added_unix),
                SortBy::Duration => a
                    .facts
                    .duration_s
                    .unwrap_or(0.0)
                    .total_cmp(&b.facts.duration_s.unwrap_or(0.0)),
                SortBy::Kind => a.kind.cmp(&b.kind),
                SortBy::Size => a.size_bytes.cmp(&b.size_bytes),
            };
            let order = primary
                .then_with(|| natural_cmp(&a.name, &b.name))
                .then(a.id.cmp(&b.id));
            if descending { order.reverse() } else { order }
        });
    }
}

/// The pool as stored: plain lists, checked on load.
#[derive(Serialize, Deserialize)]
struct PoolFile {
    bins: Vec<Arc<Bin>>,
    items: Vec<Arc<MediaItem>>,
    #[serde(default)]
    label_names: BTreeMap<ColourLabel, String>,
    next_id: u64,
}

impl From<MediaPool> for PoolFile {
    fn from(pool: MediaPool) -> Self {
        Self {
            bins: pool.bins.values().cloned().collect(),
            items: pool.items.values().cloned().collect(),
            label_names: pool.label_names.into_iter().collect(),
            next_id: pool.next_id,
        }
    }
}

impl TryFrom<PoolFile> for MediaPool {
    type Error = String;

    fn try_from(file: PoolFile) -> Result<Self, String> {
        let bins: OrdMap<BinId, Arc<Bin>> = file.bins.into_iter().map(|b| (b.id, b)).collect();
        match bins.get(&ROOT) {
            Some(root) if root.parent.is_none() => {}
            _ => return Err("media pool has no root bin".into()),
        }
        if let Some(bin) = bins
            .values()
            .find(|b| b.id != ROOT && !b.parent.is_some_and(|p| bins.contains_key(&p)))
        {
            return Err(format!("bin {} has no parent bin", bin.id));
        }
        let items: OrdMap<MediaId, Arc<MediaItem>> =
            file.items.into_iter().map(|i| (i.id, i)).collect();
        if let Some(item) = items.values().find(|i| !bins.contains_key(&i.bin)) {
            return Err(format!("media item {} is in a missing bin", item.id));
        }
        let max_id = bins
            .keys()
            .map(|b| b.0)
            .chain(items.keys().map(|i| i.0))
            .max()
            .unwrap_or(0);
        let pool = Self {
            bins,
            items,
            label_names: file.label_names.into_iter().collect(),
            next_id: file.next_id.max(max_id + 1),
        };
        if let Some(bin) = pool.bins.values().find(|b| !pool.is_within(b.id, ROOT)) {
            return Err(format!("bin {} is in a cycle", bin.id));
        }
        Ok(pool)
    }
}

/// Compares names the way a person sorts them: `Day 2` before `Day 10`, case ignored.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    b.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(name: &str) -> MediaItem {
        MediaItem::new(name, MediaKind::Video, format!("/card/{name}.mov"))
    }

    #[test]
    fn nested_bins_move_and_remove_without_losing_media() {
        let mut pool = MediaPool::new("Harbour film");
        let root = pool.root();
        let day1 = pool.add_bin(root, "Day 1").unwrap();
        let a_cam = pool.add_bin(day1, "A-cam").unwrap();
        let day10 = pool.add_bin(root, "Day 10").unwrap();
        let day2 = pool.add_bin(root, "Day 2").unwrap();
        let names: Vec<&str> = pool
            .children(root)
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(names, ["Day 1", "Day 2", "Day 10"]);
        assert_eq!(pool.bin_names(a_cam), ["Harbour film", "Day 1", "A-cam"]);

        let clip = pool.add_item(a_cam, clip("A014_C003")).unwrap();
        assert_eq!(pool.items_in(day1, true).len(), 1);
        assert!(pool.items_in(day1, false).is_empty());

        assert_eq!(pool.move_bin(day1, a_cam), Err(PoolError::BinCycle));
        assert_eq!(pool.move_bin(root, day2), Err(PoolError::RootBin));
        pool.move_bin(a_cam, day10).unwrap();
        assert_eq!(pool.item(clip).unwrap().bin, a_cam);
        assert!(pool.is_within(a_cam, day10));

        pool.remove_bin(day10).unwrap();
        assert_eq!(pool.bin(a_cam).unwrap().parent, Some(root));
        pool.remove_bin(a_cam).unwrap();
        assert_eq!(pool.item(clip).unwrap().bin, root);
        assert_eq!(pool.remove_bin(a_cam), Err(PoolError::NoSuchBin(a_cam)));

        let path = ["Card".to_string(), "Clips".to_string()];
        let clips = pool.bin_path(root, &path).unwrap();
        assert_eq!(pool.bin_path(root, &path).unwrap(), clips);
        assert_eq!(pool.bin_names(clips), ["Harbour film", "Card", "Clips"]);
    }

    #[test]
    fn labels_and_tags_share_untouched_items() {
        let mut pool = MediaPool::new("");
        let root = pool.root();
        let ids: Vec<MediaId> = (0..1000)
            .map(|i| pool.add_item(root, clip(&format!("Clip {i}"))).unwrap())
            .collect();
        let before = pool.clone();
        assert!(before.ptr_eq(&pool));

        pool.set_label(&ids[10..11], Some(ColourLabel::Amber))
            .unwrap();
        pool.add_tag(&ids[..2], "Golden hour").unwrap();
        pool.add_tag(&ids[1..3], "golden HOUR").unwrap();
        assert!(!before.ptr_eq(&pool));
        assert_eq!(pool.item(ids[10]).unwrap().label, Some(ColourLabel::Amber));
        assert_eq!(pool.item(ids[2]).unwrap().tags, ["Golden hour"]);
        assert_eq!(pool.item(ids[1]).unwrap().tags, ["Golden hour"]);
        assert_eq!(pool.tags(), [("Golden hour".to_string(), 3)]);
        let changed = [ids[0], ids[1], ids[2], ids[10]];
        for id in &ids {
            let shared = Arc::ptr_eq(before.item(*id).unwrap(), pool.item(*id).unwrap());
            assert_eq!(shared, !changed.contains(id), "item {id}");
        }
        // Setting what is already there copies nothing.
        let again = pool.clone();
        pool.set_label(&ids[10..11], Some(ColourLabel::Amber))
            .unwrap();
        assert!(Arc::ptr_eq(
            again.item(ids[10]).unwrap(),
            pool.item(ids[10]).unwrap()
        ));

        pool.remove_tag(&ids, "GOLDEN hour").unwrap();
        assert!(pool.tags().is_empty());

        // A failed edit changes nothing.
        let snapshot = pool.clone();
        let missing = MediaId(99_999);
        assert_eq!(
            pool.set_label(&[ids[0], missing], Some(ColourLabel::Red)),
            Err(PoolError::NoSuchItem(missing))
        );
        assert!(snapshot.ptr_eq(&pool));

        pool.set_label_name(ColourLabel::Amber, " Selects ");
        assert_eq!(pool.label_name(ColourLabel::Amber), Some("Selects"));
        pool.set_label_name(ColourLabel::Amber, "");
        assert_eq!(pool.label_name(ColourLabel::Amber), None);
    }

    #[test]
    fn ids_from_the_caller_and_status() {
        let mut pool = MediaPool::new("");
        let root = pool.root();
        pool.insert_item(MediaId(40), root, clip("Relinked"))
            .unwrap();
        assert_eq!(
            pool.insert_item(MediaId(40), root, clip("Again")),
            Err(PoolError::DuplicateItem(MediaId(40)))
        );
        let next = pool.add_item(root, clip("Next")).unwrap();
        assert_eq!(next, MediaId(41));

        let offline = MediaStatus::Offline {
            reason: OfflineReason::VolumeNotConnected,
        };
        pool.set_status(&[next], offline).unwrap();
        assert_eq!(pool.offline().count(), 1);
        pool.update_item(next, |item| {
            item.name = "Renamed".into();
            item.bin = BinId(1234);
        })
        .unwrap();
        let item = pool.item(next).unwrap();
        assert_eq!((item.name.as_str(), item.bin), ("Renamed", root));
        assert!(pool.find_by_path(Path::new("/card/Next.mov")).is_some());
        assert_eq!(pool.remove_item(next).unwrap().name, "Renamed");
        assert!(!pool.contains(next));
    }

    #[test]
    fn saves_and_loads() {
        let mut pool = MediaPool::new("Harbour film");
        let root = pool.root();
        let bin = pool.add_bin(root, "Selects").unwrap();
        let mut item = clip("Harbour at dusk");
        item.facts.duration_s = Some(12.2);
        item.metadata.camera_model = Some("ILME-FX6V".into());
        let id = pool.add_item(bin, item).unwrap();
        pool.set_label(&[id], Some(ColourLabel::Amber)).unwrap();
        pool.add_tag(&[id], "Golden hour").unwrap();
        pool.set_label_name(ColourLabel::Amber, "Selects");
        let json = serde_json::to_string(&pool).unwrap();
        let back: MediaPool = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pool);
        let mut back = back;
        assert_eq!(back.add_bin(root, "New").unwrap(), BinId(3));

        let orphan = json.replace(r#""parent":0"#, r#""parent":77"#);
        assert!(serde_json::from_str::<MediaPool>(&orphan).is_err());
    }

    #[test]
    fn sorts_like_people_do() {
        let mut pool = MediaPool::new("");
        let root = pool.root();
        for (name, added) in [("Day 10", 3), ("day 2", 1), ("Day 1", 2)] {
            let mut item = clip(name);
            item.added_unix = added;
            pool.add_item(root, item).unwrap();
        }
        let mut items: Vec<&Arc<MediaItem>> = pool.items().collect();
        MediaPool::sort(&mut items, SortBy::Name, false);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Day 1", "day 2", "Day 10"]);
        MediaPool::sort(&mut items, SortBy::Added, true);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Day 10", "Day 1", "day 2"]);
        assert_eq!(natural_cmp("A014_C003", "A014_C0010"), Ordering::Less);
    }
}
