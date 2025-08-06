//! # 自定义函数监听器模块
//!
//! 本模块实现了CustomListener，用于在JavaScript运行时环境中注册和管理自定义函数。
//!
//! ## 主要功能
//!
//! - **函数注册**: 在运行时启动时自动将Rust自定义函数注册到JavaScript的md命名空间
//! - **命名空间管理**: 创建和管理md作用域，避免全局命名冲突
//! - **类型转换**: 处理Rust和JavaScript之间的数据类型转换
//! - **异步支持**: 提供异步函数调用支持，确保不阻塞JavaScript执行
//! - **错误处理**: 完善的错误捕获和处理机制
//!
//! ## 使用场景
//!
//! 该监听器主要用于规则引擎中，允许在规则表达式中通过`md.functionName()`的形式
//! 调用预定义的Rust函数，从而扩展JavaScript运行时的功能。
//!
//! ## 架构说明
//!
//! ```text
//! MfFunctionRegistry → CustomListener → JavaScript Runtime (md namespace)
//!        ↓                      ↓                    ↓
//!    函数定义存储           函数注册处理          md.functionName() 调用执行
//! ```

use std::future::Future;
use std::pin::Pin;
use crate::handler::function::error::{FunctionResult, ResultExt};
use crate::handler::function::listener::{RuntimeEvent, RuntimeListener};
use crate::handler::function::module::export_default;
use crate::handler::function::serde::JsValue;
use rquickjs::module::{Declarations, Exports, ModuleDef};
use rquickjs::prelude::{Async, Func};
use rquickjs::{CatchResultExt, Ctx};
use zen_expression::functions::arguments::Arguments;
use zen_expression::functions::mf_function::MfFunctionRegistry;

/// 自定义函数监听器
///
/// 该监听器负责在JavaScript运行时启动时，将所有注册的自定义函数
/// 绑定到JavaScript的md命名空间中，使得这些函数可以在规则表达式中通过
/// `md.functionName()`的形式被调用
///
/// # 工作流程
/// 1. 监听运行时启动事件
/// 2. 创建或获取md命名空间对象
/// 3. 从CustomFunctionRegistry获取所有已注册的函数
/// 4. 将每个函数包装为异步JavaScript函数
/// 5. 注册到JavaScript的md命名空间中
pub struct ModuforgeListener {
    // 目前为空结构体，后续可以添加配置或状态字段
}

