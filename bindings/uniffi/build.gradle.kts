import java.util.*

group = "io.gorules"
version = "0.1.0"

plugins {
    kotlin("jvm") version "2.1.0"
    id("maven-publish")
    id("signing")
    id("io.github.gradle-nexus.publish-plugin") version "2.0.0"
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

signing {
    val signingKeyBase64 = providers.environmentVariable("GPG_SIGNING_KEY")
    val signingPassphrase = providers.environmentVariable("GPG_SIGNING_PASSPHRASE")

    if (signingKeyBase64.isPresent && signingPassphrase.isPresent) {
        val signingKey = Base64.getDecoder().decode(signingKeyBase64.get()).toString(Charsets.UTF_8)
        useInMemoryPgpKeys(signingKey, signingPassphrase.get())
        sign(publishing.publications["mavenJava"])
    }
}

nexusPublishing {
    repositories {
        sonatype {
            nexusUrl.set(uri("https://s01.oss.sonatype.org/service/local/"))
            snapshotRepositoryUrl.set(uri("https://s01.oss.sonatype.org/content/repositories/snapshots/"))

            val remoteUsername = providers.environmentVariable("OSSRH_USERNAME")
            val remotePassword = providers.environmentVariable("OSSRH_PASSWORD")
            if (remoteUsername.isPresent && remotePassword.isPresent) {
                username.set(remoteUsername.get())
                password.set(remotePassword.get())
            }
        }
    }
}