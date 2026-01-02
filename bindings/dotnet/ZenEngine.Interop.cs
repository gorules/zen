using System;
using System.IO;
using System.Reflection;
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

        /// <summary>
        /// Static constructor to register custom native library resolver
        /// </summary>
        static ZenNative()
        {
            NativeLibrary.SetDllImportResolver(typeof(ZenNative).Assembly, ResolveNativeLibrary);
        }

        /// <summary>
        /// Custom resolver for the native zen_ffi library.
        /// Searches in multiple locations to support consistent runtimes/{rid}/native/ structure across all platforms.
        /// </summary>
        private static IntPtr ResolveNativeLibrary(string libraryName, Assembly assembly, DllImportSearchPath? searchPath)
        {
            if (libraryName != LibraryName)
            {
                return IntPtr.Zero; // Let default resolver handle other libraries
            }

            // Determine platform-specific library name and runtime identifier
            string rid;
            string libFileName;

            if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
            {
                rid = RuntimeInformation.ProcessArchitecture == Architecture.Arm64 ? "win-arm64" : "win-x64";
                libFileName = "zen_ffi.dll";
            }
            else if (RuntimeInformation.IsOSPlatform(OSPlatform.Linux))
            {
                rid = RuntimeInformation.ProcessArchitecture == Architecture.Arm64 ? "linux-arm64" : "linux-x64";
                libFileName = "libzen_ffi.so";
            }
            else if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
            {
                rid = RuntimeInformation.ProcessArchitecture == Architecture.Arm64 ? "osx-arm64" : "osx-x64";
                libFileName = "libzen_ffi.dylib";
            }
            else
            {
                return IntPtr.Zero; // Unsupported platform
            }

            // Get base directories to search
            var assemblyLocation = Path.GetDirectoryName(assembly.Location) ?? AppContext.BaseDirectory;
            var baseDirectory = AppContext.BaseDirectory;

            // Build list of search paths
            var searchPaths = new[]
            {
                // 1. runtimes/{rid}/native/ relative to assembly location
                Path.Combine(assemblyLocation, "runtimes", rid, "native", libFileName),
                // 2. runtimes/{rid}/native/ relative to base directory
                Path.Combine(baseDirectory, "runtimes", rid, "native", libFileName),
                // 3. Direct in assembly directory (fallback)
                Path.Combine(assemblyLocation, libFileName),
                // 4. Direct in base directory (fallback)
                Path.Combine(baseDirectory, libFileName),
            };

            // Try each path
            foreach (var path in searchPaths)
            {
                if (File.Exists(path) && NativeLibrary.TryLoad(path, out var handle))
                {
                    return handle;
                }
            }

            // Try LD_LIBRARY_PATH / DYLD_LIBRARY_PATH
            var envVar = RuntimeInformation.IsOSPlatform(OSPlatform.OSX) 
                ? "DYLD_LIBRARY_PATH" 
                : "LD_LIBRARY_PATH";
            var libraryPath = Environment.GetEnvironmentVariable(envVar);

            if (!string.IsNullOrEmpty(libraryPath))
            {
                var paths = libraryPath.Split(Path.PathSeparator);
                foreach (var dir in paths)
                {
                    var fullPath = Path.Combine(dir, libFileName);
                    if (File.Exists(fullPath) && NativeLibrary.TryLoad(fullPath, out var handle))
                    {
                        return handle;
                    }
                }
            }

            // Let the default resolver try as last resort
            return IntPtr.Zero;
        }

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
        /// Allocate a string using Rust's allocator.
        /// The string must be freed using zen_free_string.
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr zen_alloc_string(IntPtr ptr, nuint len);

        /// <summary>
        /// Free a string allocated by Rust (zen_ffi library)
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
        public static extern void zen_free_string(IntPtr ptr);

        /// <summary>
        /// Free an integer pointer allocated by Rust (zen_ffi library)
        /// </summary>
        [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
        public static extern void zen_free_int(IntPtr ptr);

        /// <summary>
        /// Free a string pointer allocated by the Rust library.
        /// This uses zen_free_string which properly deallocates memory on both Windows and Linux.
        /// </summary>
        public static void FreeRustString(IntPtr ptr)
        {
            if (ptr == IntPtr.Zero) return;
            zen_free_string(ptr);
        }

        /// <summary>
        /// Free an integer pointer allocated by the Rust library.
        /// This uses zen_free_int which properly deallocates memory on both Windows and Linux.
        /// </summary>
        public static void FreeRustInt(IntPtr ptr)
        {
            if (ptr == IntPtr.Zero) return;
            zen_free_int(ptr);
        }

        /// <summary>
        /// Allocate memory for callback strings using Rust's allocator.
        /// This ensures proper memory management when strings are returned to Rust.
        /// </summary>
        public static IntPtr AllocateCString(string? value)
        {
            if (value == null) return IntPtr.Zero;

            var bytes = System.Text.Encoding.UTF8.GetBytes(value);
            
            // Use a temporary pinned buffer to pass to Rust
            unsafe
            {
                fixed (byte* pBytes = bytes)
                {
                    return zen_alloc_string((IntPtr)pBytes, (nuint)bytes.Length);
                }
            }
        }
    }
}
