plugins {
    id("com.android.application")
}

android {
    namespace = "com.owncam"
    compileSdk = 37

    defaultConfig {
        applicationId = "com.owncam"
        // Android 10 = API 29, hedef telefon burasi
        minSdk = 29
        targetSdk = 36
        versionCode = 3
        versionName = "0.1.2"
    }

    // Yayin imzasi depoda **degil**: anahtar deposu ve parolalari ortam
    // degiskeninden geliyor. Verilmezse yayin APK'si imzasiz cikar - bu
    // bilerek boyle, cunku sessizce hata ayiklama anahtariyla imzalamak
    // sonraki surumlerin ustune yuklenmesini kalici olarak bozardi.
    val ks = System.getenv("OWNCAM_KEYSTORE")
    signingConfigs {
        if (ks != null) {
            create("release") {
                storeFile = file(ks)
                storePassword = System.getenv("OWNCAM_KEYSTORE_PAROLA")
                keyAlias = System.getenv("OWNCAM_KEY_ALIAS") ?: "owncam"
                keyPassword = System.getenv("OWNCAM_KEY_PAROLA")
                    ?: System.getenv("OWNCAM_KEYSTORE_PAROLA")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            if (ks != null) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
        debug {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.appcompat:appcompat:1.7.0")
}
