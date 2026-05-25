# dugite-config: schema-driven sub-fields for Object params

Date: 2026-05-25
Author: Michael Fazio
Status: Approved (design phase) — pending implementation plan

## Problem

`dugite-config` renders JSON Object parameters (`AcceptedConnectionsLimit`,
`Rpc`, `Storage`) as a single row whose value is the raw JSON object. To edit
any sub-field the operator must hand-type valid JSON into the `EditMode::Typing`
buffer. There is no per-sub-field validation, no defaults visibility, no
discoverability of which sub-fields exist, and a typo silently corrupts the
whole object on save.

`schema.rs` documents the sub-field shape only in prose (in the `description`
and `tuning_hint` strings) — the schema layer has no structural knowledge of
what's inside an Object.

## Goal

Replace raw-JSON editing for known Object params with structured, typed
sub-field editing in the existing tree. Each sub-field becomes its own row with
the same affordances as every other scalar parameter (in-line edit, type
validation, defaults, reloadability indicator, diff).

## Non-goals

- Schema-driven editing for nested arrays (no current Object param uses them).
- Reordering or hiding sub-fields per operator preference — render order is
  fixed by schema order.
- Wiring sub-field hot-reload at the runtime level (the editor only displays
  the indicator; runtime reload semantics are unchanged).
- Per-leaf "revert" key — top-level revert still covers it.
- Promoting top-level `Unknown` objects (no schema at all) to structured
  editing. They keep the raw-JSON path.

## Decisions

These four were locked in during brainstorming:

1. **Hydrate defaults on load; prune to non-default on save.** Mirrors today's
   `inject_schema_defaults` for top-level keys. Operators see every sub-field;
   files stay minimal-diff.
2. **Object rows are read-only at the container level.** Pressing Enter on the
   Object row toggles tree expansion. All edits happen at leaf sub-rows.
3. **Full recursion via `ParamType::Object { fields: &[SubParamDef] }`** —
   sub-fields can themselves be Objects (`Rpc.Tls`), to any depth declared in
   the schema.
4. **Unknown sub-keys are preserved as raw-JSON leaves** alongside schema-known
   leaves. Round-trips safely even when cardano-node adds a field we haven't
   modelled yet.

## Design

### A. Schema data model

`ParamType` becomes:

```rust
pub enum ParamType {
    Bool,
    U64 { min: u64, max: u64 },
    F64 { min: f64, max: f64 },
    String,
    Enum { values: &'static [&'static str] },
    Path,
    Object { fields: &'static [SubParamDef] },
}

pub struct SubParamDef {
    pub key: &'static str,
    pub param_type: ParamType,            // recursive
    pub default: &'static str,
    pub description: &'static str,
    pub tuning_hint: &'static str,
    pub reloadability: Reloadability,
}
```

