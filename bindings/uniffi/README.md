# Java, Kotlin, Android & .NET Rules Engine

**Business logic humans can read and machines can run.** One copy of your rules: the owner reads it, every system runs it.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Maven Central](https://img.shields.io/maven-central/v/io.gorules/zen-engine.svg)](https://central.sonatype.com/artifact/io.gorules/zen-engine)
[![NuGet](https://img.shields.io/nuget/v/GoRules.ZenEngine.svg)](https://www.nuget.org/packages/GoRules.ZenEngine)

<img width="1280" alt="GoRules ZEN Engine" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/hero.png">

ZEN Engine is a cross-platform, open-source [Business Rules Engine (BRE)](https://gorules.io) written in **Rust**. This directory builds the UniFFI-based bindings for **Java**, **Kotlin**, **Android** and **.NET**, alongside the native Node.js, Python and Go packages. Decisions evaluate in microseconds, run identically on every platform, and are stored as portable JSON. Loading the JSON is up to you: file system, database or service call.

Try it in the free [Online Editor](https://editor.gorules.io) with a built-in simulator, or embed the open-source React [JDM Editor](https://github.com/gorules/jdm-editor) in your own product. Learn more about the [Java rules engine](https://gorules.io/open-source/java-rules-engine), [Kotlin rules engine](https://gorules.io/open-source/kotlin-rules-engine) and [C# rules engine](https://gorules.io/open-source/csharp-rules-engine) on the GoRules website.

## Packages

| Platform | Package | Install | Documentation |
|:---------|:--------|:--------|:--------------|
| Java (JDK 22+) | [`io.gorules:zen-engine`](https://central.sonatype.com/artifact/io.gorules/zen-engine) | `implementation("io.gorules:zen-engine:2.0.0")` | [Java SDK](https://docs.gorules.io/developers/sdks/java) |
| Kotlin | [`io.gorules:zen-engine-kotlin`](https://central.sonatype.com/artifact/io.gorules/zen-engine-kotlin) | `implementation("io.gorules:zen-engine-kotlin:2.0.0")` | [Kotlin SDK](https://docs.gorules.io/developers/sdks/kotlin) |
| Android (AAR) | [`io.gorules:zen-engine-kotlin-android`](https://central.sonatype.com/artifact/io.gorules/zen-engine-kotlin-android) | `implementation("io.gorules:zen-engine-kotlin-android:2.0.0")` | [Android SDK](https://docs.gorules.io/developers/sdks/android) |
| .NET | [`GoRules.ZenEngine`](https://www.nuget.org/packages/GoRules.ZenEngine) | `dotnet add package GoRules.ZenEngine` | [.NET SDK](https://docs.gorules.io/developers/sdks/csharp) |

## Quickstart

### Java

```java
import io.gorules.zen_engine.ZenEngine;
import io.gorules.zen_engine.JsonBuffer;

try (var engine = new ZenEngine(null, null)) {
    var ruleJson = Main.class.getResourceAsStream("/rules/pricing.json").readAllBytes();
    var decision = engine.createDecision(new JsonBuffer(ruleJson));

    var input = new JsonBuffer("""
        { "customer": { "tier": "gold" } }
        """);
    var response = decision.evaluate(input, null).join();

    System.out.println(response.result());
}
```

> [!NOTE]
> The Java bindings require JDK 22+ (FFM API). Add `--enable-native-access=ALL-UNNAMED` to silence native-access warnings on JDK 24+. The bundled native library is extracted from the jar automatically; on versions 2.0.1 and earlier, point the JVM at it manually with `-Duniffi.component.zen_uniffi.libraryOverride=/absolute/path/to/libzen_uniffi.dylib` after extracting it from the jar for your platform.

### Kotlin

```kotlin
import io.gorules.zen_engine.kotlin.ZenEngine
import io.gorules.zen_engine.kotlin.JsonBuffer
import kotlinx.coroutines.runBlocking

fun main() = runBlocking {
    val ruleJson = object {}.javaClass.getResourceAsStream("/rules/pricing.json")!!.readBytes()

    ZenEngine(null, null).use { engine ->
        val decision = engine.createDecision(JsonBuffer(ruleJson))

        val input = JsonBuffer("""{ "customer": { "tier": "gold" } }""")
        val response = decision.evaluate(input, null)

        println(response.result)
    }
}
```

### .NET

```csharp
using GoRules.ZenEngine;

var engine = new ZenEngine(loader: null, customNode: null);
var decision = engine.CreateDecision(new JsonBuffer(File.ReadAllBytes("my-decision.json")));
var context = new JsonBuffer("""{"input": 42}""");
var response = await decision.Evaluate(context, null);
Console.WriteLine(response.Result);
```

Each SDK also supports loader configurations (`Static`, `Filesystem`, `Zip`, `Callback`) that pre-load and pre-compile decisions at engine creation, plus tracing, custom nodes and direct expression evaluation. Full guides are in the per-platform documentation linked above.

## Rules that read like sentences

Conditions are written the way the business says them, in the ZEN Expression Language. The developer view is one toggle away, and the two can never drift apart: there is only one source of truth, and this engine runs it.

<img width="1280" alt="Readable rules" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/tables.png">

## Rules as graphs, or as documents

Model a decision on a visual canvas of decision tables, switches, expressions, functions and reusable sub-decisions. Or write it as a policy document with prose, typed data models and tables. Both compile to the same engine and return the same answers.

<img width="1280" alt="Graphs and documents" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/graphs-docs.png">

To go deeper, see the [decision graph guide](https://docs.gorules.io/learn/authoring/decision-graphs) and the [ZEN Expression Language](https://docs.gorules.io/learn/zen-language/syntax) reference.

## Other platforms

* **Node.js** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/nodejs) | [Documentation](https://docs.gorules.io/developers/sdks/nodejs) | [npm](https://www.npmjs.com/package/@gorules/zen-engine)
* **Python** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/python) | [Documentation](https://docs.gorules.io/developers/sdks/python) | [PyPI](https://pypi.org/project/zen-engine/)
* **Go** - [GitHub](https://github.com/gorules/zen-go) | [Documentation](https://docs.gorules.io/developers/sdks/go)
* **Rust (Core)** - [GitHub](https://github.com/gorules/zen) | [Documentation](https://docs.gorules.io/developers/sdks/rust) | [crates.io](https://crates.io/crates/zen-engine)

## The GoRules platform

The engine is open at the core; [GoRules](https://gorules.io) is the platform around it. Managed cloud, self-hosted, or embedded with no network hop. SOC 2 Type II.

## Support matrix

| Arch            | Java / Kotlin      | .NET               |
|:----------------|:-------------------|:-------------------|
| linux-x64-gnu   | :heavy_check_mark: | :heavy_check_mark: |
| linux-arm64-gnu | :heavy_check_mark: | :heavy_check_mark: |
| darwin-x64      | :heavy_check_mark: | :heavy_check_mark: |
| darwin-arm64    | :heavy_check_mark: | :heavy_check_mark: |
| win32-x64-msvc  | :heavy_check_mark: | :heavy_check_mark: |
| linux-s390x     | :heavy_check_mark: | :x:                |

Android (AAR) and iOS (XCFramework) packages are published from the same core via UniFFI.

## Contribution

The JDM standard is growing and we need to keep tight control over its development and roadmap, as a number of companies use GoRules ZEN Engine and GoRules BRMS. For this reason we can't accept code contributions at this moment, apart from help with documentation and additional tests.

## License

[MIT License](https://opensource.org/licenses/MIT)
