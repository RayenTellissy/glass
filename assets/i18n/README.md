# User-facing strings

Every string Glass shows to a user lives here, one JSON file per locale. English (`en.json`) is embedded in the binary; it is the source every other locale is translated from and the fallback for any key a locale is missing.

## File format

```json
{
  "locale": "en",
  "name": "English",
  "direction": "ltr",
  "strings": {
    "app.name": "Glass",
    "export.progress": "{done} of {total}"
  }
}
```

- `locale`: the language code (BCP 47, for example `en`, `pt-BR`, `ar`).
- `name`: the language's own name for itself, for the language picker.
- `direction`: `ltr` or `rtl`. Right-to-left layout is Phase 6; the flag is already read so the layout code has one place to ask (`Locale::is_rtl`).
- `strings`: a flat map of dotted keys to text. Flat keys keep every key greppable as written in code, and the dots give the grouping a nested file would.

Keys are lowercase dotted segments with at least two parts, each `[a-z0-9_]+`: `<area>.<thing>`, or `<area>.<group>.<thing>` (`settings.crash_reports.title`). The area is the screen or panel the string belongs to. Name a key after what the string is for, not after its English words, so the key survives a copy change.

Placeholders are named: `{done}`, `{total}`. `{{` and `}}` are literal braces. Every call passes exactly the placeholders its string uses; the source check fails otherwise.

## In code

```rust
use ui::t;

ui.label(t!("app.name"));
ui.label(t!("export.progress", done = finished, total = queued));
```

`t!` is defined in the `assets` crate (`assets::t!`, re-exported as `ui::t!`) and returns a `String`. The key must be a string literal so it can be checked. A key missing from the current locale falls back to English; a key missing from English too shows as `⟦key⟧` and logs one warning per key. A lookup never panics.

Other locales load with `assets::i18n::Locale::from_json` (or come from `assets::i18n::BUNDLED` once more are bundled) and become current with `assets::i18n::set_locale`.

## The check

`cargo test -p app --test ui_source -- --nocapture` (CI runs it in the check job) scans `crates/ui/src` and `crates/app/src` and fails when:

- a `t!` key does not exist in `en.json`, or its named arguments differ from the string's placeholders;
- a `t!` key is not a string literal;
- a string literal with words in it goes to an egui text API (`label`, `heading`, `button`, `small_button`, `checkbox`, `radio_value`, `selectable_label`, `hyperlink_to`, `collapsing`, `menu_button`, `on_hover_text`, `hint_text`, `prefix`, `suffix`, painter `text`, `RichText::new`, `Label::new`, `Button::new`, `Window::new`, `CollapsingHeader::new`, `ComboBox::from_label` and a few more; the full lists are `TEXT_METHODS` and `TEXT_CONSTRUCTORS` in `crates/app/tests/support/source_scan.rs`). Text built with `format!` counts too; `format!("{} · {}", a, b)` does not, since it has no words of its own.

Keys that no code uses yet are printed as warnings, not failures, so strings for a screen can land before the screen.

A string that is never shown to users (a debug overlay, a developer-only label) can be marked with a comment on the same line or the line before, and the reason is required:

```rust
ui.label(format!("{fps:.0} fps")); // allow-literal-text: debug overlay, developers only
```

`#[cfg(test)]` code and `crates/*/tests/` are not scanned.

## Voice

Copy follows the tone of voice in the Feel extraction on the `00 Foundations` page of the Paper file "Inspiration 2", and the language rules in its design brief:

- Calm, short and reassuring. A title plus one plain line.
- Say what happened and what to do next. Never "Oops".
- Sentence case. No exclamation marks.
- Plain language on Beginner surfaces; Pro surfaces may use the standard editing terms.
- Promises and metadata are split by middle dots (" · "); numbers are shown in Geist Mono by the UI, not by the string.
- No lorem ipsum, ever; a string that is not written yet does not get a key yet.
