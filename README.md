# cuelight

An embeddable multimedia engine: scenes, layers and timelines driven by
triggers and variables. Written in Rust, rendering with
[vello](https://github.com/linebender/vello) and
[wgpu](https://github.com/gfx-rs/wgpu).

> Status: early design and proof-of-concept. The API surface and data model
> are being proven out; nothing here is stable yet.

## What it is

Think of it as an extended slide deck rather than a game engine:

- **Scenes** are states: a tree of typed layers (groups, shapes, text, media)
  composited by priority.
- **Timelines** animate layer properties with keyframes and easing, and can be
  started on load or fired by triggers.
- **Variables and triggers** are the only inputs: hosts push named values
  (a score, a sensor reading, a lamp state) and fire named events; content
  binds properties to variables and reacts to triggers. Everything is data.
- **Embeddable first**: the engine renders into an offscreen texture the host
  composites, ticked by the host through `advance_frame`. No window, no event
  loop, no network protocol in the core.

The core API is deliberately tiny:

```text
load_scene      load a declarative scene description
set_variable    push a named value from the host
trigger         fire a named event
advance_frame   advance time by dt
get_texture     obtain the rendered output
```

Everything else (hosts, wire protocols, hardware bridges, import from
existing content formats) is an adapter on top of that contract, never part
of the core.

## Try it

```sh
cargo run --example render_to_images
```

renders the example scene at `examples/scenes/minigolf.json` to a series of PNG
frames in `target/frames/`.

```sh
cargo run --example render_to_window
```

renders the same scene live into a window: the score variable pulses, the
`go` trigger re-fires every two seconds (or press space), escape quits.

```sh
cargo run --example slideshow
```

opens a window cycling three procedurally generated in-memory images (a
rainbow, TV noise, a sine wave) with a crossfade every two seconds, looping
via autoplay timelines with no external events.

```sh
cargo run --example mic_pop
```

listens to the default microphone: a circle continuously resizes with the
audio level (a `scale` binding fed by `set_variable` every frame) and a
ring pops outward on noise spikes/beats (a timeline fired by `trigger`).
Space pops manually, useful without a microphone.

```sh
cargo run --example player -- path/to/scene.json
```

opens any scene file (the bundled minigolf scene when no path is given),
lists its actions (trigger names) and variables on the console, and lets
you drive it: type an action number or name to fire it (digit keys in the
window work too), `name=value` to set a variable, `q` to quit. Image
layers whose pixels the host never registered are logged as warnings and
skipped.

An optional second argument plays a driver file: a scripted sequence of
the same commands with delays, standing in for a live host, optionally
looping, conventionally `<scene>.driver.json` next to its scene:

```sh
cargo run --example player -- \
  crates/cuelight/examples/scenes/minigolf.json \
  crates/cuelight/examples/scenes/minigolf.driver.json
```

The windowed examples log startup info (render backend, windowing system,
window size and scale) and events; set `RUST_LOG=debug` for more detail.

```sh
cargo test
```

exercises the data model, bindings, triggers and timelines with no GPU
required.

## Scene format

Scenes are declarative JSON; see [docs/scene-format.md](docs/scene-format.md)
and the generated JSON Schema at
`crates/cuelight/schemas/scene.schema.json` (kept in sync with the model
types by a CI check; scene files can reference it via `$schema` for editor
autocomplete and validation).

## License

Licensed under either of

 * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.