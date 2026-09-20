# Key rendering and animations

How the Stream Deck key images are produced, where the animations come from,
what every constant means, and how to add or change an animation. Everything
here is in `src/render.rs` unless another file is named. Commands run from
this `herdr-deck/` directory.

## Pipeline

Keys are drawn from scratch as SVG text and rasterised on every frame; there
are no image assets. `Renderer::render_key` builds one SVG per key:

1. a flat background rect and a rounded panel in the state's background colour
   (`states.<state>.background` from the config);
2. a yellow focus border (`#ffd60a`, at least 4 px or a 24th of the width)
   when the agent is focused and the key is not offline;
3. the animation, emitted as `<line>` and `<circle>` elements in a
   grey-scale ink over the panel;
4. the heading (agent label, uppercased, at most 11 characters), the subtitle
   and state text when the key is not animated, the session badge, and the
   active-session dot.

The SVG goes through `resvg` with the system font loaded once in
`Renderer::new`, then `turbojpeg` encodes the pixmap as baseline JPEG, quality
88, 4:2:0 subsampling, progressive off. A test checks those JPEG markers
because the device rejects other encodings. Animated keys are cached in a
32 MiB cache keyed on size, state, colours, heading, border, active-session
flag, frame index, and session badge; when full it evicts in insertion order.
A full loop is rendered once per distinct key and then replayed from memory.

The touch strip (`render_touch_strip`) is a separate static SVG with the
session filter label and the working, done, and blocked counts; it is not
animated.

## Provenance

