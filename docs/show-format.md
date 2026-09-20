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

Naming note: a **show** is the whole loaded document; **scenes** are the
switchable views inside it (see [Scenes](#scenes)).

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
      fonts/            bitmap fonts: .fnt plus its page PNGs, registered
                        by .fnt filename stem
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
- `layers` are always present; `scenes` (optional) are switchable views
  painted on top of them.

## Scenes

```json
{
  "scenes": [
    { "name": "attract", "trigger": "attract", "layers": [] },
    { "name": "game", "trigger": "start", "layers": [] }
  ]
}
```

Exactly one scene is active at a time: the first one when the show loads,
afterwards the one whose `trigger` fired last. Only the show's own
`layers` and the active scene's layers render, and only their timelines
respond to triggers.

Entering a scene restarts it: the previous scene's running timelines stop,
the entered scene's `autoplay` timelines start at 0. Firing the trigger of
the scene that is already active restarts it the same way. The show's own
layers are not affected by scene changes. A trigger that enters a scene
also starts the timelines that declare the same trigger, in the show's
layers and in the newly entered scene.

## Output

```json
{ "output": { "mode": "gray4", "tint": "#FF5820" } }
```

`output` says how the finished frame's colors reach the display:

- `rgb` (default): full color, unchanged.
- `gray4` / `gray2`: each pixel's brightness (luma, see below) is
  quantized to 16 / 4 levels and multiplied by `tint` (`#RRGGBB`, white by
  default). This is how a monochrome DMD shows content: author in any
  color, it lands as shades of the display's color.

Luma is how bright a color looks, as one number. The eye is far more
sensitive to green than to red, and least to blue, so a plain average of
r, g and b would make pure blue look as bright as pure green. The weights
come from Rec. 709, the HDTV standard that also defines the sRGB primaries:

```text
luma = 0.2126 r + 0.7152 g + 0.0722 b
```

They are applied directly to the 8-bit channel values (no linearization),
so white is full brightness, pure green lands at about 72 %, pure red at
21 % and pure blue at 7 %: in `gray4`, levels 11, 3 and 1 of 15.

A scene may declare its own `output`, used while it is active; otherwise
the show's applies. The conversion happens on the whole frame after
compositing, so antialiased edges and opacity quantize like everything
else. Hosts read the active mode with `Engine::output()`; the offscreen
renderer applies it after readback, and `render::OutputPass` does the same
on the GPU for hosts rendering on their own device.

## Layers

`layers` is a tree painted in order: earlier layers are behind later ones,
group children behind whatever follows the group. Every layer has:

- `name`: identifier, also surfaced in the resolved draw list.
- `type`: `group`, `shape`, `image` or `text` (see below).
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
- `text`: `text` (use `\n` for line breaks) drawn in `font`, a style
  declared in the show's `fonts` (see [Fonts](#fonts)). Optional `size`
  `[width, height]` is a box whose top-left corner sits at the layer's
  x/y; `align` places the text in it (`top_left`, `top`, `top_right`,
  `left`, `center` (default), `right`, `bottom_left`, `bottom`,
  `bottom_right`). Without `size` the box is exactly the text's size.
  Multi-line text aligns each line on its own within the box width.
  The `text` property can be bound (see [Bindings](#bindings)) but not
  keyframed.

## Fonts

```json
{
  "fonts": {
    "score": { "file": "teeny_tiny_pixls-5", "color": "#808080" },
    "title": { "file": "bm_army-12", "border": { "color": "#102C80", "width": 1 } }
  }
}
```

Text uses bitmap fonts in the [BMFont](https://www.angelcode.com/products/bmfont/doc/file_format.html)
text format, a common format for pixel fonts. Fonts are host assets
like images: the host parses the `.fnt` description with
`BitmapFont::parse` and registers it with its page images through
`Engine::set_font`, by convention under the `.fnt` file stem. A text
layer whose font is not registered is skipped.

A style in `fonts` names the registered font (`file`), a `color` that
multiplies the glyph colors (default white, keeping the font's own), and
an optional `border`: an outline of `width` pixels in `color` around every
glyph, which also widens each character's advance by two widths.

Text is rasterized on the CPU pixel for pixel from the font pages, never
resampled, so pixel fonts stay exact on DMD-sized canvases. Layout: a
line is as wide as its characters' advances plus kerning; a
block is as tall as its line heights, except that the last line grows to
fit its tallest glyph. Characters missing from the font fall back to their
uppercase form, then to a space.

## Bindings

A binding wires a layer property to a variable, evaluated every frame:

```json
{ "property": "opacity", "variable": "score", "scale": 0.001, "offset": 0.2 }
```

means `opacity = score * 0.001 + 0.2`. Animatable/bindable properties:
`x`, `y`, `opacity`, `scale`.

A text layer's `text` property can be bound too: the variable's text as
is, a boolean as `true`/`false`, a number after `scale`/`offset` formatted
per `format`: `plain` (default: `1500`, `2.5`) or `thousands` (rounded, with
comma separators: `1,500`).

```json
{ "property": "text", "variable": "score", "format": "thousands" }
```

A text layer's `font` property can be bound to a variable naming a font
style, typically through a map.

`map` looks the variable's value up (as text: `1`, `2.5`, `true`,
`attract`) and uses the mapped value in its place; `default` covers values
the map does not list, and without a default an unlisted value leaves the
property at its base. The mapped value then goes through the same
conversion as a plain variable (`scale`/`offset`, `format`). Highlighting
the active player's score:

```json
{ "property": "font", "variable": "player", "map": { "2": "score_active" }, "default": "score_inactive" }
```

Font bindings may only map to declared font styles; `text` and `font`
can be bound but not keyframed. Binding or keyframing a property the
layer's kind does not have (`font` on a shape) is rejected at load.

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
