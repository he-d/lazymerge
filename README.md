<h1 align="center">
  <img src="logo.svg" alt="LazyMerge" width="240">
</h1>

<p align="center">
  Compare and merge two folders side by side, right in your browser.<br>
  One self-contained HTML file, no installation, no server, no network – with vim-style keys.
</p>

## Features

- **Folder compare**: both folders as one tree, every file marked as different, only left, only right, nearly identical (only line endings or BOM differ) or identical. Show all files or only the differences, find files by name, filter by pattern.
- **File compare**: the two versions side by side with aligned lines, changed characters highlighted within a line, an overview strip of all differences, and syntax colours for AL and XML-like files.
- **Merge**: take over a section, a few marked lines or the whole file to the other side, edit lines in place, undo every step. Nothing is written until you save.
- **Copy files and folders** from one side to the other (with a confirmation). LazyMerge never deletes anything.
- **Keeps your files as they are**: encoding (UTF-8 with or without BOM) and line endings (CRLF / LF) of the saved file stay unchanged.
- **Notices changes made elsewhere**: if an open file is changed by another program, the page shows it and asks before overwriting.
- **Keyboard first**: vim-style navigation in the folder tree and in the file compare, find with `/`.

## Getting started

1. Download [`lazymerge.html`](lazymerge.html).
2. Open it in **Microsoft Edge** or **Google Chrome** on the desktop, e.g. by double-clicking it. Nothing to install.
3. Click **Left folder…** and **Right folder…**. The browser asks once per folder whether the page may read and write it.

LazyMerge needs the [File System Access API](https://developer.mozilla.org/en-US/docs/Web/API/Window/showDirectoryPicker) of the browser to work with local folders. In a browser without it, the page says so.

## Your files stay on your machine

The page contains no network code: it loads nothing from the internet and sends nothing anywhere. It reads the two folders you picked and writes to them only when you copy or save.

What the browser keeps for the page:

- the settings (filters, labels, the size of the panes) in its local storage,
- the two picked folders, so that the last pair is compared again when you open the page later. If the browser no longer grants access (e.g. after a restart), **Reload** asks for it once more.

**Clear** (`Alt+C`) forgets both folders and the labels.

## Folder compare

| Mark | Meaning |
|---|---|
| red `≠` | different |
| violet | only on one side |
| blue `≈` | only line endings, BOM or (with *Ignore whitespace*) spaces differ |
| grey | identical |

- **All** / **Differences** shows all files or only those that differ. **Expand** / **Collapse** opens or closes all folders; after the first compare the folders with differences are open.
- **Find** (`/`) shows only the files whose name contains the text, while you type (not case-sensitive), with their folders opened.
- **Filter** (e.g. `*.al;*.xlf`) and **Exclude** (default `.git;.alpackages;.vs;node_modules`) decide which files are read; press Enter to apply. **Ignore whitespace** treats lines that differ only in spaces or tabs as equal.
- **Copy →** / **← Copy** copies the selected file to the other side, or for a folder all files below it that differ or are missing there. You are asked first; existing files are overwritten.
- The label fields name the two sides in the headers, e.g. "Release" and "Main".

## File compare

Click a file or press Enter to open it below the tree (the divider can be dragged).

- The strip on the far left shows all differing sections; click to jump.
- The arrows in the middle (or **Section →** / **← Section**, `L` / `H`) take a section over to the other side; the cursor then moves on to the next section.
- **All →** / **← All** (`Alt+l` / `Alt+h`) take over the whole file, including the line break at the end of the file. A section or marked lines that reach the end of both files take it along as well.
- Mark whole lines by dragging with the mouse, with **Shift+click** or with `v`. With lines marked, the section commands and the editor work on those lines only, so you can take over part of a section.
- Double-click a line or use **✎ Left** / **✎ Right** to edit it (inside a differing section: the whole section of that side). **Apply** takes the text, **Cancel** discards it, an empty text removes the lines.
- **Undo** reverts every change. **Save left** / **Save right** write to disk and turn green while that side has unsaved changes.
- If an open file changes on disk, **Reload** turns orange (checked every 3 seconds and when you come back to the window).
- **Find** (`/`) searches both files while you type and marks every place found.

Files in other encodings (e.g. Windows-1252, UTF-16) can be compared but not edited, because the browser can only write UTF-8. Binary files can only be copied as a whole.

## Keyboard shortcuts

Folder compare (right after loading and while no file is open the keys work anywhere except in text fields; with a file open, click into the tree first):

| Key | Action |
|---|---|
| `j` / `↓`, `k` / `↑` | next / previous row |
| `l` / `→` | open folder |
| `h` / `←` | close folder, otherwise go to the parent folder |
| `Enter` | open file, open or close folder |
| `r` | read both folders again (**Reload**) |
| `e` / `c` | open / close all folders (**Expand** / **Collapse**) |
| `a` / `d` | all files / differences only (**All** / **Differences**) |
| `/` | **Find** by file name; in the field `Enter` goes to the first file found, `Esc` clears the search |
| `Alt+l` / `Alt+h` | copy the selected file or folder to the right / to the left (like **Copy →** / **← Copy**, asks first; only while no file is open) |
| `Alt+C` | **Clear** (asks first; works anywhere on the page) |

File compare:

| Key | Action |
|---|---|
| `j` / `↓`, `k` / `↑` | cursor row down / up; 7 rows stay visible above and below it (like vim `scrolloff`) |
| `gg` / `G` | start / end of the file |
| `zz` | cursor row to the middle |
| `Ctrl+d` / `Ctrl+u` | half a page down / up, then the cursor row in the middle |
| `n` / `N` | next / previous differing section |
| `/` | **Find** in both files; in the field `Enter` / `Shift+Enter` next / previous place, `Esc` back to the lines with the cursor on it |
| `h` / `←`, `l` / `→` | scroll sideways |
| `v` | mark lines from the cursor row on, moving extends the marking; `v` again ends it |
| `L` / `H` | take the marked lines, otherwise the section under the cursor, over to the right / to the left |
| `Alt+l` / `Alt+h` | take the whole file over to the right / to the left (like **All →** / **← All**) |
| `i` / `I` | edit on the left / right (marked lines, otherwise the line or the whole section) |
| `y` / `Y` | copy the marked lines of the left / right side to the clipboard |
| `Ctrl+C` | copy the marked lines of the side where you marked them with the mouse |
| `u` | undo |
| `r` | read both files again |
| `Alt+S` | save both sides (also while editing) |
| `Esc` | remove the marking, otherwise close the file compare |

Editor: `Esc` or `Ctrl+Enter` takes the text and returns to the keys above (a second `Esc` closes the file compare), `Tab` inserts four spaces.

`Ctrl+S` and `Ctrl+Z` stay with the browser on purpose. Taking over sections never writes to disk, so a wrong key press is undone with `u`.

## How it is built

Everything lives in `lazymerge.html`: the styles, a script with the pure logic (line diff, encodings, syntax colours, folder compare) and a script for the page. There are no dependencies and no build step; the page works offline.

Colours: the Wave palette of [Kanagawa](https://github.com/rebelot/kanagawa.nvim).

## License

[MIT](LICENSE)
