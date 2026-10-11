//! Media pool edits: pure functions from a project to a new project, like the timeline
//! edits ([`crate::edit`]), each one undo step with its [`crate::EditLabel`].
//!
//! New items and bins take their ids from the project's node counter, so a media id is
//! unique among every node of the project. Each edit checks its values first (an empty
//! name or tag), then its ids in the order given, then what it would break; a refused
//! edit changes nothing, and one that changes nothing returns the project as it was.
//!
//! Removing media that a clip uses is refused ([`EditError::MediaInUse`]) rather than
//! deleting the clips with it, as deleting a sequence a clip nests is (Decision 69):
//! the user removes or replaces the clips first. Removing a bin keeps its media and bins,
//! which move to its parent.

use crate::error::EditError;
use crate::id::{BinId, ItemId, MediaId};
use crate::media_pool::{ColourLabel, MediaItem, file_stem};
use crate::model::{ClipSource, Project};

impl Project {
    /// Adds `items` to `bin`, in order, each under a new id from the node counter (their
    /// `id` and `bin` fields are replaced). A name that is empty once trimmed becomes the
    /// file's name without its extension. Label [`crate::EditLabel::AddMedia`].
    pub fn import_media(
        &self,
        bin: BinId,
        items: impl IntoIterator<Item = MediaItem>,
    ) -> Result<(Project, Vec<MediaId>), EditError> {
        if self.media.bin(bin).is_none() {
            return Err(EditError::UnknownBin(bin));
        }
        let mut next = self.clone();
        let mut ids = Vec::new();
        for mut item in items {
            let name = item.name.trim();
            item.name = if name.is_empty() {
                file_stem(&item.path).to_string()
            } else {
                name.to_string()
            };
            let id = MediaId(next.alloc_id());
            next.media.insert_item(id, bin, item)?;
            ids.push(id);
        }
        if ids.is_empty() {
            return Ok((self.clone(), ids));
        }
        Ok((next, ids))
    }

    /// The first clip showing `media`, in sequence, track and item order (compounds'
    /// contents included).
    pub fn media_user(&self, media: MediaId) -> Option<ItemId> {
        self.sequences
            .iter()
            .flat_map(|s| s.tracks.iter())
            .flat_map(|t| t.items.iter())
            .find(|i| {
                i.clip()
                    .is_some_and(|c| c.source == ClipSource::Media(media))
            })
            .map(|i| i.id)
    }

    /// Removes media items from the pool. Refused with [`EditError::UnknownMedia`] for the
    /// first id the pool does not hold, then with [`EditError::MediaInUse`] for the first
    /// one a clip still shows ([`Project::media_user`]). Label
    /// [`crate::EditLabel::RemoveMedia`].
    pub fn remove_media(&self, media: &[MediaId]) -> Result<Project, EditError> {
        self.check_media(media)?;
        for id in media {
            if let Some(item) = self.media_user(*id) {
                return Err(EditError::MediaInUse { media: *id, item });
            }
        }
        if media.is_empty() {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        for id in media {
            // Listed twice: already gone.
            let _ = next.media.remove_item(*id);
        }
        Ok(next)
    }

    /// Renames a media item; the name is trimmed and may not be empty. Label
    /// [`crate::EditLabel::RenameMedia`].
    pub fn rename_media(&self, media: MediaId, name: &str) -> Result<Project, EditError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(EditError::EmptyMediaName(media));
        }
        let item = self
            .media
            .get(media)
            .ok_or(EditError::UnknownMedia(media))?;
        if item.name == name {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        next.media
            .update_item(media, |i| i.name = name.to_string())?;
        Ok(next)
    }

