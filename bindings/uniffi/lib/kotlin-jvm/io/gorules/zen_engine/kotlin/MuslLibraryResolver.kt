package io.gorules.zen_engine.kotlin

import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.nio.file.StandardCopyOption

internal object MuslLibraryResolver {
    private const val FILE_NAME = "libzen_uniffi.so"

    val libraryPath: String? by lazy { resolve() }

    private fun resolve(): String? {
        val os = System.getProperty("os.name").lowercase()
        if (!os.contains("linux") || !isMusl()) {
            return null
        }

        val arch = System.getProperty("os.arch").lowercase()
        val platform = if (arch.contains("aarch64")) "linux-musl-aarch64" else "linux-musl-x86-64"
        val resource = "/$platform/$FILE_NAME"
        val input = MuslLibraryResolver::class.java.getResourceAsStream(resource)
            ?: throw UnsatisfiedLinkError("zen_uniffi native library not found on classpath: $resource")

        return try {
            input.use {
                val dir = Files.createTempDirectory("zen_uniffi")
                dir.toFile().deleteOnExit()
                val lib = dir.resolve(FILE_NAME)
                lib.toFile().deleteOnExit()
                Files.copy(it, lib, StandardCopyOption.REPLACE_EXISTING)
                lib.toAbsolutePath().toString()
            }
        } catch (e: IOException) {
            throw UnsatisfiedLinkError("failed to extract zen_uniffi native library: ${e.message}")
        }
    }

    private fun isMusl(): Boolean = try {
        File("/proc/self/maps").useLines { lines ->
            lines.any { it.contains("/ld-musl-") || it.contains("/libc.musl-") }
        }
    } catch (e: IOException) {
        false
    }
}
