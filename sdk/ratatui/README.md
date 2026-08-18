# herdr-ratatui

Shared Ratatui chrome for Herdr popup plugins:

- `SearchLine` builds a consistent search prompt and computes its cursor position
- `key_hints` styles compact keyboard help
- `Separator` and `theme` keep popup colors consistent

It intentionally does not own application state, rows, search, or event loops.
