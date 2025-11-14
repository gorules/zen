package io.gorules.zen;

import io.gorules.zen.loader.DecisionLoader;

/**
 * Configuration for ZenEngineWrapper.
 */
public class ZenEngineConfig {
    private final DecisionLoader loader;
    private final int maxDepth;
    private final boolean enableTrace;

    /**
     * Create configuration.
     *
     * @param loader      Decision loader
     * @param maxDepth    Maximum evaluation depth
     * @param enableTrace Whether to enable tracing
     */
    public ZenEngineConfig(DecisionLoader loader, int maxDepth, boolean enableTrace) {
        this.loader = loader;
        this.maxDepth = maxDepth;
        this.enableTrace = enableTrace;
    }

    /**
     * Get the decision loader.
     *
     * @return Decision loader
     */
    public DecisionLoader getLoader() {
        return loader;
    }

    /**
     * Get maximum evaluation depth.
     *
     * @return Maximum depth
     */
    public int getMaxDepth() {
        return maxDepth;
    }

    /**
     * Check if tracing is enabled.
     *
     * @return true if tracing enabled
     */
    public boolean isEnableTrace() {
        return enableTrace;
    }
}
