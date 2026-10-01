//! What the About dialog says about this build: the package version and repository from
//! Cargo metadata (so the source archives link follows `repository` in Cargo.toml), and
//! the commit and date `build.rs` recorded.

use ui::dialogs::about::AboutInfo;

use crate::locations::Locations;

pub fn info(locations: &Locations) -> AboutInfo {
    AboutInfo {
        commit: option_env!("GLASS_GIT_COMMIT").map(str::to_owned),
        build_date: option_env!("GLASS_BUILD_DATE").map(str::to_owned),
        settings_folder: Some(locations.config.display().to_string()),
        ..AboutInfo::new(env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_REPOSITORY"))
    }
}
