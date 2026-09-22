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

## Format version

```json
{ "format": 1, "name": "my_show", ... }
```

`format` is the version of the show format the document was written for;
a show without it is format 1. It goes up whenever a change could make an
older engine misread a show, and an engine refuses a show of a newer
format (`Error::UnsupportedFormat`) rather than play it wrongly. The
version this engine reads is the `FORMAT` constant.

Within a format, fields the engine does not know are ignored, so additions
stay compatible, but they are not silent: `Engine::load_warnings()` lists
their paths (`layers[2].colour`), which is how a typo shows up. Keys
starting with `$`, like `$schema`, are never reported. The player logs the
warnings at load.

Naming note: a **show** is the whole loaded document; **scenes** are the
switchable views inside it (see [Scenes](#scenes)).

## On disk

A show exists in three forms:

- A **loose file**: one `.json` document, self-sufficient as long as the
  show needs no images. A driver script for it is conventionally named
  `<show>.test-driver.json` next to it.
- A **show folder**: the self-contained unit for anything with assets.
  The folder is the show; its contents follow fixed names:

  ```text
  myshow/
    show.json           the show document (required)
    test-driver.json    optional driver script, hosts may pick it up
    assets/             images (PNG) and vector artwork (SVG), registered
                        by filename stem
      fonts/            fonts, registered by filename stem: bitmap (.fnt
                        plus its page PNGs) or outline (.ttf, .otf)
      sounds/           sounds, registered by filename stem (.wav, .flac,
                        .ogg, .mp3)
  ```

- A **packed show**: the folder as one file, `myshow.cuelight`, a plain
  zip with the folder's contents at its root (`show.json`, the driver,
  `assets/...`). The `cuelight-pack` tool of `cuelight-loader` writes it;
  any zip tool opens it, and a zip that wraps the folder in a directory is
  accepted too. Media that is already compressed (PNG, Ogg, MP3, FLAC) is
  stored as is, the rest deflated. The player and the loader open it like
  the folder.

Any of these forms is loaded the same way: the engine only ever receives the single JSON document through
`load_show` plus `set_image`, `set_vector`, `set_font` and `set_sound`
calls; it does no file I/O itself. Resolving a folder (reading the manifest, decoding and
registering `assets/`, picking up the driver) is host-side convention,
implemented by the `cuelight-loader` crate (sounds are decoded by
`cuelight-audio`). A zipped folder is the natural future single-file
distribution form.

## Top level

```json
{
  "format": 1,
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
  **top-left corner** sits at the layer's x/y, unless the layer sets an
  `anchor` (see [Layers](#layers)).
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

`scaling` tells hosts how to bring the frame up to their surface:
`smooth` (default: any factor, filtered) or `pixel_perfect` (whole-number
factors with nearest-neighbor sampling, so every canvas pixel becomes a
crisp square block, which is what DMD-resolution content wants).
`render::Presenter` honors it; hosts doing their own presentation read it
with `Engine::scaling()` and place the frame with `render::fit`.

`passes` lists effects applied to the finished frame as it is shown, in
order. One exists so far, `dots`, the dot matrix look: every canvas pixel
becomes a separate dot on black.

```json
"output": { "mode": "gray4", "tint": "#FF5820",
            "passes": [ { "dots": { "size": 0.8, "unlit": "#1A0904", "glow": 0.3 } } ] }
```

- `size` (default 0.8): dot diameter as a share of the pixel pitch, above
  0 up to 1.
- `shape`: `round` (default) or `square` (an LED matrix).
- `unlit`: color of a dot that is off, as on a real panel; dots never get
  darker than this. None by default. It is not run through the output
  `mode`, so give it in the tint's hue.
- `glow` (default 0): how much lit dots bleed into the dark around them,
  0 to 1.

Passes work at the surface's resolution, so `render::Presenter` applies
them (the player, the web player) and the offscreen renderer, whose frames
are canvas size, does not. A dot needs room: below three surface pixels
per canvas pixel the frame is shown plain. Hosts doing their own
presentation read the list with `Engine::passes()`.

A scene may declare its own `output`; while it is active, the fields it
sets override the show's and the rest still come from the show (a scene's
`passes` replace the show's list; an empty list turns them off). The conversion happens on the whole frame after
compositing, so antialiased edges and opacity quantize like everything
else. Hosts read the active mode with `Engine::output()`; the offscreen
renderer applies it after readback. Hosts rendering on their own device
use `render::Presenter`, which fits the show into their surface, applies
the output mode on the GPU and honors `scaling` (`render::OutputPass` is
the conversion alone, for hosts that want to do the rest themselves).

## Layers

`layers` is a tree painted in order: earlier layers are behind later ones,
group children behind whatever follows the group. Every layer has:

- `name`: identifier, also surfaced in the resolved draw list.
- `type`: `group`, `shape`, `vector`, `image`, `text`, `digits` or
  `audio` (see below).
- `x`, `y` (default 0): translation. Groups pass it down to their subtree.
- `opacity` (default 1): multiplied down the tree.
- `scale` (default 1): uniform scale of the layer around its x/y origin
  (or its anchor point). `scale_x`, `scale_y` (default 1) scale along one
  axis on top of it, for stretching and flips (negative mirrors).
- `rotation` (default 0): degrees, clockwise on the canvas, around the
  layer's x/y origin or its anchor point (so an image anchored at
  `center` spins in place). A needle is a shape with `rotation` bound to a
  value; a reel window is a group turned by a timeline.

  A group's translation, scale and rotation apply to its whole subtree,
  in that order (children turn and scale with the group). In the draw list
  anything only translated and uniformly scaled arrives in canvas
  coordinates; a rotation or an uneven scale anywhere above leaves the
  shape in its own space and places it with the item's `transform`, an
  affine hosts drawing the list themselves apply (see
  [Host contract](#host-contract)).
- `anchor` (optional): which point of the layer's content box sits at its
  x/y: `top_left`, `top`, `top_right`, `left`, `center`, `right`,
  `bottom_left`, `bottom`, `bottom_right`. The content box is an image's
  drawn rectangle, a shape's bounding box or a text layer's box (its
  `size`, or the measured text) or a digit row's `size`, after `scale`, so an image
  anchored at `center` stays centered while it scales (see the beacon
  show). Without an anchor, images put their top-left corner and shapes
  their local origin at x/y. Groups cannot be anchored.
- `visible` (default true): invisible layers (and their subtrees) resolve
  to nothing and are not heard. Bindable (on when the binding's number is
  not 0), not keyframed.
- `blend` (default `normal`): how the layer combines with what is painted
  beneath it. `add` sums the colors (light that adds to the art: a lamp
  behind a backglass, overlapping glows; white saturates), `screen` adds
  softly and never saturates, `multiply` darkens and tints (a coloured
  gel over white art). A group blends its children as one picture, so two
  overlapping glows inside an `add` group add once, not twice. Opacity
  applies on top.
- `bindings`, `timelines`: see below.

Layer kinds:

- `group`: `children` is a nested layer list. Optional `clip` is a shape
  (a `rect`, `circle` or `path` as a shape layer writes it, in the group's
  local space) outside which the children do not show: a window onto its
  content. `gain` (default 1)
  scales the loudness of the audio layers in the subtree, see
  [Sound](#sound).
- `shape`: `shape` is `{ "rect": [x, y, width, height] }`,
  `{ "circle": [cx, cy, radius] }` or `{ "path": "M 0 0 L 10 0 L 5 8 Z" }`
  in the layer's local space, plus a `fill` color (`#00000000` for an
  outline only). `path` takes SVG path data: `M L H V C S Q T A Z`,
  absolute or relative; arcs become curves. Optional `stroke`
  `{ "color": "#RRGGBB", "width": 1 }` outlines the shape, centered on its
  edge, in the layer's units (so it scales with the layer).
