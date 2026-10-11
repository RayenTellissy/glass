//! The pool itself: nested bins, media items keyed by [`MediaId`], colour labels and
//! tags. Both collections are `imbl` maps of `Arc` nodes, so cloning a pool is O(1) and
//! an edit copies only the path to the nodes it changes: labelling one clip of ten
//! thousand leaves the other 9,999 items the same allocations in the old and new pool
//! (undo keeps both for the cost of one item).
//!
//! The project embeds a pool as [`crate::Project::media`] (schema v9), whose edits go
//! through the edit tree ([`crate::media_pool::edit`]) with ids from the project's node
//! counter. A pool can also be built on its own (views, tests): [`MediaPool::add_item`]
//! and [`MediaPool::add_bin`] then hand out the next id after the highest in use, and
//! [`MediaPool::insert_item`] takes an id the caller chose.

use std::cmp::Ordering;
use std::collections::BTreeMap;
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

impl MediaKind {
    /// A first guess from a path's extension, for references added without a probe
    /// ([`crate::Project::add_media`]): audio and still image extensions, anything else
    /// video. The import (`engine::media_mgmt`) classifies files properly.
    pub fn guess(path: &str) -> MediaKind {
        let ext = file_name(path)
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "wav" | "aif" | "aiff" | "mp3" | "m4a" | "aac" | "flac" | "ogg" | "opus" | "caf" => {
                MediaKind::Audio
            }
            "png" | "jpg" | "jpeg" | "tif" | "tiff" | "heic" | "heif" | "bmp" | "gif" | "webp"
            | "exr" | "dpx" => MediaKind::Still,
            _ => MediaKind::Video,
        }
    }
}

/// The last component of a path written with `/` or `\` separators (both, on every
/// platform, so a project file names its media the same everywhere).
pub fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The file name without its last extension (`A014_C003.MXF` → `A014_C003`); a name
/// that is only an extension (`.wav`) stays whole.
pub fn file_stem(path: &str) -> &str {
    let name = file_name(path);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}

/// One source file (or spanned recording, or image sequence) in the pool. Saved in the
/// project file as it is (schema v9); whether its file is available right now is engine
/// runtime state (`engine::media_mgmt::offline`), never stored here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaItem {
    /// Set by the pool when the item is added.
    pub id: MediaId,
    /// Set by the pool; change it with [`MediaPool::move_items`].
    pub bin: BinId,
    pub name: String,
    pub kind: MediaKind,
    /// The first (or only) file; for a sequence, its first frame. A string, as the user
    /// chose it, so every path can be saved.
    pub path: String,
    /// Further files of a recording the camera split, in order.
    pub spanned: Vec<String>,
    /// Camera sidecar files (XML, SRT) the metadata came from.
    pub sidecars: Vec<String>,
    /// The content hash (`media::ContentHash`, hex) the analysis cache and relinking key
    /// on; empty when unknown.
    pub content_hash: String,
    pub size_bytes: u64,
    /// The camera card format the item came from (`XDCAM`, `DJI`).
    pub card: Option<String>,
    /// When it was imported, seconds since the Unix epoch (0 when unknown).
    pub added_unix: u64,
    /// Stream facts read at import, kept so the pool can show and search them while
    /// the file is offline.
    pub facts: Facts,
    /// Camera metadata read at import.
    pub metadata: Metadata,
    pub label: Option<ColourLabel>,
    pub tags: Vec<String>,
}

impl MediaItem {
    /// A new item with no label or tags and nothing known about the file (the pool
    /// assigns `id` and `bin`).
    pub fn new(name: impl Into<String>, kind: MediaKind, path: impl Into<String>) -> Self {
        Self {
            id: MediaId(0),
            bin: BinId(0),
            name: name.into(),
            kind,
            path: path.into(),
            spanned: Vec::new(),
            sidecars: Vec::new(),
            content_hash: String::new(),
            size_bytes: 0,
            card: None,
            added_unix: 0,
            facts: Facts::default(),
            metadata: Metadata::default(),
            label: None,
            tags: Vec::new(),
        }
    }

