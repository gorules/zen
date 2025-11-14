package io.gorules.zen.loader;

import io.gorules.zen_engine.JsonBuffer;

import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;

/**
 * In-memory decision loader.
 * Decisions must be added programmatically via {@link #addDecision(String, String)} or {@link #addDecision(String, byte[])}.
 */
public class MemoryDecisionLoader implements DecisionLoader {

    private final ConcurrentHashMap<String, JsonBuffer> decisions = new ConcurrentHashMap<>();

    /**
     * Create a new empty memory loader.
     */
    public MemoryDecisionLoader() {
    }

    @Override
    public CompletableFuture<JsonBuffer> load(String key) {
        return CompletableFuture.supplyAsync(() -> {
            JsonBuffer buffer = decisions.get(key);
            if (buffer == null) {
                throw new RuntimeException("Decision not found in memory: " + key);
            }
            return buffer;
        });
    }

    /**
     * Add a decision to memory.
     *
     * @param key     Decision key
     * @param content Decision content as JSON string
     */
    public void addDecision(String key, String content) {
        decisions.put(key, new JsonBuffer(content.getBytes()));
    }

    /**
     * Add a decision to memory.
     *
     * @param key     Decision key
     * @param content Decision content as bytes
     */
    public void addDecision(String key, byte[] content) {
        decisions.put(key, new JsonBuffer(content));
    }

    /**
     * Add a decision to memory.
     *
     * @param key    Decision key
     * @param buffer Decision content as JsonBuffer
     */
    public void addDecision(String key, JsonBuffer buffer) {
        decisions.put(key, buffer);
    }

    /**
     * Remove a decision from memory.
     *
     * @param key Decision key
     * @return true if removed, false if not found
     */
    public boolean removeDecision(String key) {
        return decisions.remove(key) != null;
    }

    /**
     * Clear all decisions from memory.
     */
    public void clear() {
        decisions.clear();
    }

    /**
     * Check if a decision exists in memory.
     *
     * @param key Decision key
     * @return true if exists
     */
    public boolean contains(String key) {
        return decisions.containsKey(key);
    }
}
