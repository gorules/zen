using System;
using System.Runtime.InteropServices;
using System.Text.Json;
using GoRules.Zen.Interop;

namespace GoRules.Zen
{
    /// <summary>
    /// Exception thrown when Zen engine operations fail
    /// </summary>
    public class ZenException : Exception
    {
        public ZenErrorCode ErrorCode { get; }
        public string? Details { get; }

        public ZenException(ZenErrorCode errorCode, string? details = null)
            : base(FormatMessage(errorCode, details))
        {
            ErrorCode = errorCode;
            Details = details;
        }

        private static string FormatMessage(ZenErrorCode code, string? details)
        {
            var message = code switch
            {
                ZenErrorCode.InvalidArgument => "Invalid argument",
                ZenErrorCode.StringNullError => "Null string error",
                ZenErrorCode.StringUtf8Error => "UTF-8 encoding error",
                ZenErrorCode.JsonSerializationFailed => "JSON serialization failed",
                ZenErrorCode.JsonDeserializationFailed => "JSON deserialization failed",
                ZenErrorCode.IsolateError => "JavaScript isolate error",
                ZenErrorCode.EvaluationError => "Evaluation error",
                ZenErrorCode.LoaderKeyNotFound => "Decision key not found",
                ZenErrorCode.LoaderInternalError => "Loader internal error",
                ZenErrorCode.TemplateEngineError => "Template engine error",
                _ => $"Unknown error (code: {(int)code})"
            };

            return details != null ? $"{message}: {details}" : message;
        }
    }

    /// <summary>
    /// Options for decision evaluation
    /// </summary>
    public class EvaluationOptions
    {
        /// <summary>
        /// Enable execution trace for debugging
        /// </summary>
        public bool Trace { get; set; } = false;

        /// <summary>
        /// Maximum recursion depth (default: 5)
        /// </summary>
        public byte MaxDepth { get; set; } = 5;

        internal ZenEngineEvaluationOptions ToNative() => new()
        {
            trace = Trace,
            max_depth = MaxDepth
        };
    }

    /// <summary>
    /// Delegate for loading decision content by key
    /// </summary>
    /// <param name="key">Decision identifier</param>
    /// <returns>JSON decision content, or null if not found</returns>
    public delegate string? DecisionLoaderDelegate(string key);

    /// <summary>
    /// Delegate for handling custom nodes
    /// </summary>
    /// <param name="request">JSON request object</param>
    /// <returns>JSON response object</returns>
    public delegate string CustomNodeDelegate(string request);

    /// <summary>
    /// A compiled decision that can be evaluated multiple times
    /// </summary>
    public class ZenDecision : IDisposable
    {
        private IntPtr _handle;
        private bool _disposed;

        internal ZenDecision(IntPtr handle)
        {
            _handle = handle;
        }

        /// <summary>
        /// Evaluate the decision with the given context
        /// </summary>
        /// <param name="context">JSON context object</param>
        /// <param name="options">Evaluation options</param>
        /// <returns>JSON result</returns>
        public string Evaluate(string context, EvaluationOptions? options = null)
        {
            ThrowIfDisposed();
            options ??= new EvaluationOptions();

            var result = ZenNative.zen_decision_evaluate(_handle, context, options.ToNative());
            return ResultHelper.ExtractString(result);
        }

        /// <summary>
        /// Evaluate the decision with a typed context
        /// </summary>
        public TResult Evaluate<TContext, TResult>(TContext context, EvaluationOptions? options = null)
        {
            var contextJson = JsonSerializer.Serialize(context);
            var resultJson = Evaluate(contextJson, options);
            return JsonSerializer.Deserialize<TResult>(resultJson)
                ?? throw new ZenException(ZenErrorCode.JsonDeserializationFailed, "Result was null");
        }

        private void ThrowIfDisposed()
        {
            if (_disposed)
                throw new ObjectDisposedException(nameof(ZenDecision));
        }

        public void Dispose()
        {
            if (!_disposed && _handle != IntPtr.Zero)
            {
                ZenNative.zen_decision_free(_handle);
                _handle = IntPtr.Zero;
                _disposed = true;
            }
            GC.SuppressFinalize(this);
        }

        ~ZenDecision()
        {
            Dispose();
        }
    }

    /// <summary>
    /// Zen Rules Engine - evaluates business rules and decisions
    /// </summary>
    public class ZenEngine : IDisposable
    {
        private IntPtr _handle;
        private bool _disposed;

