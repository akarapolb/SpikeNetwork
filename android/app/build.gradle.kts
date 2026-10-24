import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "network.spike.connect"
    compileSdk = 35

    defaultConfig {
        applicationId = "network.spike.connect"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.0.1"
        // The Rust library is built for these (see android/build-native.sh): the emulator and phones.
        ndk { abiFilters += listOf("x86_64", "arm64-v8a") }
    }

    // Signing: keep the keystore and its passwords out of the repository. Put them in android/keystore.properties
    // (git-ignored) or in the environment (CI): storeFile, storePassword, keyAlias, keyPassword.
    // Without them `assembleRelease` still builds, but the APK is unsigned and cannot be installed.
    val keystoreProps = Properties().apply {
        val f = rootProject.file("keystore.properties")
        if (f.exists()) f.inputStream().use { load(it) }
    }
    fun signing(name: String): String? = keystoreProps.getProperty(name) ?: System.getenv("SPIKE_" + name.uppercase())
    val releaseSigning = if (signing("storeFile") != null) {
        signingConfigs.create("release") {
            storeFile = file(signing("storeFile")!!)
            storePassword = signing("storePassword")
            keyAlias = signing("keyAlias")
            keyPassword = signing("keyPassword")
            // v2+ only: the v1 (JAR) scheme is weaker and every minSdk 26 phone supports v2.
            enableV1Signing = false
            enableV2Signing = true
            enableV3Signing = true
        }
    } else null

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            isDebuggable = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            if (releaseSigning != null) signingConfig = releaseSigning
        }
    }
    // No dependency-info block in the APK: it is signed with our key, and a reproducible build must not depend on it.
    dependenciesInfo {
        includeInApk = false
        includeInBundle = false
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
    buildFeatures {
        compose = true
        buildConfig = true
    }
    sourceSets["main"].jniLibs.srcDir("src/main/jniLibs")
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2024.10.01"))
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-core")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.7")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.7")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    // A current Fragment: the one the biometric library pulls in is too old for permission requests from Compose.
    implementation("androidx.fragment:fragment-ktx:1.8.5")
    // Unlock with fingerprint or face.
    implementation("androidx.biometric:biometric:1.1.0")
    // Scanning contact QR codes: the camera, and a plain Java decoder (no Google services, no network).
    implementation("androidx.camera:camera-core:1.3.4")
    implementation("androidx.camera:camera-camera2:1.3.4")
    implementation("androidx.camera:camera-lifecycle:1.3.4")
    implementation("androidx.camera:camera-view:1.3.4")
    implementation("com.google.zxing:core:3.5.3")
}
