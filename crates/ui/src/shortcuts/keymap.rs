//! Chords, keymaps and conflicts.
//!
//! A [`Chord`] is one key with modifiers. `cmd` is the platform's primary modifier: ⌘ on
//! macOS, Ctrl on Windows and Linux, so one preset serves every platform. `ctrl` is the
//! Control key on macOS; elsewhere it folds into Ctrl too ([`Chord::on`]), and any clash
//! that makes is reported like any other. Presets need no multi-key sequences, so there
//! are none.
//!
//! A [`Keymap`] is a preset plus the user's overrides: a command with an override uses
//! exactly the override's chords (none for an unbound command); every other command uses
//! the preset's. Only the preset name and the overrides are stored, so a preset fixed in
//! a later version reaches users who never touched that command.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use egui::Key;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::commands::{self, COMMANDS};
use super::presets::{self, PresetId};

/// The platform a keymap is read for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    Mac,
    Windows,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }

    pub fn is_mac(self) -> bool {
        self == Platform::Mac
    }
}

/// Modifier keys of a chord.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Mods {
    /// ⌘ on macOS, Ctrl elsewhere.
    pub cmd: bool,
    /// Control on macOS; folds into Ctrl elsewhere.
    pub ctrl: bool,
    /// ⌥ Option on macOS, Alt elsewhere.
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        cmd: false,
        ctrl: false,
        alt: false,
        shift: false,
    };

    pub fn is_none(self) -> bool {
        self == Mods::NONE
    }
}

/// One key with modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

impl Chord {
    pub const fn key(key: Key) -> Self {
        Self {
            mods: Mods::NONE,
            key,
        }
    }

    pub const fn cmd(mut self) -> Self {
        self.mods.cmd = true;
        self
    }

    pub const fn ctrl(mut self) -> Self {
        self.mods.ctrl = true;
        self
    }

    pub const fn alt(mut self) -> Self {
        self.mods.alt = true;
        self
    }

    pub const fn shift(mut self) -> Self {
        self.mods.shift = true;
        self
    }

    /// The chord as `platform` sees it: shifted punctuation becomes its base key with
    /// Shift (`+` is ⇧=, `?` is ⇧/), and off macOS Control folds into Ctrl. Matching
    /// and clash checks compare chords in this form.
    pub fn on(self, platform: Platform) -> Self {
        let mut chord = self.unshifted();
        if !platform.is_mac() && chord.mods.ctrl {
            chord.mods.ctrl = false;
            chord.mods.cmd = true;
        }
        chord
    }

    fn unshifted(self) -> Self {
        let base = match self.key {
            Key::Plus => Key::Equals,
            Key::Questionmark => Key::Slash,
            Key::Colon => Key::Semicolon,
            Key::Pipe => Key::Backslash,
            Key::OpenCurlyBracket => Key::OpenBracket,
            Key::CloseCurlyBracket => Key::CloseBracket,
            Key::Exclamationmark => Key::Num1,
            _ => return self,
        };
        Self {
            mods: Mods {
                shift: true,
                ..self.mods
            },
            key: base,
        }
    }

    /// The chord an egui key event makes on `platform`.
    pub fn from_egui(key: Key, modifiers: egui::Modifiers, platform: Platform) -> Self {
        let mods = if platform.is_mac() {
            Mods {
                cmd: modifiers.mac_cmd,
                ctrl: modifiers.ctrl,
                alt: modifiers.alt,
                shift: modifiers.shift,
            }
        } else {
            Mods {
                cmd: modifiers.ctrl || modifiers.command,
                ctrl: false,
                alt: modifiers.alt,
                shift: modifiers.shift,
            }
        };
        Self { mods, key }.on(platform)
    }

