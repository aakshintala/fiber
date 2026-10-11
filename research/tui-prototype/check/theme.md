# Themes

One run per case: `--theme CASE`.

- light: run `--static --theme light`: the conversation, panel, approval and question form on light tints with dark text; rules and comments dim but legible, the alert red.
- dark-256: run `--static --theme dark-256`: the same screen through the xterm palette; neutral tints grey, hues kept, the alert still red.
- light-256: run `--static --theme light-256`: the light screen through the xterm palette; dark text on light tints, the alert still red.
- dark-no-color: run `--static --theme dark-no-color`, or with NO_COLOR=1: no colour anywhere, only bold, dim, reversed and underline; edges blank but rows kept.
