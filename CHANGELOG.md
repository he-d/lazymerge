# Changelog

All notable changes to LazyMerge are documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Earlier versions are listed on
[GitHub Releases](https://github.com/he-d/lazymerge/releases).

## [Unreleased]

## [0.1.5] - 2026-11-25

### Added

- Terminal app: compare two files directly, e.g. `lazymerge -l left_file.txt -r right_file.txt` or
  `lazymerge left_file.txt right_file.txt`. The file view opens at once and fills the window, without
  the folder tree; the files may have different names. The path prompts accept files as well, and
  completion offers files once the other side is a file. A folder and a file are rejected.
- Terminal app: `dd` deletes the line, or the selected lines, on both sides. The deletion is staged
  until saved, and one `u` restores both sides.
- Terminal app: `zz` scrolls the cursor row to the middle, like vim; in the folder tree it centers
  the selected row.

### Changed

- Terminal app: `Ctrl+d` / `Ctrl+u` put the cursor row in the middle after moving half a page, as in
  the browser app.
- Lua config: new bindings `delete_both = "dd"` and `center = "zz"`.

## [0.1.4] - 2026-11-19

### Added

- Terminal app: `dh` / `dl` delete the line on the left / right, or the selected lines on that
  side. The deletion is staged until saved, `u` undoes it, and the cursor moves on to that side's
  next line.
- Terminal app: a line missing on one side is shown with red diagonal stripes, as in the browser app.
- Lua config: key bindings of two characters such as `gg` are typed one after the other.

### Changed

- Terminal app: `gg` also jumps to the first row in the folder tree; a single `g` no longer does.
  The default binding is now `top = "gg"`. Older configs with `top = "g"` keep working as `gg`.

### Fixed

- The help, `config.example.lua` and the README listed `g` instead of `gg` for jumping to the top.

[Unreleased]: https://github.com/he-d/lazymerge/compare/v0.1.5...HEAD
[0.1.5]: https://github.com/he-d/lazymerge/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/he-d/lazymerge/compare/v0.1.3...v0.1.4
