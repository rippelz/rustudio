# Building RuStudioAPK

## Prerequisites

- JDK 17 and the [Android SDK](https://developer.android.com/studio).
- Android SDK Platform 35, Build Tools 35.0.0, and NDK 29.0.14206865.
- Rust installed with [rustup](https://rustup.rs/).
- A host C/C++ toolchain. On Debian and Ubuntu, install `build-essential`. On Windows, install Visual Studio Build Tools with **Desktop development with C++** and the Windows SDK.

Install the SDK packages using the Android Studio SDK Manager or the command line.

```sh
sdkmanager --licenses
sdkmanager "platforms;android-35" "build-tools;35.0.0" "ndk;29.0.14206865"
rustup target add aarch64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
```

Set `JAVA_HOME` to the JDK directory and `ANDROID_HOME` to the SDK directory. Android Studio can also set `sdk.dir` in `local.properties`.

## Build locally

Run `./gradlew assembleDebug` on Linux, `.\gradlew.bat assembleDebug` on Windows, or the `:app:assembleDebug` task in Android Studio. If Android Studio cannot find Cargo, see `cargo.dir` in `local.properties.example`.

The ARM64 debug APK is written to `app/build/outputs/apk/debug/app-debug.apk`.

## GitHub Actions

Open [Actions in saidrbx/RuStudioAPK](https://github.com/saidrbx/RuStudioAPK/actions) and enable workflows if GitHub prompts you. The **Android APK** workflow builds on Ubuntu and Windows and uploads one debug APK artifact per runner. It runs on pushes to `android` and `main` and on pull requests targeting those branches.

Push the Android sources to `android` for the first automatic run. To enable **Run workflow** in the Actions UI, the workflow file must also exist on the fork's default branch. [GitHub workflow events](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_dispatch)

After a successful run, open its summary and download an APK from **Artifacts**. CI uses the runner's debug signing key and requires no repository secrets. Use a persistent signing key for distributed APKs so installs can receive in-place updates. For public downloads, attach the signed APK to a [GitHub Release](https://github.com/saidrbx/RuStudioAPK/releases).
