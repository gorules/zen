import Foundation
import Testing
@testable import ZenUniffi

enum TestDataError: Error {
    case notFound
}

func testDataRoot() throws -> URL {
    var current = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
    while current.path != "/" {
        let candidate = current.appendingPathComponent("test-data")
        if FileManager.default.fileExists(atPath: candidate.path) {
            return candidate
        }
        current = current.deletingLastPathComponent()
    }
    throw TestDataError.notFound
}

func readTestFile(_ name: String) throws -> Data {
    try Data(contentsOf: testDataRoot().appendingPathComponent(name))
}

func json(_ buffer: Data) throws -> [String: Any] {
    try #require(JSONSerialization.jsonObject(with: buffer) as? [String: Any])
}

final class FilesystemCallback: ZenDecisionLoaderCallback {
    let root: URL

    init(root: URL) {
        self.root = root
    }

    func load(key: String) async throws -> JsonBuffer? {
        try? Data(contentsOf: root.appendingPathComponent(key))
    }
}

final class SumNodeCallback: ZenCustomNodeCallback, @unchecked Sendable {
    var seenRequest: ZenEngineHandlerRequest?

    func handle(key: ZenEngineHandlerRequest) async throws -> ZenEngineHandlerResponse {
        seenRequest = key
        let input = try JSONSerialization.jsonObject(with: key.input) as? [String: Any]
        let a = input?["a"] as? Int ?? 0
        let output = try JSONSerialization.data(withJSONObject: ["data": a + 20])
        return ZenEngineHandlerResponse(output: output, traceData: nil)
    }
}

struct ZenEngineTests {
    @Test func staticLoader() async throws {
        let loader = ZenLoader.static(content: ["table.json": try readTestFile("table.json")])
        let engine = try ZenEngine(loader: loader)
        let response = try await engine.evaluate(
            key: "table.json",
            context: Data("{\"input\":12}".utf8),
            options: nil
        )
        #expect(try json(response.result)["output"] as? Int == 10)
        #expect(response.trace == nil)
    }

    @Test func filesystemLoader() async throws {
        let engine = try ZenEngine(loader: .filesystem(path: testDataRoot().path))
        let response = try await engine.evaluate(
            key: "table.json",
            context: Data("{\"input\":5}".utf8),
            options: nil
        )
        #expect(try json(response.result)["output"] as? Int == 0)
    }

