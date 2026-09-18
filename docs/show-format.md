# Show format

A cuelight show is a single JSON document describing everything the engine
can play: a layer tree, the variables the host may drive, and keyframed
timelines fired by triggers. The normative, machine-readable definition is
the generated JSON Schema at
[`crates/cuelight/schemas/show.schema.json`](../crates/cuelight/schemas/show.schema.json);
this page explains the concepts the schema cannot.

Point your editor at the schema for autocomplete and validation:

```json
{ "$schema": "../../schemas/show.schema.json", "name": "my_show", ... }
```

Unknown fields (like `$schema` itself) are ignored by the engine.

Naming note: the word **scene** is deliberately reserved. A show is the
whole loaded document; when switchable views / overlay queues land in the
model (FlexDMD-style), those inner units will be called scenes.

## On disk

A show exists in two forms:

- A **loose file**: one `.json` document, self-sufficient as long as the
  show needs no images. A driver script for it is conventionally named
  `<show>.test-driver.json` next to it.
- A **show folder**: the self-contained unit for anything with assets.
  The folder is the show; its contents follow fixed names:

  ```text
  myshow/
    show.json           the show document (required)
    test-driver.json    optional driver script, hosts may pick it up
    assets/             images, registered by filename stem (PNG)
  ```

Either way the engine only ever receives the single JSON document through
`load_show` plus `set_image` calls; it does no file I/O itself. Resolving
a folder (reading the manifest, decoding and registering `assets/`,
picking up the driver) is host-side convention, implemented today by the
`player` example. A zipped folder is the natural future single-file
distribution form.

## Top level

```json
{
  "name": "minigolf",
  "size": [512, 128],
  "background": "#101018",
  "variables": { "score": 0 },
  "layers": []
}
```

- `size` is the logical canvas in pixels. All coordinates in the show are
  authored against this space; hosts scale the rendered output to whatever
  surface they have (the bundled examples letterbox it into the window).
- The origin is the **top-left** corner: x grows right, y grows down.
  Shapes place their geometry relative to the layer's x/y (a circle at
  `[0, 0, r]` is centered on the layer origin), while an image's
  **top-left corner** sits at the layer's x/y - there is no anchor
  property yet, so scaling an image grows it toward the bottom-right and
  keeping it centered means counter-animating x/y (see the beacon show).
- `background` and every `fill` are `#RRGGBB` or `#RRGGBBAA`.
- `variables` declares the host-drivable inputs and their initial values
  (numbers, booleans, or text; bindings read them as numbers, booleans as
  0/1).

## Layers

`layers` is a tree painted in order: earlier layers are behind later ones,
group children behind whatever follows the group. Every layer has:

- `name`: identifier, also surfaced in the resolved draw list.
- `type`: `group`, `shape`, or `image` (see below).
- `x`, `y` (default 0): translation. Groups pass it down to their subtree.
- `opacity` (default 1): multiplied down the tree.
- `scale` (default 1): uniform scale of this layer's own geometry around
  its x/y origin. Not yet inherited by group children.
- `visible` (default true): invisible layers (and their subtrees) resolve
  to nothing.
- `bindings`, `timelines`: see below.

Layer kinds:

- `group`: `children` is a nested layer list.
- `shape`: `shape` is `{ "rect": [x, y, width, height] }` or
  `{ "circle": [cx, cy, radius] }` in the layer's local space, plus a
  `fill` color.
- `image`: `image` names pixels the host registers at runtime with
  `Engine::set_image` (RGBA8, kept in memory). Optional `size`
  `[width, height]` scales the image into the canvas; omitted, it draws at
  its natural pixel size. Images are host assets, not show content: a
  layer whose image is not (yet) registered is skipped, so hosts can
  stream assets in after `load_show`.

## Bindings

A binding wires a layer property to a variable, evaluated every frame:

```json
{ "property": "opacity", "variable": "score", "scale": 0.001, "offset": 0.2 }
```

means `opacity = score * 0.001 + 0.2`. Animatable/bindable properties:
`x`, `y`, `opacity`, `scale`.

## Timelines

A timeline is a keyframed animation owned by its layer:

```json
{
  "name": "slide",
  "trigger": "go",
  "autoplay": false,
  "loop": false,
  "tracks": [
    {
      "property": "x",
      "keys": [
        { "t": 0.0, "v": 64 },
        { "t": 1.0, "v": 440, "ease": "cubic_in_out" }
      ]
    }
  ]
}
```

- It starts when the host fires its `trigger`, or at load when
  `autoplay` is true. Re-firing the trigger restarts it from 0.
- Keys are `(t seconds, value)`; between two keys the value interpolates
  using the **later** key's `ease` (before the first key it holds the
  first value, after the last it holds the last). Easings: `linear`
  (default), `quad_in`, `quad_out`, `quad_in_out`, `cubic_in`,
  `cubic_out`, `cubic_in_out`, `step`.
- The timeline's duration is its longest track's last key. When it ends
  it stops and its properties fall back (see precedence); with `loop`
  the playhead wraps instead. For a seamless loop, author each track's
  value at the end equal to its value at 0.

## Property precedence

Each frame a property resolves to, strongest first:

1. a running timeline that animates it
2. a binding
3. the base value on the layer

So a timeline temporarily owns whatever it animates; when it finishes the
property falls back to its binding or base value instantly. To avoid a
visible jump, make base values match the timeline's endpoints.

## Host contract

The engine is driven exclusively through four calls: `load_show` (JSON in),
`set_variable`, `trigger`, and `advance_frame(dt)`. Output is either
`resolved_layers()` (a flat, GPU-free draw list) or the `render` feature's
vello rasterizer. Everything else, including where variable values and
trigger events come from (game state, audio, MIDI, a console), is the
host's business — see the `player` and `mic_pop` examples.

## Regenerating the schema

The schema is generated from the Rust model types (`schemars`) and checked
by CI, so it cannot drift. After changing the model:

```sh
UPDATE_SCHEMA=1 cargo test --features schema --test schema
```