    /// Every media file of the item, in order.
    pub fn files(&self) -> impl Iterator<Item = &String> {
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
    #[error("bin {0} is already in the pool")]
    DuplicateBin(BinId),
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

/// The media pool: bins, items and label names. See the module docs. Saved as
/// `{"bins": [...], "items": [...], "label_names": {...}}`, bins and items by id (the
/// root bin first), label names by label; [`MediaPool::check`] holds the invariants.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "PoolFile", into = "PoolFile")]
pub struct MediaPool {
    pub(crate) bins: OrdMap<BinId, Arc<Bin>>,
    pub(crate) items: OrdMap<MediaId, Arc<MediaItem>>,
    /// Names the user gave the colour labels (`Selects`, `Interviews`).
    pub(crate) label_names: OrdMap<ColourLabel, String>,
}

/// The root bin's id. Node ids start at 1, so it never names another node.
pub const ROOT: BinId = BinId(0);

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
        }
    }

    /// The id after the highest bin or item id in use.
    fn alloc(&self) -> u64 {
        let bins = self.bins.get_max().map_or(0, |(id, _)| id.0);
        let items = self.items.get_max().map_or(0, |(id, _)| id.0);
        bins.max(items) + 1
    }

    /// True when `other` is this very version (nothing edited in between).
    pub fn ptr_eq(&self, other: &MediaPool) -> bool {
        self.bins.ptr_eq(&other.bins)
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
        let id = BinId(self.alloc());
        self.insert_bin(id, parent, name)?;
        Ok(id)
    }

    /// Adds a bin under an id the caller chose (the project's node counter).
    pub fn insert_bin(
        &mut self,
        id: BinId,
        parent: BinId,
        name: impl Into<String>,
    ) -> Result<(), PoolError> {
        self.check_bin(parent)?;
        if self.bins.contains_key(&id) {
            return Err(PoolError::DuplicateBin(id));
        }
        let bin = Bin {
            id,
            name: name.into(),
            parent: Some(parent),
        };
        self.bins.insert(id, Arc::new(bin));
        Ok(())
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

    /// Renames a bin; the root bin carries the project's name and is refused.
    pub fn rename_bin(&mut self, id: BinId, name: impl Into<String>) -> Result<(), PoolError> {
        if id == ROOT {
            return Err(PoolError::RootBin);
        }
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

    /// The item (reference) with id `id`: [`MediaPool::item`] under the name the
    /// timeline code reads references by.
    pub fn get(&self, id: MediaId) -> Option<&Arc<MediaItem>> {
        self.items.get(&id)
    }

    /// Every item by id: [`MediaPool::items`] under the name the timeline code reads
    /// references by.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<MediaItem>> {
        self.items.values()
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
    /// a relinked item keeping its identity).
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
        self.put(id, bin, item);
        Ok(())
    }

    fn put(&mut self, id: MediaId, bin: BinId, mut item: MediaItem) {
        item.id = id;
        item.bin = bin;
        self.items.insert(id, Arc::new(item));
    }

    /// Changes one item in place (name, path, facts, metadata...). Its `id` and `bin`
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
    /// [`crate::Project::remove_media`] does).
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
            .find(|i| !hash_hex.is_empty() && i.content_hash == hash_hex)
    }

    pub fn find_by_path(&self, path: &str) -> Option<&Arc<MediaItem>> {
        self.items.values().find(|i| i.files().any(|f| f == path))
    }

    /// The pool's invariants, checked when a pool comes from a file: a root bin with no
    /// parent, every other bin under an existing bin and none in a cycle, every bin and
    /// item stored under its own id, every item in an existing bin.
    pub fn check(&self) -> Result<(), String> {
        match self.bins.get(&ROOT) {
            Some(root) if root.parent.is_none() => {}
            _ => return Err("media pool has no root bin".into()),
        }
        for (id, bin) in &self.bins {
            if *id != bin.id {
                return Err(format!("bin {} stored under {id}", bin.id));
            }
            if bin.id != ROOT && !bin.parent.is_some_and(|p| self.bins.contains_key(&p)) {
                return Err(format!("bin {} has no parent bin", bin.id));
            }
            if !self.is_within(bin.id, ROOT) {
                return Err(format!("bin {} is in a cycle", bin.id));
            }
        }
        for (id, item) in &self.items {
            if *id != item.id {
                return Err(format!("media {} stored under {id}", item.id));
            }
            if !self.bins.contains_key(&item.bin) {
                return Err(format!("media item {} is in a missing bin", item.id));
            }
        }
        Ok(())
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

/// The pool as stored: plain lists (checked by [`MediaPool::check`] when a project
/// validates).
#[derive(Serialize, Deserialize)]
struct PoolFile {
    bins: Vec<Arc<Bin>>,
    items: Vec<Arc<MediaItem>>,
    label_names: BTreeMap<ColourLabel, String>,
}

impl From<MediaPool> for PoolFile {
    fn from(pool: MediaPool) -> Self {
        Self {
            bins: pool.bins.values().cloned().collect(),
            items: pool.items.values().cloned().collect(),
            label_names: pool.label_names.into_iter().collect(),
        }
    }
}

impl From<PoolFile> for MediaPool {
    fn from(file: PoolFile) -> Self {
        Self {
            bins: file.bins.into_iter().map(|b| (b.id, b)).collect(),
            items: file.items.into_iter().map(|i| (i.id, i)).collect(),
            label_names: file.label_names.into_iter().collect(),
        }
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
    fn ids_from_the_caller() {
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
        assert_eq!(
            pool.insert_bin(BinId(41), root, "Taken"),
            Ok(()),
            "bins and items are separate maps"
        );
        assert_eq!(
            pool.insert_bin(BinId(41), root, "Again"),
            Err(PoolError::DuplicateBin(BinId(41)))
        );
        assert_eq!(pool.add_bin(root, "Next").unwrap(), BinId(42));
        assert_eq!(pool.rename_bin(root, "Project"), Err(PoolError::RootBin));

        pool.update_item(next, |item| {
            item.name = "Renamed".into();
            item.bin = BinId(1234);
        })
        .unwrap();
        let item = pool.item(next).unwrap();
        assert_eq!((item.name.as_str(), item.bin), ("Renamed", root));
        assert!(pool.find_by_path("/card/Next.mov").is_some());
        assert!(pool.check().is_ok());
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
        assert!(back.check().is_ok());
        let mut back = back;
        assert_eq!(back.add_bin(root, "New").unwrap(), BinId(3));

        let orphan = json.replace(r#""parent":0"#, r#""parent":77"#);
        let orphan: MediaPool = serde_json::from_str(&orphan).unwrap();
        assert_eq!(orphan.check(), Err("bin #1 has no parent bin".into()));
        let lost = json.replace(r#""bin":1"#, r#""bin":9"#);
        let lost: MediaPool = serde_json::from_str(&lost).unwrap();
        assert_eq!(
            lost.check(),
            Err("media item #2 is in a missing bin".into())
        );
    }

    #[test]
    fn kinds_and_names_from_paths() {
        assert_eq!(MediaKind::guess("/a/b/Tone.WAV"), MediaKind::Audio);
        assert_eq!(MediaKind::guess("C:\\stills\\IMG_0001.heic"), MediaKind::Still);
        assert_eq!(MediaKind::guess("/a/A014_C003.mxf"), MediaKind::Video);
        assert_eq!(MediaKind::guess("noext"), MediaKind::Video);
        assert_eq!(file_stem("/a/b/A014_C003.MXF"), "A014_C003");
        assert_eq!(file_stem("C:\\clips\\take.2.mov"), "take.2");
        assert_eq!(file_stem("/a/.wav"), ".wav");
        assert_eq!(file_stem("plain"), "plain");
        assert_eq!(file_name("dir/"), "");
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