impl RuntimeListener for ModuforgeListener {
    /// 处理运行时事件的核心方法
    ///
    /// # 参数
    /// - `ctx`: QuickJS上下文，用于操作JavaScript环境
    /// - `event`: 运行时事件类型
    ///
    /// # 返回值
    /// 返回一个异步Future，包含操作结果
    fn on_event<'js>(
        &self,
        ctx: Ctx<'js>,
        event: RuntimeEvent,
    ) -> Pin<Box<dyn Future<Output = FunctionResult> + 'js>> {
        Box::pin(async move {
            // 只在运行时启动事件时执行函数注册
            if event != RuntimeEvent::Startup {
                return Ok(());
            };

            // 设置全局函数及变量
            // 创建或获取 md 命名空间对象
            let md_namespace = if ctx.globals().contains_key("md")? {
                // 如果 md 已存在，获取它
                ctx.globals().get("md")?
            } else {
                // 如果 md 不存在，创建一个新的空对象
                let md_obj = rquickjs::Object::new(ctx.clone())?;
                ctx.globals().set("md", md_obj.clone())?;
                md_obj
            };

            // 从自定义函数注册表中获取所有函数名称
            let functions_keys = MfFunctionRegistry::list_functions();

            // 遍历每个注册的函数
            for function_key in functions_keys {
                // 根据函数名获取函数定义
                let function_definition =
                    MfFunctionRegistry::get_definition(&function_key);

                if let Some(function_definition) = function_definition {
                    // 将Rust函数包装为JavaScript异步函数并注册到md命名空间下

                    let function_definition = function_definition.clone();
                    let parameters = function_definition.required_parameters();
                    match parameters {
                        0 => {
                            md_namespace
                                .set(
                                    function_key, // 函数名作为md对象的属性名
                                    Func::from(Async(move |ctx: Ctx<'js>| {
                                        // 克隆函数定义以避免生命周期问题
                                        let function_definition =
                                            function_definition.clone();

                                        async move {
                                            // 调用Rust函数，传入JavaScript参数
                                            let response = function_definition
                                                .call(Arguments(&[]))
                                                .or_throw(&ctx)?;

                                            // 将Rust函数的返回值序列化为JSON，再转换为JavaScript值
                                            let k =
                                                serde_json::to_value(response)
                                                    .or_throw(&ctx)?
                                                    .into();

                                            return rquickjs::Result::Ok(
                                                JsValue(k),
                                            );
                                        }
                                    })),
                                )
                                .catch(&ctx)?; // 捕获并处理可能的JavaScript异常
                        },
                        1 => {
                            md_namespace
                            .set(
                                function_key, // 函数名作为md对象的属性名
                                Func::from(Async(
                                    move |ctx: Ctx<'js>, context: JsValue| {
                                        // 克隆函数定义以避免生命周期问题
                                        let function_definition =
                                            function_definition.clone();
                                        async move {
                                            // 调用Rust函数，传入JavaScript参数
                                            let response = function_definition
                                                .call(Arguments(&[context.0]))
                                                .or_throw(&ctx)?;
                                            // 将Rust函数的返回值序列化为JSON，再转换为JavaScript值
                                            let k = serde_json::to_value(response)
                                                .or_throw(&ctx)?
                                                .into();
                                            return rquickjs::Result::Ok(JsValue(
                                                k,
                                            ));
                                        }
                                    },
                                )),
                            )
                            .catch(&ctx)?; // 捕获并处理可能的JavaScript异常
                        },
                        2 => {
                            md_namespace
                            .set(
                                function_key, // 函数名作为md对象的属性名
                                Func::from(Async(
                                    move |ctx: Ctx<'js>, context: JsValue,context2: JsValue| {
                                        // 克隆函数定义以避免生命周期问题
                                        let function_definition =
                                            function_definition.clone();
                                        async move {
                                            // 调用Rust函数，传入JavaScript参数
                                            let response = function_definition
                                                .call(Arguments(&[context.0,context2.0]))
                                                .or_throw(&ctx)?;
                                            // 将Rust函数的返回值序列化为JSON，再转换为JavaScript值
                                            let k = serde_json::to_value(response)
                                                .or_throw(&ctx)?
                                                .into();
                                            return rquickjs::Result::Ok(JsValue(
                                                k,
                                            ));
                                        }
                                    },
                                )),
                            )
                            .catch(&ctx)?; // 捕获并处理可能的JavaScript异常
                        },
                        3 => {
                            md_namespace
                            .set(
                                function_key, // 函数名作为md对象的属性名
                                Func::from(Async(
                                    move |ctx: Ctx<'js>, context: JsValue,context2: JsValue,context3: JsValue| {
                                        // 克隆函数定义以避免生命周期问题
                                        let function_definition =
                                            function_definition.clone();
                                        async move {
                                            // 调用Rust函数，传入JavaScript参数
                                            let response = function_definition
                                                .call(Arguments(&[context.0,context2.0,context3.0]))
                                                .or_throw(&ctx)?;
                                            // 将Rust函数的返回值序列化为JSON，再转换为JavaScript值
                                            let k: zen_expression::Variable = serde_json::to_value(response)
                                                .or_throw(&ctx)?
                                                .into();
                                            return rquickjs::Result::Ok(JsValue(
                                                k,
                                            ));
                                        }
                                    },
                                )),
                            )
                            .catch(&ctx)?; // 捕获并处理可能的JavaScript异常
                        },
                        _ => {
                            md_namespace
                            .set(
                                function_key, // 函数名作为md对象的属性名
                                Func::from(Async(
                                    move |ctx: Ctx<'js>, context: Vec<JsValue>| {
                                        // 克隆函数定义以避免生命周期问题
                                        let function_definition =
                                            function_definition.clone();
                                        async move {
                                            // 调用Rust函数，传入JavaScript参数
                                            let response = function_definition
                                                .call(Arguments(&context.iter().map(|arg| arg.0.clone()).collect::<Vec<_>>()))
                                                .or_throw(&ctx)?;
                                            // 将Rust函数的返回值序列化为JSON，再转换为JavaScript值
                                            let k = serde_json::to_value(response)
                                                .or_throw(&ctx)?
                                                .into();
                                            return rquickjs::Result::Ok(JsValue(
                                                k,
                                            ));
                                        }
                                    },
                                )),
                            )
                            .catch(&ctx)?; // 捕获并处理可能的JavaScript异常
                        },
                    }
                }
            }

            Ok(()) // 成功完成函数注册
        })
    }
}

