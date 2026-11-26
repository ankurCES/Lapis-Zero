plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.ksp)
}

android {
    namespace = "dev.lapiszero.app"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.lapiszero.app"
        minSdk = 29
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    testOptions {
        unitTests {
            isReturnDefaultValues = true
            // Robolectric screen smoke tests need the merged manifest and resources.
            isIncludeAndroidResources = true
            all { test ->
                // Shared cross-language vectors live at the repository root.
                test.systemProperty(
                    "lapis.vectors",
                    rootProject.file("../tests/vectors/ble_crypto_vectors.json").absolutePath,
                )
                // Opt-in: the integration test against a running `lapis-daemon --mock`, the contact
                // sheets of the agent's head (`-Plapis.head.sheets=<directory>`) and of the launcher
                // icon (`-Plapis.icon.png=<file>`).
                listOf("lapis.integration.url", "lapis.integration.token", "lapis.head.sheets", "lapis.icon.png").forEach { key ->
                    providers.gradleProperty(key).orNull?.let { test.systemProperty(key, it) }
                }
            }
        }
    }

    lint {
        abortOnError = true
        checkDependencies = false
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.lifecycle.process)
    implementation(libs.androidx.navigation.compose)
    implementation(libs.androidx.browser)

    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.graphics)
    implementation(libs.compose.foundation)
    implementation(libs.compose.material3)
    implementation(libs.compose.material.icons.core)

    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.okhttp)

    implementation(libs.room.runtime)
    implementation(libs.room.ktx)
    ksp(libs.room.compiler)

    implementation(libs.tink.android)
    implementation(libs.nordic.ble.ktx)

    implementation(libs.camerax.core)
    implementation(libs.camerax.camera2)
    implementation(libs.camerax.lifecycle)
    implementation(libs.camerax.view)
    implementation(libs.zxing.core)

    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    // Compose screens are smoke-tested on the JVM (no device needed) with Robolectric.
    testImplementation(libs.robolectric)
    testImplementation(platform(libs.compose.bom))
    testImplementation(libs.compose.ui.test.junit4)
}
