# Android API 35 resource parameter contracts

The bundled `android-35-resource-contracts.json` contains 344 exact framework method signatures and 394 integer parameter
contracts. It is generated metadata, with no Android implementations or SDK
runtime dependency.

The primary source is
`${ANDROID_HOME}/platforms/android-35/data/annotations.zip`
(SHA-256 `bbbe0b4bd708951887a2e372b28aadf57c3d8de7dde405e8aee32eb8e283f452`).
External annotation signatures are joined against the complete exact declarations
in the bundled `android-35-source-declarations.json`
(SHA-256 `2d68393523f78b028f2abd5b154eb1c04f859de75304349bebc89425f3094cdc`),
which pins Android API 35 `android.jar` SHA-256
`4566663c3876e022b4fa4ced8c8697c4ab1688267f090114fd92d027b32e619b`.

Regenerate and validate the positive and negative examples with:

```sh
RDX_ROOT="$(git rev-parse --show-toplevel)"
python3 "$RDX_ROOT/scripts/generate_resource_contracts.py" \
  "$ANDROID_HOME/platforms/android-35/data/annotations.zip" \
  "$RDX_ROOT/data/android-35-source-declarations.json" \
  "$RDX_ROOT/data/android-35-resource-contracts.json"
```

The `contracts` object maps original CodeLink method identities to arrays of
`[zero_based_parameter_index, [resource_table_kinds]]`. Original binary nested
names and return descriptors remain intact. For example,
`android.app.AlertDialog$Builder.setMessage(I)Landroid/app/AlertDialog$Builder;`
maps parameter zero to `string`. This supports source display without changing
method navigation identity.

Only explicit parameter annotations whose exact parameter descriptor is `I`
are admitted. Field and return annotations, unresolved source types, ambiguous
declarations and conflicting specific resource annotations are excluded. Generic
type arguments may be erased only when the resulting signature matches one exact
existing method declaration; unresolved type variables are never guessed.
`StyleableRes` denotes an index and is excluded, as are `IntDef`, `ColorInt`, and
the unspecialized `AnyRes` qualifier. Counts of declined annotations are retained
in `skipped`.

`DrawableRes` admits `drawable`, `mipmap`, and `color`, reflecting drawable
resource loading of image and color values. `InterpolatorRes` admits
`interpolator` and legacy `anim` resources. Other annotations map to their
specific resource-table type.

The generator validates `Activity.setContentView`, `Context.getDrawable`,
`AlertDialog.Builder.setMessage`, `TextView.setText`, and both animation slots of
`Activity.overrideActivityTransition`. It also rejects the integer index of
`TypedArray.getDrawable`, the flags of `WallpaperManager.getDrawable`, and the
`TextView.setText(CharSequence)` overload. The source archive does not contain a
resource annotation for `Activity.overridePendingTransition`; existing explicit
verified contracts for that API remain independent of this dataset.

Runtime resolution must still establish exact declaration ownership, argument
position, resource kind, and numeric provenance. Loaded APK declarations take
precedence over framework facts. An unannotated loaded override does not gain a
resource contract merely through ancestry. These presentation contracts do not
prove runtime behavior of overriding application code.

The runtime follows unchanged integer parameters through bounded straight-line
DEX forwarding methods. An abstract delegate is admitted only when all located
concrete subclass declarations agree on the resource argument contract. Cycles,
transforms, unknown consumers, exception regions, branches, conflicting
annotations, or exhausted analysis budgets leave the literal unchanged. The
analysis is local to the loaded class graph; it is not a runtime reachability
claim. The original method navigation labels are preserved.

A verified resource argument with no table entry keeps its numeric literal and
gets a hover explanation including decimal/hex IDs and expected resource kinds.
Opening that reference displays the missing-entry explanation rather than an
invented resource name or file. Numeric search still matches both resolved
symbols and preserved literals.