    /// Sets (or with `None` clears) the colour label of media items. Label
    /// [`crate::EditLabel::LabelMedia`].
    pub fn set_media_label(
        &self,
        media: &[MediaId],
        label: Option<ColourLabel>,
    ) -> Result<Project, EditError> {
        self.check_media(media)?;
        if self.all_media(media, |i| i.label == label) {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        next.media.set_label(media, label)?;
        Ok(next)
    }

    /// Adds `tag` (trimmed, not empty) to the media items that do not have it; tags
    /// compare ignoring case, and a tag already in the pool keeps its first spelling.
    /// Label [`crate::EditLabel::TagMedia`].
    pub fn add_media_tag(&self, media: &[MediaId], tag: &str) -> Result<Project, EditError> {
        let tag = tag.trim();
        if tag.is_empty() {
            return Err(EditError::EmptyTag);
        }
        self.check_media(media)?;
        if self.all_media(media, |i| i.has_tag(tag)) {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        next.media.add_tag(media, tag)?;
        Ok(next)
    }

    /// Removes `tag` (ignoring case) from media items. Label
    /// [`crate::EditLabel::UntagMedia`].
    pub fn remove_media_tag(&self, media: &[MediaId], tag: &str) -> Result<Project, EditError> {
        let tag = tag.trim();
        if tag.is_empty() {
            return Err(EditError::EmptyTag);
        }
        self.check_media(media)?;
        if self.all_media(media, |i| !i.has_tag(tag)) {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        next.media.remove_tag(media, tag)?;
        Ok(next)
    }

    /// Moves media items into `bin`. Label [`crate::EditLabel::MoveMedia`].
    pub fn move_media(&self, media: &[MediaId], bin: BinId) -> Result<Project, EditError> {
        self.check_media(media)?;
        if self.media.bin(bin).is_none() {
            return Err(EditError::UnknownBin(bin));
        }
        if self.all_media(media, |i| i.bin == bin) {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        next.media.move_items(media, bin)?;
        Ok(next)
    }

    /// Adds a bin named `name` (trimmed, not empty) inside `parent`, under a new id from
    /// the node counter. Label [`crate::EditLabel::NewBin`].
    pub fn add_bin(&self, parent: BinId, name: &str) -> Result<(Project, BinId), EditError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(EditError::EmptyBinName);
        }
        if self.media.bin(parent).is_none() {
            return Err(EditError::UnknownBin(parent));
        }
        let mut next = self.clone();
        let id = BinId(next.alloc_id());
        next.media.insert_bin(id, parent, name)?;
        Ok((next, id))
    }

    /// Renames a bin (trimmed, not empty). The root bin, which carries the project's
    /// name, is refused. Label [`crate::EditLabel::RenameBin`].
    pub fn rename_bin(&self, bin: BinId, name: &str) -> Result<Project, EditError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(EditError::EmptyBinName);
        }
        let old = self.media.bin(bin).ok_or(EditError::UnknownBin(bin))?;
        if bin == self.media.root() {
            return Err(EditError::RootBin);
        }
        if old.name == name {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        next.media.rename_bin(bin, name)?;
        Ok(next)
    }

    /// Moves a bin into `parent`. Refuses the root bin, unknown bins and moving a bin
    /// into itself or one of its own bins. Label [`crate::EditLabel::MoveBin`].
    pub fn move_bin(&self, bin: BinId, parent: BinId) -> Result<Project, EditError> {
        let mut next = self.clone();
        next.media.move_bin(bin, parent)?;
        if self
            .media
            .bin(bin)
            .is_some_and(|b| b.parent == Some(parent))
        {
            return Ok(self.clone());
        }
        Ok(next)
    }

    /// Removes a bin; its media and bins move to its parent, so no media is lost. The
    /// root bin is refused. Label [`crate::EditLabel::RemoveBin`].
    pub fn remove_bin(&self, bin: BinId) -> Result<Project, EditError> {
        let mut next = self.clone();
        next.media.remove_bin(bin)?;
        Ok(next)
    }

    /// Names a colour label (`Selects`); the name is trimmed, and an empty one goes back
    /// to the label's colour name. Label [`crate::EditLabel::RenameLabel`].
    pub fn set_label_name(&self, label: ColourLabel, name: &str) -> Result<Project, EditError> {
        let name = name.trim();
        if self.media.label_name(label).unwrap_or("") == name {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        next.media.set_label_name(label, name);
        Ok(next)
    }

    /// The first id of `media` the pool does not hold.
    fn check_media(&self, media: &[MediaId]) -> Result<(), EditError> {
        match media.iter().find(|id| self.media.get(**id).is_none()) {
            Some(id) => Err(EditError::UnknownMedia(*id)),
            None => Ok(()),
        }
    }

    fn all_media(&self, media: &[MediaId], f: impl Fn(&MediaItem) -> bool) -> bool {
        media
            .iter()
            .filter_map(|id| self.media.get(*id))
            .all(|i| f(i))
    }
}
