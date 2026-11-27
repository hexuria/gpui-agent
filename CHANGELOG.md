# Changelog

## 0.5.0

The recipe wire is unchanged for recipes that use neither `within` nor the new recipe features: they
validate and run as before, with the same plan fingerprint. The Rust API is breaking because
`Op::Press` and `Op::LongPress` have a new public field in the combined release: struct literals need
`within: None`, and exhaustive matches need `..`.

- **Bounded scroll-until.** `.wants` gains `scroll <target> --dy <n> [--dx <n>]`, the JSON `scroll`
  op it already had. JSON and `.wants` gain `scroll_until` (`scroll_until <container> <pattern>
  --dy <n> [--dx <n>] [--max-steps <k>]`). The runner snapshots first, sends ordinary `scroll` and
  `snapshot` requests, and searches only the container's visible descendants. A match must be the
  whole stable id or whole visible text of the regex-lite pattern and, when geometry exists, intersect
  the container's bounds. `max_steps` is 1 to 50 (default 10); zero deltas, repeated `.wants` flags,
  a container that does not move after two scrolls, and a 60 second overall budget are refused or
  stopped clearly. Success carries the matched node id; a dependent step can use `$<step>.node`.
  There is no wire `scroll_until` op, so hosts need no new capability.
- **Numeric params.** JSON numeric recipe fields `x`, `y`, `within.x`, `within.y`, `dx`, `dy`,
  `duration_ms`, `key_delay_ms`, and `max_steps` may be exactly `$param`; the value is bound before
  the field's numeric validation and is still checked as finite, integral where required, and in range.
- **State assertions.** `assert` and `wait_until` accept optional `state` (one exact state string) and
  `states` (a list whose entries must all be present), matched against `UiNode.states`; `.wants` accepts
  `state="exact state"` and `states="one,two"`. Omitting them leaves validation and fingerprints unchanged.
  This adds public `AssertSpec` fields, so direct struct literals without `..` need them.
- **Read-back pinning.** No behavior change: `Client::act` documents and tests that a target gone after
  a click or press is a verified change (`after.exists == false`), not a failure.

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
