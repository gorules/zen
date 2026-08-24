package io.gorules.zen_engine;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.atomic.AtomicReference;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ZenEngineTest {
    private static final ObjectMapper MAPPER = new ObjectMapper();

    private static Path testDataRoot() {
        Path current = Paths.get("").toAbsolutePath();
        while (current != null && !Files.isDirectory(current.resolve("test-data"))) {
            current = current.getParent();
        }
        if (current == null) {
            throw new IllegalStateException("test-data directory not found");
        }
        return current.resolve("test-data");
    }

    private static JsonBuffer readTestFile(String name) throws IOException {
        return new JsonBuffer(Files.readAllBytes(testDataRoot().resolve(name)));
    }

    private static JsonNode json(JsonBuffer buffer) throws IOException {
        return MAPPER.readTree(buffer.value());
    }

    @Test
    void staticLoader() throws Exception {
        var loader = new ZenLoader.Static(Map.of("table.json", readTestFile("table.json")));
        try (var engine = new ZenEngine(loader, null)) {
            var response = engine.evaluate("table.json", new JsonBuffer("{\"input\":12}"), null).get();
            assertEquals(10, json(response.result()).get("output").asInt());
            assertNull(response.trace());
        }
    }

    @Test
    void filesystemLoader() throws Exception {
        try (var engine = new ZenEngine(new ZenLoader.Filesystem(testDataRoot().toString()), null)) {
            var response = engine.evaluate("table.json", new JsonBuffer("{\"input\":5}"), null).get();
            assertEquals(0, json(response.result()).get("output").asInt());
        }
    }

    @Test
    void zipLoader() throws Exception {
        var buffer = new ByteArrayOutputStream();
        try (var zip = new ZipOutputStream(buffer)) {
            zip.putNextEntry(new ZipEntry("table.json"));
            zip.write(readTestFile("table.json").value());
            zip.closeEntry();
        }

        try (var engine = new ZenEngine(new ZenLoader.Zip(buffer.toByteArray()), null)) {
            var response = engine.evaluate("table.json", new JsonBuffer("{\"input\":12}"), null).get();
            assertEquals(10, json(response.result()).get("output").asInt());
        }
    }

    @Test
    void invalidZipFailsOnConstruction() {
        assertThrows(ZenException.class, () -> new ZenEngine(new ZenLoader.Zip(new byte[]{1, 2, 3, 4}), null));
    }

    @Test
    void callbackLoader() throws Exception {
        ZenDecisionLoaderCallback callback = key -> {
            try {
                return CompletableFuture.completedFuture(readTestFile(key));
            } catch (IOException e) {
                return CompletableFuture.completedFuture(null);
            }
        };

        try (var engine = new ZenEngine(new ZenLoader.Callback(callback), null)) {
            var response = engine.evaluate("table.json", new JsonBuffer("{\"input\":12}"), null).get();
            assertEquals(10, json(response.result()).get("output").asInt());

            var error = assertThrows(
                ExecutionException.class,
                () -> engine.evaluate("missing.json", new JsonBuffer("{}"), null).get()
            );
            assertInstanceOf(ZenException.class, error.getCause());
        }
    }

    @Test
    void missingKeyFails() throws Exception {
        try (var engine = new ZenEngine(new ZenLoader.Static(Map.of()), null)) {
            var error = assertThrows(
                ExecutionException.class,
                () -> engine.evaluate("missing.json", new JsonBuffer("{}"), null).get()
            );
            assertInstanceOf(ZenException.class, error.getCause());
        }
    }

    @Test
    void createDecision() throws Exception {
        try (var engine = new ZenEngine(null, null)) {
            try (var decision = engine.createDecision(readTestFile("table.json"))) {
                decision.validate();
                var response = decision.evaluate(new JsonBuffer("{\"input\":12}"), null).get();
                assertEquals(10, json(response.result()).get("output").asInt());
            }
        }
    }

    @Test
    void getDecision() throws Exception {
        try (var engine = new ZenEngine(new ZenLoader.Filesystem(testDataRoot().toString()), null)) {
            try (var decision = engine.getDecision("table.json").get()) {
                var response = decision.evaluate(new JsonBuffer("{\"input\":12}"), null).get();
                assertEquals(10, json(response.result()).get("output").asInt());
            }
        }
    }

    @Test
    void evaluateBatch() throws Exception {
        try (var engine = new ZenEngine(new ZenLoader.Filesystem(testDataRoot().toString()), null)) {
            var results = engine.evaluateBatch(List.of(
                new ZenBatchRequest("table.json", new JsonBuffer("{\"input\":12}")),
                new ZenBatchRequest("missing.json", new JsonBuffer("{}")),
                new ZenBatchRequest("table.json", new JsonBuffer("{\"input\":5}"))
            ), null).get();

            assertEquals(3, results.size());
            assertTrue(results.get(0).success());
            assertEquals(10, json(results.get(0).data().result()).get("output").asInt());
            assertFalse(results.get(1).success());
            assertNotNull(results.get(1).error());
            assertTrue(results.get(2).success());
            assertEquals(0, json(results.get(2).data().result()).get("output").asInt());
        }
    }

    @Test
    void traceOption() throws Exception {
        try (var engine = new ZenEngine(new ZenLoader.Filesystem(testDataRoot().toString()), null)) {
            var options = new ZenEvaluateOptions((byte) 5, true);
            var response = engine.evaluate("table.json", new JsonBuffer("{\"input\":12}"), options).get();
            assertNotNull(response.trace());
            assertFalse(response.trace().isEmpty());
        }
    }

    @Test
    void expressionGraph() throws Exception {
        try (var engine = new ZenEngine(new ZenLoader.Filesystem(testDataRoot().toString()), null)) {
            var context = new JsonBuffer("{\"numbers\":[1,5,15,25],\"firstName\":\"John\",\"lastName\":\"Doe\"}");
            var response = engine.evaluate("expression.json", context, null).get();
            var expected = MAPPER.readTree(
                "{\"deep\":{\"nested\":{\"sum\":46}},\"fullName\":\"John Doe\",\"largeNumbers\":[15,25],\"smallNumbers\":[1,5]}"
            );
            assertEquals(expected, json(response.result()));
        }
    }

    @Test
    void functionGraph() throws Exception {
        try (var engine = new ZenEngine(new ZenLoader.Filesystem(testDataRoot().toString()), null)) {
            var response = engine.evaluate("function.json", new JsonBuffer("{\"input\":15}"), null).get();
            assertEquals(30, json(response.result()).get("output").asInt());
        }
    }

    @Test
    void customNodeHandler() throws Exception {
        var seenRequest = new AtomicReference<ZenEngineHandlerRequest>();
        ZenCustomNodeCallback handler = request -> {
            seenRequest.set(request);
            try {
                var a = json(request.input()).get("a").asInt();
                var output = new JsonBuffer("{\"data\":" + (a + 20) + "}");
                return CompletableFuture.completedFuture(new ZenEngineHandlerResponse(output, null));
            } catch (IOException e) {
                return CompletableFuture.failedFuture(e);
            }
        };

        try (var engine = new ZenEngine(new ZenLoader.Filesystem(testDataRoot().toString()), handler)) {
            var response = engine.evaluate("custom.json", new JsonBuffer("{\"a\":5}"), null).get();
            assertEquals(25, json(response.result()).get("data").asInt());
        }

        var request = seenRequest.get();
        assertNotNull(request);
        assertEquals("sum", request.node().kind());
        assertEquals("customNode1", request.node().name());
        assertEquals("{{ a + 10 }}", json(request.node().config()).get("prop1").asText());
    }

    @Test
    void evaluateExpression() throws Exception {
        var result = ZenUniffi.evaluateExpression("sum(numbers)", new JsonBuffer("{\"numbers\":[1,2,3]}"));
        assertEquals(6, json(result).asInt());
    }

    @Test
    void evaluateUnaryExpression() throws Exception {
        assertTrue(ZenUniffi.evaluateUnaryExpression("$ > 10", new JsonBuffer("{\"$\":15}")));
        assertFalse(ZenUniffi.evaluateUnaryExpression("$ > 10", new JsonBuffer("{\"$\":5}")));
    }

    @Test
    void invalidExpressionFails() {
        assertThrows(ZenException.class, () -> ZenUniffi.evaluateExpression("a +* b", new JsonBuffer("{}")));
    }

    @Test
    void compiledExpression() throws Exception {
        try (var expression = ZenExpression.compile("a + b")) {
            var result = expression.evaluate(new JsonBuffer("{\"a\":1,\"b\":2}"));
            assertEquals(3, json(result).asInt());
        }

        try (var unary = ZenExpressionUnary.compile("$ > 3")) {
            assertTrue(unary.evaluate(new JsonBuffer("{\"$\":4}")));
            assertFalse(unary.evaluate(new JsonBuffer("{\"$\":2}")));
        }
    }
}
