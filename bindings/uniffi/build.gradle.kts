import java.util.*

plugins {
    kotlin("jvm") version "2.1.0"
    id("maven-publish")
    id("signing")
}

group = "io.gorules"
version = "0.1.0"

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
        maven {
            url = uri("https://s01.oss.sonatype.org/service/local/staging/deploy/maven2/")
            credentials {
                username = System.getenv("MAVEN_USERNAME")
                password = System.getenv("MAVEN_PASSWORD")
            }
        }
        mavenLocal()
    }
}

signing {
    val signingKeyBase64 = System.getenv("MAVEN_GPG_SIGNING_KEY")
    val signingPassword = System.getenv("MAVEN_GPG_SIGNING_PASSWORD")

    if (signingKeyBase64 != null && signingPassword != null) {
        val signingKey = Base64.getDecoder().decode(signingKeyBase64).toString(Charsets.UTF_8)
        useInMemoryPgpKeys(signingKey, signingPassword)
        sign(publishing.publications["mavenJava"])
    }
}