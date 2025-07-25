use zen_expression::{Isolate, Variable};
use zen_expression::functions::mf_function::{
    MfFunctionHelper, MfFunctionRegistry,
};
use zen_expression::variable::VariableType;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// 1. 定义一个简单的、我们自己的状态
#[derive(Debug)]
struct MyState {
    call_count: Mutex<u32>,
}

impl MyState {
    fn new() -> Self {
        Self { call_count: Mutex::new(0) }
    }

    fn increment(&self) -> u32 {
        let mut count = self.call_count.lock().unwrap();
        *count += 1;
        *count
    }
}

fn main() -> anyhow::Result<()> {
    println!("===  自定义函数与泛型State集成演示 ===\n");

    // === 第一部分: 演示使用我们自定义的 `MyState` ===
    println!("--- 场景1: 使用自定义的 MyState ---");
    let my_state = Arc::new(MyState::new());

    // 2. 为 `MyState` 创建一个 Helper
    let my_helper = MfFunctionHelper::<MyState>::new();

    // 3. 注册一个可以访问 `MyState` 的函数
    println!("注册函数: getMyStateCallCount()");
    my_helper
        .register_function(
            "getMyStateCallCount".to_string(),
            vec![],
            VariableType::Number,
            Box::new(|_args, state_opt: Option<&MyState>| {
                if let Some(state) = state_opt {
                    // `state` 的类型是 &MyState
                    let count = state.increment();
                    Ok(Variable::Number(count.into()))
                } else {
                    Ok(Variable::Number((-1i32).into()))
                }
            }),
        )
        .map_err(|e| anyhow::anyhow!(e))?;

    // 4. 创建 Isolate 并使用 `MyState` 执行表达式
    let mut isolate = Isolate::new();
    println!("使用 `MyState` 执行 'getMyStateCallCount()'");
    let result1 = isolate
        .run_standard_with_state("getMyStateCallCount()", my_state.clone())?;
    println!("  第一次调用结果: {}", result1);
    let result2 = isolate
        .run_standard_with_state("getMyStateCallCount()", my_state.clone())?;
    println!("  第二次调用结果: {}", result2);

    // === 第三部分: 验证两种函数可以共存 ===
    println!("\n--- 场景3: 验证两种状态的函数可以共存 ---");
    println!("再次调用 `getMyStateCallCount` (应为3)");
    let result4 = isolate
        .run_standard_with_state("getMyStateCallCount()", my_state.clone())?;
    println!("  结果: {}", result4);

    // 显示所有已注册的自定义函数
    println!("\n=== 已注册的自定义函数 ===");
    let functions = MfFunctionRegistry::list_functions();
    for func in functions {
        println!("- {}", func);
    }

    // 清理
    println!("\n清理所有自定义函数...");
    MfFunctionRegistry::clear();

    println!("演示完成！");
    Ok(())
}
