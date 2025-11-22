package io.gorules.zen.loader;

import io.gorules.zen_engine.JsonBuffer;

import java.util.concurrent.CompletableFuture;

/**
 * Interface for loading decision models.
 */
public interface DecisionLoader {

    /**
     * Load a decision by its key.
     *
     * @param key Decision key/filename
     * @return CompletableFuture with decision content as JsonBuffer
     */
    CompletableFuture<JsonBuffer> load(String key);
}
