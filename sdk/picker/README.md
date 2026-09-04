# herdr-picker-sdk

Shared support for the repository's Rust picker examples:

- the item, snapshot, and error wire types consumed by `herdr-picker`;
- strict item validation;
- live multi-session Herdr Hub model publication;
- hub-routed selected-value submission helpers;
- shared Herdr agent-status presentation.

It keeps the agent and workspace examples independently installable without
duplicating their protocol or process plumbing.

Hub outages emit an error message and clear stale picker rows. The provider
keeps reconnecting; a fresh snapshot clears the error once the Hub returns.
