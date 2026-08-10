# herdr-ratatui

Shared Ratatui chrome for Herdr popup plugins:

- `SearchLine` renders a consistent search prompt and cursor position
- `key_hints` styles compact keyboard help
- `Separator` and `theme` keep popup colors consistent

It intentionally does not own application state, rows, search, or event loops.
