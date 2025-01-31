plugins {
    kotlin("jvm") version "2.1.0"
    id("maven-publish")
}

group = "io.gorules"
version = "0.39.0"

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
        }
    }
    repositories {
        mavenLocal()
    }
}