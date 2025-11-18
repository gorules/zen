package io.gorules.zen.loader;

import lombok.Getter;

import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.*;

/**
 * Configuration for ApiDecisionLoader.
 * Supports flexible header configuration for any HTTP headers you need.
 */
@Getter
public class ApiLoaderConfig {

    private final String baseUrl;
    private final Map<String, String> staticHeaders;
    private final HeaderProvider dynamicHeaderProvider;
    private final Duration timeout;
    private final int maxRetries;
    private final Duration retryDelay;
    private final boolean enableCaching;

    // Cache configuration
    private final Duration cacheTtl;
    private final long cacheMaxSize;
    private final long cacheMaxMemoryMb;
    private final CacheEvictionPolicy cacheEvictionPolicy;

    private ApiLoaderConfig(Builder builder) {
        this.baseUrl = builder.baseUrl.endsWith("/")
            ? builder.baseUrl.substring(0, builder.baseUrl.length() - 1)
            : builder.baseUrl;
        this.staticHeaders = Collections.unmodifiableMap(new HashMap<>(builder.staticHeaders));
        this.dynamicHeaderProvider = builder.dynamicHeaderProvider;
        this.timeout = builder.timeout;
        this.maxRetries = builder.maxRetries;
        this.retryDelay = builder.retryDelay;
        this.enableCaching = builder.enableCaching;
        this.cacheTtl = builder.cacheTtl;
        this.cacheMaxSize = builder.cacheMaxSize;
        this.cacheMaxMemoryMb = builder.cacheMaxMemoryMb;
        this.cacheEvictionPolicy = builder.cacheEvictionPolicy;
    }

    /**
     * Create a new builder with the given base URL.
     *
     * @param baseUrl Base URL for API (e.g., "https://api.example.com/decisions")
     * @return Builder instance
     */
    public static Builder builder(String baseUrl) {
        return new Builder(baseUrl);
    }

    /**
     * Get all headers (static + dynamic) for a request.
     *
     * @return Combined headers map
     */
    public Map<String, String> getAllHeaders() {
        Map<String, String> allHeaders = new HashMap<>(staticHeaders);

        // Add dynamic headers if provider exists
        if (dynamicHeaderProvider != null) {
            Map<String, String> dynamicHeaders = dynamicHeaderProvider.getHeaders();
            if (dynamicHeaders != null) {
                allHeaders.putAll(dynamicHeaders);
            }
        }

        return allHeaders;
    }

    /**
     * Cache eviction policy for API loader.
     */
    public enum CacheEvictionPolicy {
        /**
         * Least Recently Used - evicts entries that haven't been accessed recently.
         */
        LRU,

        /**
         * Least Frequently Used - evicts entries that are accessed least often.
         */
        LFU,

        /**
         * Size-based - evicts based on memory size using LRU as tie-breaker.
         */
        SIZE_BASED
    }

    /**
     * Builder for ApiLoaderConfig with flexible header configuration.
     */
    public static class Builder {
        private static final Duration DEFAULT_TIMEOUT = Duration.ofSeconds(30);
        private static final int DEFAULT_MAX_RETRIES = 3;
        private static final Duration DEFAULT_RETRY_DELAY = Duration.ofSeconds(1);
        private static final boolean DEFAULT_CACHING = true;
        private static final Duration DEFAULT_CACHE_TTL = Duration.ofMinutes(5);
        private static final long DEFAULT_CACHE_MAX_SIZE = 10_000L;
        private static final long DEFAULT_CACHE_MAX_MEMORY_MB = 100L;
        private static final CacheEvictionPolicy DEFAULT_CACHE_EVICTION_POLICY =
                CacheEvictionPolicy.LRU;

        private final String baseUrl;
        private final Map<String, String> staticHeaders = new HashMap<>();
        private HeaderProvider dynamicHeaderProvider;
        private Duration timeout = DEFAULT_TIMEOUT;
        private int maxRetries = DEFAULT_MAX_RETRIES;
        private Duration retryDelay = DEFAULT_RETRY_DELAY;
        private boolean enableCaching = DEFAULT_CACHING;

        // Cache configuration defaults
        private Duration cacheTtl = DEFAULT_CACHE_TTL;
        private long cacheMaxSize = DEFAULT_CACHE_MAX_SIZE;
        private long cacheMaxMemoryMb = DEFAULT_CACHE_MAX_MEMORY_MB;
        private CacheEvictionPolicy cacheEvictionPolicy = DEFAULT_CACHE_EVICTION_POLICY;

        private Builder(String baseUrl) {
            Objects.requireNonNull(baseUrl, "baseUrl cannot be null");
            String trimmed = baseUrl.trim();
            if (trimmed.isEmpty()) {
                throw new IllegalArgumentException("baseUrl cannot be empty");
            }
            if (!trimmed.startsWith("http://") && !trimmed.startsWith("https://")) {
                throw new IllegalArgumentException("baseUrl must start with http:// or https://");
            }
            this.baseUrl = trimmed.endsWith("/") ? trimmed.substring(0, trimmed.length()-1) : trimmed;
        }
        

