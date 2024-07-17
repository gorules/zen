import zen
import asyncio


def loader(key):
    with open("../../test-data/" + key, "r") as f:
        return f.read()


def custom_handler(request):
    p1 = request.get_field("prop1")
    return {
        "output": {"sum": p1}
    }


async def custom_async_handler(request):
    p1 = request.get_field("prop1")
    # await asyncio.sleep(1)
    return {
        "output": {"sum": p1}
    }


async def evaluate_custom_handler():
    engine = zen.ZenEngine({"loader": loader, "customHandler": custom_async_handler})
    r1 = await engine.async_evaluate("custom.json", {"a": 10})
    # r2 = engine.async_evaluate("custom.json", {"a": 20})
    # r3 = engine.async_evaluate("custom.json", {"a": 30})

    # results = await asyncio.gather(r1, r2, r3)
    # print(results)
    print(r1)

asyncio.run(evaluate_custom_handler())