import org.jreleaser.model.Active
import java.util.*

group = "io.gorules"
version = "0.1.0"

plugins {
    kotlin("jvm") version "2.1.0"
    id("maven-publish")
    id("org.jreleaser") version "1.16.0"
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

jreleaser {
    signing {
        active = Active.ALWAYS
        armored = true

        val signingKeyBase64 = providers.environmentVariable("GPG_SIGNING_KEY")
        val signingPassphrase = providers.environmentVariable("GPG_SIGNING_PASSPHRASE")

        if (signingKeyBase64.isPresent && signingPassphrase.isPresent) {
            val signingKey = Base64.getDecoder().decode(signingKeyBase64.get()).toString(Charsets.UTF_8)

            secretKey.set(signingKey)
            passphrase.set(signingPassphrase.get())
        }
    }
    deploy {
        maven {
            mavenCentral {
                create("sonatype") {
                    active = Active.ALWAYS
                    url = "https://central.sonatype.com/api/v1/publisher"
                    stagingRepository("target/staging-deploy")

                    val remoteUsername = providers.environmentVariable("OSSRH_USERNAME")
                    val remotePassword = providers.environmentVariable("OSSRH_PASSWORD")
                    if (remoteUsername.isPresent && remotePassword.isPresent) {
                        username.set(remoteUsername.get())
                        password.set(remotePassword.get())
                    }
                }
            }
        }
    }
}