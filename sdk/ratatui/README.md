# herdr-ratatui

Shared Ratatui chrome for Herdr popup plugins:

- `SearchLine` renders a width-aware search prompt and places its cursor
- `key_hints` styles compact keyboard help
- `theme` keeps popup colors consistent

It intentionally does not own application state, rows, search, or event loops.
