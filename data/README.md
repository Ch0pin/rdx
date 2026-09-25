# Android API type and exception facts

`android-35-exceptions.json` records class and interface ancestry plus exact method throws
declarations extracted from the Android SDK API 35 `android.jar`. It contains
metadata only, not class implementations. The input SHA-256 is embedded in the
file. Regenerate with:

```
python3 scripts/generate_platform_exceptions.py /path/to/android-35/android.jar data/android-35-exceptions.json
```

Used as conservative source-level evidence for Java checked catches when the APK
does not define the class. APK definitions take precedence. This does not claim
that a method exists on every Android version or infer runtime exception paths.
The installed application does not need an Android SDK or Java runtime.
