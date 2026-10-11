//! The media pool panel's view of the open project. The project file holds only media
//! references until the media pool joins the document (`project::media_pool`), so the
//! pool lists every reference in one bin under the project's name, named after its
//! file, its kind from the extension, offline when the file is missing.

use std::path::Path;

use engine::media_mgmt::classify::{FileKind, MediaClass, classify};
use engine::project::media_pool::{Facts, MediaKind, Metadata};
use engine::project::{BinId, MediaId, MediaRefs, Project};
use ui::panels::media_pool::{BinRow, ItemRow, MediaPoolAction, MediaPoolView, ProxyState};
use ui::panels::metadata::MetadataView;

/// The pool view and the references it was built from.
#[derive(Default)]
pub struct Pool {
    pub view: MediaPoolView,
    pub metadata: MetadataView,
    built_from: Option<MediaRefs>,
}

fn kind(path: &Path) -> MediaKind {
    match classify(path) {
        FileKind::Media(MediaClass::Audio) => MediaKind::Audio,
        FileKind::Media(MediaClass::Still) => MediaKind::Still,
        FileKind::Media(MediaClass::Sequence) => MediaKind::Sequence,
        _ => MediaKind::Video,
    }
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl Pool {
    /// Rebuilds the rows when the project's references changed since the last call
    /// (checking file presence then, never per frame).
    pub fn refresh(&mut self, project: &Project) {
        if self
            .built_from
            .as_ref()
            .is_some_and(|refs| refs.ptr_eq(project.media()))
        {
            return;
        }
        self.built_from = Some(project.media().clone());
        let items: Vec<ItemRow> = project
            .media()
            .iter()
            .map(|media| {
                let path = Path::new(&media.path);
                ItemRow {
                    id: media.id,
                    name: name(path),
                    kind: kind(path),
                    label: None,
                    duration_s: None,
                    width: 0,
                    height: 0,
                    fps: None,
                    codec: String::new(),
                    camera: String::new(),
                    tags: Vec::new(),
                    offline: !path.exists(),
                    proxy: ProxyState::None,
                    analysing: None,
                    thumbnail: None,
                }
            })
            .collect();
        let view = &mut self.view;
        view.bins = vec![BinRow {
            id: BinId(0),
            name: project.name().to_owned(),
            depth: 0,
            count: items.len(),
            has_children: false,
            expanded: true,
        }];
        view.bin_name = project.name().to_owned();
        view.total = items.len();
        view.selected.retain(|id| items.iter().any(|i| i.id == *id));
        view.items = items;
        self.select_changed(project);
    }

    /// Applies what the panel returned that concerns the pool itself.
    pub fn action(&mut self, action: &MediaPoolAction, project: &Project) {
        if matches!(action, MediaPoolAction::SelectionChanged) {
            self.select_changed(project);
        }
        if let MediaPoolAction::SetView(view) = action {
            self.view.view = *view;
        }
    }

    fn select_changed(&mut self, project: &Project) {
        let first: Option<MediaId> = self.view.selected.iter().next().copied();
        self.metadata = match first.and_then(|id| project.media().get(id)) {
            Some(media) => {
                let path = Path::new(&media.path);
                let file = name(path);
                MetadataView::new(
                    &file,
                    Some(&file),
                    None,
                    &Facts::default(),
                    &Metadata::default(),
                )
            }
            None => MetadataView::default(),
        };
        self.view.metadata = Some(self.metadata.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_follow_the_references() {
        let (p, a) = Project::new("uid-pool", "Film")
            .add_media("h1", "/nowhere/a.mov")
            .expect("media");
        let (p, _) = p.add_media("h2", "/nowhere/b.wav").expect("media");
        let mut pool = Pool::default();
        pool.refresh(&p);
        let names: Vec<&str> = pool.view.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["a.mov", "b.wav"]);
        assert_eq!(pool.view.items[1].kind, MediaKind::Audio);
        assert!(pool.view.items.iter().all(|i| i.offline));
        pool.view.selected.insert(a);
        pool.action(&MediaPoolAction::SelectionChanged, &p);
        assert_eq!(pool.metadata.clip_name, "a.mov");
    }
}
