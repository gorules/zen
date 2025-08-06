use zen_expression::{Isolate, evaluate_expression, Variable};
use serde_json::json;
use rust_decimal_macros::dec;

fn main() {
    println!("=== rules_expression 编译与VM执行过程演示 ===\n");

    // 演示1: 基础表达式编译和执行
    demo_basic_compilation();

    // 演示2: 复杂表达式的字节码分析
    demo_complex_expression();

    // 演示3: 高性能重复执行
    demo_performance_execution();

    // 演示4: 不同数据类型的处理
    demo_data_types();

    // 演示5: 区间和条件表达式
    demo_intervals_and_conditions();
}

fn demo_basic_compilation() {
    println!("【演示1: 基础表达式编译和执行】");

    // 创建上下文环境
    let context = json!({
        "tax": {
            "percentage": 10
        },
        "amount": 50
    });

    let expression = "amount * tax.percentage / 100";
    println!("表达式: {}", expression);
    println!("上下文: {}", context);

    // 方式1: 直接评估（内部完成完整的编译->执行流程）
    let result =
        evaluate_expression(expression, context.clone().into()).unwrap();
    println!("计算结果: {:?}", result);

    // 方式2: 使用Isolate查看详细过程
    let mut isolate = Isolate::with_environment(context.into());

    // 编译表达式获取字节码
    let compiled = isolate.compile_standard(expression).unwrap();
    println!("编译后的字节码: {:?}", compiled.bytecode());

    // 执行编译后的表达式
    let new_context = json!({"tax": {"percentage": 15}, "amount": 100});
    let result2 = compiled.evaluate(new_context.into()).unwrap();
    println!("新上下文执行结果: {:?}\n", result2);
}

fn demo_complex_expression() {
    println!("【演示2: 复杂表达式的字节码分析】");

    let mut isolate = Isolate::new();
    let expression = "(a + b) * c - d / 2";

    println!("复杂表达式: {}", expression);

    // 编译并查看字节码
    let compiled = isolate.compile_standard(expression).unwrap();
    println!("生成的字节码指令:");
    for (i, opcode) in compiled.bytecode().iter().enumerate() {
        println!("  {}: {:?}", i, opcode);
    }

    // 执行演示
    let context = json!({"a": 10, "b": 20, "c": 3, "d": 8});
    println!("执行上下文: {}", context);

    let result = compiled.evaluate(context.into()).unwrap();
    println!("计算结果: {:?}", result);
    println!("验证: (10 + 20) * 3 - 8 / 2 = 30 * 3 - 4 = 90 - 4 = 86\n");
}

fn demo_performance_execution() {
    println!("【演示3: 高性能重复执行】");

    let context = json!({
        "items": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
        "factor": 0.1
    });

    let mut isolate = Isolate::with_environment(context.into());

    // 预编译表达式
    let expression = "sum(items) * factor";
    let compiled = isolate.compile_standard(expression).unwrap();

    println!("表达式: {}", expression);
    println!("预编译完成，开始高性能重复执行...");

    // 模拟高频执行
    let iterations = 100_000;
    let start = std::time::Instant::now();

    for _ in 0..iterations {
        // 重复使用预编译的字节码，VM重用内存
        let _result = isolate.run_standard(expression).unwrap();
    }

    let duration = start.elapsed();
    println!("执行 {} 次耗时: {:?}", iterations, duration);
    println!("平均每次执行: {:?}", duration / iterations);
    println!(
        "每秒执行次数: {:.0}\n",
        iterations as f64 / duration.as_secs_f64()
    );
}

fn demo_data_types() {
    println!("【演示4: 不同数据类型处理】");

    let context = json!({
        "user": {
            "name": "Alice",
            "age": 25,
            "active": true,
            "scores": [85, 92, 78, 96]
        },
        "settings": {
            "threshold": 80
        }
    });

    let mut isolate = Isolate::with_environment(context.into());

    let test_cases = vec![
        (
            "user.name + \" is \" + string(user.age) + \" years old\"",
            "字符串拼接",
        ),
        ("user.age >= 18", "布尔运算"),
        ("len(user.scores)", "数组长度"),
        ("max(user.scores)", "数组最大值"),
        ("avg(user.scores) > settings.threshold", "数组平均值比较"),
    ];

    for (expr, desc) in test_cases {
        let result = isolate.run_standard(expr).unwrap();
        println!("{}: {} = {:?}", desc, expr, result);
    }
    println!();
}

fn demo_intervals_and_conditions() {
    println!("【演示5: 区间和条件表达式】");

    let context = json!({
        "student": {
            "age": 20,
            "score": 85,
            "grade": "B+"
        },
        "rules": {
            "adult_age": 18,
            "passing_score": 60,
            "excellent_score": 90
        }
    });

    let mut isolate = Isolate::with_environment(context.into());

    let test_expressions = vec![
        ("student.age >= rules.adult_age", "成年判断"),
        ("student.score in [rules.passing_score..100]", "及格区间判断"),
        ("student.score in [rules.excellent_score..100]", "优秀区间判断"),
        (
            "student.score in (rules.passing_score..rules.excellent_score)",
            "良好区间判断（开区间）",
        ),
        (
            "student.age in [18..65) and student.score >= rules.passing_score",
            "复合条件",
        ),
    ];

    for (expr, desc) in test_expressions {
        let result = isolate.run_standard(expr).unwrap();
        println!("{}: {} = {:?}", desc, expr, result);
    }

    // 演示区间转换为数组
    println!("\n区间数组转换演示:");
    let range_expr = "[1..5]";
    let compiled = isolate.compile_standard(range_expr).unwrap();
    println!("区间表达式: {}", range_expr);
    println!("字节码: {:?}", compiled.bytecode());

    let result = compiled.evaluate(Variable::empty_object()).unwrap();
    println!("区间结果: {:?}", result);
}
