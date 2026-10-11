//! Showing a finished export in the platform's file manager: Finder with the file
//! selected (`open -R`), Explorer with the file selected (`explorer /select,`), or the
//! desktop's handler for the containing folder on Linux (`xdg-open`, which cannot select
//! a file). The command is spawned and left running, never waited for.
//!
//! [`reveal_command`] builds the command for a platform without running it, and
//! [`FileManager`] runs it, so tests check the commands without opening windows.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// The desktop platforms Glass reveals files on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    MacOs,
    Windows,
    /// Linux and other Unix desktops with `xdg-open`.
    Linux,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// A program and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevealCommand {
    pub program: &'static str,
    pub args: Vec<OsString>,
}

/// The command that shows `path` in `platform`'s file manager.
pub fn reveal_command(path: &Path, platform: Platform) -> RevealCommand {
    match platform {
        Platform::MacOs => RevealCommand {
            program: "open",
            args: vec!["-R".into(), path.as_os_str().to_owned()],
        },
        // Explorer reads `/select,` and the path as separate arguments too, which keeps a
        // path with spaces in one quoted argument.
        Platform::Windows => RevealCommand {
            program: "explorer",
            args: vec!["/select,".into(), path.as_os_str().to_owned()],
        },
        Platform::Linux => {
            let folder = match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => Path::new("."),
            };
            RevealCommand {
                program: "xdg-open",
                args: vec![folder.as_os_str().to_owned()],
            }
        }
    }
}

/// Runs a reveal command.
pub trait FileManager {
    fn launch(&self, command: &RevealCommand) -> std::io::Result<()>;
}

/// Spawns the command and does not wait for it.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemFileManager;

impl FileManager for SystemFileManager {
    fn launch(&self, command: &RevealCommand) -> std::io::Result<()> {
        Command::new(command.program)
            .args(&command.args)
            .spawn()
            .map(drop)
    }
}

/// Shows `path` in this platform's file manager through `manager`.
pub fn reveal_with(path: &Path, manager: &dyn FileManager) -> std::io::Result<()> {
    manager.launch(&reveal_command(path, Platform::current()))
}

/// Shows `path` in Finder, Explorer or the Linux desktop's file manager.
pub fn reveal_in_file_manager(path: &Path) -> std::io::Result<()> {
    reveal_with(path, &SystemFileManager)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use super::*;

    fn args(command: &RevealCommand) -> Vec<&str> {
        command
            .args
            .iter()
            .map(|a| a.to_str().expect("utf-8"))
            .collect()
    }

    #[test]
    fn commands_per_platform() {
        let file = Path::new("/Users/me/Movies/My Film.mp4");
        let mac = reveal_command(file, Platform::MacOs);
        assert_eq!(mac.program, "open");
        assert_eq!(args(&mac), ["-R", "/Users/me/Movies/My Film.mp4"]);

        let file = Path::new(r"C:\Users\me\Videos\My Film.mp4");
        let windows = reveal_command(file, Platform::Windows);
        assert_eq!(windows.program, "explorer");
        assert_eq!(
            args(&windows),
            ["/select,", r"C:\Users\me\Videos\My Film.mp4"]
        );

        let linux = reveal_command(Path::new("/home/me/Videos/out.mkv"), Platform::Linux);
        assert_eq!(linux.program, "xdg-open");
        assert_eq!(args(&linux), ["/home/me/Videos"]);
        let bare = reveal_command(Path::new("out.mkv"), Platform::Linux);
        assert_eq!(args(&bare), ["."]);
    }

    #[derive(Default)]
    struct Recorder(RefCell<Vec<RevealCommand>>);

    impl FileManager for Recorder {
        fn launch(&self, command: &RevealCommand) -> std::io::Result<()> {
            self.0.borrow_mut().push(command.clone());
            Ok(())
        }
    }

    #[test]
    fn reveal_hands_this_platforms_command_to_the_file_manager() {
        let recorder = Recorder::default();
        let path = PathBuf::from("/tmp/export/out.mov");
        reveal_with(&path, &recorder).expect("recorded");
        assert_eq!(
            recorder.0.into_inner(),
            [reveal_command(&path, Platform::current())]
        );
    }
}
