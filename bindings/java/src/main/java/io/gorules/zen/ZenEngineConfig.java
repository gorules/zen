package io.gorules.zen;

import io.gorules.zen.loader.DecisionLoader;
import lombok.Value;

/**
 * Configuration for ZenEngineWrapper.
 */
@Value
public class ZenEngineConfig {
    DecisionLoader loader;
    int maxDepth;
    boolean enableTrace;
}