    /// Parses the stored form, `Cmd+Shift+K` (modifiers in any order, `Ctrl`, `Alt`,
    /// `Shift`, `Cmd`; the key by egui's key name or character).
    pub fn parse(text: &str) -> Result<Self, ChordError> {
        let text = text.trim();
        // A trailing `+` is the key itself (`Cmd++`).
        let (mods_part, key_part) = match text.strip_suffix("++") {
            Some(rest) => (Some(rest), "+"),
            None => match text.rsplit_once('+') {
                Some((mods, key)) => (Some(mods), key),
                None => (None, text),
            },
        };
        let key = Key::from_name(key_part.trim())
            .ok_or_else(|| ChordError(format!("unknown key {key_part:?} in {text:?}")))?;
        let mut mods = Mods::NONE;
        for m in mods_part.into_iter().flat_map(|m| m.split('+')) {
            let flag = match m.trim().to_ascii_lowercase().as_str() {
                "cmd" | "command" => &mut mods.cmd,
                "ctrl" | "control" => &mut mods.ctrl,
                "alt" | "option" | "opt" => &mut mods.alt,
                "shift" => &mut mods.shift,
                _ => return Err(ChordError(format!("unknown modifier {m:?} in {text:?}"))),
            };
            if *flag {
                return Err(ChordError(format!("modifier {m:?} twice in {text:?}")));
            }
            *flag = true;
        }
        Ok(Self { mods, key })
    }
}

/// The stored form, `Ctrl+Alt+Shift+Cmd+K`.
impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.mods;
        for (on, name) in [
            (m.ctrl, "Ctrl"),
            (m.alt, "Alt"),
            (m.shift, "Shift"),
            (m.cmd, "Cmd"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(self.key.name())
    }
}

impl Serialize for Chord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Chord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Chord::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChordError(pub String);

impl fmt::Display for ChordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ChordError {}

/// Chords the operating system keeps: they never reach the app, so no command may
/// take them (`Sheet / Keyboard`, "Kept by macOS").
pub fn reserved(platform: Platform) -> &'static [Chord] {
    const MAC: &[Chord] = &[
        Chord::key(Key::Q).cmd(),
        Chord::key(Key::H).cmd(),
        Chord::key(Key::M).cmd(),
        Chord::key(Key::Tab).cmd(),
        Chord::key(Key::Space).cmd(),
        Chord::key(Key::F).ctrl().cmd(),
    ];
    const WINDOWS: &[Chord] = &[
        Chord::key(Key::Tab).alt(),
        Chord::key(Key::F4).alt(),
        Chord::key(Key::Escape).cmd(),
        Chord::key(Key::Escape).cmd().shift(),
    ];
    const LINUX: &[Chord] = &[Chord::key(Key::Tab).alt(), Chord::key(Key::F4).alt()];
    match platform {
        Platform::Mac => MAC,
        Platform::Windows => WINDOWS,
        Platform::Linux => LINUX,
    }
}

pub fn is_reserved(chord: Chord, platform: Platform) -> bool {
    let chord = chord.on(platform);
    reserved(platform).iter().any(|r| r.on(platform) == chord)
}

/// The current keymap format.
pub const FORMAT_VERSION: u32 = 1;

fn format_version() -> u32 {
    FORMAT_VERSION
}

/// A preset plus the user's changes, as stored in settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keymap {
    #[serde(default = "format_version")]
    pub version: u32,
    pub preset: PresetId,
    /// Command id to its chords, replacing the preset's for that command. An empty list
    /// unbinds the command. Ids this version does not know are kept and ignored.
    #[serde(default)]
    pub overrides: BTreeMap<String, Vec<Chord>>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new(PresetId::Glass)
    }
}

/// Two or more commands on one chord.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub chord: Chord,
    /// In registry order.
    pub commands: Vec<&'static str>,
}

impl Keymap {
    pub fn new(preset: PresetId) -> Self {
        Self {
            version: FORMAT_VERSION,
            preset,
            overrides: BTreeMap::new(),
        }
    }

