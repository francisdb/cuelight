# cuelight

An embeddable multimedia engine: shows, layers and timelines driven by
triggers and variables. Written in Rust, rendering with
[vello](https://github.com/linebender/vello) and
[wgpu](https://github.com/gfx-rs/wgpu).

> Status: early design and proof-of-concept. The API surface and data model
> are being proven out; nothing here is stable yet.

## What it is

Think of it as an extended slide deck rather than a game engine:

- **Shows** are states: a tree of typed layers (groups, shapes, text, media)
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
load_show      load a declarative show description
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

renders the example show at `examples/shows/minigolf.json` to a series of PNG
frames in `target/frames/`.

```sh
cargo run --example render_to_window
```

renders the same show live into a window: the score variable pulses, the
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
cargo run -p cuelight-player -- path/to/show
```

is the player (`cargo install --path crates/cuelight-player` puts
`cuelight-player` on your path). It opens a show folder or a loose show
file (a built-in demo when no path is given), lists its actions (trigger
names) and variables on the console, and lets you drive it: type an action
number or name to fire it (digit keys in the window work too),
`name=value` to set a variable, `q` to quit. See `--help` for the options.

A show folder holds `show.json` plus an optional `test-driver.json` and
`assets/` with images and bitmap fonts; the bundled beacon show
demonstrates it:

```sh
cargo run -p cuelight-player -- crates/cuelight/examples/shows/beacon
```

A driver file is a scripted sequence of the same commands with delays,
standing in for a live host, optionally looping. The one next to the show
(`test-driver.json` in a folder, `<show>.test-driver.json` beside a loose
file) plays automatically; a second argument names another one, and
`--no-driver` plays none.

More shows live in the
[cuelight-examples](https://github.com/francisdb/cuelight-examples)
repository, one show folder each, playable directly with the player.

The windowed examples log startup info (render backend, windowing system,
window size and scale) and events; set `RUST_LOG=debug` for more detail.

```sh
cargo test
```

exercises the data model, bindings, triggers and timelines with no GPU
required.

## Crates

- [`cuelight`](crates/cuelight): the engine, plus the optional vello
  renderer. It does no I/O: hosts hand it a show document, images and fonts.
- [`cuelight-loader`](crates/cuelight-loader): that host work, shared:
  loading show folders from disk (or images and fonts from bytes, for hosts
  without a filesystem) and playing `test-driver.json` scripts. Image
  decoders are cargo features, for hosts that decode images themselves.
- [`cuelight-player`](crates/cuelight-player): a windowed player for show
  folders, driven from the console, the keyboard or a driver script.

## Show format

Shows are declarative JSON; see [docs/show-format.md](docs/show-format.md)
and the generated JSON Schema at
`crates/cuelight/schemas/show.schema.json` (kept in sync with the model
types by a CI check; show files can reference it via `$schema` for editor
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