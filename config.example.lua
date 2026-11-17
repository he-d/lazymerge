-- Save as $XDG_CONFIG_HOME/lazymerge/config.lua, or ~/.config/lazymerge/config.lua.
-- Alternatively: lazymerge --config /path/to/config.lua left right
-- Return a Lua table. Omitted settings and nested colors/keys keep their defaults.
-- This is executable Lua: expressions and local variables work here.
return {
  show_logo = true,           -- centered ASCII logo; a compact header stays in small terminals
  differences_only = false,  -- hide identical and minor (EOL/BOM/whitespace-only) changes
  ignore_whitespace = false,
  include = "",              -- e.g. "*.rs;*.json"; blank means all files
  exclude = ".git;.vs;node_modules;target",
  left_label = "Left",
  right_label = "Right",
  tree_percent = 35,          -- folder-tree height when a file is open, 10..80
  scrolloff = 5,             -- cursor context rows, 0..100
  tab_width = 4,             -- 1..16 spaces
  poll_seconds = 3,          -- refresh overview/clean files; staged edits are kept, 1..3600

  -- Kanagawa Wave, matching the HTML app and SVG logo. Use #RRGGBB colors.
  colors = {
    bg = "#1F1F28",
    fg = "#DCD7BA",
    muted = "#727169",
    border = "#54546D",
    selected = "#223249",
    different = "#E46876",
    one_sided = "#957FB8",
    minor = "#7FB4CA",
    logo_lazy = "#957FB8",
    logo_merge = "#98BB6C",
    added = "#2B3328",
    removed = "#43242B",
    changed = "#49443C",
    accent = "#E6C384",
    warning = "#FFA066",       -- Kanagawa surimiOrange: file-change notice border
  },

  -- Map an action to one key: characters preserve case; modifiers use +.
  -- Examples: "ctrl+q", "alt+l", "shift+tab", "f2". "" disables a binding.
  -- Duplicate bindings are rejected. Disable/rebind the old owner before reusing a key.
  -- Quit always asks for confirmation. Text fields retain normal typing behavior.
  -- Alt+s always saves both sides of an open file, including in the editor.
  keys = {
    quit = "q", help = "?", down = "j", up = "k",
    expand = "l", collapse = "h", open = "enter", reload = "r",
    expand_all = "e", collapse_all = "c", all = "a", differences = "d",
    find = "/", copy_right = "alt+l", copy_left = "alt+h",
    merge_right = "L", merge_left = "H", edit_left = "i", edit_right = "I",
    undo = "u", save_left = "ctrl+s", save_right = "ctrl+alt+s", save_both = "alt+s",
    accept_edit = "alt+enter", -- Ctrl+Enter also applies if the terminal delivers it
    next = "n", previous = "N", select = "v", top = "g", bottom = "G",
    paths = "p", include = "f", exclude = "F", whitespace = "w", focus = "tab",
  },
}
