package io.gorules.zen.loader;

import io.gorules.zen_engine.JsonBuffer;
import lombok.Getter;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.concurrent.CompletableFuture;

/**
 * Loads decision models from the filesystem.
 */
@Getter
public class FilesystemDecisionLoader implements DecisionLoader {

    private final String rootPath;

    /**
     * Create filesystem loader.
     *
     * @param rootPath Root directory path
     */
    public FilesystemDecisionLoader(String rootPath) {
        this.rootPath = rootPath.endsWith("/") ? rootPath : rootPath + "/";
    }

    @Override
    public CompletableFuture<JsonBuffer> load(String key) {
        return CompletableFuture.supplyAsync(() -> {
            try {
                // Build file path
                Path filePath = Paths.get(rootPath + key);

                if (!Files.exists(filePath)) {
                    throw new RuntimeException("Decision file not found: " + filePath);
                }

                // Read file content
                byte[] content = Files.readAllBytes(filePath);
                JsonBuffer buffer = new JsonBuffer(content);

                return buffer;

            } catch (IOException e) {
                throw new RuntimeException("Failed to load decision: " + key, e);
            }
        });
    }


}
