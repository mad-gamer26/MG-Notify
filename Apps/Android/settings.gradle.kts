// MG Notify for Android: a native app (Kotlin, Jetpack Compose) that mirrors
// the iOS app. Build with: gradlew assembleDebug
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "MG Notify"
include(":app")