The animations are ports of the MIT-licensed
[thinking-orbs](https://github.com/Jakubantalik/thinking-orbs) canvas engine
by Jakub Antalik, demoed at <https://orbs.jakubantalik.com/>. The site is the
visual reference: each Herdr state got an orb picked from it.

| Herdr state | Orb on the site | Engine mode | Generator here |
| --- | --- | --- | --- |
| working | Connecting | `web` | `web()` |
| done | Composing, at half speed | `ribbon` | `ribbon(.., ring = false)` |
| idle | Breathing, at quarter speed | `ring` | `ribbon(.., ring = true)` |
| blocked, unknown, offline | none | | static text |

History, for context rather than verification: the engine was first an npm
dependency of a TypeScript daemon, then hand-ported to Rust in the rewrite.
Working started as the Solving orb (`rubik`) and was replaced by Connecting
because it can loop without reversing. Idle was deliberately kept moving,
slowly, rather than frozen. The port keeps the engine's math and its size-64
presets; attribution is in `THIRD_PARTY_NOTICES.md`.

## Two clocks

Animation time is wall-clock: `render_key` receives `now` in seconds since the
epoch and derives the frame from it, so every key and every reconnect stays in
phase. Two different rates are involved:

- **Frame-table fps** (`animation()` in `render.rs`): the rate at which time
  is quantised into frames, which fixes how many unique images a loop has.
  Working and done use 20, idle uses 5.
- **Delivery rate** (`daemon.rs`): how often the daemon repaints a key on the
  device. A 30 Hz kqueue timer (`ANIMATION_TIMER_FPS`, marked
  `NOTE_CRITICAL` so App Nap cannot coalesce it) drives repaints at
  `WORKING_UPDATE_FPS = 15`, `DONE_UPDATE_FPS = 10`, and every
  `IDLE_INTERVAL_MS = 200` for idle, staggered per button so idle keys do not
  all repaint on the same tick. `animation_tick` turns those into a counter
  that is part of each key's repaint signature; a state it does not know
  gets a constant tick and therefore never repaints.

The delivery rates were set by measuring on the device with eight animated
keys: idle went to 5 fps first, working and done were later lowered to 15 and
10, while their frame tables stayed at 20 so the cached loops did not change.

## Loop shapes

`animation(state)` returns a `Loop { frames, fps, shape }` and `render_key`
derives the frame index from the shape alone; only the generator `match` in
`render_key` still names individual states.

**`PingPong`** (done and idle). `frames` unique frames play forward then
backward, period `2 * frames - 2` (94 for `FRAMES = 48`), via
`animation_frame`. `animation_frame_time` maps the frame index to the
generator's time with a quadratic ease-in and ease-out over the first and last
10 % of the loop (`edge = 0.1`), so the reversal reads as a gentle swing. One
loop spans `(frames - 1) / fps` seconds of generator time before the call-site
speed multiplier: 2.35 s at 20 fps, 9.4 s at 5 fps. 48 was chosen as a cache
budget, not derived from the motion. This shape works for any generator, at
the price of visible reversal on directional motion.

**`Forward`** (working). `frames` play forward and wrap. The generator takes a
phase in `[0, 1)` and every rate inside it is an exact integer number of
cycles per loop, so the last frame flows into frame 0. `WEB_FRAMES = 316`.
`web_orb_loops_seamlessly` compares `web(size, 0.0)` with `web(size, 1.0)`:
dot and line counts, and each dot's x, y, radius, and alpha to 1e-6. It does
not compare line endpoints, so a rate that only affects links could slip by.

**`Static`**: one frame, `frame_time` is zero.

## Generators

A generator produces dots, and optionally lines, in key pixel coordinates
with `size` the key's smaller dimension:

```rust
struct Dot  { x, y, z, r, white, alpha }   // z is depth, for sorting only
struct Line { x1, y1, x2, y2, width, white, alpha }
```

`web(size, phase) -> (Vec<Dot>, Vec<Line>)` and
`ribbon(size, time, ring) -> Vec<Dot>` are the two generators; `render_key`
wraps a dot-only result in a tuple with an empty line list. `ink(white, alpha)`
turns a dot or line into its fill colour, and `white` is the amount of black
mixed in: 0 is `#ffffff`, 1 is `#000000`. Shared helpers: `projection` rotates
a point by yaw and pitch and projects it to the key, with the caller's scale;
`sphere_point(i, n)` is a golden-angle Fibonacci sphere; `hash` and `noise2`
are the engine's value noise; `finalize(dots, min_radius)` drops dots below
alpha 0.02, raises any radius below `min_radius` up to it (the preset's
`rMin`, 0.3 for both orbs), and depth-sorts.

`ribbon` draws bands of dots on a sphere. The lane count is
`round(base_lanes * bandMul)`: 12 for the Composing orb, 11 for Breathing.
Non-ring is Composing, with 38 faint ghost dots on the same sphere, mixed
into the bands by depth; ring is Breathing, face-on, with no ghosts and a
smaller wobble.

`web` draws 41 nodes drifting on a rotating sphere, links every pair whose
distance on the unit sphere (before projection) is under `THRESHOLD`, and
runs seven bright signals that hop between hash-chosen nodes every hop
period, whether or not a link joins them.

## Where the numbers come from

The engine ships base profiles plus per-size presets, and `resolvePreset(state,
64)` combines them with three rules: paired counts (`lanes`, `segs`) scale by
`sqrt(count)`, flat counts (`nodeN`, `ghostN`, `signals`) scale by `count`,
radii (`rBase`, `rDepth`, `nodeR`, `nodeRDepth`) scale by `size`. The Rust
code holds the resolved values as literals. To change a shape, change the
preset inputs and re-resolve rather than nudging a literal.

Engine inputs (base profile, then the size-64 preset), copied from the engine
source, which is not vendored here:

```text
ribbon: lanes 5, segs 88, ghostN 150, rBase 1.1, rDepth 1.7, rMin 0.3
        @64: speed 2.34, count 0.25, size 0.85,  bandMul 3.9,   wobMul 1
ring:   lanes 5, segs 88, ghostN 0,   rBase 1.1, rDepth 1.7, faceOn
        @64: speed 3.24, count 0.25, size 0.956, bandMul 3.627, wobMul 0.368
web:    nodeN 30, thr 0.72, signals 5, nodeR 1.4, nodeRDepth 1.8, lineW 0.8
        @64: speed 3.315, count 1.35, size 0.95
```

| Literal in `render.rs` | Origin |
| --- | --- |
| `ribbon(size, frame_time * 2.34 * 0.5, false)` | ribbon speed 2.34, times the chosen half speed for done |
| `ribbon(size, frame_time * 3.24 * 0.25, true)` | ring speed 3.24, times the chosen quarter speed for idle |
| `1.1 * 0.956`, `1.7 * 0.956` | rBase and rDepth scaled by ring size |
| `1.1 * 0.85`, `1.7 * 0.85` | the same scaled by ribbon size |
| `3.627`, `0.368` / `3.9`, `1.0` | ring / ribbon bandMul and wobMul |
| `base_lanes = 3`, `segs = 44` | 5 and 88 scaled by sqrt(0.25); lanes are then multiplied by bandMul |
| `ghosts = 38` / `0` | 150 scaled by 0.25; the ring has none |
| `spin = 0.0` | both presets ship spin 0, so the `time * 0.1 * spin` term is inert |
| `NODES = 41`, `SIGNALS = 7` | 30 and 5 scaled by 1.35 |
| `NODE_R = 1.33`, `NODE_R_DEPTH = 1.71` | 1.4 and 1.8 scaled by 0.95 |
| `THRESHOLD = 0.72` | thr, unscaled |
| `(size / 300.0).powf(0.6)` | the engine's radius scaling with key size |

The web loop length and its integer rates are derived from the engine's
rates, which run in orb time `s = t * 3.315`:

- yaw is `0.12 * s` per second, so one revolution takes `2π / (0.12 * 3.315)`
  = 15.8 s, which at 20 fps is `WEB_FRAMES = 316`;
- the node pulse `sin(1.4 * s)` makes 11.67 cycles per revolution, snapped to
  12 (`12.0 * angle`);
- the signal hop rate `0.55 * s` makes 28.8 hops per revolution, snapped to
  `SIGNAL_HOPS = 29`;
- the engine drifts nodes along a straight line through noise space, which
  never repeats; the port samples the same noise field along a closed circle
  whose circumference matches the drift speed, so the drift loops too.

The cost is that the working loop repeats every 15.8 s where the site never
repeats. Done and idle keep the 48-frame ping-pong because their motion is
drifty enough not to read as reversing.

## Previewing off the device

`herdr-deck frames <state> <dir> [config]` renders one full loop of a state's
key at Plus key size (120 × 120) as `<state>-NNN.jpg`, zero-padded, one file
per unique frame. Frame `n` is what the device shows at `n / fps` seconds;
the command prints the count and fps. Working writes 316 files, done and idle
48, blocked and unknown one. The palette comes from the config the service
uses (`HERDR_DECK_CONFIG`, else `~/.config/herdr-deck/config.json`); pass a
config path as the third argument to preview without an installed one, for
example the repository's `herdr-deck.json`.

```sh
cargo build --release --locked -p herdr-deck
../target/release/herdr-deck frames working /tmp/deck-frames herdr-deck.json
```

Check the seam (first and last file side by side), the turnaround for
ping-pong loops, and the heading's legibility over the orb before anything
goes near the device.

## Adding or changing an animation

Speed changes are the multipliers at the generator call sites in
`render_key`; colour changes are configuration (`states` in
`herdr-deck.json`), not code. For a new shape:

1. **Pick the shape and preset.** Choose an orb on the site, read its base
   profile and size-64 preset from the engine source, resolve them with the
   three rules above, and record the resolved numbers in the table in this
   file.
2. **Write the generator** next to `web` and `ribbon`, returning `Vec<Dot>` or
   `(Vec<Dot>, Vec<Line>)` in key coordinates, using the shared helpers, and
   ending with `finalize(dots, r_min)`. A ping-pong generator takes eased time
   in seconds; a forward-loop generator takes a phase in `[0, 1)` and must
   make every rate an integer number of cycles per loop, sampling any noise on
   closed circles.
3. **Declare the loop** in `animation()`: frames per loop, frame-table fps,
   and `PingPong` or `Forward`. Each state may have its own length; put a
   named constant next to `FRAMES` and `WEB_FRAMES`. For a forward loop add a
   seam test modelled on `web_orb_loops_seamlessly`.
4. **Select the generator** in the `match state` in `render_key`, using
   `frame_time` for ping-pong or `phase` for forward. If the generator takes
   any input that is not already in the cache key, add it to the key, or
   stale frames will be served.
5. **Only for a new state**, which means a new `AgentStatus` variant. In
   `src/config.rs`: the variant, `as_str()`, `AgentStatus::ALL`, and the
   `states` field allow-list in `parse_config`, since every config must now
   carry `background` and `foreground` for it. In `render.rs`: the `status()`
   mapping. In `daemon.rs`, three places, and the key never repaints if either
   of the first two is missed: the `Event::Animation` handler, which marks the
   frame dirty only when a visible agent is in an animated status; a branch in
   `animation_tick`, whose fallback is a constant; and the `animated` match in
   the key loop, which keeps the subtitle out of the repaint signature while
   animating.
6. **Verify**: `cargo test -p herdr-deck`, build, preview with `frames`, then
   `install-service`.
