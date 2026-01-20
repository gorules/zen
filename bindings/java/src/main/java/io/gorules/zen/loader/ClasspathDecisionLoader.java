package io.gorules.zen.loader;

import io.gorules.zen_engine.JsonBuffer;
import lombok.Getter;

import java.io.IOException;
import java.io.InputStream;
import java.util.concurrent.CompletableFuture;

/**
 * Loads decision models from classpath resources.
 */
@Getter
public class ClasspathDecisionLoader implements DecisionLoader {

    private final String rootPath;

    /**
     * Create classpath loader.
     *
     * @param rootPath Root classpath path
     */
    public ClasspathDecisionLoader(String rootPath) {
        // Normalize root path
        String normalized = rootPath;
        if (normalized.startsWith("/")) {
            normalized = normalized.substring(1);
        }
        if (!normalized.isEmpty() && !normalized.endsWith("/")) {
            normalized = normalized + "/";
        }

        this.rootPath = normalized;
    }

    @Override
    public CompletableFuture<JsonBuffer> load(String key) {
        return CompletableFuture.supplyAsync(() -> {
            try {
                // Build resource path
                String resourcePath = rootPath + key;

                // Load from classpath
                InputStream is = getClass().getClassLoader().getResourceAsStream(resourcePath);
                if (is == null) {
                    throw new RuntimeException("Decision not found in classpath: " + resourcePath);
                }

                // Read content
                byte[] content = is.readAllBytes();
                is.close();

                JsonBuffer buffer = new JsonBuffer(content);

                return buffer;

            } catch (IOException e) {
                throw new RuntimeException("Failed to load decision from classpath: " + key, e);
            }
        });
    }


}
