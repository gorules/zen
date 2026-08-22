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
    id("com.gradleup.nmcp") version "1.4.4"
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

    val kotlinAndroid by creating {
        kotlin {
            srcDirs("lib/kotlin", "build/generated/kotlin_android")
        }

        compileClasspath += sourceSets["main"].compileClasspath
        runtimeClasspath += sourceSets["main"].runtimeClasspath
    }
}


dependencies {
    "kotlinImplementation"("net.java.dev.jna:jna:5.17.0")
    "kotlinImplementation"("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")
    "kotlinAndroidImplementation"("net.java.dev.jna:jna:5.17.0")
    "kotlinAndroidImplementation"("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")
}

tasks.withType<JavaCompile>().configureEach {
    options.release = 21
}

tasks.named<JavaCompile>("compileJavaJava") {
    options.release = 22
}

tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile>().configureEach {
    compilerOptions.jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_21)
}


tasks {
    val patchJavaNativeLoader by creating {
        doLast {
            val namespaceLibrary = file("build/generated/java/io/gorules/zen_engine/NamespaceLibrary.java")
            if (namespaceLibrary.exists() && !namespaceLibrary.readText().contains("NativeLibraryExtractor")) {
                namespaceLibrary.writeText(
                    namespaceLibrary.readText().replace(
                        "System.loadLibrary(name);",
                        "try {\n" +
                            "                System.loadLibrary(name);\n" +
                            "            } catch (UnsatisfiedLinkError e) {\n" +
                            "                System.load(NativeLibraryExtractor.extract());\n" +
                            "            }",
                    ),
                )
            }
        }
    }

    named(sourceSets["java"].compileJavaTaskName) {
        dependsOn(patchJavaNativeLoader)
    }

    val generateJavaJar by creating(Jar::class) {
        archiveBaseName.set("zen_engine")
        from(sourceSets["java"].output)
        dependsOn(sourceSets["java"].classesTaskName)
    }

    val generateJavaSourcesJar by creating(Jar::class) {
        archiveBaseName.set("zen_engine")
        archiveClassifier.set("sources")
        from(sourceSets["java"].allJava)
        dependsOn(patchJavaNativeLoader)
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

    val generateKotlinAndroidSourcesJar by creating(Jar::class) {
        archiveBaseName.set("zen_engine_kotlin_android")
        archiveClassifier.set("sources")
        from(sourceSets["kotlinAndroid"].kotlin)
    }

    val dokkaJavadocJava by creating(DokkaTask::class) {
        outputDirectory.set(layout.buildDirectory.dir("dokka/java"))
        dokkaSourceSets { named("java") }

    }

    val dokkaJavadocKotlin by creating(DokkaTask::class) {
        outputDirectory.set(layout.buildDirectory.dir("dokka/kotlin"))
        dokkaSourceSets { named("kotlin") }
    }

    val dokkaJavadocKotlinAndroid by creating(DokkaTask::class) {
        outputDirectory.set(layout.buildDirectory.dir("dokka/kotlin_android"))
        dokkaSourceSets { named("kotlinAndroid") }
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

    val javadocJarKotlinAndroid by creating(Jar::class) {
        dependsOn(dokkaGeneratePublicationJavadoc)
        archiveBaseName.set("zen_engine_kotlin_android")
        archiveClassifier.set("javadoc")

        from(dokkaGeneratePublicationJavadoc.get())
    }
}

// Use -PpublishScope=java|kotlin|desktop|android|all to control which publications are included
// desktop = java + kotlin
val publishScope = (findProperty("publishScope") as String?) ?: "all"

publishing {
    publications {
        if (publishScope in listOf("all", "desktop", "java")) {
            create<MavenPublication>("mavenJava") {
                groupId = "io.gorules"
                artifactId = "zen-engine"
                artifact(tasks["generateJavaJar"])
                artifact(tasks["generateJavaSourcesJar"])
                artifact(tasks["javadocJarJava"])

                configurePom(
                    pomName = "GoRules ZEN Engine for Java",
                    pomDescription = "Open-source Business Rules Engine (BRE) for Java, powered by a native Rust core. " +
                        "Evaluates JSON Decision Models (decision tables, graphs and policies) in microseconds. " +
                        "Part of the GoRules platform (https://gorules.io). " +
                        "Documentation: https://docs.gorules.io/developers/sdks/java",
                ) {
                }
            }
        }

        if (publishScope in listOf("all", "desktop", "kotlin")) {
            create<MavenPublication>("mavenKotlin") {
                groupId = "io.gorules"
                artifactId = "zen-engine-kotlin"
                artifact(tasks["generateKotlinJar"])
                artifact(tasks["generateKotlinSourcesJar"])
                artifact(tasks["javadocJarKotlin"])

                configurePom(
                    pomName = "GoRules ZEN Engine for Kotlin",
                    pomDescription = "Open-source Business Rules Engine (BRE) for Kotlin, powered by a native Rust core. " +
                        "Evaluates JSON Decision Models (decision tables, graphs and policies) in microseconds. " +
                        "Part of the GoRules platform (https://gorules.io). " +
                        "Documentation: https://docs.gorules.io/developers/sdks/kotlin",
                ) {
                    dependency("net.java.dev.jna:jna:5.17.0")
                }
            }
        }

        if (publishScope in listOf("all", "android")) {
            create<MavenPublication>("mavenKotlinAndroid") {
                groupId = "io.gorules"
                artifactId = "zen-engine-kotlin-android"

                artifact("build-android/aar/zen-engine-android-release.aar") {
                    extension = "aar"
                }
                artifact(tasks["generateKotlinAndroidSourcesJar"])
                artifact(tasks["javadocJarKotlinAndroid"])

                configurePom(
                    pomName = "GoRules ZEN Engine for Android",
                    pomDescription = "Open-source Business Rules Engine (BRE) for Android (AAR), powered by a native Rust core. " +
                        "Evaluates JSON Decision Models (decision tables, graphs and policies) on-device, offline-capable. " +
                        "Part of the GoRules platform (https://gorules.io). " +
                        "Documentation: https://docs.gorules.io/developers/sdks/android",
                ) {
                    dependency("net.java.dev.jna:jna:5.14.0", "aar")
                    dependency("androidx.core:core-ktx:1.12.0")
                    dependency("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")
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
        sign(*publishing.publications.toTypedArray())
    }
}

nmcp {
    publishAllPublicationsToCentralPortal {
        publishingType = "USER_MANAGED"

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

fun MavenPublication.configurePom(
    pomName: String = "GoRules ZEN Engine",
    pomDescription: String = "GoRules ZEN Engine is a cross-platform, Open-Source Business Rules Engine (BRE)",
    dependencyConfig: PomDependencyBuilder.() -> Unit,
) {
    val depBuilder = PomDependencyBuilder()
    depBuilder.dependencyConfig()

    pom {
        name = pomName
        description = pomDescription
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
    private val dependencies = mutableListOf<Map<String, String>>()

    fun dependency(notation: String, type: String = "jar") {
        val parts = notation.split(":")
        require(parts.size == 3) { "Dependency notation must be 'group:artifact:version'" }
        dependencies.add(mapOf(
            "groupId" to parts[0],
            "artifactId" to parts[1],
            "version" to parts[2],
            "type" to type
        ))
    }

    fun addToXml(dependenciesNode: groovy.util.Node) {
        dependencies.forEach { dep ->
            val dependencyNode = dependenciesNode.appendNode("dependency")
            dependencyNode.appendNode("groupId", dep["groupId"])
            dependencyNode.appendNode("artifactId", dep["artifactId"])
            dependencyNode.appendNode("version", dep["version"])
            if (dep["type"] != "jar") {
                dependencyNode.appendNode("type", dep["type"])
            }
            dependencyNode.appendNode("scope", "runtime")
        }
    }
}