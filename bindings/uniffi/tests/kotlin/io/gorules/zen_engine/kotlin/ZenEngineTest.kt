package io.gorules.zen_engine.kotlin

import com.fasterxml.jackson.databind.JsonNode
import com.fasterxml.jackson.databind.ObjectMapper
import kotlinx.coroutines.runBlocking
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import java.io.ByteArrayOutputStream
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

class ZenEngineTest {
    private val mapper = ObjectMapper()

    private fun testDataRoot(): Path {
        var current: Path? = Paths.get("").toAbsolutePath()
        while (current != null && !Files.isDirectory(current.resolve("test-data"))) {
            current = current.parent
        }
        return checkNotNull(current) { "test-data directory not found" }.resolve("test-data")
    }

    private fun readTestFile(name: String): JsonBuffer =
        JsonBuffer(Files.readAllBytes(testDataRoot().resolve(name)))

    private fun json(buffer: JsonBuffer): JsonNode = mapper.readTree(buffer.value)

    @Test
    fun staticLoader() = runBlocking {
        val loader = ZenLoader.Static(mapOf("table.json" to readTestFile("table.json")))
        ZenEngine(loader).use { engine ->
            val response = engine.evaluate("table.json", JsonBuffer("""{"input":12}"""), null)
            assertEquals(10, json(response.result).get("output").asInt())
            assertNull(response.trace)
        }
    }

    @Test
    fun filesystemLoader() = runBlocking {
        ZenEngine(ZenLoader.Filesystem(testDataRoot().toString())).use { engine ->
            val response = engine.evaluate("table.json", JsonBuffer("""{"input":5}"""), null)
            assertEquals(0, json(response.result).get("output").asInt())
        }
    }

    @Test
    fun zipLoader() = runBlocking {
        val buffer = ByteArrayOutputStream()
        ZipOutputStream(buffer).use { zip ->
            zip.putNextEntry(ZipEntry("table.json"))
            zip.write(readTestFile("table.json").value)
            zip.closeEntry()
        }

        ZenEngine(ZenLoader.Zip(buffer.toByteArray())).use { engine ->
            val response = engine.evaluate("table.json", JsonBuffer("""{"input":12}"""), null)
            assertEquals(10, json(response.result).get("output").asInt())
        }
    }

    @Test
    fun invalidZipFailsOnConstruction() {
        assertThrows<ZenException> { ZenEngine(ZenLoader.Zip(byteArrayOf(1, 2, 3, 4))) }
    }

    @Test
    fun callbackLoader() = runBlocking<Unit> {
        val callback = object : ZenDecisionLoaderCallback {
            override suspend fun load(key: String): JsonBuffer? =
                runCatching { readTestFile(key) }.getOrNull()
        }

        ZenEngine(ZenLoader.Callback(callback)).use { engine ->
            val response = engine.evaluate("table.json", JsonBuffer("""{"input":12}"""), null)
            assertEquals(10, json(response.result).get("output").asInt())

            assertThrows<ZenException> {
                runBlocking { engine.evaluate("missing.json", JsonBuffer("{}"), null) }
            }
        }
    }

    @Test
    fun missingKeyFails() = runBlocking<Unit> {
        ZenEngine(ZenLoader.Static(emptyMap())).use { engine ->
            assertThrows<ZenException> {
                runBlocking { engine.evaluate("missing.json", JsonBuffer("{}"), null) }
            }
        }
    }

    @Test
    fun createDecision() = runBlocking {
        ZenEngine().use { engine ->
            engine.createDecision(readTestFile("table.json")).use { decision ->
                decision.validate()
                val response = decision.evaluate(JsonBuffer("""{"input":12}"""), null)
                assertEquals(10, json(response.result).get("output").asInt())
            }
        }
    }

    @Test
    fun getDecision() = runBlocking {
        ZenEngine(ZenLoader.Filesystem(testDataRoot().toString())).use { engine ->
            engine.getDecision("table.json").use { decision ->
                val response = decision.evaluate(JsonBuffer("""{"input":12}"""), null)
                assertEquals(10, json(response.result).get("output").asInt())
            }
        }
    }

