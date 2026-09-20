# Third-party notices

The device protocol was independently verified against the MIT-licensed
[FreeMicro](https://github.com/eliBenven/freemicro) and
[house-of-herdr](https://github.com/alasano/house-of-herdr) projects. Their
source is not bundled here.

[Hunk](https://www.hunk.dev/) is an optional external diff viewer and is not
bundled.

The Rust binaries directly depend on `anyhow`, `base64`, `libc`, `serde`,
`serde_json`, `signal-hook`, `tempfile`, and the `objc2` crate family. Their
licenses are declared in the crates' published metadata or source;
`Cargo.lock` pins the resolved versions. macOS frameworks used through `objc2`
(AppKit, CoreFoundation, CoreGraphics, Foundation, and IOKit) are provided by
Apple and are not bundled.

`herdr-frontend` is consumed as a git dependency from the Apache-2.0
[gjermundgaraba/herdr](https://github.com/gjermundgaraba/herdr) fork;
`Cargo.lock` pins the exact commit.

Work Louder, Codex, OpenAI, Claude, and Pi are trademarks of their respective
owners. This is an unofficial community project.
