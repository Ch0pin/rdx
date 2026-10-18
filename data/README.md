# Android API type and exception facts

`android-35-exceptions.json` records class and interface ancestry plus exact method throws
declarations extracted from the Android SDK API 35 `android.jar`. It contains
metadata only, not class implementations. The input SHA-256 is embedded in the
file. Regenerate with:

```
python3 scripts/generate_platform_exceptions.py /path/to/android-35/android.jar data/android-35-exceptions.json --constructors-output data/android-35-constructors.json
```

Used as conservative source-level evidence for Java checked catches when the APK
does not define the class. APK definitions take precedence. This does not claim
that a method exists on every Android version or infer runtime exception paths.
The installed application does not need an Android SDK or Java runtime.

`android-35-constructors.json` records accessible parent constructor signatures,
access flags, and declared exceptions. Its `member_types` entries map exact binary
names to a source owner, member path, and enclosing descriptor chain. Public static
member classes require consistent `InnerClasses` metadata and a fully accessible
enclosing chain. These facts support constructor recovery and readable names such
as `View.BaseSavedState`; loaded APK definitions take precedence over every SDK
type in the chain. A dollar sign alone supplies no nesting evidence.

`android-35-source-declarations.json` records complete, directly declared SDK
method headers, including methods with no checked throws declaration. It supplies
proof-only source authority for bounded same-object catchall rethrow recovery.
Lookup requires an exact owner, name, parameters, return type, and staticness;
ambiguous Java signatures, inaccessible or nested SDK owners, and APK-shadowed
owners remain unresolved. The existing sparse exception facts keep their original
meaning. These headers constrain emitted Java declarations; they do not suppress
operational exception effects or establish runtime callback reachability.

Regenerate the complete declaration metadata with:

```
python3 /Users/ch0pin/Desktop/Git-Projects/rdx/scripts/generate_platform_source_declarations.py /Users/ch0pin/Library/Android/sdk/platforms/android-35/android.jar /Users/ch0pin/Desktop/Git-Projects/rdx/data/android-35-source-declarations.json
```
