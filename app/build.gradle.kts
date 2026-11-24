import java.io.File
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

val rustDir: File = rootProject.file("rust")
val rustJniLibs: File = layout.buildDirectory.dir("rustJniLibs").get().asFile
val rustAbis = listOf("arm64-v8a")

android {
    namespace = "com.rustudio.app"
    compileSdk = 35
    ndkVersion = "29.0.14206865"

    defaultConfig {
        applicationId = "com.rustudio.app"
        minSdk = 26
        targetSdk = 34 // 35 would force edge-to-edge insets on the native window
        versionCode = 27
        versionName = "0.5.1-android.3"
        ndk { abiFilters += rustAbis }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Signed with the debug key so the release APK installs directly; use your own for distribution.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    sourceSets["main"].jniLibs.srcDir(rustJniLibs)
    packaging { jniLibs { useLegacyPackaging = true } }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
}

// ---- Rust -> librustudio.so (cargo-ndk) ------------------------------------
val cargoNdkBuild = tasks.register("cargoNdkBuild") {
    group = "rust"
    description = "Builds rust/ into librustudio.so with cargo-ndk"
    inputs.dir(rustDir.resolve("src"))
    // The updated upstream moved most code into workspace crates.
    inputs.dir(rustDir.resolve("crates"))
    inputs.dir(rustDir.resolve("vendor"))
    inputs.dir(rustDir.resolve("assets"))
    inputs.dir(rustDir.resolve(".cargo"))
    inputs.files(rustDir.resolve("build.rs"), rustDir.resolve("api-dump.json"), rustDir.resolve("mechanics.json"))
    inputs.files(rustDir.resolve("Cargo.toml"), rustDir.resolve("Cargo.lock"))
    inputs.property("ndkVersion", android.ndkVersion)
    inputs.property("abis", rustAbis)
    inputs.property("minSdk", requireNotNull(android.defaultConfig.minSdk))
    inputs.files(rootProject.file("local.properties"))
    outputs.dir(rustJniLibs)

    doLast {
        val ndkDir = android.ndkDirectory
        val cargoHome = System.getenv("CARGO_HOME") ?: "${System.getProperty("user.home")}/.cargo"
        val local = Properties()
        rootProject.file("local.properties").takeIf(File::isFile)?.inputStream()?.use { local.load(it) }
        val path = listOfNotNull(local.getProperty("cargo.dir"), "$cargoHome/bin", System.getenv("PATH"))
            .joinToString(File.pathSeparator)
        val cargoName = if (System.getProperty("os.name").startsWith("Windows")) "cargo.exe" else "cargo"
        val cargo = path.split(File.pathSeparator).map { File(it, cargoName) }
            .firstOrNull { it.isFile }?.absolutePath ?: cargoName

        val check = ProcessBuilder(cargo, "ndk", "--version")
            .redirectErrorStream(true)
            .redirectOutput(ProcessBuilder.Redirect.DISCARD)
        check.environment()["PATH"] = path
        val ok = try { check.start().waitFor() == 0 } catch (e: Exception) { false }
        if (!ok) {
            throw GradleException(
                "cargo-ndk not found. Install Rust (rustup.rs), then run:\n" +
                "  rustup target add aarch64-linux-android\n  cargo install cargo-ndk --version 4.1.2 --locked\n" +
                "and, if cargo isn't on Android Studio's PATH, set cargo.dir=... in local.properties."
            )
        }

        val cmd = mutableListOf(cargo, "ndk")
        rustAbis.forEach { cmd += listOf("-t", it) }
        val minSdk = requireNotNull(android.defaultConfig.minSdk)
        cmd += listOf(
            "--platform", minSdk.toString(), "--link-libcxx-shared", "-o", rustJniLibs.absolutePath,
            "build", "--release", "--locked", "--lib",
        )
        val pb = ProcessBuilder(cmd).directory(rustDir).redirectErrorStream(true)
        pb.environment()["PATH"] = path
        pb.environment()["ANDROID_NDK_HOME"] = ndkDir.absolutePath
        pb.environment()["ANDROID_NDK_ROOT"] = ndkDir.absolutePath
        local.getProperty("cargo.targetDir")?.let { pb.environment()["CARGO_TARGET_DIR"] = it }
        val process = pb.start()
        process.inputStream.bufferedReader().useLines { lines ->
            lines.forEach { logger.lifecycle(it) }
        }
        val code = process.waitFor()
        if (code != 0) throw GradleException("cargo ndk build failed (exit $code); see the log above")
    }
}

tasks.configureEach {
    if (name.startsWith("merge") && name.endsWith("JniLibFolders")) dependsOn(cargoNdkBuild)
}
