//! State 守卫模块
//!
//! 提供 RAII 模式的 State 管理，确保异常安全

use std::sync::Arc;
use super::mf_function::MfFunctionRegistry;
use std::marker::PhantomData;

/// State 守卫，使用 RAII 模式自动管理 State 的设置和清理
///
/// 当 StateGuard 被创建时，会自动设置当前线程的 State 上下文
/// 当 StateGuard 被丢弃时（包括异常情况），会自动清理 State 上下文
///
/// # 示例
/// ```rust,ignore
/// use std::sync::Arc;
/// use mf_state::State;
/// use mf_rules_expression::functions::StateGuard;
///
/// // 创建 State
/// let state = Arc::new(State::default());
///
/// {
///     // 设置 State 上下文
///     let _guard = StateGuard::new(state);
///     
///     // 在这个作用域内，自定义函数可以访问 State
///     // 即使发生 panic，State 也会被正确清理
///     
/// } // 这里 StateGuard 被自动丢弃，State 上下文被清理
/// ```
pub struct StateGuard<S> {
    _private: PhantomData<S>,
}

impl<S: Send + Sync + 'static> StateGuard<S> {
    /// 创建新的 State 守卫
    ///
    /// # 参数
    /// * `state` - 要设置的 State 对象
    ///
    /// # 返回值
    /// 返回 StateGuard 实例，当其被丢弃时会自动清理 State
    pub fn new(state: Arc<S>) -> Self {
        MfFunctionRegistry::set_current_state(Some(state));
        Self { _private: PhantomData }
    }

    /// 创建空的 State 守卫（用于清理已有的 State）
    ///
    /// # 返回值
    /// 返回 StateGuard 实例，会立即清理当前 State 并在丢弃时保持清理状态
    pub fn empty() -> Self {
        MfFunctionRegistry::clear_current_state();
        Self { _private: PhantomData }
    }

    /// 获取当前是否有活跃的 State
    ///
    /// # 返回值
    /// * `true` - 当前线程有活跃的 State
    /// * `false` - 当前线程没有 State
    pub fn has_active_state() -> bool {
        MfFunctionRegistry::has_current_state()
    }
}

impl<S> Drop for StateGuard<S> {
    /// 自动清理 State 上下文
    ///
    /// 当 StateGuard 被丢弃时（正常情况或异常情况），
    /// 会自动清理当前线程的 State 上下文
    fn drop(&mut self) {
        MfFunctionRegistry::clear_current_state();
    }
}

/// 便利宏，用于在指定作用域内设置 State
///
/// # 示例
/// ```rust,ignore
/// use mf_rules_expression::with_state;
///
/// let state = Arc::new(State::default());
///
/// with_state!(state => {
///     // 在这个块内，State 是活跃的
/// });
/// // State 在这里已经被清理
/// ```
#[macro_export]
macro_rules! with_state {
    ($state:expr => $block:block) => {{
        let _guard = $crate::functions::StateGuard::new($state);
        $block
    }};
}

/// 异步版本的 State 守卫便利函数
///
/// # 参数
/// * `state` - 要设置的 State 对象
/// * `future` - 要在 State 上下文中执行的异步操作
///
/// # 返回值
/// 返回异步操作的结果
///
/// # 示例
/// ```rust,ignore
/// use mf_rules_expression::functions::with_state_async;
///
/// let state = Arc::new(State::default());
///
/// let result = with_state_async(state, async {
///     // aync block
/// }).await;
/// ```
pub async fn with_state_async<S, T, F, Fut>(
    state: Arc<S>,
    future: F,
) -> T
where
    S: Send + Sync + 'static,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let _guard = StateGuard::new(state);
    future().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // A dummy struct for testing purposes
    struct DummyState;

    #[test]
    fn test_state_guard_basic() {
        // 初始状态应该没有 State
        assert!(!StateGuard::<DummyState>::has_active_state());

        {
            // 创建一个模拟的 State
            let state = Arc::new(DummyState);
            let _guard = StateGuard::new(state);

            // 在这个作用域内应该有活跃的 State
            assert!(StateGuard::<DummyState>::has_active_state());
        }

        // 离开作用域后，State 应该被清理
        assert!(!StateGuard::<DummyState>::has_active_state());
    }

    #[test]
    fn test_state_guard_panic_safety() {
        assert!(!StateGuard::<DummyState>::has_active_state());

        let result = std::panic::catch_unwind(|| {
            let state = Arc::new(DummyState);
            let _guard = StateGuard::new(state);

            // 模拟 panic
            panic!("测试 panic 安全性");
        });

        // 即使发生了 panic，State 也应该被正确清理
        assert!(!StateGuard::<DummyState>::has_active_state());
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_guard() {
        let state = Arc::new(DummyState);
        let _guard = StateGuard::new(state);
        assert!(StateGuard::<DummyState>::has_active_state());

        // 创建空守卫应该立即清理 State
        let _guard_empty = StateGuard::<DummyState>::empty();
        assert!(!StateGuard::<DummyState>::has_active_state());
    }
}
