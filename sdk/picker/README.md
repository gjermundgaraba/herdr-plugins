# herdr-picker-sdk

Shared support for the repository's Rust picker examples:

- the item and snapshot wire types consumed by `herdr-picker`;
- strict item validation;
- live multi-session Herdr Hub model publication;
- hub-routed selected-value submission helpers;
- shared Herdr agent-status presentation.

It keeps the agent and workspace examples independently installable without
duplicating their protocol or process plumbing.