        // Keep delegates alive to prevent GC
        private readonly ZenDecisionLoaderCallback? _nativeLoaderCallback;
        private readonly ZenCustomNodeCallback? _nativeCustomNodeCallback;
        private readonly DecisionLoaderDelegate? _loaderDelegate;
        private readonly CustomNodeDelegate? _customNodeDelegate;

        /// <summary>
        /// Create a new ZenEngine without callbacks
        /// </summary>
        public ZenEngine()
        {
            _handle = ZenNative.zen_engine_new();
            if (_handle == IntPtr.Zero)
                throw new InvalidOperationException("Failed to create ZenEngine");
        }

        /// <summary>
        /// Create a new ZenEngine with optional callbacks
        /// </summary>
        /// <param name="loader">Callback to load decisions by key</param>
        /// <param name="customNode">Callback to handle custom nodes</param>
        public ZenEngine(DecisionLoaderDelegate? loader, CustomNodeDelegate? customNode = null)
        {
            _loaderDelegate = loader;
            _customNodeDelegate = customNode;

            if (loader != null)
                _nativeLoaderCallback = CreateNativeLoaderCallback(loader);

            if (customNode != null)
                _nativeCustomNodeCallback = CreateNativeCustomNodeCallback(customNode);

            _handle = ZenNative.zen_engine_new_native(_nativeLoaderCallback, _nativeCustomNodeCallback);
            if (_handle == IntPtr.Zero)
                throw new InvalidOperationException("Failed to create ZenEngine");
        }

        /// <summary>
        /// Create a decision from JSON content
        /// </summary>
        /// <param name="content">JSON decision definition</param>
        /// <returns>Compiled decision</returns>
        public ZenDecision CreateDecision(string content)
        {
            ThrowIfDisposed();

            var result = ZenNative.zen_engine_create_decision(_handle, content);
            var handle = ResultHelper.ExtractDecision(result);
            return new ZenDecision(handle);
        }

        /// <summary>
        /// Get a decision by key using the loader callback
        /// </summary>
        /// <param name="key">Decision identifier</param>
        /// <returns>Compiled decision</returns>
        public ZenDecision GetDecision(string key)
        {
            ThrowIfDisposed();

            var result = ZenNative.zen_engine_get_decision(_handle, key);
            var handle = ResultHelper.ExtractDecision(result);
            return new ZenDecision(handle);
        }

        /// <summary>
        /// Evaluate a decision by key
        /// </summary>
        /// <param name="key">Decision identifier</param>
        /// <param name="context">JSON context</param>
        /// <param name="options">Evaluation options</param>
        /// <returns>JSON result</returns>
        public string Evaluate(string key, string context, EvaluationOptions? options = null)
        {
            ThrowIfDisposed();
            options ??= new EvaluationOptions();

            var result = ZenNative.zen_engine_evaluate(_handle, key, context, options.ToNative());
            return ResultHelper.ExtractString(result);
        }

        /// <summary>
        /// Evaluate a decision with typed context and result
        /// </summary>
        public TResult Evaluate<TContext, TResult>(string key, TContext context, EvaluationOptions? options = null)
        {
            var contextJson = JsonSerializer.Serialize(context);
            var resultJson = Evaluate(key, contextJson, options);
            return JsonSerializer.Deserialize<TResult>(resultJson)
                ?? throw new ZenException(ZenErrorCode.JsonDeserializationFailed, "Result was null");
        }

        private static ZenDecisionLoaderCallback CreateNativeLoaderCallback(DecisionLoaderDelegate loader)
        {
            return (IntPtr keyPtr) =>
            {
                var result = new ZenDecisionLoaderResult();
                try
                {
                    var key = Marshal.PtrToStringUTF8(keyPtr) ?? "";
                    var content = loader(key);

                    if (content != null)
                        result.content = ZenNative.AllocateCString(content);
                    else
                        result.error = ZenNative.AllocateCString("Decision not found");
                }
                catch (Exception ex)
                {
                    result.error = ZenNative.AllocateCString(ex.Message);
                }
                return result;
            };
        }

        private static ZenCustomNodeCallback CreateNativeCustomNodeCallback(CustomNodeDelegate customNode)
        {
            return (IntPtr requestPtr) =>
            {
                var result = new ZenCustomNodeResult();
                try
                {
                    var request = Marshal.PtrToStringUTF8(requestPtr) ?? "{}";
                    var response = customNode(request);
                    result.content = ZenNative.AllocateCString(response);
                }
                catch (Exception ex)
                {
                    result.error = ZenNative.AllocateCString(ex.Message);
                }
                return result;
            };
        }

