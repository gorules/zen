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
    implementation("net.java.dev.jna:jna:5.15.0")
    "kotlinImplementation"("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
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

            configurePom(configurations["implementation"])
        }

        create<MavenPublication>("mavenKotlin") {
            groupId = "io.gorules"
            artifactId = "zen-engine-kotlin"
            artifact(tasks["generateKotlinJar"])
            artifact(tasks["generateKotlinSourcesJar"])
            artifact(tasks["javadocJarKotlin"])

            configurePom(configurations["implementation"])
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

fun MavenPublication.configurePom(configuration: Configuration) {
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

            configuration.allDependencies.forEach { dep ->
                val dependencyNode = dependenciesNode.appendNode("dependency")
                dependencyNode.appendNode("groupId", dep.group)
                dependencyNode.appendNode("artifactId", dep.name)
                dependencyNode.appendNode("version", dep.version)
                dependencyNode.appendNode("scope", "runtime")
            }
        }
    }
}