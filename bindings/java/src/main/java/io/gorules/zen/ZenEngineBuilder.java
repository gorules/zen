package io.gorules.zen;

import io.gorules.zen.loader.*;

/**
 * Builder for creating ZenEngineWrapper instances with fluent API.
 */
public class ZenEngineBuilder {

    private DecisionLoader loader;
    private int maxDepth = 5;
    private boolean enableTrace = false;

    private ZenEngineBuilder() {}

    /**
     * Create a new builder instance.
     *
     * @return New ZenEngineBuilder
     */
    public static ZenEngineBuilder create() {
        return new ZenEngineBuilder();
    }

    /**
     * Use filesystem loader with given root directory.
     *
     * @param rootPath Root directory containing decision files
     * @return this builder
     */
    public ZenEngineBuilder withFilesystemLoader(String rootPath) {
        this.loader = new FilesystemDecisionLoader(rootPath);
        return this;
    }

    /**
     * Use classpath loader with given root path.
     *
     * @param rootPath Root classpath path (e.g., "decisions/" or "/decisions/")
     * @return this builder
     */
    public ZenEngineBuilder withClasspathLoader(String rootPath) {
        this.loader = new ClasspathDecisionLoader(rootPath);
        return this;
    }

    /**
     * Use in-memory loader.
     * Decisions must be added programmatically.
     *
     * @return this builder
     */
    public ZenEngineBuilder withMemoryLoader() {
        this.loader = new MemoryDecisionLoader();
        return this;
    }

    /**
     * Use API loader with given base URL.
     * Loads decisions from HTTP API.
     *
     * @param baseUrl Base URL for API (e.g., "https://api.example.com/decisions")
     * @return this builder
     */
    public ZenEngineBuilder withApiLoader(String baseUrl) {
        ApiLoaderConfig config = ApiLoaderConfig.builder(baseUrl).build();
        this.loader = new ApiDecisionLoader(config);
        return this;
    }

    /**
     * Use API loader with given base URL and Bearer token.
     * Convenience method for common authentication pattern.
     *
     * @param baseUrl     Base URL for API
     * @param bearerToken Bearer token for Authorization header
     * @return this builder
     */
    public ZenEngineBuilder withApiLoader(String baseUrl, String bearerToken) {
        ApiLoaderConfig config = ApiLoaderConfig.builder(baseUrl)
            .bearerToken(bearerToken)
            .build();
        this.loader = new ApiDecisionLoader(config);
        return this;
    }

    /**
     * Use API loader with full configuration.
     * Allows complete control over headers, timeout, retries, etc.
     *
     * @param config API loader configuration
     * @return this builder
     */
    public ZenEngineBuilder withApiLoader(ApiLoaderConfig config) {
        this.loader = new ApiDecisionLoader(config);
        return this;
    }

    /**
     * Use a custom decision loader.
     *
     * @param loader Custom loader implementation
     * @return this builder
     */
    public ZenEngineBuilder withLoader(DecisionLoader loader) {
        this.loader = loader;
        return this;
    }
    /**
     * Set maximum evaluation depth.
     *
     * @param maxDepth Maximum depth (default: 5)
     * @return this builder
     */
    public ZenEngineBuilder withMaxDepth(int maxDepth) {
        this.maxDepth = maxDepth;
        return this;
    }

    /**
     * Enable or disable tracing by default.
     *
     * @param enable true to enable tracing
     * @return this builder
     */
    public ZenEngineBuilder withTracing(boolean enable) {
        this.enableTrace = enable;
        return this;
    }

    /**
     * Build the ZenEngineWrapper instance.
     *
     * @return configured ZenEngineWrapper
     */
    public ZenEngineWrapper build() {
        if (loader == null) {
            // Default to classpath loader
            loader = new ClasspathDecisionLoader("decisions/");
        }

        ZenEngineConfig config = new ZenEngineConfig(
            loader,
            maxDepth,
            enableTrace
        );

        return new ZenEngineWrapper(config);
    }
}
