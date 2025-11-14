package io.gorules.zen.loader;

import io.gorules.zen_engine.JsonBuffer;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Loads decision models from the filesystem.
 */
public class FilesystemDecisionLoader implements DecisionLoader {

    private final String rootPath;
    private final boolean useCache;
    private final ConcurrentHashMap<String, JsonBuffer> cache;

    /**
     * Create filesystem loader with default caching enabled.
     *
     * @param rootPath Root directory path
     */
    public FilesystemDecisionLoader(String rootPath) {
        this(rootPath, true);
    }

    /**
     * Create filesystem loader with caching option.
     *
     * @param rootPath Root directory path
     * @param useCache Whether to cache loaded decisions
     */
    public FilesystemDecisionLoader(String rootPath, boolean useCache) {
        this.rootPath = rootPath.endsWith("/") ? rootPath : rootPath + "/";
        this.useCache = useCache;
        this.cache = useCache ? new ConcurrentHashMap<>() : null;
    }

    @Override
    public CompletableFuture<JsonBuffer> load(String key) {
        return CompletableFuture.supplyAsync(() -> {
            // Check cache first
            if (useCache && cache.containsKey(key)) {
                return cache.get(key);
            }

            try {
                // Build file path
                Path filePath = Paths.get(rootPath + key);

                if (!Files.exists(filePath)) {
                    throw new RuntimeException("Decision file not found: " + filePath);
                }

                // Read file content
                byte[] content = Files.readAllBytes(filePath);
                JsonBuffer buffer = new JsonBuffer(content);

                // Cache if enabled
                if (useCache) {
                    cache.put(key, buffer);
                }

                return buffer;

            } catch (IOException e) {
                throw new RuntimeException("Failed to load decision: " + key, e);
            }
        });
    }

    /**
     * Clear the cache.
     */
    public void clearCache() {
        if (cache != null) {
            cache.clear();
        }
    }

    /**
     * Evict a specific decision from cache.
     *
     * @param key Decision key to evict
     */
    public void evict(String key) {
        if (cache != null) {
            cache.remove(key);
        }
    }
}
