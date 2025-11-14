package io.gorules.zen.loader;

import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.*;

/**
 * Configuration for ApiDecisionLoader.
 * Supports flexible header configuration for any HTTP headers you need.
 *
 * <pre>{@code
 * ApiLoaderConfig config = ApiLoaderConfig.builder("https://api.example.com/decisions")
 *     .header("Authorization", "Bearer token123")
 *     .header("X-API-Key", "your-key")
 *     .header("X-Custom-Header", "any-value")
 *     .timeout(Duration.ofSeconds(30))
 *     .caching(true)
 *     .build();
 * }</pre>
 */
public class ApiLoaderConfig {

    private final String baseUrl;
    private final Map<String, String> staticHeaders;
    private final HeaderProvider dynamicHeaderProvider;
    private final Duration timeout;
    private final int maxRetries;
    private final Duration retryDelay;
    private final boolean enableCaching;

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
     * Get the base URL for API requests.
     *
     * @return Base URL
     */
    public String getBaseUrl() {
        return baseUrl;
    }

    /**
     * Get static headers (does not include dynamic headers from provider).
     *
     * @return Map of static headers
     */
    public Map<String, String> getStaticHeaders() {
        return staticHeaders;
    }

    /**
     * Get request timeout duration.
     *
     * @return Timeout duration
     */
    public Duration getTimeout() {
        return timeout;
    }

    /**
     * Get maximum number of retry attempts.
     *
     * @return Maximum retries
     */
    public int getMaxRetries() {
        return maxRetries;
    }

    /**
     * Get delay between retry attempts.
     *
     * @return Retry delay duration
     */
    public Duration getRetryDelay() {
        return retryDelay;
    }

    /**
     * Check if caching is enabled.
     *
     * @return true if caching is enabled
     */
    public boolean isEnableCaching() {
        return enableCaching;
    }

    /**
     * Builder for ApiLoaderConfig with flexible header configuration.
     */
    public static class Builder {
        private final String baseUrl;
        private final Map<String, String> staticHeaders = new HashMap<>();
        private HeaderProvider dynamicHeaderProvider;
        private Duration timeout = Duration.ofSeconds(30);
        private int maxRetries = 3;
        private Duration retryDelay = Duration.ofSeconds(1);
        private boolean enableCaching = true;

        private Builder(String baseUrl) {
            Objects.requireNonNull(baseUrl, "baseUrl cannot be null");
            this.baseUrl = baseUrl;
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
         * Add API key header.
         * Convenience method for: header("X-API-Key", key)
         *
         * @param key API key
         * @return this builder
         */
        public Builder apiKey(String key) {
            Objects.requireNonNull(key, "API key cannot be null");
            return header("X-API-Key", key);
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
         * Build the ApiLoaderConfig.
         *
         * @return ApiLoaderConfig instance
         */
        public ApiLoaderConfig build() {
            return new ApiLoaderConfig(this);
        }
    }
}