    /// The chords `id` has in the preset alone.
    pub fn preset_chords(&self, id: &str) -> &'static [Chord] {
        presets::chords(self.preset, id)
    }

    /// The chords that run `id`, first one shown first.
    pub fn chords(&self, id: &str) -> &[Chord] {
        match self.overrides.get(id) {
            Some(chords) => chords,
            None => self.preset_chords(id),
        }
    }

    /// Whether the user changed `id`'s keys from the preset.
    pub fn is_changed(&self, id: &str) -> bool {
        self.overrides.contains_key(id)
    }

    /// Ids of the known commands the user changed, in registry order.
    pub fn changed(&self) -> Vec<&'static str> {
        COMMANDS
            .iter()
            .map(|c| c.id)
            .filter(|id| self.is_changed(id))
            .collect()
    }

    /// Gives `id` exactly `chords`. Setting the preset's own chords removes the
    /// override.
    pub fn set(&mut self, id: &str, chords: Vec<Chord>) {
        if chords.as_slice() == self.preset_chords(id) {
            self.overrides.remove(id);
        } else {
            self.overrides.insert(id.to_owned(), chords);
        }
    }

    /// Puts `id` back to the preset's keys.
    pub fn reset(&mut self, id: &str) {
        self.overrides.remove(id);
    }

    pub fn reset_all(&mut self) {
        self.overrides.clear();
    }

    /// Switches preset. With `keep_changes` the user's overrides stay on top (and any
    /// that now equal the new preset are dropped); without, they are cleared.
    pub fn switch_preset(&mut self, preset: PresetId, keep_changes: bool) {
        self.preset = preset;
        if keep_changes {
            let overrides = std::mem::take(&mut self.overrides);
            for (id, chords) in overrides {
                self.set(&id, chords);
            }
        } else {
            self.overrides.clear();
        }
    }

    /// The commands bound to `chord` on `platform`, in registry order.
    pub fn commands_on(&self, chord: Chord, platform: Platform) -> Vec<&'static str> {
        let chord = chord.on(platform);
        COMMANDS
            .iter()
            .filter(|c| self.chords(c.id).iter().any(|k| k.on(platform) == chord))
            .map(|c| c.id)
            .collect()
    }

    /// Every chord two or more commands share on `platform`, sorted by the first
    /// command's registry position.
    pub fn conflicts(&self, platform: Platform) -> Vec<Conflict> {
        let mut by_chord: BTreeMap<Chord, Vec<&'static str>> = BTreeMap::new();
        for c in COMMANDS {
            let mut seen = Vec::new();
            for chord in self.chords(c.id) {
                let chord = chord.on(platform);
                if !seen.contains(&chord) {
                    seen.push(chord);
                    by_chord.entry(chord).or_default().push(c.id);
                }
            }
        }
        let mut out: Vec<Conflict> = by_chord
            .into_iter()
            .filter(|(chord, ids)| ids.len() > 1 && !self.shared_by_preset(*chord, ids, platform))
            .map(|(chord, commands)| Conflict { chord, commands })
            .collect();
        out.sort_by_key(|c| commands::index(c.commands[0]));
        out
    }

    /// Whether the preset itself gives every one of `ids` the chord and none was
    /// changed: Final Cut Pro's and Resolve's one trim tool key runs both ripple and roll
    /// trim, which is the preset's design, not a clash.
    fn shared_by_preset(&self, chord: Chord, ids: &[&'static str], platform: Platform) -> bool {
        ids.iter().all(|id| {
            !self.is_changed(id)
                && self
                    .preset_chords(id)
                    .iter()
                    .any(|k| k.on(platform) == chord)
        })
    }

    /// Commands bound to a chord the system keeps on `platform`.
    pub fn reserved_bindings(&self, platform: Platform) -> Vec<(&'static str, Chord)> {
        COMMANDS
            .iter()
            .flat_map(|c| {
                self.chords(c.id)
                    .iter()
                    .filter(|k| is_reserved(**k, platform))
                    .map(move |k| (c.id, *k))
            })
            .collect()
    }

    /// Gives `id` the chord as its only key, leaving any other command on it (a clash
    /// the editor shows). Returns the commands that already had it.
    pub fn assign(&mut self, id: &str, chord: Chord, platform: Platform) -> Vec<&'static str> {
        let others: Vec<&'static str> = self
            .commands_on(chord, platform)
            .into_iter()
            .filter(|other| *other != id)
            .collect();
        self.set(id, vec![chord]);
        others
    }

    /// Gives `id` the chord and takes it from every other command ("Take it").
    pub fn take(&mut self, id: &str, chord: Chord, platform: Platform) {
        let target = chord.on(platform);
        for other in self.commands_on(chord, platform) {
            if other != id {
                let left: Vec<Chord> = self
                    .chords(other)
                    .iter()
                    .copied()
                    .filter(|k| k.on(platform) != target)
                    .collect();
                self.set(other, left);
            }
        }
        self.set(id, vec![chord]);
    }

    /// Gives `id` the chord and the commands that had it `id`'s old first key
    /// ("Swap").
    pub fn swap(&mut self, id: &str, chord: Chord, platform: Platform) {
        let target = chord.on(platform);
        let old = self.chords(id).first().copied();
        for other in self.commands_on(chord, platform) {
            if other != id {
                let chords: Vec<Chord> = self
                    .chords(other)
                    .iter()
                    .filter_map(|k| {
                        if k.on(platform) == target {
                            old
                        } else {
                            Some(*k)
                        }
                    })
                    .collect();
                self.set(other, chords);
            }
        }
        self.set(id, vec![chord]);
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a keymap always serialises")
    }

    pub fn from_json(text: &str) -> Result<Self, KeymapError> {
        let keymap: Keymap = serde_json::from_str(text).map_err(KeymapError::Format)?;
        if keymap.version > FORMAT_VERSION {
            return Err(KeymapError::Newer(keymap.version));
        }
        Ok(keymap)
    }

    /// Reads a keymap file. A missing file is `Ok(None)`.
    pub fn load(path: &Path) -> Result<Option<Self>, KeymapError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_json(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(KeymapError::Io(e)),
        }
    }

    /// Writes the keymap to `path` through a temporary file in the same folder, so a
    /// crash mid-write leaves the old file.
    pub fn save(&self, path: &Path) -> Result<(), KeymapError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(KeymapError::Io)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        std::fs::write(&tmp, self.to_json()).map_err(KeymapError::Io)?;
        std::fs::rename(&tmp, path).map_err(KeymapError::Io)
    }
}

