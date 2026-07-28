# herdr client for Go

Small client for Herdr's public plugin socket API. It provides:

- typed session, workspace, tab, pane, agent, layout, and plugin-context models
- generic calls for every socket method
- typed helpers for common read operations
- long-lived event subscriptions
- Unix socket and Windows named-pipe transport

```go
client, err := herdr.NewFromEnv()
if err != nil {
    return err
}
snapshot, err := client.Snapshot(context.Background())
```

Methods added after this module's tested Herdr version remain usable through
`Client.Call`.

Validated against Herdr 0.7.5, socket protocol 17.
