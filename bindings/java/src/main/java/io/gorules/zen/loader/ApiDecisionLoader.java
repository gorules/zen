package io.gorules.zen.loader;

import com.github.benmanes.caffeine.cache.Cache;
import com.github.benmanes.caffeine.cache.Caffeine;
import com.github.benmanes.caffeine.cache.Weigher;
import io.gorules.zen_engine.JsonBuffer;
import lombok.Getter;

import java.net.URI;
import java.net.URLEncoder;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;

/**
 * Decision loader that fetches decisions from an HTTP API.
 * Supports flexible header configuration, caching, and automatic retries.
 */
@Getter
public class ApiDecisionLoader implements DecisionLoader {

    private final HttpClient httpClient;
    private final ApiLoaderConfig config;
    private final Cache<String, JsonBuffer> cache;

    /**
     * Create a new ApiDecisionLoader with the given configuration.
     *
     * @param config API loader configuration
     */
    public ApiDecisionLoader(ApiLoaderConfig config) {
        this.config = config;
        this.httpClient = HttpClient.newBuilder()
            .connectTimeout(config.getTimeout())
            .build();

        // Build Caffeine cache based on configuration
        if (config.isEnableCaching()) {
            this.cache = buildCache(config);
        } else {
            this.cache = null;
        }
    }

    /**
     * Build Caffeine cache with configured policies.
     */
    private Cache<String, JsonBuffer> buildCache(ApiLoaderConfig config) {
        Caffeine<Object, Object> builder = Caffeine.newBuilder()
            .expireAfterWrite(config.getCacheTtl());

        // Configure eviction policy
        switch (config.getCacheEvictionPolicy()) {
            case LRU:
                // LRU is the default behavior with maximumSize
                builder.maximumSize(config.getCacheMaxSize());
                break;

            case LFU:
                // LFU requires frequency tracking
                builder.maximumSize(config.getCacheMaxSize());
                // Caffeine uses W-TinyLFU by default which is better than pure LFU
                break;

            case SIZE_BASED:
                // Memory-based eviction using weigher
                long maxBytes = config.getCacheMaxMemoryMb() * 1024 * 1024;
                builder.maximumWeight(maxBytes)
                    .weigher((Weigher<String, JsonBuffer>) (key, value) -> {
                        // Calculate approximate memory size
                        int keySize = Math.min(Integer.MAX_VALUE / 2, key.length() * 2); // Java chars are 2 bytes
                        int valueSize = value == null || value.value() == null ? 0 : value.value().length; // JsonBuffer is a record with value() accessor
                        long size = (long) keySize + (long) valueSize + 64L; // Add overhead for object headers
                        return (int) Math.min(Integer.MAX_VALUE, size);
                    });
                break;
        }

        return builder.build();
    }

    @Override
    public CompletableFuture<JsonBuffer> load(String key) {
        // Check cache first if caching is enabled
        if (cache != null) {
            JsonBuffer cached = cache.getIfPresent(key);
            if (cached != null) {
                return CompletableFuture.completedFuture(cached);
            }
        }

        return loadFromApi(key, 0);
    }

    /**
     * Load decision from API with retry logic.
     *
     * @param key     Decision key
     * @param attempt Current attempt number (0-based)
     * @return CompletableFuture with JsonBuffer
     */
    private CompletableFuture<JsonBuffer> loadFromApi(String key, int attempt) {
        try {
            // Build URL - encode the key to handle special characters
            String encodedKey = URLEncoder.encode(key, StandardCharsets.UTF_8);
            String url = config.getBaseUrl() + "/" + encodedKey;

            // Build HTTP request
            HttpRequest.Builder requestBuilder = HttpRequest.newBuilder()
                .uri(URI.create(url))
                .timeout(config.getTimeout())
                .GET();

            // Add all headers (static + dynamic)
            Map<String, String> headers = config.getAllHeaders();
            headers.forEach(requestBuilder::header);

            HttpRequest request = requestBuilder.build();

            // Send request
            return httpClient.sendAsync(request, HttpResponse.BodyHandlers.ofByteArray())
            .handle((response, ex) -> {
                if (ex != null) {
                    return handleException(ex, key, attempt);
                } else {
                    return handleResponse(response, key, attempt);
                }
            })
            .thenCompose(future -> future);
        

        } catch (Exception e) {
            return CompletableFuture.failedFuture(
                new ApiLoaderException("Failed to build request for key: " + key, e)
            );
        }
    }

    /**
     * Handle HTTP response.
     *
     * @param response HTTP response
     * @param key      Decision key
     * @param attempt  Current attempt number
     * @return CompletableFuture with JsonBuffer
     */
    private CompletableFuture<JsonBuffer> handleResponse(
            HttpResponse<byte[]> response,
            String key,
            int attempt) {

        int statusCode = response.statusCode();

        if (statusCode == 200) {
            // Success - cache and return
            JsonBuffer buffer = new JsonBuffer(response.body());
            if (cache != null) {
                cache.put(key, buffer);
            }
            return CompletableFuture.completedFuture(buffer);

        } else if (statusCode >= 500 && statusCode < 600 && attempt < config.getMaxRetries()) {
            // Server error - retry
            return retryWithBackoff(key, attempt);

        } else if (statusCode == 404) {
            // Not found - don't retry
            return CompletableFuture.failedFuture(
                new ApiLoaderException("Decision not found: " + key + " (HTTP 404)")
            );

        }  else if (statusCode == 401 || statusCode == 403 || statusCode == 400) {
            // Authentication/authorization error or bad request - don't retry
            return CompletableFuture.failedFuture(
                new ApiLoaderException(
                    "Authentication failed for key: " + key + " (HTTP " + statusCode + ")"
                )
            );

        } else {
            // Other error
            String body = new String(response.body(), StandardCharsets.UTF_8);
            return CompletableFuture.failedFuture(
                new ApiLoaderException(
                    "Failed to load decision: " + key +
                    " (HTTP " + statusCode + "): " + body
                )
            );
        }
    }

   
    private CompletableFuture<JsonBuffer> handleException(Throwable ex, String key, int attempt) {
        if (attempt < config.getMaxRetries()) {
            return retryWithBackoff(key, attempt);
        }
        Throwable cause = (ex instanceof java.util.concurrent.CompletionException && ex.getCause() != null) ? ex.getCause() : ex;
        return CompletableFuture.failedFuture(
            new ApiLoaderException("Failed to load decision: " + key, cause)
        );
    }
    


    /**
     * Retry loading with exponential backoff.
     *
     * @param key     Decision key
     * @param attempt Current attempt number
     * @return CompletableFuture with retry
     */
    private CompletableFuture<JsonBuffer> retryWithBackoff(String key, int attempt) {
        long baseDelayMs = Math.max(1L, config.getRetryDelay().toMillis());
        long delayMs = baseDelayMs * (1L << Math.min(attempt, 30)); 

        return CompletableFuture
            .supplyAsync(() -> null, CompletableFuture.delayedExecutor(delayMs, TimeUnit.MILLISECONDS))
            .thenCompose(v -> loadFromApi(key, attempt + 1));
    }
}
