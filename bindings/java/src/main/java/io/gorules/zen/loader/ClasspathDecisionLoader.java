package io.gorules.zen.loader;

import io.gorules.zen_engine.JsonBuffer;

import java.io.IOException;
import java.io.InputStream;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Loads decision models from classpath resources.
 */
public class ClasspathDecisionLoader implements DecisionLoader {

    private final String rootPath;
    private final boolean useCache;
    private final ConcurrentHashMap<String, JsonBuffer> cache;

    /**
     * Create classpath loader with default caching enabled.
     *
     * @param rootPath Root classpath path
     */
    public ClasspathDecisionLoader(String rootPath) {
        this(rootPath, true);
    }

    /**
     * Create classpath loader with caching option.
     *
     * @param rootPath Root classpath path
     * @param useCache Whether to cache loaded decisions
     */
    public ClasspathDecisionLoader(String rootPath, boolean useCache) {
        // Normalize root path
        String normalized = rootPath;
        if (normalized.startsWith("/")) {
            normalized = normalized.substring(1);
        }
        if (!normalized.isEmpty() && !normalized.endsWith("/")) {
            normalized = normalized + "/";
        }

        this.rootPath = normalized;
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

                // Cache if enabled
                if (useCache) {
                    cache.put(key, buffer);
                }

                return buffer;

            } catch (IOException e) {
                throw new RuntimeException("Failed to load decision from classpath: " + key, e);
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