        private void ThrowIfDisposed()
        {
            if (_disposed)
                throw new ObjectDisposedException(nameof(ZenEngine));
        }

        public void Dispose()
        {
            if (!_disposed && _handle != IntPtr.Zero)
            {
                ZenNative.zen_engine_free(_handle);
                _handle = IntPtr.Zero;
                _disposed = true;
            }
            GC.SuppressFinalize(this);
        }

        ~ZenEngine()
        {
            Dispose();
        }
    }

    /// <summary>
    /// Static helper methods for expression and template evaluation
    /// </summary>
    public static class ZenExpression
    {
        /// <summary>
        /// Evaluate an expression with context
        /// </summary>
        /// <param name="expression">Expression string (e.g., "a + b")</param>
        /// <param name="context">JSON context (e.g., {"a": 1, "b": 2})</param>
        /// <returns>JSON result</returns>
        public static string Evaluate(string expression, string context)
        {
            var result = ZenNative.zen_evaluate_expression(expression, context);
            return ResultHelper.ExtractString(result);
        }

        /// <summary>
        /// Evaluate an expression with typed context
        /// </summary>
        public static TResult Evaluate<TContext, TResult>(string expression, TContext context)
        {
            var contextJson = JsonSerializer.Serialize(context);
            var resultJson = Evaluate(expression, contextJson);
            return JsonSerializer.Deserialize<TResult>(resultJson)
                ?? throw new ZenException(ZenErrorCode.JsonDeserializationFailed, "Result was null");
        }

        /// <summary>
        /// Evaluate a unary (boolean) expression
        /// </summary>
        /// <param name="expression">Boolean expression (e.g., "a > 10")</param>
        /// <param name="context">JSON context</param>
        /// <returns>Boolean result</returns>
        public static bool EvaluateUnary(string expression, string context)
        {
            var result = ZenNative.zen_evaluate_unary_expression(expression, context);
            return ResultHelper.ExtractBool(result);
        }

        /// <summary>
        /// Evaluate a unary expression with typed context
        /// </summary>
        public static bool EvaluateUnary<TContext>(string expression, TContext context)
        {
            var contextJson = JsonSerializer.Serialize(context);
            return EvaluateUnary(expression, contextJson);
        }

        /// <summary>
        /// Render a template with context
        /// </summary>
        /// <param name="template">Template string (e.g., "Hello {{ name }}")</param>
        /// <param name="context">JSON context</param>
        /// <returns>Rendered result</returns>
        public static string RenderTemplate(string template, string context)
        {
            var result = ZenNative.zen_evaluate_template(template, context);
            return ResultHelper.ExtractString(result);
        }

        /// <summary>
        /// Render a template with typed context
        /// </summary>
        public static string RenderTemplate<TContext>(string template, TContext context)
        {
            var contextJson = JsonSerializer.Serialize(context);
            return RenderTemplate(template, contextJson);
        }
    }

    /// <summary>
    /// Internal helper for extracting results and handling errors
    /// </summary>
    internal static class ResultHelper
    {
        public static string ExtractString(ZenResult_c_char result)
        {
            try
            {
                if (result.error != 0)
                {
                    var details = result.details != IntPtr.Zero
                        ? Marshal.PtrToStringUTF8(result.details)
                        : null;
                    throw new ZenException((ZenErrorCode)result.error, details);
                }

                return result.result != IntPtr.Zero
                    ? Marshal.PtrToStringUTF8(result.result) ?? ""
                    : "";
            }
            finally
            {
                ZenNative.FreeRustString(result.result);
                ZenNative.FreeRustString(result.details);
            }
        }

        public static IntPtr ExtractDecision(ZenResult_ZenDecisionStruct result)
        {
            try
            {
                if (result.error != 0)
                {
                    var details = result.details != IntPtr.Zero
                        ? Marshal.PtrToStringUTF8(result.details)
                        : null;
                    throw new ZenException((ZenErrorCode)result.error, details);
                }

                return result.result;
            }
            finally
            {
                ZenNative.FreeRustString(result.details);
            }
        }

        public static bool ExtractBool(ZenResult_c_int result)
        {
            try
            {
                if (result.error != 0)
                {
                    var details = result.details != IntPtr.Zero
                        ? Marshal.PtrToStringUTF8(result.details)
                        : null;
                    throw new ZenException((ZenErrorCode)result.error, details);
                }

                if (result.result == IntPtr.Zero)
                    return false;

                return Marshal.ReadInt32(result.result) != 0;
            }
            finally
            {
                ZenNative.FreeRustString(result.result);
                ZenNative.FreeRustString(result.details);
            }
        }
    }
}