- `vector`: `vector` names artwork the host registered with
  `Engine::set_vector`; the loader does that for every `assets/*.svg`,
  by stem. Drawn like an image: its top-left corner at the layer's x/y
  (or by `anchor`), at its own size (the viewBox) or scaled into `size`
  `[width, height]`. What an SVG keeps: paths, basic shapes and text (as
  outlines, through the system's fonts), with solid fills and strokes,
  group transforms and opacities. A gradient paints as its first stop's
  color; patterns, raster images, clip paths, masks, filters, dashes and
  line joins are dropped. Animation is not read: address moving parts as
  separate vector layers and animate those.
- `image`: `image` names pixels the host registers at runtime with
  `Engine::set_image` (RGBA8, kept in memory). Optional `size`
  `[width, height]` scales the image into the canvas; omitted, it draws at
  its natural pixel size. Images are host assets, not show content: a
  layer whose image is not (yet) registered is skipped, so hosts can
  stream assets in after `load_show`. Optional `tint` (`#RRGGBB` or
  `#RRGGBBAA`) multiplies the image's colors, leaving its transparency
  alone: white changes nothing, a color stains the art (a coloured bulb
  behind white artwork, one sprite reused in several colors, a worn look
  over a clean texture).
- `text`: `text` (use `\n` for line breaks) drawn in `font`, a style
  declared in the show's `fonts` (see [Fonts](#fonts)). Optional `size`
  `[width, height]` is a box whose top-left corner sits at the layer's
  x/y; `align` places the text in it (`top_left`, `top`, `top_right`,
  `left`, `center` (default), `right`, `bottom_left`, `bottom`,
  `bottom_right`). Without `size` the box is exactly the text's size.
  Multi-line text aligns each line on its own within the box width.
  The `text` property can be bound (see [Bindings](#bindings)) but not
  keyframed.
- `digits`: a row of `digits` equal cells across `size` `[width, height]`
  (top-left at the layer's x/y) showing `text`, one character per cell.
  `justify` is `left` (default) or `right`, as scores are shown; text that
  does not fit is cut at the other side. `text` can be bound like a text
  layer's. How a cell is drawn is up to `display`:

  ```json
  { "type": "digits", "digits": 16, "size": [128, 16], "text": "HELLO",
    "display": { "segments": { "style": "alpha14", "fill": "#FF5820", "unlit": "#2A0E05" } } }
  ```

  `segments` is a segment display, as on pre-DMD pinball machines: lit
  segments in `fill`, the dark ones in `unlit` when given. `style` is
  `alpha14` (14 segments plus dot: letters, digits, `- + * / \ = _ '`) or
  `numeric7` (7 segments plus dot: digits and `-`). A `.` or `,` lights
  the dot of the cell before it instead of taking a cell, so `1,250`
  needs four cells. Characters the style cannot show stay dark. In shows
  that are rendered on their own pixel grid (a gray `output.mode`, or
  `pixel_perfect` scaling) the straight bars are a whole number of pixels
  thick, lie on pixel boundaries and end flat, so small displays stay crisp.

  `reel` is a row of wheels: every cell carries a ring of symbols and
  rolls to the one the text asks of it, as an odometer, a counter or a
  departure board does. A symbol is named by a character of the ring's
  `charset` and drawn either as that character in a font style of the
  show, which needs no artwork and stays sharp at any size, or as the
  artwork `cells` gives it.

  ```json
  { "type": "digits", "digits": 6, "size": [180, 48], "justify": "right",
    "display": { "reel": { "font": "score", "charset": "0123456789",
                           "duration": 0.12, "ease": "quad_in",
                           "direction": "forward", "stagger": 0.03,
                           "offset": [ { "t": 0.12, "v": 0 }, { "t": 0.17, "v": 0.12 },
                                       { "t": 0.22, "v": 0 } ] } } }
  ```

  - `charset` (default `0123456789`): the characters naming the ring's
    symbols, in the order they pass by. A cell shows nothing for anything
    the text asks of it that the ring does not carry, so a ring can hold
    letters, punctuation or a blank as well as digits.
  - `font`: a style from the show's `fonts`, bitmap or outline, that a
    symbol is drawn in as its own character. Characters are centred on
    their ink rather than on the line they are laid out in, so a ring of
    digits sits in the middle of its cell instead of high, and the whole
    ring is measured at once so the row keeps still while it rolls: a ring
    holding a descender reserves room for it, one of digits does not.
  - `cells`: artwork for the symbols instead, one entry per character of
    the charset: `{ "vectors": ["cherry", "bar", "seven"] }` or
    `{ "images": [...] }`, named as the host registered them. The charset
    stays the ring's identity, so the text still says which symbol a cell
    lands on; this only says what that symbol looks like. Artwork is
    fitted into the cell and centred, keeping its shape, and vector
    artwork stays sharp at any size. One of `font` or `cells` is needed.
  - `duration`, `ease`, `direction` and `offset` say how a cell moves, and
    mean what they do on a binding's [transition](#transitions):
    `forward` is a wheel that only turns one way, an `offset` settles a
    cell against its stop.
  - `step` (default 1): how far a cell travels in one move, in symbols.
    One means it lands on every symbol on the way, each taking `duration`,
    which is how a counter or a split-flap board reads. `null` makes the
    whole journey a single move instead, so `duration` covers all of it
    and `ease` shapes the journey rather than each symbol: that is how a
    wheel spins, and how a meter whose lowest digit never stops rolling
    behaves.
  - `turns` (default 0): whole turns of the ring a cell adds before it
    lands. A spinning wheel takes a few; with `step` set to `null` the
    symbols fly past and the ease brings it to rest.
  - `window` (default 1): how many symbols of the ring the cell shows at
    once, stacked, with the one it stands on in the middle. A wheel behind
    a tall window shows its neighbours, as a machine with several rows
    does; the row's height is shared between them.
  - `stagger` (default 0): seconds each cell waits behind the one to its
    right, so a row does not move as one piece.
  - `spin` (a trigger name or a list): fires the row off. Every cell
    travels its `turns` and lands on the symbol its text names at that
    moment, whether or not that is the one it already shows. Without it a
    cell only moves when the symbol it is asked for changes, which is
    what a binding means everywhere else, and a wheel asked for what it
    already carries would stand still while its neighbours spin. A host
    that already fires a trigger for the lever and the sound lets the
    reels hear it too, and the value still says where they land: set it
    and fire in the same breath and the wheel makes one journey to the
    new symbol, not two.

  A row is one value across several cells, which is what a counter or a
  board line is. Things that move independently are independent rows: a
  machine with three wheels is three one-cell reel layers side by side,
  each bound to its own variable, so a host can start and stop them when
  it likes and give each its own charset, turns and duration. `stagger` is
  for the other case, where one value ripples across a row as a carry
  does.

  Each cell keeps its own place on the ring, so a change moves only the
  cells it reaches: going from `109` to `119` rolls the tens wheel and
  leaves the others standing. Where a cell stands is counted straight
  rather than around the ring, so a journey can be longer than one turn,
  and a cell stopped mid-travel carries on from exactly where it stands. A
  cell shows the symbol it stands on and the one coming up behind it,
  clipped to the cell, which is what makes a roll look like a wheel rather
  than a fade.
- `audio`: a sound, played like a timeline is; draws nothing. See
  [Sound](#sound).

## Sound

```json
{ "name": "thunder", "type": "audio", "sound": "thunder",
  "trigger": "strike", "stop": "hush", "gain": 0.8, "bus": "sfx",
  "on_end": "thunder_done" }
```

An audio layer plays `sound`, a sound the host registered by name (by
convention the file's stem, from `assets/sounds/`). It sits in the layer
tree like anything else: a scene's music starts with the scene and stops
when the scene is left, a group's `gain` scales every sound below it, and
`gain` is a normal numeric property, so bindings, transitions and
timelines give volume control, fades and warm-ups for free. It is
controlled the way a timeline is, with the same names and meanings:

- `trigger` (a name or a list) plays it; `autoplay` plays it when the show
  loads or its scene is entered.
- `delay`, `loop`, `repeat`, `on_end`: as on timelines. `on_end` fires when
  a play finishes (after its repeats, never for loops) and is reported to
  the host like a timeline's.
- `stop` (a name or a list): a trigger that ends the play at once, without
  `on_end`.
- `retrigger` says what the trigger does while the sound already plays:
  `restart` (default: the play so far stops and a new one begins),
  `overlap` (another play sounds on top, up to `voices` at once, default
  4; the oldest stops beyond that: footsteps, flaps, coins) or `ignore`
  (the play finishes undisturbed).
- `gain` (default 1): loudness, 0 to 1 and above, multiplied by the gains
  of the groups above it. Only groups and audio layers have it.
- `bus` (optional): a name the host may route to an output; nothing in
  the engine depends on it yet.

An invisible audio layer, or one in an invisible subtree, is not heard,
like everything else in that subtree resolves to nothing.

The engine never opens an audio device or touches a sample. A host
registers each sound with only its duration (`Engine::set_sound`), which
is all the engine needs to loop, repeat and end plays, and reads back what
should be heard each frame with `Engine::voices()`: one voice per play,
with a stable id, the sound's name, the position in seconds, the effective
gain, whether it loops, and the bus. An audio backend diffs that list
frame by frame (a new id starts at its position, a vanished id stops, a
changed gain ramps, a position that jumped is resynced); a host with a
mixer of its own consumes the same list. Positions are a function of
engine time, so an offline render mixes sound sample-exact against the
frames, and a seek only needs the backend to resync. A sound registered
after its layer started playing is heard from where it would be by then;
until then the play is silent and does not end. The `cuelight-audio` crate
is that backend: decoding, a mixer, a sound device for the player and a
WAV file for offline rendering.

## Fonts

```json
{
  "fonts": {
    "score": { "file": "teeny_tiny_pixls-5", "color": "#808080" },
    "title": { "file": "bm_army-12", "border": { "color": "#102C80", "width": 1 } }
  }
}
```

A style names a font the host registered (`file`, by convention the font
file's stem), a `color`, and an optional `border` of `width` pixels in
`color` outside every glyph. Fonts are host assets like images: a text
layer whose font is not registered is skipped. There are two kinds, and
which one a style uses is decided by what was registered under that name,
so the layers using it do not change.

**Bitmap fonts**, in the [BMFont](https://www.angelcode.com/products/bmfont/doc/file_format.html)
text format common for pixel fonts: the host parses the `.fnt` with
`BitmapFont::parse` and registers it with its page images through
`Engine::set_font`. They have one fixed size (setting `size` is an error),
`color` multiplies the glyph colors (white keeps the font's own), and a
border also widens each character's advance by two widths. Text is
rasterized on the CPU pixel for pixel from the font pages, never
resampled, so pixel fonts stay exact on DMD-sized canvases. A line is as
wide as its characters' advances plus kerning; a block is as tall as its
line heights, except that the last line grows to fit its tallest glyph.
Characters missing from the font fall back to their uppercase form, then
to a space.

**Outline fonts** (TrueType / OpenType), for text that stays sharp at any
size: the host registers the file's bytes with `Engine::set_outline_font`
(cargo feature `outline-fonts`), and the style sets `size`, the em size in
canvas pixels (required). Text resolves to a glyph run the renderer draws
from the font's outlines, so nothing is rasterized or cached per string.
Line height and the baseline come from the font's metrics. Glyphs are
placed by their advance width alone: there is no kerning, ligatures or
complex shaping yet, so text sets slightly looser than in a browser. A
character the font lacks shows as the font's "missing" box.

```json
{ "fonts": { "speed": { "file": "inter_bold", "size": 180, "color": "#FFFFFF" } } }
```

## Bindings

A binding wires a layer property to a variable, evaluated every frame:

```json
{ "property": "opacity", "variable": "score", "scale": 0.001, "offset": 0.2 }
```

means `opacity = score * 0.001 + 0.2`. Animatable/bindable properties:
`x`, `y`, `opacity`, `scale`, `scale_x`, `scale_y`, `rotation`, `frame`
for sprite sheet images, and `gain` for groups and audio layers. `visible` can be bound but not keyframed.

A text or digits layer's `text` property can be bound too: the variable's text as
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

Font bindings may only map to declared font styles; `text`, `font` and
`visible` can be bound but not keyframed. Binding or keyframing a property
the layer's kind does not have (`font` on a shape) is rejected at load.

Two more knobs sit on a binding, for inputs that are levels or that
flicker:

- `threshold`: the value becomes 1 at or above this level and 0 below,
  before `scale` and `offset`. A lamp whose brightness arrives as a float
  lights at a half; with a `transition` it warms up from there. On a
  `visible` binding it is the level the layer shows from.
- `debounce`: seconds a new value has to hold before it reaches the
  property. Shorter changes (a strobing lamp, a switch that bounces) never
  show. At load and on entering a scene the current value applies at once.

The order is: `debounce`, then `map`, `threshold`, `scale` and `offset`,
then `transition`.

```json
{ "property": "visible", "variable": "lamp_41", "threshold": 0.5, "debounce": 0.05 }
```

### Transitions

By default a bound property jumps when its variable changes. With a
`transition` it moves there instead:

```json
{ "property": "x", "variable": "speed", "scale": 2.4,
  "transition": { "duration": 0.4, "ease": "cubic_out" } }
```

- `duration`: seconds a change takes, from the value the property has now.
- `ease` (default `linear`): any easing timelines know.
- `wrap`: the value lives on a ring of this size: 360 for an angle, 10 for
  a sheet with one frame per digit. `direction` picks the way round:
  `shortest` (default), `forward` (a reel: 9 to 0 rolls on) or `backward`.

- `step`: the size of one move. A larger change plays as several moves in
  a row, each taking `duration` with the `ease` applied per move, the last
  one shorter when the change is no whole number of steps: a reel kicked
  digit by digit.
- `offset`: motion added on top of a move, along its direction of travel.
  Keys like a timeline track's: `t` in seconds from the start of the move,
  `v` in the units of the binding's result, optional `ease`. It has to
  start and end at 0. Unlike an overshooting ease, which swings by a share
  of the distance, an offset is the same size however far the move goes.
  With `step` every move gets it, and a move lasts as long as the longer of
  `duration` and the offset.

A score reel from a sheet with one frame per digit: kicked one digit at a
time, there in 0.12 s, settling against its stop by an eighth of a digit:

```json
{ "property": "frame", "variable": "credits",
  "transition": { "duration": 0.12, "ease": "quad_in", "step": 1,
                  "wrap": 10, "direction": "forward",
                  "offset": [ { "t": 0.12, "v": 0 }, { "t": 0.17, "v": 0.12 },
                              { "t": 0.22, "v": 0 } ] } }
```

What is eased is the binding's result, after `map`, `scale` and `offset`,
so a map from states to opacities fades between them. On a `text` binding
a number counts up or down before it is formatted, in whole numbers when
it goes from one whole number to another; text that is not a number
jumps. `font` bindings cannot have a transition.

A change while a transition runs starts a new one from the value reached
so far, with the full duration. When a show loads or a scene is entered,
properties start at their value: nothing eases in from the base. The value
is a function of time only, so it does not depend on the frame rate.

A transition also smooths a variable the host updates less often than the
display refreshes, at the price of showing the value late by about the
duration. For a regular feed use a `linear` ease with the feed's interval
as `duration`: the value then moves continuously, one interval behind. An
ease that slows down at the end pauses before every update.

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
  `autoplay` is true. Re-firing the trigger restarts it from 0. `trigger`
  is one name or a list (`["turn_left", "hazard"]`); firing any of them
  has the same effect. A scene's `trigger` takes the same two forms.
- Keys are `(t seconds, value)`; between two keys the value interpolates
  using the **later** key's `ease` (before the first key it holds the
  first value, after the last it holds the last). Easings: `linear`
  (default), `quad_in`, `quad_out`, `quad_in_out`, `cubic_in`,
  `cubic_out`, `cubic_in_out`, `step`, and three kinds that do not go
  straight there: `back_in`, `back_out`, `back_in_out` swing about 10%
  past an end (`back_out` overshoots and comes back: a reel snapping
  against its stop), `elastic_out` arrives fast and rings around the end,
  `bounce_out` hits the end and bounces off it without passing it.
- The timeline's duration is its longest track's last key. When it ends
  it stops and its properties fall back (see precedence); with `loop`
  the playhead wraps instead. For a seamless loop, author each track's
  value at the end equal to its value at 0.
- `delay` (seconds) postpones the first key after the timeline starts;
  meanwhile it does not own its properties. A loop or repeat does not
  wait again, so `delay` plus `loop` is "wait, then repeat forever".
- `repeat` plays it that many times (fractions stop partway: `2.5` ends
  halfway through the third play); `loop` repeats forever. The two cannot
  be combined.
- `on_end` names a trigger fired when the timeline finishes (after its
  last repeat; loops never finish). It behaves exactly like a host firing
  the trigger, so it can start other timelines or restart a sequence, and
  it is reported to the host as an event (see [Host contract](#host-contract)).

## Property precedence

Each frame a property resolves to, strongest first:

1. a running timeline that animates it
2. a binding
3. the base value on the layer

So a timeline temporarily owns whatever it animates; when it finishes the
property falls back to its binding or base value instantly. To avoid a
visible jump, make base values match the timeline's endpoints. A binding's
transition keeps following its variable underneath a timeline, so the
property falls back to where the transition is by then.

## Host contract

The engine is driven exclusively through four calls: `load_show` (JSON in),
`set_variable`, `trigger`, and `advance_frame(dt)`. What the show itself
fires (`on_end` triggers) comes back through `drain_events()`, so content
can tell the host that something finished. Output is either
`resolved_layers()` (a flat, GPU-free draw list; each item carries a
`transform` that is the identity unless a rotation or uneven scale placed
it, in which case the shape is in its layer's space and the transform
puts it on the canvas) or the `render` feature's vello rasterizer, plus
`voices()` for what should be heard (see [Sound](#sound)). Everything else, including where variable values and
trigger events come from (game state, audio, MIDI, a console), is the
host's business: see the `cuelight-player` crate and the `mic_pop` example.

## Regenerating the schema

The schema is generated from the Rust model types (`schemars`) and checked
by CI, so it cannot drift. After changing the model:

```sh
UPDATE_SCHEMA=1 cargo test --features schema --test schema
```
