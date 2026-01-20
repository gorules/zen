package io.gorules.zen.loader;

import java.util.Map;

/**
 * Functional interface for providing dynamic HTTP headers.
 * Headers can be computed at request time, allowing for dynamic values
 * like timestamps, request IDs, or rotating tokens.
 */
@FunctionalInterface
public interface HeaderProvider {

    /**
     * Provides headers for the HTTP request.
     * Called before each request, allowing dynamic header generation.
     *
     * @return Map of header names to values
     */
    Map<String, String> getHeaders();
}
