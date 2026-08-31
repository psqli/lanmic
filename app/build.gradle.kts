plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// ---------------------------------------------------------------------------
// The audio engine is a Rust crate in ../rust, built into one .so per ABI by
// cargo-ndk and packaged straight out of the build directory.
//
//   cargo install cargo-ndk
//   rustup target add aarch64-linux-android armv7-linux-androideabi \
//                     x86_64-linux-android
//
// It is built in release even for a debug APK. An unoptimised realtime audio
// path misses its deadlines, so a debug build of it would not be worth testing
// against - the same reason the CMake build that preceded this passed -O3 for
// every variant.
// ---------------------------------------------------------------------------
val rustDir = rootProject.file("rust")
val rustAbis = listOf("arm64-v8a", "armeabi-v7a", "x86_64")
val rustJniLibs = layout.buildDirectory.dir("rustJniLibs")

val buildRustEngine by tasks.registering(Exec::class) {
    group = "build"
    description = "Builds the Rust audio engine for every packaged ABI"
    workingDir = rustDir
    inputs.dir(rustDir.resolve("src"))
    inputs.file(rustDir.resolve("Cargo.toml"))
    inputs.file(rustDir.resolve("Cargo.lock"))
    outputs.dir(rustJniLibs)

    val outDir = rustJniLibs.get().asFile
    commandLine(buildList {
        add("cargo")
        add("ndk")
        rustAbis.forEach { add("-t"); add(it) }
        // Matches minSdk below; cargo-ndk picks the toolchain from it.
        add("--platform"); add("26")
        add("-o"); add(outDir.absolutePath)
        add("build")
        add("--release")
        add("--locked")
    })

    doFirst {
        outDir.mkdirs()
        // cargo-ndk finds the NDK on its own from ANDROID_HOME, but saying so
        // explicitly gives a better error than a toolchain-not-found deep
        // inside a C++ compile.
        environment("ANDROID_NDK_HOME", android.ndkDirectory.absolutePath)
    }
}

tasks.matching { it.name == "preBuild" }.configureEach { dependsOn(buildRustEngine) }

android {
    namespace = "com.lanmic.audio"
    compileSdk = 35
    // Pinned so cargo-ndk and Gradle agree on one toolchain.
    ndkVersion = "27.0.12077973"

    defaultConfig {
        applicationId = "com.lanmic.audio"
        // AAudio arrived in 26; MMAP/EXCLUSIVE paths land in 27+. On anything
        // older Oboe falls back to OpenSL ES and latency roughly doubles.
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "1.0"

        ndk {
            abiFilters += rustAbis
        }
    }

    sourceSets["main"].jniLibs.srcDir(rustJniLibs)

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
        }
        debug {
            isJniDebuggable = true
        }
    }

    buildFeatures {
        compose = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
        freeCompilerArgs += listOf(
            "-opt-in=androidx.compose.material3.ExperimentalMaterial3Api"
        )
    }
    packaging {
        resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
    }
}

dependencies {
    // Oboe is not here any more: the Rust `oboe` crate vendors it and links it
    // into liblanmic.so, so there is nothing for Gradle to resolve.
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.8.7")
    implementation("androidx.lifecycle:lifecycle-service:2.8.7")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")

    val composeBom = platform("androidx.compose:compose-bom:2024.12.01")
    implementation(composeBom)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")
}
