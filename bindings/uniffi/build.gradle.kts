import org.jetbrains.dokka.gradle.DokkaTask
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
    id("org.jetbrains.dokka") version "2.0.0"
    id("org.jetbrains.dokka-javadoc") version "2.0.0"
    id("com.gradleup.nmcp") version "0.0.9"
}

repositories {
    mavenCentral()
}

sourceSets {
    val java by creating {
        java {
            srcDirs("lib/java", "build/generated/java")
        }
        resources {
            srcDirs("build/generated/resources")
        }

        compileClasspath += sourceSets["main"].compileClasspath
        runtimeClasspath += sourceSets["main"].runtimeClasspath
    }

    val kotlin by creating {
        kotlin {
            srcDirs("lib/kotlin", "build/generated/kotlin")
        }
        resources {
            srcDirs("build/generated/resources")
        }

        compileClasspath += sourceSets["main"].compileClasspath
        runtimeClasspath += sourceSets["main"].runtimeClasspath
    }
}


dependencies {
    implementation("net.java.dev.jna:jna:5.17.0")
    "kotlinImplementation"("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")
}


tasks {
    val generateJavaJar by creating(Jar::class) {
        archiveBaseName.set("zen_engine")
        from(sourceSets["java"].output)
        dependsOn(sourceSets["java"].classesTaskName)
    }

    val generateJavaSourcesJar by creating(Jar::class) {
        archiveBaseName.set("zen_engine")
        archiveClassifier.set("sources")
        from(sourceSets["java"].allJava)
    }

    val generateKotlinJar by creating(Jar::class) {
        archiveBaseName.set("zen_engine_kotlin")
        from(sourceSets["kotlin"].output)
        dependsOn(sourceSets["kotlin"].classesTaskName)
    }

    val generateKotlinSourcesJar by creating(Jar::class) {
        archiveBaseName.set("zen_engine_kotlin")
        archiveClassifier.set("sources")
        from(sourceSets["kotlin"].kotlin)
    }

    val dokkaJavadocJava by creating(DokkaTask::class) {
        outputDirectory.set(layout.buildDirectory.dir("dokka/java"))
        dokkaSourceSets { named("java") }

    }

    val dokkaJavadocKotlin by creating(DokkaTask::class) {
        outputDirectory.set(layout.buildDirectory.dir("dokka/kotlin"))
        dokkaSourceSets { named("kotlin") }
    }

    val javadocJarJava by creating(Jar::class) {
        dependsOn(dokkaGeneratePublicationJavadoc)
        archiveBaseName.set("zen_engine")
        archiveClassifier.set("javadoc")

        from(dokkaGeneratePublicationJavadoc.get())
    }

    val javadocJarKotlin by creating(Jar::class) {
        dependsOn(dokkaGeneratePublicationJavadoc)
        archiveBaseName.set("zen_engine_kotlin")
        archiveClassifier.set("javadoc")

        from(dokkaGeneratePublicationJavadoc.get())
    }
}

publishing {
    publications {
        create<MavenPublication>("mavenJava") {
            groupId = "io.gorules"
            artifactId = "zen-engine"
            artifact(tasks["generateJavaJar"])
            artifact(tasks["generateJavaSourcesJar"])
            artifact(tasks["javadocJarJava"])

            configurePom {
                dependency("net.java.dev.jna:jna:5.17.0")
            }
        }

        create<MavenPublication>("mavenKotlin") {
            groupId = "io.gorules"
            artifactId = "zen-engine-kotlin"
            artifact(tasks["generateKotlinJar"])
            artifact(tasks["generateKotlinSourcesJar"])
            artifact(tasks["javadocJarKotlin"])

            configurePom {
                dependency("net.java.dev.jna:jna:5.17.0")
            }
        }
    }
    repositories {
        maven {
            val releasesRepoUrl = uri("https://nexus.infra.dreamplug.net/repository/maven-releases/")
            val snapshotsRepoUrl = uri("https://nexus.infra.dreamplug.net/repository/maven-snapshots/")
            val isSnapshot = version.toString().endsWith("-SNAPSHOT")
            url = if (isSnapshot) snapshotsRepoUrl else releasesRepoUrl

            credentials {
                username = findProperty("nexusUsername")?.toString() ?: System.getenv("DP_NEXUS_USER")
                password = findProperty("nexusPassword")?.toString() ?: System.getenv("DP_NEXUS_PASS")
            }
        }
    }
}

signing {
    val signingKeyBase64 = providers.environmentVariable("GPG_SIGNING_KEY")
    val signingPassphrase = providers.environmentVariable("GPG_SIGNING_PASSPHRASE")

    if (signingKeyBase64.isPresent and signingPassphrase.isPresent) {
        val signingKey = Base64.getDecoder().decode(signingKeyBase64.get()).toString(Charsets.UTF_8)

        useInMemoryPgpKeys(signingKey, signingPassphrase.get())
        sign(publishing.publications["mavenJava"], publishing.publications["mavenKotlin"])
    }
}

nmcp {
    publishAllPublications {
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

fun MavenPublication.configurePom(dependencyConfig: PomDependencyBuilder.() -> Unit) {
    val depBuilder = PomDependencyBuilder()
    depBuilder.dependencyConfig()

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

        withXml {
            val dependenciesNode = asNode().appendNode("dependencies")
            depBuilder.addToXml(dependenciesNode)
        }
    }
}

class PomDependencyBuilder {
    private val dependencies = mutableListOf<Triple<String, String, String>>()

    fun dependency(notation: String) {
        val parts = notation.split(":")
        require(parts.size == 3) { "Dependency notation must be 'group:artifact:version'" }
        dependencies.add(Triple(parts[0], parts[1], parts[2]))
    }

    fun addToXml(dependenciesNode: groovy.util.Node) {
        dependencies.forEach { (groupId, artifactId, version) ->
            val dependencyNode = dependenciesNode.appendNode("dependency")
            dependencyNode.appendNode("groupId", groupId)
            dependencyNode.appendNode("artifactId", artifactId)
            dependencyNode.appendNode("version", version)
            dependencyNode.appendNode("scope", "runtime")
        }
    }
}