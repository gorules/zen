package io.gorules.zen.loader;

import io.gorules.zen_engine.JsonBuffer;

import java.net.URI;
import java.net.URLEncoder;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Decision loader that fetches decisions from an HTTP API.
 * Supports flexible header configuration, caching, and automatic retries.
 *
 * <pre>{@code
 * ApiLoaderConfig config = ApiLoaderConfig.builder("https://api.example.com/decisions")
 *     .header("Authorization", "Bearer token123")
 *     .header("X-Custom-Header", "value")
 *     .timeout(Duration.ofSeconds(30))
 *     .caching(true)
 *     .build();
 *
 * ApiDecisionLoader loader = new ApiDecisionLoader(config);
 *
 * // Load decision from: GET https://api.example.com/decisions/pricing.json
 * CompletableFuture<JsonBuffer> decision = loader.load("pricing.json");
 * }</pre>
 */
public class ApiDecisionLoader implements DecisionLoader {

    private final HttpClient httpClient;
    private final ApiLoaderConfig config;
    private final ConcurrentHashMap<String, JsonBuffer> cache;

    /**
     * Create a new ApiDecisionLoader with the given configuration.
     *
     * @param config API loader configuration
     */
    public ApiDecisionLoader(ApiLoaderConfig config) {
        this.config = config;
        this.cache = config.isEnableCaching() ? new ConcurrentHashMap<>() : null;
        this.httpClient = HttpClient.newBuilder()
            .connectTimeout(config.getTimeout())
            .build();
    }

    @Override
    public CompletableFuture<JsonBuffer> load(String key) {
        // Check cache first if caching is enabled
        if (cache != null && cache.containsKey(key)) {
            return CompletableFuture.completedFuture(cache.get(key));
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
                .thenCompose(response -> handleResponse(response, key, attempt))
                .exceptionally(ex -> handleException(ex, key, attempt));

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

        } else if (statusCode == 401 || statusCode == 403) {
            // Authentication/authorization error - don't retry
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

    /**
     * Handle exceptions during HTTP request.
     *
     * @param ex      Exception
     * @param key     Decision key
     * @param attempt Current attempt number
     * @return JsonBuffer or throws exception
     */
    private JsonBuffer handleException(Throwable ex, String key, int attempt) {
        if (attempt < config.getMaxRetries()) {
            // Retry on network errors
            try {
                return retryWithBackoff(key, attempt).join();
            } catch (Exception retryEx) {
                throw new ApiLoaderException(
                    "Failed to load decision after " + (attempt + 1) + " attempts: " + key,
                    retryEx
                );
            }
        }

        throw new ApiLoaderException(
            "Failed to load decision: " + key,
            ex
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
        long delayMs = config.getRetryDelay().toMillis() * (1L << attempt); // Exponential backoff

        return CompletableFuture.supplyAsync(() -> {
            try {
                Thread.sleep(delayMs);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new ApiLoaderException("Retry interrupted for key: " + key, e);
            }
            return null;
        }).thenCompose(v -> loadFromApi(key, attempt + 1));
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
     * @param key Decision key
     */
    public void evict(String key) {
        if (cache != null) {
            cache.remove(key);
        }
    }

    /**
     * Check if a decision is cached.
     *
     * @param key Decision key
     * @return true if cached
     */
    public boolean isCached(String key) {
        return cache != null && cache.containsKey(key);
    }

    /**
     * Get the configuration for this loader.
     *
     * @return ApiLoaderConfig
     */
    public ApiLoaderConfig getConfig() {
        return config;
    }
}