#[derive(Debug)]
pub enum KeymapError {
    Io(std::io::Error),
    Format(serde_json::Error),
    /// Written by a newer Glass with this format version.
    Newer(u32),
}

impl fmt::Display for KeymapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeymapError::Io(e) => write!(f, "keymap file: {e}"),
            KeymapError::Format(e) => write!(f, "keymap format: {e}"),
            KeymapError::Newer(v) => write!(f, "keymap format {v} is newer than this Glass"),
        }
    }
}

impl std::error::Error for KeymapError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_round_trip_through_their_stored_form() {
        for text in [
            "K",
            "Cmd+K",
            "Ctrl+Alt+Shift+Cmd+Backspace",
            "Shift+Slash",
            "Space",
        ] {
            let chord = Chord::parse(text).unwrap();
            assert_eq!(chord.to_string(), text);
        }
        assert_eq!(
            Chord::parse("shift + cmd + z").unwrap(),
            Chord::key(Key::Z).cmd().shift()
        );
        assert_eq!(Chord::parse("Cmd++").unwrap(), Chord::key(Key::Plus).cmd());
        assert!(Chord::parse("Hyper+K").is_err());
        assert!(Chord::parse("Cmd+Cmd+K").is_err());
        assert!(Chord::parse("Cmd+Nope").is_err());
    }

    #[test]
    fn control_folds_into_ctrl_off_macos() {
        let chord = Chord::key(Key::D).ctrl();
        assert_eq!(chord.on(Platform::Mac), chord);
        assert_eq!(chord.on(Platform::Windows), Chord::key(Key::D).cmd());
        let plus = Chord::key(Key::Plus).cmd();
        assert_eq!(
            plus.on(Platform::Mac),
            Chord::key(Key::Equals).cmd().shift()
        );
    }

    #[test]
    fn egui_modifiers_map_per_platform() {
        let mac = egui::Modifiers {
            mac_cmd: true,
            command: true,
            ..Default::default()
        };
        assert_eq!(
            Chord::from_egui(Key::K, mac, Platform::Mac),
            Chord::key(Key::K).cmd()
        );
        let control = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(
            Chord::from_egui(Key::K, control, Platform::Mac),
            Chord::key(Key::K).ctrl()
        );
        let windows = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        assert_eq!(
            Chord::from_egui(Key::K, windows, Platform::Windows),
            Chord::key(Key::K).cmd()
        );
    }

    #[test]
    fn overrides_sit_on_top_and_reset() {
        let mut keymap = Keymap::default();
        assert_eq!(
            keymap.chords("edit.freeze_frame"),
            &[Chord::key(Key::F).alt()]
        );
        keymap.set("edit.freeze_frame", vec![Chord::key(Key::F)]);
        assert!(keymap.is_changed("edit.freeze_frame"));
        assert_eq!(keymap.chords("edit.freeze_frame"), &[Chord::key(Key::F)]);
        assert_eq!(keymap.changed(), vec!["edit.freeze_frame"]);
        keymap.set("edit.freeze_frame", vec![Chord::key(Key::F).alt()]);
        assert!(
            !keymap.is_changed("edit.freeze_frame"),
            "the preset's own keys"
        );
        keymap.set("tool.hand", vec![]);
        assert!(keymap.chords("tool.hand").is_empty());
        keymap.reset("tool.hand");
        assert_eq!(keymap.chords("tool.hand"), &[Chord::key(Key::H)]);
    }

    #[test]
    fn switching_preset_keeps_or_drops_changes() {
        let mut keymap = Keymap::default();
        keymap.set("edit.freeze_frame", vec![Chord::key(Key::F)]);
        keymap.switch_preset(PresetId::FinalCut, true);
        assert_eq!(keymap.chords("tool.select"), &[Chord::key(Key::A)]);
        assert_eq!(keymap.chords("edit.freeze_frame"), &[Chord::key(Key::F)]);
        keymap.switch_preset(PresetId::Premiere, false);
        assert!(keymap.changed().is_empty());
    }

    #[test]
    fn a_shared_chord_is_a_conflict_and_take_and_swap_resolve_it() {
        let platform = Platform::Mac;
        let mut keymap = Keymap::default();
        assert!(
            keymap.conflicts(platform).is_empty(),
            "the default has none"
        );
        let s = Chord::key(Key::S);
        let others = keymap.assign("view.snapping", s, platform);
        assert_eq!(others, vec!["tool.rate_stretch"]);
        assert_eq!(
            keymap.conflicts(platform),
            vec![Conflict {
                chord: s,
                commands: vec!["tool.rate_stretch", "view.snapping"],
            }]
        );

        let mut taken = keymap.clone();
        taken.take("view.snapping", s, platform);
        assert!(taken.conflicts(platform).is_empty());
        assert!(taken.chords("tool.rate_stretch").is_empty());

        let mut swapped = Keymap::default();
        swapped.swap("view.snapping", s, platform);
        assert!(swapped.conflicts(platform).is_empty());
        assert_eq!(
            swapped.chords("tool.rate_stretch"),
            &[Chord::key(Key::S).shift()],
            "rate stretch gets snapping's old key"
        );
    }

    #[test]
    fn conflicts_are_per_platform() {
        let mut keymap = Keymap::default();
        keymap.set("edit.group", vec![Chord::key(Key::G).ctrl()]);
        keymap.set("edit.link", vec![Chord::key(Key::G).cmd()]);
        assert!(keymap.conflicts(Platform::Mac).is_empty());
        assert_eq!(keymap.conflicts(Platform::Windows).len(), 1);
    }

    #[test]
    fn reserved_chords_are_found() {
        let mut keymap = Keymap::default();
        assert!(keymap.reserved_bindings(Platform::Mac).is_empty());
        keymap.set("edit.save", vec![Chord::key(Key::Q).cmd()]);
        assert_eq!(
            keymap.reserved_bindings(Platform::Mac),
            vec![("edit.save", Chord::key(Key::Q).cmd())]
        );
        assert!(is_reserved(Chord::key(Key::F4).alt(), Platform::Windows));
    }

    #[test]
    fn keymaps_round_trip_through_json_and_files() {
        let mut keymap = Keymap::new(PresetId::Resolve);
        keymap.set("edit.freeze_frame", vec![Chord::key(Key::F)]);
        keymap.set("tool.hand", vec![]);
        let json = keymap.to_json();
        assert!(json.contains("\"preset\": \"resolve\""), "{json}");
        assert!(
            json.contains("\"edit.freeze_frame\": [\n      \"F\"\n    ]"),
            "{json}"
        );
        assert_eq!(Keymap::from_json(&json).unwrap(), keymap);

        let dir = std::env::temp_dir().join(format!("glass-keymap-{}", std::process::id()));
        let path = dir.join("keymap.json");
        assert!(Keymap::load(&path).unwrap().is_none());
        keymap.save(&path).unwrap();
        assert_eq!(Keymap::load(&path).unwrap(), Some(keymap));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_ids_are_kept_and_newer_formats_refused() {
        let json = r#"{ "preset": "premiere", "overrides": { "future.thing": ["Cmd+J"] } }"#;
        let keymap = Keymap::from_json(json).unwrap();
        assert_eq!(keymap.version, FORMAT_VERSION);
        assert!(keymap.changed().is_empty());
        assert!(keymap.to_json().contains("future.thing"));
        assert!(matches!(
            Keymap::from_json(r#"{ "version": 99, "preset": "glass" }"#),
            Err(KeymapError::Newer(99))
        ));
        assert!(Keymap::from_json(r#"{ "preset": "nope" }"#).is_err());
    }
}
