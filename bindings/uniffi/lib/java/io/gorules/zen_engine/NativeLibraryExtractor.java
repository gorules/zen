package io.gorules.zen_engine;

import java.io.IOException;
import java.io.InputStream;
import java.io.UncheckedIOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.stream.Stream;

final class NativeLibraryExtractor {
    private NativeLibraryExtractor() {}

    static String extract() {
        String os = System.getProperty("os.name").toLowerCase();
        String arch = System.getProperty("os.arch").toLowerCase();

        String platform;
        String fileName;
        if (os.contains("mac") || os.contains("darwin")) {
            platform = arch.contains("aarch64") ? "darwin-aarch64" : "darwin-x86-64";
            fileName = "libzen_uniffi.dylib";
        } else if (os.contains("win")) {
            platform = "win32-x86-64";
            fileName = "zen_uniffi.dll";
        } else {
            String prefix = isMusl() ? "linux-musl-" : "linux-";
            if (arch.contains("aarch64")) {
                platform = prefix + "aarch64";
            } else if (arch.contains("s390x")) {
                platform = "linux-s390x";
            } else {
                platform = prefix + "x86-64";
            }
            fileName = "libzen_uniffi.so";
        }

        String resource = "/" + platform + "/" + fileName;
        try (InputStream in = NativeLibraryExtractor.class.getResourceAsStream(resource)) {
            if (in == null) {
                throw new UnsatisfiedLinkError("zen_uniffi native library not found on classpath: " + resource);
            }
            Path dir = Files.createTempDirectory("zen_uniffi");
            dir.toFile().deleteOnExit();
            Path lib = dir.resolve(fileName);
            lib.toFile().deleteOnExit();
            Files.copy(in, lib, StandardCopyOption.REPLACE_EXISTING);
            return lib.toAbsolutePath().toString();
        } catch (IOException e) {
            throw new UnsatisfiedLinkError("failed to extract zen_uniffi native library: " + e.getMessage());
        }
    }

    private static boolean isMusl() {
        try (Stream<String> lines = Files.lines(Path.of("/proc/self/maps"))) {
            return lines.anyMatch(line -> line.contains("/ld-musl-") || line.contains("/libc.musl-"));
        } catch (IOException | UncheckedIOException e) {
            return false;
        }
    }
}
