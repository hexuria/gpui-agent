# Changelog

## 0.4.1

Recipes, additive: a 0.4.0 recipe validates and runs as before, with the same plan fingerprint.

- **Paced typing.** A `type` step may carry `key_delay_ms` (`.wants`: `--key-delay-ms <n>`). The client then
  types its text one character per ordinary `type` request, that many milliseconds apart, as a person types. No
  wire change: 0.4.0 hosts take it. See [docs/RECIPES.md](docs/RECIPES.md#paced-typing).
  - The target is focused once. Later keys go to the box the key before went to, or to the next text box after it
    in the same group when the focus moved on to it (a code box's next cell), so a filled box keeps its caret after
    the typed text and a code box gets one digit per cell. Focus that goes anywhere else stops the step
    (`delivery_uncertain`, "the focus left ... for ..."): to a field directly on a window, page or dialog, after a
    `\n` or `\t` key, or back or past the next box.
  - A virtual step checks every character has a keystroke before the first key.
  - A refusal after the first key is reported as `delivery_uncertain`, with how many keys reached the app.
  - `key_delay_ms` is 1 to 10000 (refused at parse otherwise, and on any op but `type`), and one step's pauses
    together at most 120000 ms (`MAX_PACED_TYPE_MS`, refused when planned).
  - `schemas/recipe.v1.schema.json` lists the field on `type` with the same bounds.
- **todo demo host:** a virtual `type` or `key` into the text input that already has the focus no longer clicks
  it again.

A 0.4.0 client refuses `key_delay_ms` (`recipe_unknown_field`): install gpui-agent-cli 0.4.1 to run paced recipes.

## 0.4.0

Recipes stable: see [docs/RECIPES.md](docs/RECIPES.md#stability).