`SubParamDef` is distinct from `ParamDef` (no `section` field — sub-fields live
under their parent's section) so the compiler enforces that a sub-field can't
accidentally appear as a top-level row, and vice versa.

A `default: ""` on a `SubParamDef` whose `param_type` is numeric
(`U64`/`F64`/`Bool`/`Enum`) signals **no schema default**:
`default_as_json` returns `None` (today's behavior for unparseable defaults),
hydration skips the leaf, and it appears in the tree only when present in the
on-disk file. Used by Storage's profile-derived fields (see F). For
`String`/`Path` leaves, `default: ""` is a valid empty-string default — it
hydrates, and save prunes it back out when still empty.

`ParamDef::default_as_json()` for `Object` switches from
`serde_json::from_str(self.default)` to a recursive walk of `fields` that
synthesises a `Value::Object` from each leaf's default. This removes the
duplicate source of truth (prose `default` string vs. structured fields) for
Object params — the prose default becomes redundant and is dropped at the
Object level (`default: ""` for Object entries; sub-leaves carry their own
default strings).

### B. Hydration and save pruning

`ConfigEntry` stays one-per-top-level-key. The on-disk JSON shape is unchanged.
Sub-field reads/writes happen via `serde_json::Value::pointer_mut()` against
the entry's `Value::Object`.

`ConfigEntry` gains:

```rust
pub struct ConfigEntry {
    pub key: String,
    pub value: Value,
    pub modified: bool,
    pub present_in_file: bool,
    /// For Object entries: JSON-pointer paths (e.g. "/Tls/CertPath") that were
    /// synthesised during inject_schema_defaults. Empty for non-Object entries
    /// and for sub-keys that were present in the on-disk file.
    pub synthetic_paths: HashSet<String>,
}
```

**Hydration.** `LoadedConfig::inject_schema_defaults` extends with a second
pass: for each entry whose schema is `ParamType::Object { fields }`, walk
`fields` recursively. For every leaf path not present in the entry's value,
insert the leaf's schema default via `pointer_mut`/`Map::insert` and record
the path in `synthetic_paths`. Nested missing objects (e.g. when `Rpc` is
`{}` and `Tls` doesn't exist) are created on the way down.

Hydration must be idempotent: re-running it on an already-hydrated entry adds
no new paths to `synthetic_paths` (the leaf is already present).

**Save pruning.** `save_config` extends per-Object:

1. Walk each Object entry's `synthetic_paths`.
2. For each synthetic path, compare the current value at that path to the
   schema default for that leaf.
3. If equal, remove the leaf (`Map::remove`).
4. After all synthetic leaves are processed, recursively drop any sub-object
   that became empty.
5. If the resulting top-level Object is empty AND `present_in_file == false`
   AND its all-defaults form is `{}`, skip emitting the top-level key
   entirely (existing top-level prune logic already covers this).

Leaves the user touched are not in `synthetic_paths` (the touch happens via
`commit_edit`, which clears synthetic status — see D). Leaves originally in
the file are not in `synthetic_paths` (load doesn't add them). Both round-trip
verbatim.

### C. Tree, cursor, expansion

```rust
pub struct Item {
    pub entry_idx: usize,
    pub path: Vec<String>,         // empty = the entry row; ["Tls"] = nested container; ["Tls","CertPath"] = leaf
    pub def: ItemDef,
    pub depth: u8,                 // 0 for top-level, +1 per nesting level
    pub is_container: bool,        // true for Object rows
    pub expanded: bool,            // only meaningful when is_container
}

pub enum ItemDef {
    Top(&'static ParamDef),
    Sub(&'static SubParamDef),
    Unknown,
}
```

`Section.items` is the **flat, fully-walked** depth-first row list. Visibility
at render time is computed by walking `path` and skipping any row whose
ancestor container has `expanded == false`. The cursor (`cursor_section`,
`cursor_item`) is unchanged; `move_up`/`move_down` skip rows that aren't
visible using a single `is_visible(&Item)` helper.

`build_sections` extends with a recursive `walk_object` that, for each Object
entry, descends `SubParamDef::fields` to emit:
- a container row (`is_container=true`) for each `ParamType::Object` sub-schema;
- a leaf row for everything else;
- after schema-known children, any unknown sub-keys present in the loaded
  value as `ItemDef::Unknown` leaves.

**Default expansion.** Top-level Object container rows start **collapsed**.
Nested containers (`Rpc.Tls`) also start collapsed. `Enter` (or `Space`) on a
container toggles expansion. Section collapse semantics unchanged.

### D. Edit flow

`begin_edit` dispatches on the resolved leaf type:

- Container row (`is_container=true`) → toggle `item.expanded`. No buffer
  opens.
- `ParamType::Bool` → inline toggle (today's behaviour).
- `ParamType::Enum` → cycle value (today's behaviour).
- `ParamType::U64` / `F64` / `String` / `Path` → open `EditMode::Typing` with
  the leaf's current display value.
- `ItemDef::Unknown` → raw-JSON edit (today's Unknown path).

`commit_edit` writes back via `entry.value.pointer_mut(&path_to_json_pointer(item.path))`
when `item.path` is non-empty; otherwise writes `entry.value` directly. In both
cases it sets the top-level `entry.modified = true` and **removes**
`item.path`'s JSON-pointer from `entry.synthetic_paths` (a touched leaf is no
longer synthetic).

`reloadability` displayed in the row indicator reads from `ItemDef::Sub(...)`
for sub-leaves, so individual sub-fields can carry `[H]` / `[R]`.

### E. Diff overlay

`OriginalValues` is unchanged (still per-top-level-key). For per-leaf diff
output, `diff::render` adds a `walk_object_diff(orig, curr, def, path)`
recursion:

- For each schema-known sub-field, recurse into both `orig` and `curr` at that
  key.
- Emit one line per changed leaf: `Parent.Sub.Leaf: <old> → <new>`.
- For unknown sub-keys (present in either side but not in schema), emit with
  a `[unknown]` tag, mirroring today's top-level Unknown handling.
- Skip leaves where `orig == curr`, even if synthetic in either snapshot.

Top-level diff for non-Object entries is unchanged.

### F. Schema migration

Three current Object params get sub-schemas:

#### `AcceptedConnectionsLimit` (3 leaves)

| Sub-key | Type | Default | Reload |
|---|---|---|---|
| `hardLimit` | `U64{0,65535}` | `512` | Restart |
| `softLimit` | `U64{0,65535}` | `384` | Restart |
| `delay` | `F64{0.0,60.0}` | `5.0` | Restart |

#### `Rpc` (8 scalar leaves + 1 nested container with 2 leaves)

| Sub-key | Type | Default | Reload |
|---|---|---|---|
| `Enabled` | `Bool` | `false` | Restart |
| `ListenAddr` | `String` | `127.0.0.1` | Restart |
| `Port` | `U64{1,65535}` | `50051` | Restart |
| `MaxConcurrentStreams` | `U64{1,4096}` | `64` | Restart |
| `StreamBufferSize` | `U64{1,65536}` | `256` | Restart |
| `ReflectionEnabled` | `Bool` | `true` | Restart |
| `WebEnabled` | `Bool` | `false` | Restart |
| `AlphaEnabled` | `Bool` | `true` | Restart |
| `Tls` | `Object{...}` | `{}` | Restart |
| `Tls.CertPath` | `Path` | `""` | Restart |
| `Tls.KeyPath` | `Path` | `""` | Restart |

Empty-string default for `Tls.CertPath` / `KeyPath` means "unset"; hydration
synthesises them, save prunes them (both equal default), and the net on-disk
shape for an Rpc with no TLS configured is `"Tls": {}` — which itself prunes
to absent at the parent. Net effect: no leftover empty `Tls` objects.

#### `Storage` (7 leaves)

| Sub-key | Type | Default | Reload |
|---|---|---|---|
| `profile` | `Enum["ultra-memory","high-memory","low-memory","minimal"]` | `high-memory` | Restart |
| `immutableIndexType` | `Enum["mmap","in-memory"]` | `mmap` | Restart |
| `mmapLoadFactor` | `F64{0.0,1.0}` | `0.7` | Restart |
| `utxoBackend` | `Enum["lsm","in-memory"]` | `lsm` | Restart |
| `utxoMemtableSizeMb` | `U64{1,65536}` | (profile default) | Restart |
| `utxoBlockCacheSizeMb` | `U64{1,65536}` | (profile default) | Restart |
| `utxoBloomFilterBits` | `U64{1,32}` | `10` | Restart |

`utxoMemtableSizeMb` / `utxoBlockCacheSizeMb` have no schema default (they
default in dugite-node based on `profile`). They are declared with no leaf
default; hydration skips them; they appear in the tree only when set in the
file. Documented in the leaf's `description`.

Sub-schema correctness is verified against `crates/dugite-node` config types
in `tests/config_coverage.rs` (extended — see G).

### G. Tests

New / extended tests in `crates/dugite-config`:

**Unit (schema.rs):**
- `default_as_json` for `ParamType::Object` recurses through `fields` and
  produces the expected `Value::Object` for `Rpc`, `AcceptedConnectionsLimit`,
  `Storage`.
- A nested Object (`Rpc.Tls`) synthesises a nested default object.
- `SubParamDef::param_type.validate()` rejects out-of-range numerics, wrong
  enum values, etc.

**Unit (config.rs):**
- `inject_schema_defaults`: for `Rpc: {}` on disk, after hydration every
  schema leaf is present with its default value; `synthetic_paths` contains
  one entry per leaf path.
- `inject_schema_defaults` is idempotent at the sub-field level — second
  invocation leaves `synthetic_paths` unchanged.
- `inject_schema_defaults` for a partially-populated Object only synthesises
  the missing leaves; existing leaves are not in `synthetic_paths`.

**Unit (config.rs save pruning):**
- `save_config` round-trip for `Rpc: {}` → output omits `Rpc` entirely (all
  synthetic, all equal to default, parent not present in file).
- `save_config` for `Rpc: { "Enabled": true }` → output emits exactly
  `{"Enabled": true}` (other leaves still synthetic + equal default → pruned).
- `save_config` for `Rpc: { "Tls": { "CertPath": "/etc/x.pem" } }` → emits
  `{"Tls":{"CertPath":"/etc/x.pem"}}` (Tls.KeyPath was synthetic + default,
  pruned; Tls parent retained because it has surviving children).
- `save_config` preserves an unknown sub-key (`Rpc.NewFeature`) verbatim
  across load/save with no intervening edit.

**Unit (app.rs):**
- `build_sections` emits a container row + leaf rows in schema order, with
  correct `depth` values.
- Top-level Object rows start `expanded=false`; nested containers also
  `expanded=false`.
- `move_up`/`move_down` skip rows whose ancestor is collapsed.
- `begin_edit` on a container row toggles `expanded`; no `EditMode::Typing`
  opens.
- `begin_edit` on a `Bool` sub-leaf toggles inline; on a numeric sub-leaf
  opens the typing buffer with the leaf's current value.
- `commit_edit` on `/Tls/CertPath` writes to the right pointer path on the
  parent entry's `Value` and clears `/Tls/CertPath` from `synthetic_paths`.
- `commit_edit` on a sub-leaf sets the top-level entry's `modified=true`.

**Diff (diff.rs):**
- Editing a single sub-leaf produces one diff line `Rpc.Port: 50051 → 5051`,
  not a whole-object diff.
- Editing two sub-leaves produces two diff lines.
- Sub-leaf back to its original value (after an intermediate change) drops
  out of the diff.

**Coverage (tests/config_coverage.rs):**
- Every `ParamType::Object` schema has non-empty `fields`.
- Every sub-leaf with a schema `default` parses to a `Value` matching its
  `param_type` (i.e. `default_as_json` returns `Some`).
- Cross-check: every sub-key documented in our schema corresponds to an
  actual field in `dugite-node`'s config struct (or is explicitly allowed —
  e.g. cardano-node-compat aliases).

## Open questions

None at design time — all four decision points resolved during brainstorming.

## Risks

- **JSON-pointer escaping.** Keys with `/` or `~` characters need RFC 6901
  escaping. None of the current schema sub-keys contain either, but the
  helper `path_to_json_pointer(&[String])` must escape regardless to stay
  safe for future schema additions and for Unknown sub-keys.
- **Pruning interaction with manually-edited files.** If an operator hand-edits
  the on-disk file to set a sub-leaf equal to its default, our save will
  retain it (because it was `present_in_file`, not synthetic). This is the
  intended behavior — user intent is preserved — but worth documenting in
  the saved config docs.
- **Hydration cost.** All three current Object params are small (≤10 leaves).
  No perf concern. Adding much larger Object schemas in future could merit
  lazy hydration.
- **Unknown sub-keys in a fully-collapsed default Object.** A `Rpc:
  {"NewFeature": ...}` on disk where every other sub-key is synthetic +
  default: save must keep the `Rpc` parent and the `NewFeature` leaf, even
  though pruning would otherwise empty `Rpc`. Save logic must check for
  surviving children (schema-known *or* unknown) before dropping the parent.

## Out of scope (revisit later)

- Per-leaf revert key in the tree.
- Schema-driven editing for top-level Unknown objects.
- Per-sub-field hot-reload runtime wiring.
- Tree-view filter mode that re-expands containers on match (search already
  works via flattened `filtered_items`; the new walk preserves that — see C).
