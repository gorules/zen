import org.tomlj.Toml
import java.util.*

group = "io.gorules"
version = loadCargoVersion()

buildscript {
    dependencies {
        classpath("org.tomlj:tomlj:1.1.1")
    }
}


plugins {
    kotlin("jvm") version "2.1.0"
    id("maven-publish")
    id("signing")
    id("com.gradleup.nmcp") version "0.0.9"
}

dependencies {
    implementation("net.java.dev.jna:jna:5.15.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}

repositories {
    mavenCentral()
}

sourceSets {
    main {
        kotlin {
            srcDirs("build/generated/main/kotlin")
        }
        resources {
            srcDirs("build/generated/main/resources")
        }
    }
}

publishing {
    publications {
        create<MavenPublication>("mavenJava") {
            from(components["java"])
            artifact(tasks["kotlinSourcesJar"])

            pom {
                name = "GoRules ZEN Engine"
                description = "GoRules ZEN Engine is a cross-platform, Open-Source Business Rules Engine (BRE)"
                url = "https://gorules.io"

                licenses {
                    license {
                        name = "MIT License"
                        url = "https://github.com/gorules/zen/blob/master/LICENSE"
                    }
                }

                developers {
                    developer {
                        id = "gorules"
                        name = "GoRules Team"
                        email = "hi@gorules.io"
                    }
                    organization {
                        name = "GoRules"
                        url = "https://gorules.io"
                    }
                }

                scm {
                    url = "https://github.com/gorules/zen"
                }
            }
        }
    }
    repositories {
        mavenLocal()
    }
}

signing {
    val signingKeyBase64 = providers.environmentVariable("GPG_SIGNING_KEY")
    val signingPassphrase = providers.environmentVariable("GPG_SIGNING_PASSPHRASE")

    if (signingKeyBase64.isPresent and signingPassphrase.isPresent) {
        val signingKey = Base64.getDecoder().decode(signingKeyBase64.get()).toString(Charsets.UTF_8)

        useInMemoryPgpKeys(signingKey, signingPassphrase.get())
        sign(publishing.publications["mavenJava"])
    }
}

nmcp {
    publish("mavenJava") {
        publicationType = "USER_MANAGED"

        val remoteUsername = providers.environmentVariable("SONATYPE_USERNAME")
        val remotePassword = providers.environmentVariable("SONATYPE_PASSWORD")

        if (remoteUsername.isPresent && remotePassword.isPresent) {
            username.set(remoteUsername.get())
            password.set(remotePassword.get())
        }
    }
}

fun loadCargoVersion(): String {
    val cargoFile = file("${projectDir}/Cargo.toml")
    val result = Toml.parse(cargoFile.toPath())
    return result.getTable("package")?.getString("version")
        ?: throw GradleException("Version not found in Cargo.toml")
}