pub struct ModuforgeModule;

impl ModuleDef for ModuforgeModule {
    fn declare<'js>(decl: &Declarations<'js>) -> rquickjs::Result<()> {
        // 声明所有可用的函数
        for function_key in MfFunctionRegistry::list_functions() {
            decl.declare(function_key.as_str())?;
        }
        decl.declare("default")?;
        Ok(())
    }

    fn evaluate<'js>(
        ctx: &Ctx<'js>,
        exports: &Exports<'js>,
    ) -> rquickjs::Result<()> {
        export_default(ctx, exports, |default| {
            // 为每个函数创建对应的异步函数
            for function_key in MfFunctionRegistry::list_functions() {
                if let Some(function_definition) =
                    MfFunctionRegistry::get_definition(&function_key)
                {
                    let function_definition = function_definition.clone();
                    let parameters = function_definition.required_parameters();
                    match parameters {
                        0 => {
                            default.set(
                                &function_key,
                                Func::from(Async(move |ctx: Ctx<'js>| {
                                    let function_definition =
                                        function_definition.clone();
                                    async move {
                                        let response = function_definition
                                            .call(Arguments(&[]))
                                            .or_throw(&ctx)?;

                                        let result =
                                            serde_json::to_value(response)
                                                .or_throw(&ctx)?
                                                .into();

                                        Ok::<JsValue, rquickjs::Error>(JsValue(
                                            result,
                                        ))
                                    }
                                })),
                            )?;
                        },
                        1 => {
                            //只有一个参数
                            default.set(
                                &function_key,
                                Func::from(Async(
                                    move |ctx: Ctx<'js>, args: JsValue| {
                                        let function_definition =
                                            function_definition.clone();
                                        async move {
                                            let response = function_definition
                                                .call(Arguments(&[args.0]))
                                                .or_throw(&ctx)?;

                                            let result =
                                                serde_json::to_value(response)
                                                    .or_throw(&ctx)?
                                                    .into();

                                            Ok::<JsValue, rquickjs::Error>(
                                                JsValue(result),
                                            )
                                        }
                                    },
                                )),
                            )?;
                        },
                        2 => {
                            //有两个参数
                            default.set(
                                &function_key,
                                Func::from(Async(
                                    move |ctx: Ctx<'js>, args: JsValue,args2: JsValue| {
                                        let function_definition =
                                            function_definition.clone();
                                        async move {
                                            let response = function_definition
                                                .call(Arguments(&[args.0,args2.0]))
                                                .or_throw(&ctx)?;

                                            let result =
                                                serde_json::to_value(response)
                                                    .or_throw(&ctx)?
                                                    .into();

                                            Ok::<JsValue, rquickjs::Error>(
                                                JsValue(result),
                                            )
                                        }
                                    },
                                )),
                            )?;
                        },
                        3 => {
                            //有三个参数
                            default.set(
                                &function_key,
                                Func::from(Async(
                                    move |ctx: Ctx<'js>, args: JsValue,args2: JsValue,args3: JsValue| {
                                        let function_definition =
                                            function_definition.clone();
                                        async move {
                                            let response = function_definition
                                                .call(Arguments(&[args.0,args2.0,args3.0]))
                                                .or_throw(&ctx)?;

                                            let result =
                                                serde_json::to_value(response)
                                                    .or_throw(&ctx)?
                                                    .into();

                                            Ok::<JsValue, rquickjs::Error>(
                                                JsValue(result),
                                            )
                                        }
                                    },
                                )),
                            )?;
                        },
                        _ => {
                            //4个以上参数 的参数必须以数组的形式传入
                            default.set(
                                &function_key,
                                Func::from(Async(
                                    move |ctx: Ctx<'js>, args: Vec<JsValue>| {
                                        let function_definition =
                                            function_definition.clone();
                                        async move {
                                            let args_vec = args
                                                .iter()
                                                .map(|arg| arg.0.clone())
                                                .collect::<Vec<_>>();
                                            let response = function_definition
                                                .call(Arguments(&args_vec))
                                                .or_throw(&ctx)?;

                                            let result =
                                                serde_json::to_value(response)
                                                    .or_throw(&ctx)?
                                                    .into();

                                            Ok::<JsValue, rquickjs::Error>(
                                                JsValue(result),
                                            )
                                        }
                                    },
                                )),
                            )?;
                        },
                    }
                }
            }

            Ok(())
        })
    }
}
