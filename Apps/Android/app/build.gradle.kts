import groovy.json.JsonSlurper
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

val appPackage = "com.matthewdovi.mgnotify"

// Push notifications come through Firebase Cloud Messaging (project
// mg-notify-bc607, the one the server sends with). Its Android client file,
// app/google-services.json, is read here instead of by the google-services
// plugin: the same string resources come out, and without the file the app
// still builds and runs, just without push.
@Suppress("UNCHECKED_CAST")
val firebaseResources: Map<String, String> = run {
    val file = file("google-services.json")
    if (!file.isFile) {
        logger.warn("MG Notify: app/google-services.json is missing; push notifications are disabled in this build.")
        return@run emptyMap()
    }
    val json = JsonSlurper().parse(file) as Map<String, Any?>
    val projectInfo = json["project_info"] as Map<String, Any?>
    val clients = json["client"] as List<Map<String, Any?>>
    val client = clients.firstOrNull {
        val info = (it["client_info"] as Map<String, Any?>)["android_client_info"] as Map<String, Any?>
        info["package_name"] == appPackage
    } ?: error("google-services.json has no Android client for $appPackage")
    val clientInfo = client["client_info"] as Map<String, Any?>
    val apiKey = ((client["api_key"] as List<Map<String, Any?>>).first())["current_key"] as String
    buildMap {
        put("google_app_id", clientInfo["mobilesdk_app_id"] as String)
        put("gcm_defaultSenderId", projectInfo["project_number"] as String)
        put("project_id", projectInfo["project_id"] as String)
        put("google_api_key", apiKey)
        put("google_crash_reporting_api_key", apiKey)
        (projectInfo["storage_bucket"] as String?)?.let { put("google_storage_bucket", it) }
    }
}

// Release signing: a keystore kept outside the repository, named in
// ~/.mgnotify-release/android-signing.properties (storeFile, storePassword,
// keyAlias, keyPassword). Without it, release builds are unsigned.
val signingFile = File(System.getProperty("user.home"), ".mgnotify-release/android-signing.properties")
val signing = Properties().apply { if (signingFile.isFile) signingFile.inputStream().use { load(it) } }

android {
    namespace = appPackage
    compileSdk = 37

    defaultConfig {
        applicationId = appPackage
        minSdk = 26
        targetSdk = 37
        // versionName matches the iOS app's MARKETING_VERSION. versionCode goes up
        // with every Android release: the updater offers only a higher one.
        versionCode = 2
        versionName = "2.0"
        buildConfigField("String", "SERVER_URL", "\"${project.property("mgNotifyServer")}\"")
        buildConfigField("String", "PORTAL_URL", "\"https://notify.mad-gamer.com\"")
        // Where release builds look for updates of themselves.
        buildConfigField("String", "UPDATES_URL", "\"https://notify.mad-gamer.com/downloads/android.json\"")
        firebaseResources.forEach { (name, value) -> resValue("string", name, value) }
    }

    signingConfigs {
        if (signing.getProperty("storeFile") != null) {
            create("release") {
                storeFile = File(signingFile.parentFile, signing.getProperty("storeFile"))
                storePassword = signing.getProperty("storePassword")
                keyAlias = signing.getProperty("keyAlias")
                keyPassword = signing.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        debug {
            // A debug build can't update itself to a release (different signing key).
            buildConfigField("String", "UPDATES_URL", "\"\"")
        }
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    lint {
        // Lint's analyzer can't follow MainActivity's and PushService's
        // superclasses (ComponentActivity, FirebaseMessagingService) with
        // this Kotlin version and reports them as not instantiable; they are.
        disable += "Instantiatable"
    }
    buildFeatures {
        compose = true
        buildConfig = true
        resValues = true
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(composeBom)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-core")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.core:core-ktx:1.19.1")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    implementation(platform("com.google.firebase:firebase-bom:34.4.0"))
    implementation("com.google.firebase:firebase-messaging")
    // Firebase brings an old Fragment; 1.3+ is needed for permission results to work right.
    implementation("androidx.fragment:fragment:1.9.1")
}
