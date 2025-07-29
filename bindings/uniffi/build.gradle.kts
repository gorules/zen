plugins {
    kotlin("jvm") version "2.1.0"
    id("maven-publish")
}

group = "io.gorules"
version = "0.39.0"

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
}

publishing {
    publications {
        create<MavenPublication>("mavenJava") {
            groupId = "io.gorules"
            artifactId = "zen-engine"
            artifact(tasks["generateJavaJar"])
            artifact(tasks["generateJavaSourcesJar"])
        }

        create<MavenPublication>("mavenKotlin") {
            groupId = "io.gorules"
            artifactId = "zen-engine-kotlin"
            artifact(tasks["generateKotlinJar"])
            artifact(tasks["generateKotlinSourcesJar"])
        }
    }
    repositories {
        mavenLocal()
    }
}