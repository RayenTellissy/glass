//! The media pool panel's view of the open project's pool (`project::media_pool`,
//! embedded in the project since schema v9): the bins tree, the open bin's items in the
//! chosen order, offline when their file is missing, and the metadata of the first
//! selected item.

use std::collections::BTreeSet;
use std::path::Path;

use engine::project::media_pool::MediaPool;
use engine::project::{BinId, MediaId, Project};
use ui::panels::media_pool::{
    BinSelection, ItemRow, MediaPoolAction, MediaPoolView, bin_rows, metadata_view,
};
use ui::panels::metadata::MetadataView;

/// The pool view and the pool it was built from.
#[derive(Default)]
pub struct Pool {
    pub view: MediaPoolView,
    pub metadata: MetadataView,
    expanded: BTreeSet<BinId>,
    built_from: Option<MediaPool>,
}

impl Pool {
    /// Rebuilds the rows when the project's pool changed since the last call (checking
    /// file presence then, never per frame).
    pub fn refresh(&mut self, project: &Project) {
        if self
            .built_from
            .as_ref()
            .is_some_and(|pool| pool.ptr_eq(project.media()))
        {
            return;
        }
        self.rebuild(project);
    }

    fn rebuild(&mut self, project: &Project) {
        let pool = project.media();
        self.built_from = Some(pool.clone());
        let view = &mut self.view;
        let bin = match view.bin {
            BinSelection::Bin(id) if pool.bin(id).is_some() => id,
            _ => pool.root(),
        };
        view.bin = BinSelection::Bin(bin);
        view.bins = bin_rows(pool, &self.expanded);
        view.bin_name = pool.bin(bin).map(|b| b.name.clone()).unwrap_or_default();
        let mut items = pool.items_in(bin, false);
        MediaPool::sort(&mut items, view.sort, view.descending);
        view.bin_duration_s = items.iter().filter_map(|i| i.facts.duration_s).sum();
        let rows: Vec<ItemRow> = items
            .iter()
            .map(|item| ItemRow::from_item(item, !Path::new(&item.path).exists()))
            .collect();
        view.total = rows.len();
        view.selected.retain(|id| rows.iter().any(|r| r.id == *id));
        view.items = rows;
        self.select_changed(project);
    }

    /// Applies what the panel returned that concerns the pool itself.
    pub fn action(&mut self, action: &MediaPoolAction, project: &Project) {
        match action {
            MediaPoolAction::SelectionChanged => self.select_changed(project),
            MediaPoolAction::SetView(view) => self.view.view = *view,
            MediaPoolAction::SelectBin(bin @ BinSelection::Bin(_)) => {
                self.view.bin = *bin;
                self.view.selected.clear();
                self.rebuild(project);
            }
            MediaPoolAction::ToggleBin(bin) => {
                if !self.expanded.remove(bin) {
                    self.expanded.insert(*bin);
                }
                self.rebuild(project);
            }
            MediaPoolAction::SortBy(by) => {
                self.view.sort = *by;
                self.rebuild(project);
            }
            _ => {}
        }
    }

    fn select_changed(&mut self, project: &Project) {
        let first: Option<MediaId> = self.view.selected.iter().next().copied();
        self.metadata = first
            .and_then(|id| project.media().get(id))
            .map_or_else(MetadataView::default, |item| metadata_view(item));
        self.view.metadata = Some(self.metadata.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::project::media_pool::MediaKind;

    #[test]
    fn rows_follow_the_pool() {
        let (p, a) = Project::new("uid-pool", "Film")
            .add_media("h1", "/nowhere/a.mov")
            .expect("media");
        let (p, _) = p.add_media("h2", "/nowhere/b.wav").expect("media");
        let mut pool = Pool::default();
        pool.refresh(&p);
        assert_eq!(pool.view.bins[0].name, "Film");
        assert_eq!(pool.view.items.len(), 2);
        assert!(pool.view.items.iter().all(|i| i.offline));
        assert!(pool.view.items.iter().any(|i| i.kind == MediaKind::Audio));
        pool.view.selected.insert(a);
        pool.action(&MediaPoolAction::SelectionChanged, &p);
        assert_eq!(
            pool.metadata.clip_name,
            p.media().get(a).expect("item").name
        );
    }
}