    @Test
    fun evaluateBatch() = runBlocking {
        ZenEngine(ZenLoader.Filesystem(testDataRoot().toString())).use { engine ->
            val results = engine.evaluateBatch(
                listOf(
                    ZenBatchRequest("table.json", JsonBuffer("""{"input":12}""")),
                    ZenBatchRequest("missing.json", JsonBuffer("{}")),
                    ZenBatchRequest("table.json", JsonBuffer("""{"input":5}""")),
                ),
                null,
            )

            assertEquals(3, results.size)
            assertTrue(results[0].success)
            assertEquals(10, json(results[0].data!!.result).get("output").asInt())
            assertFalse(results[1].success)
            assertNotNull(results[1].error)
            assertTrue(results[2].success)
            assertEquals(0, json(results[2].data!!.result).get("output").asInt())
        }
    }

    @Test
    fun traceOption() = runBlocking {
        ZenEngine(ZenLoader.Filesystem(testDataRoot().toString())).use { engine ->
            val options = ZenEvaluateOptions(maxDepth = 5u, trace = true)
            val response = engine.evaluate("table.json", JsonBuffer("""{"input":12}"""), options)
            assertNotNull(response.trace)
            assertTrue(response.trace!!.isNotEmpty())
        }
    }

    @Test
    fun expressionGraph() = runBlocking {
        ZenEngine(ZenLoader.Filesystem(testDataRoot().toString())).use { engine ->
            val context = JsonBuffer("""{"numbers":[1,5,15,25],"firstName":"John","lastName":"Doe"}""")
            val response = engine.evaluate("expression.json", context, null)
            val expected = mapper.readTree(
                """{"deep":{"nested":{"sum":46}},"fullName":"John Doe","largeNumbers":[15,25],"smallNumbers":[1,5]}""",
            )
            assertEquals(expected, json(response.result))
        }
    }

    @Test
    fun functionGraph() = runBlocking {
        ZenEngine(ZenLoader.Filesystem(testDataRoot().toString())).use { engine ->
            val response = engine.evaluate("function.json", JsonBuffer("""{"input":15}"""), null)
            assertEquals(30, json(response.result).get("output").asInt())
        }
    }

    @Test
    fun customNodeHandler() = runBlocking {
        var seenRequest: ZenEngineHandlerRequest? = null
        val handler = object : ZenCustomNodeCallback {
            override suspend fun handle(key: ZenEngineHandlerRequest): ZenEngineHandlerResponse {
                seenRequest = key
                val a = json(key.input).get("a").asInt()
                return ZenEngineHandlerResponse(JsonBuffer("""{"data":${a + 20}}"""), null)
            }
        }

        ZenEngine(ZenLoader.Filesystem(testDataRoot().toString()), handler).use { engine ->
            val response = engine.evaluate("custom.json", JsonBuffer("""{"a":5}"""), null)
            assertEquals(25, json(response.result).get("data").asInt())
        }

        val request = checkNotNull(seenRequest)
        assertEquals("sum", request.node.kind)
        assertEquals("customNode1", request.node.name)
        assertEquals("{{ a + 10 }}", json(request.node.config).get("prop1").asText())
    }

    @Test
    fun evaluateExpressionFunction() {
        val result = evaluateExpression("sum(numbers)", JsonBuffer("""{"numbers":[1,2,3]}"""))
        assertEquals(6, json(result).asInt())
    }

    @Test
    fun evaluateUnaryExpressionFunction() {
        assertTrue(evaluateUnaryExpression("$ > 10", JsonBuffer("""{"$":15}""")))
        assertFalse(evaluateUnaryExpression("$ > 10", JsonBuffer("""{"$":5}""")))
    }

    @Test
    fun invalidExpressionFails() {
        assertThrows<ZenException> { evaluateExpression("a +* b", JsonBuffer("{}")) }
    }

    @Test
    fun compiledExpression() {
        ZenExpression.compile("a + b").use { expression ->
            val result = expression.evaluate(JsonBuffer("""{"a":1,"b":2}"""))
            assertEquals(3, json(result).asInt())
        }

        ZenExpressionUnary.compile("$ > 3").use { unary ->
            assertTrue(unary.evaluate(JsonBuffer("""{"$":4}""")))
            assertFalse(unary.evaluate(JsonBuffer("""{"$":2}""")))
        }
    }
}