        /**
         * Add a single HTTP header.
         * Can be called multiple times to add multiple headers.
         *
         * @param name  Header name (e.g., "Authorization", "X-API-Key")
         * @param value Header value
         * @return this builder
         */
        public Builder header(String name, String value) {
            Objects.requireNonNull(name, "header name cannot be null");
            Objects.requireNonNull(value, "header value cannot be null");
            staticHeaders.put(name, value);
            return this;
        }

        /**
         * Add multiple headers at once from a Map.
         *
         * @param headers Map of header names to values
         * @return this builder
         */
        public Builder headers(Map<String, String> headers) {
            if (headers != null) {
                staticHeaders.putAll(headers);
            }
            return this;
        }

        /**
         * Add Bearer token authorization header.
         * Convenience method for: header("Authorization", "Bearer " + token)
         *
         * @param token Bearer token
         * @return this builder
         */
        public Builder bearerToken(String token) {
            Objects.requireNonNull(token, "token cannot be null");
            return header("Authorization", "Bearer " + token);
        }

        /**
         * Add Basic authentication header.
         * Convenience method for: header("Authorization", "Basic " + base64(username:password))
         *
         * @param username Username
         * @param password Password
         * @return this builder
         */
        public Builder basicAuth(String username, String password) {
            Objects.requireNonNull(username, "username cannot be null");
            Objects.requireNonNull(password, "password cannot be null");

            String credentials = username + ":" + password;
            String encoded = Base64.getEncoder()
                .encodeToString(credentials.getBytes(StandardCharsets.UTF_8));

            return header("Authorization", "Basic " + encoded);
        }

        /**
         * Set a provider for dynamic headers that are computed per request.
         * Useful for headers that change per request like timestamps, request IDs, etc.
         *
         * @param provider HeaderProvider instance
         * @return this builder
         */
        public Builder headerProvider(HeaderProvider provider) {
            this.dynamicHeaderProvider = provider;
            return this;
        }

        /**
         * Set request timeout.
         *
         * @param timeout Timeout duration (default: 30 seconds)
         * @return this builder
         */
        public Builder timeout(Duration timeout) {
            Objects.requireNonNull(timeout, "timeout cannot be null");
            this.timeout = timeout;
            return this;
        }

        /**
         * Set maximum number of retry attempts for failed requests.
         *
         * @param maxRetries Maximum retries (default: 3)
         * @return this builder
         */
        public Builder maxRetries(int maxRetries) {
            if (maxRetries < 0) {
                throw new IllegalArgumentException("maxRetries cannot be negative");
            }
            this.maxRetries = maxRetries;
            return this;
        }

        /**
         * Set delay between retry attempts.
         *
         * @param retryDelay Delay duration (default: 1 second)
         * @return this builder
         */
        public Builder retryDelay(Duration retryDelay) {
            Objects.requireNonNull(retryDelay, "retryDelay cannot be null");
            this.retryDelay = retryDelay;
            return this;
        }

        /**
         * Enable or disable caching of loaded decisions.
         *
         * @param enable true to enable caching (default: true)
         * @return this builder
         */
        public Builder caching(boolean enable) {
            this.enableCaching = enable;
            return this;
        }

        /**
         * Set cache TTL (time to live).
         * Entries will be automatically expired after this duration from write.
         *
         * @param ttl Cache TTL duration (default: 5 minutes)
         * @return this builder
         */
        public Builder cacheTtl(Duration ttl) {
            Objects.requireNonNull(ttl, "cacheTtl cannot be null");
            if (ttl.isNegative() || ttl.isZero()) {
                throw new IllegalArgumentException("cacheTtl must be positive");
            }
            this.cacheTtl = ttl;
            return this;
        }

        /**
         * Set maximum cache size (number of entries).
         *
         * @param maxSize Maximum number of entries (default: 10000)
         * @return this builder
         */
        public Builder cacheMaxSize(long maxSize) {
            if (maxSize <= 0) {
                throw new IllegalArgumentException("cacheMaxSize must be positive");
            }
            this.cacheMaxSize = maxSize;
            return this;
        }

        /**
         * Set maximum cache memory in MB.
         * Cache will evict entries when memory limit is reached.
         *
         * @param maxMemoryMb Maximum memory in MB (default: 100)
         * @return this builder
         */
        public Builder cacheMaxMemoryMb(long maxMemoryMb) {
            if (maxMemoryMb <= 0) {
                throw new IllegalArgumentException("cacheMaxMemoryMb must be positive");
            }
            this.cacheMaxMemoryMb = maxMemoryMb;
            return this;
        }

        /**
         * Set cache eviction policy.
         *
         * @param policy Eviction policy (default: LRU)
         * @return this builder
         */
        public Builder cacheEvictionPolicy(CacheEvictionPolicy policy) {
            Objects.requireNonNull(policy, "cacheEvictionPolicy cannot be null");
            this.cacheEvictionPolicy = policy;
            return this;
        }

        /**
         * Build the ApiLoaderConfig.
         *
         * @return ApiLoaderConfig instance
         */
        public ApiLoaderConfig build() {
            if (maxRetries > 0 && (retryDelay == null || retryDelay.isZero() || retryDelay.isNegative())) {
                throw new IllegalArgumentException("retryDelay must be positive when maxRetries > 0");
            }
            return new ApiLoaderConfig(this);
        }
        
    }
}