    @Test func invalidZipFailsOnConstruction() {
        #expect(throws: (any Error).self) {
            try ZenEngine(loader: .zip(bytes: Data([1, 2, 3, 4])))
        }
    }

    @Test func callbackLoader() async throws {
        let callback = FilesystemCallback(root: try testDataRoot())
        let engine = try ZenEngine(loader: .callback(callback: callback))
        let response = try await engine.evaluate(
            key: "table.json",
            context: Data("{\"input\":12}".utf8),
            options: nil
        )
        #expect(try json(response.result)["output"] as? Int == 10)

        await #expect(throws: (any Error).self) {
            try await engine.evaluate(key: "missing.json", context: Data("{}".utf8), options: nil)
        }
    }

    @Test func missingKeyFails() async throws {
        let engine = try ZenEngine(loader: .static(content: [:]))
        await #expect(throws: (any Error).self) {
            try await engine.evaluate(key: "missing.json", context: Data("{}".utf8), options: nil)
        }
    }

    @Test func createDecision() async throws {
        let engine = try ZenEngine()
        let decision = try engine.createDecision(content: try readTestFile("table.json"))
        try decision.validate()
        let response = try await decision.evaluate(context: Data("{\"input\":12}".utf8), options: nil)
        #expect(try json(response.result)["output"] as? Int == 10)
    }

    @Test func getDecision() async throws {
        let engine = try ZenEngine(loader: .filesystem(path: testDataRoot().path))
        let decision = try await engine.getDecision(key: "table.json")
        let response = try await decision.evaluate(context: Data("{\"input\":12}".utf8), options: nil)
        #expect(try json(response.result)["output"] as? Int == 10)
    }

    @Test func evaluateBatch() async throws {
        let engine = try ZenEngine(loader: .filesystem(path: testDataRoot().path))
        let results = await engine.evaluateBatch(
            requests: [
                ZenBatchRequest(key: "table.json", context: Data("{\"input\":12}".utf8)),
                ZenBatchRequest(key: "missing.json", context: Data("{}".utf8)),
                ZenBatchRequest(key: "table.json", context: Data("{\"input\":5}".utf8)),
            ],
            options: nil
        )

        #expect(results.count == 3)
        #expect(results[0].success)
        #expect(try json(#require(results[0].data).result)["output"] as? Int == 10)
        #expect(!results[1].success)
        #expect(results[1].error != nil)
        #expect(results[2].success)
        #expect(try json(#require(results[2].data).result)["output"] as? Int == 0)
    }

    @Test func traceOption() async throws {
        let engine = try ZenEngine(loader: .filesystem(path: testDataRoot().path))
        let response = try await engine.evaluate(
            key: "table.json",
            context: Data("{\"input\":12}".utf8),
            options: ZenEvaluateOptions(maxDepth: 5, trace: true)
        )
        let trace = try #require(response.trace)
        #expect(!trace.isEmpty)
    }

    @Test func expressionGraph() async throws {
        let engine = try ZenEngine(loader: .filesystem(path: testDataRoot().path))
        let context = Data("{\"numbers\":[1,5,15,25],\"firstName\":\"John\",\"lastName\":\"Doe\"}".utf8)
        let response = try await engine.evaluate(key: "expression.json", context: context, options: nil)
        let result = try json(response.result)
        let expected = try json(Data(
            "{\"deep\":{\"nested\":{\"sum\":46}},\"fullName\":\"John Doe\",\"largeNumbers\":[15,25],\"smallNumbers\":[1,5]}".utf8
        ))
        #expect(result as NSDictionary == expected as NSDictionary)
    }

    @Test func functionGraph() async throws {
        let engine = try ZenEngine(loader: .filesystem(path: testDataRoot().path))
        let response = try await engine.evaluate(
            key: "function.json",
            context: Data("{\"input\":15}".utf8),
            options: nil
        )
        #expect(try json(response.result)["output"] as? Int == 30)
    }

    @Test func customNodeHandler() async throws {
        let handler = SumNodeCallback()
        let engine = try ZenEngine(loader: .filesystem(path: testDataRoot().path), customNode: handler)
        let response = try await engine.evaluate(
            key: "custom.json",
            context: Data("{\"a\":5}".utf8),
            options: nil
        )
        #expect(try json(response.result)["data"] as? Int == 25)

        let request = try #require(handler.seenRequest)
        #expect(request.node.kind == "sum")
        #expect(request.node.name == "customNode1")
        #expect(try json(request.node.config)["prop1"] as? String == "{{ a + 10 }}")
    }

    @Test func evaluateExpressionFunction() throws {
        let result = try evaluateExpression(
            expression: "sum(numbers)",
            context: Data("{\"numbers\":[1,2,3]}".utf8)
        )
        let value = try JSONSerialization.jsonObject(with: result, options: [.fragmentsAllowed])
        #expect(value as? Int == 6)
    }

    @Test func evaluateUnaryExpressionFunction() throws {
        #expect(try evaluateUnaryExpression(expression: "$ > 10", context: Data("{\"$\":15}".utf8)))
        #expect(try !evaluateUnaryExpression(expression: "$ > 10", context: Data("{\"$\":5}".utf8)))
    }

    @Test func invalidExpressionFails() {
        #expect(throws: (any Error).self) {
            try evaluateExpression(expression: "a +* b", context: Data("{}".utf8))
        }
    }

    @Test func compiledExpression() throws {
        let expression = try ZenExpression.compile(expression: "a + b")
        let result = try expression.evaluate(context: Data("{\"a\":1,\"b\":2}".utf8))
        let value = try JSONSerialization.jsonObject(with: result, options: [.fragmentsAllowed])
        #expect(value as? Int == 3)

        let unary = try ZenExpressionUnary.compile(expression: "$ > 3")
        #expect(try unary.evaluate(context: Data("{\"$\":4}".utf8)))
        #expect(try !unary.evaluate(context: Data("{\"$\":2}".utf8)))
    }
}
