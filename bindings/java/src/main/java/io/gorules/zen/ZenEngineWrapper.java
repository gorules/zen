package io.gorules.zen;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import io.gorules.zen.loader.DecisionLoader;
import io.gorules.zen_engine.*;

import java.io.IOException;
import java.util.concurrent.CompletableFuture;

/**
 * Simple Java wrapper for ZEN Engine with easy-to-use API.
 *
 * <pre>{@code
 * // Create engine
 * ZenEngineWrapper engine = ZenEngineBuilder.create()
 *     .withFilesystemLoader("/app/decisions")
 *     .build();
 *
 * // Evaluate decision
 * JsonNode input = objectMapper.readTree("{\"amount\": 1000}");
 * JsonNode result = engine.evaluate("pricing.json", input).get();
 * }</pre>
 */
public class ZenEngineWrapper implements AutoCloseable {

    private final ZenEngine engine;
    private final ZenEngineConfig config;
    private final ObjectMapper objectMapper;

    /**
     * Create wrapper with configuration.
     *
     * @param config ZEN engine configuration
     */
    public ZenEngineWrapper(ZenEngineConfig config) {
        this.config = config;
        this.objectMapper = new ObjectMapper();

        // Create ZEN engine with loader callback
        DecisionLoader loader = config.getLoader();
        ZenDecisionLoaderCallback loaderCallback = loader::load;

        this.engine = new ZenEngine(loaderCallback, null);
    }

    /**
     * Evaluate a decision with the given key and input context.
     *
     * @param key     Decision key/filename (e.g., "pricing.json")
     * @param input   Input context as JsonNode
     * @return CompletableFuture with evaluation result
     */
    public CompletableFuture<JsonNode> evaluate(String key, JsonNode input) {
        return evaluate(key, input, null);
    }

    /**
     * Evaluate a decision with custom options.
     *
     * @param key     Decision key/filename
     * @param input   Input context as JsonNode
     * @param options Evaluation options (null for defaults)
     * @return CompletableFuture with evaluation result
     */
    public CompletableFuture<JsonNode> evaluate(String key, JsonNode input, ZenEvaluateOptions options) {
        try {
            byte[] inputBytes = objectMapper.writeValueAsBytes(input);
            JsonBuffer inputBuffer = new JsonBuffer(inputBytes);

            if (options == null) {
                options = new ZenEvaluateOptions(
                    (byte) config.getMaxDepth(),
                    config.isEnableTrace()
                );
            }

            return engine.evaluate(key, inputBuffer, options)
                .thenApply(response -> {
                    try {
                        String resultJson = response.result().toString();
                        return objectMapper.readTree(resultJson);
                    } catch (IOException e) {
                        throw new RuntimeException("Failed to parse result", e);
                    }
                });
        } catch (IOException e) {
            return CompletableFuture.failedFuture(
                new RuntimeException("Failed to serialize input", e)
            );
        }
    }

    /**
     * Evaluate with full response including trace and performance data.
     *
     * @param key     Decision key
     * @param input   Input context
     * @param options Evaluation options
     * @return CompletableFuture with full response
     */
    public CompletableFuture<ZenEngineResponse> evaluateWithTrace(
            String key, JsonNode input, ZenEvaluateOptions options) {
        try {
            byte[] inputBytes = objectMapper.writeValueAsBytes(input);
            JsonBuffer inputBuffer = new JsonBuffer(inputBytes);

            if (options == null) {
                options = new ZenEvaluateOptions(
                    (byte) config.getMaxDepth(),
                    true  // Force trace
                );
            }

            return engine.evaluate(key, inputBuffer, options);
        } catch (IOException e) {
            return CompletableFuture.failedFuture(
                new RuntimeException("Failed to serialize input", e)
            );
        }
    }

    /**
     * Get a decision by key (for reuse/caching).
     *
     * @param key Decision key
     * @return CompletableFuture with decision instance
     */
    public CompletableFuture<ZenDecision> getDecision(String key) {
        return engine.getDecision(key);
    }

    /**
     * Create a decision from JSON content.
     *
     * @param content Decision content as JsonNode
     * @return Decision instance
     * @throws ZenException if creation fails
     */
    public ZenDecision createDecision(JsonNode content) throws ZenException {
        try {
            byte[] contentBytes = objectMapper.writeValueAsBytes(content);
            JsonBuffer contentBuffer = new JsonBuffer(contentBytes);
            return engine.createDecision(contentBuffer);
        } catch (IOException e) {
            throw new RuntimeException("Failed to serialize decision content", e);
        }
    }

    /**
     * Create a decision from JSON string.
     *
     * @param jsonContent Decision content as JSON string
     * @return Decision instance
     * @throws ZenException if creation fails
     */
    public ZenDecision createDecision(String jsonContent) throws ZenException {
        byte[] contentBytes = jsonContent.getBytes();
        JsonBuffer contentBuffer = new JsonBuffer(contentBytes);
        return engine.createDecision(contentBuffer);
    }

    /**
     * Get the underlying ZEN engine for advanced use cases.
     *
     * @return ZenEngine instance
     */
    public ZenEngine getEngine() {
        return engine;
    }

    /**
     * Get the ObjectMapper used for JSON processing.
     *
     * @return ObjectMapper instance
     */
    public ObjectMapper getObjectMapper() {
        return objectMapper;
    }

    @Override
    public void close() {
        engine.close();
    }
}
