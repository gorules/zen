using System;
using System.Runtime.InteropServices;

namespace GoRules.Zen.Interop
{
    /// <summary>
    /// Result type for functions returning strings (char*)
    /// </summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct ZenResult_c_char
    {
        public IntPtr result;   // char* - JSON result on success
        public byte error;      // Error code (0 = success)
        public IntPtr details;  // char* - Error details JSON when error != 0
    }

    /// <summary>
    /// Result type for functions returning ZenDecisionStruct*
    /// </summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct ZenResult_ZenDecisionStruct
    {
        public IntPtr result;   // ZenDecisionStruct* on success
        public byte error;      // Error code (0 = success)
        public IntPtr details;  // char* - Error details JSON when error != 0
    }

    /// <summary>
    /// Result type for functions returning int* (unary expression)
    /// </summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct ZenResult_c_int
    {
        public IntPtr result;   // int* - 0 or 1
        public byte error;      // Error code (0 = success)
        public IntPtr details;  // char* - Error details JSON when error != 0
    }

    /// <summary>
    /// Options for decision evaluation
    /// </summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct ZenEngineEvaluationOptions
    {
        [MarshalAs(UnmanagedType.U1)]
        public bool trace;      // Enable execution trace
        public byte max_depth;  // Maximum recursion depth (0 = default)
    }

    /// <summary>
    /// Result returned from decision loader callback
    /// </summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct ZenDecisionLoaderResult
    {
        public IntPtr content;  // char* - JSON decision content (malloc'd)
        public IntPtr error;    // char* - Error message (malloc'd), NULL on success
    }

    /// <summary>
    /// Result returned from custom node callback
    /// </summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct ZenCustomNodeResult
    {
        public IntPtr content;  // char* - JSON response (malloc'd)
        public IntPtr error;    // char* - Error message (malloc'd), NULL on success
    }

    /// <summary>
    /// Error codes returned by the Zen engine
    /// </summary>
    public enum ZenErrorCode : byte
    {
        Success = 0,
        InvalidArgument = 1,
        StringNullError = 2,
        StringUtf8Error = 3,
        JsonSerializationFailed = 4,
        JsonDeserializationFailed = 5,
        IsolateError = 6,
        EvaluationError = 7,
        LoaderKeyNotFound = 8,
        LoaderInternalError = 9,
        TemplateEngineError = 10
    }

    /// <summary>
    /// Callback delegate for loading decisions by key
    /// </summary>
    /// <param name="key">Decision identifier</param>
    /// <returns>Result with JSON content or error</returns>
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    public delegate ZenDecisionLoaderResult ZenDecisionLoaderCallback(IntPtr key);

    /// <summary>
    /// Callback delegate for handling custom nodes
    /// </summary>
    /// <param name="request">JSON request string</param>
    /// <returns>Result with JSON response or error</returns>
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    public delegate ZenCustomNodeResult ZenCustomNodeCallback(IntPtr request);

    /// <summary>
    /// Native P/Invoke imports for zen_ffi library
    /// </summary>
    public static class ZenNative
    {
        private const string LibraryName = "zen_ffi";

        // ============================================================
        // Engine Lifecycle
        // ============================================================

        /// <summary>
        /// Create a new ZenEngine without callbacks
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr zen_engine_new();

        /// <summary>
        /// Create a new ZenEngine with native callbacks
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr zen_engine_new_native(
            ZenDecisionLoaderCallback? loader_callback,
            ZenCustomNodeCallback? custom_node_callback);

        /// <summary>
        /// Free a ZenEngine instance
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
        public static extern void zen_engine_free(IntPtr engine);

        // ============================================================
        // Decision Management
        // ============================================================

        /// <summary>
        /// Create a decision from JSON content
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
        public static extern ZenResult_ZenDecisionStruct zen_engine_create_decision(
            IntPtr engine,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string content);

        /// <summary>
        /// Get a decision by key (uses loader callback)
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
        public static extern ZenResult_ZenDecisionStruct zen_engine_get_decision(
            IntPtr engine,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string key);

        /// <summary>
        /// Free a decision instance
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
        public static extern void zen_decision_free(IntPtr decision);

        // ============================================================
        // Evaluation
        // ============================================================

        /// <summary>
        /// Evaluate a decision with context
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
        public static extern ZenResult_c_char zen_decision_evaluate(
            IntPtr decision,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string context,
            ZenEngineEvaluationOptions options);

        /// <summary>
        /// Evaluate a decision by key using loader
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
        public static extern ZenResult_c_char zen_engine_evaluate(
            IntPtr engine,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string key,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string context,
            ZenEngineEvaluationOptions options);

        // ============================================================
        // Expression Evaluation
        // ============================================================

        /// <summary>
        /// Evaluate an expression with context
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
        public static extern ZenResult_c_char zen_evaluate_expression(
            [MarshalAs(UnmanagedType.LPUTF8Str)] string expression,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string context);

        /// <summary>
        /// Evaluate a unary (boolean) expression
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
        public static extern ZenResult_c_int zen_evaluate_unary_expression(
            [MarshalAs(UnmanagedType.LPUTF8Str)] string expression,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string context);

        /// <summary>
        /// Render a template with context
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
        public static extern ZenResult_c_char zen_evaluate_template(
            [MarshalAs(UnmanagedType.LPUTF8Str)] string template,
            [MarshalAs(UnmanagedType.LPUTF8Str)] string context);

        // ============================================================
        // Memory Management Helpers
        // ============================================================

        /// <summary>
        /// Free a C string (cross-platform)
        /// </summary>
        [DllImport("libc", EntryPoint = "free", CallingConvention = CallingConvention.Cdecl)]
        private static extern void free_libc(IntPtr ptr);

        [DllImport("msvcrt", EntryPoint = "free", CallingConvention = CallingConvention.Cdecl)]
        private static extern void free_msvcrt(IntPtr ptr);

        /// <summary>
        /// Allocate memory for callback return strings
        /// </summary>
        [DllImport("libc", EntryPoint = "malloc", CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr malloc_libc(nuint size);

        [DllImport("msvcrt", EntryPoint = "malloc", CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr malloc_msvcrt(nuint size);

        /// <summary>
        /// Free a pointer allocated by the Rust library
        /// </summary>
        public static void FreeRustString(IntPtr ptr)
        {
            if (ptr == IntPtr.Zero) return;

            if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
                free_msvcrt(ptr);
            else
                free_libc(ptr);
        }

        /// <summary>
        /// Allocate memory for callback strings (must use matching allocator)
        /// </summary>
        public static IntPtr AllocateCString(string? value)
        {
            if (value == null) return IntPtr.Zero;

            var bytes = System.Text.Encoding.UTF8.GetBytes(value);
            var size = (nuint)(bytes.Length + 1);

            IntPtr ptr;
            if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
                ptr = malloc_msvcrt(size);
            else
                ptr = malloc_libc(size);

            if (ptr == IntPtr.Zero)
                throw new OutOfMemoryException("Failed to allocate native memory");

            Marshal.Copy(bytes, 0, ptr, bytes.Length);
            Marshal.WriteByte(ptr, bytes.Length, 0); // null terminator

            return ptr;
        }
    }
}
